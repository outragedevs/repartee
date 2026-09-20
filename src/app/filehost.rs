use crate::filehost::{Credentials, Filehost, MAX_UPLOAD_BYTES};
use crate::state::connection::ConnectionStatus;

pub struct UploadResult {
    buffer_id: String,
    connection_id: String,
    scope: String,
    result: Result<String, String>,
    response: Option<tokio::sync::oneshot::Sender<Result<String, String>>>,
}

pub fn command(app: &mut super::App, args: &[String]) {
    let result = start(app, args);
    if let Err(error) = result {
        crate::commands::helpers::add_local_event(app, &error);
    }
}

fn start(app: &mut super::App, args: &[String]) -> Result<(), String> {
    if app.submit_origin != super::translate::SubmitOrigin::Tui {
        return Err("Use the browser file picker to upload a local file".into());
    }
    if args.is_empty() || args.len() > 2 {
        return Err("Usage: /upload <path> [content-type]".into());
    }
    let buffer_id = app.state.active_buffer_id.as_deref().ok_or("No active conversation")?;
    let (host, credentials, mut result) = prepare(app, buffer_id)?;
    let path = std::path::PathBuf::from(&args[0]);
    let filename = path.file_name().and_then(|name| name.to_str()).ok_or("Invalid filename")?.to_string();
    let mime = args.get(1).cloned().unwrap_or_else(|| match path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("txt") => "text/plain",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }.into());
    let tx = app.upload_tx.clone();
    app.upload_pending = true;
    tokio::spawn(async move {
        result.result = upload_path(host, credentials, path, filename, mime).await;
        let _ = tx.send(result).await;
    });
    Ok(())
}

fn prepare(app: &super::App, buffer_id: &str) -> Result<(Filehost, Credentials, UploadResult), String> {
    if app.upload_pending {
        return Err("A file upload is already running".into());
    }
    let buffer = app.state.buffers.get(buffer_id).ok_or("No conversation")?;
    if !matches!(buffer.buffer_type, crate::state::buffer::BufferType::Channel | crate::state::buffer::BufferType::Query) {
        return Err("Select a channel or private conversation before uploading".into());
    }
    let conn = app.state.connections.get(&buffer.connection_id).ok_or("No connection")?;
    if conn.status != ConnectionStatus::Connected || !conn.server_owns_history() || conn.bouncer_control() {
        return Err("Uploads require a connected bouncer network".into());
    }
    let endpoint = conn.isupport_parsed.get("soju.im/FILEHOST").ok_or("The bouncer does not advertise FILEHOST")?;
    let host = Filehost::new(endpoint, conn.origin_config.tls).map_err(|error| error.to_string())?;
    let credentials = Credentials::from_config(&conn.origin_config).map_err(|error| error.to_string())?;
    let result = UploadResult { buffer_id: buffer.id.clone(), connection_id: conn.id.clone(),
        scope: conn.network_key().to_string(), response: None, result: Err("Upload did not finish".into()) };
    Ok((host, credentials, result))
}

async fn upload_path(host: Filehost, credentials: Credentials, path: std::path::PathBuf, filename: String, mime: String) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await.map_err(|_| "Cannot open upload file")?;
    let metadata = file.metadata().await.map_err(|_| "Cannot inspect upload file")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_UPLOAD_BYTES as u64 {
        return Err("Upload requires a regular file between 1 byte and 64 MiB".into());
    }
    let mut body = Vec::new();
    file.take(MAX_UPLOAD_BYTES as u64 + 1).read_to_end(&mut body).await.map_err(|_| "Cannot read upload file")?;
    host.upload(&credentials, &filename, &mime, body).await.map_err(|error| error.to_string())
}

impl super::App {
    pub(crate) fn start_web_upload(&mut self, request: crate::web::upload::Submission) {
        let (host, credentials, mut result) = match prepare(self, &request.buffer_id) {
            Ok(prepared) => prepared,
            Err(error) => { let _ = request.response.send(Err(error)); return; }
        };
        result.response = Some(request.response);
        self.upload_pending = true;
        let tx = self.upload_tx.clone();
        tokio::spawn(async move {
            result.result = host.upload(&credentials, &request.filename, &request.content_type, request.body)
                .await.map_err(|error| error.to_string());
            let _ = tx.send(result).await;
        });
    }

    pub(crate) fn finish_upload(&mut self, mut result: UploadResult) {
        self.upload_pending = false;
        if self.state.buffers.get(&result.buffer_id).is_none_or(|buffer| buffer.connection_id != result.connection_id)
            || self.state.connections.get(&result.connection_id).is_none_or(|conn| conn.network_key() != result.scope)
        {
            if let Some(response) = result.response.take() { let _ = response.send(Err("The upload conversation changed".into())); }
            return;
        }
        let web = result.response.is_some();
        if let Some(response) = result.response.take() { let _ = response.send(result.result.clone()); }
        match result.result {
            Ok(url) => {
                self.add_event_to_buffer(&result.buffer_id, format!("File uploaded: {}", crate::commands::helpers::escape_format(&url)));
                if !web && self.state.active_buffer_id.as_deref() == Some(&result.buffer_id) {
                    self.restore_input_text_to(&url, &super::translate::SubmitOrigin::Tui);
                }
            }
            Err(error) => self.add_event_to_buffer(&result.buffer_id, format!("Upload failed: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_command_cannot_read_daemon_files() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for origin in [super::super::translate::SubmitOrigin::Web("session".into()),
            super::super::translate::SubmitOrigin::Script] {
            app.submit_origin = origin;
            assert!(start(&mut app, &["/any/local/file".into()]).unwrap_err().contains("file picker"));
            assert!(!app.upload_pending);
        }
    }

    fn setup() -> (crate::app::App, UploadResult) {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='test'\naddress='localhost'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'").unwrap();
        app.setup_connection("test", &config);
        for name in ["#one", "#two"] {
            app.state.add_buffer(crate::state::buffer::Buffer::for_test("test", crate::state::buffer::BufferType::Channel, name));
        }
        let result = UploadResult { buffer_id: "test/#one".into(), connection_id: "test".into(),
            scope: app.state.connections["test"].network_key().into(),
            response: None, result: Ok("https://example.invalid/file.png".into()) };
        app.upload_pending = true;
        (app, result)
    }

    #[test]
    fn changing_conversation_during_upload_never_inserts_into_other_input() {
        let (mut app, result) = setup();
        app.state.set_active_buffer("test/#two");
        app.finish_upload(result);
        assert!(app.input.value.is_empty());
        assert!(!app.upload_pending);
        assert!(app.state.buffers["test/#one"].messages.back().unwrap().text.contains("https://example.invalid/file.png"));
    }

    #[test]
    fn changed_account_discards_the_old_upload_result() {
        let (mut app, result) = setup();
        app.state.connections.get_mut("test").unwrap().network_scope = Some("replacement".into());
        let before = app.state.buffers["test/#one"].messages.len();
        app.finish_upload(result);
        assert_eq!(app.state.buffers["test/#one"].messages.len(), before);
        assert!(app.input.value.is_empty());
    }
    #[test]
    fn encoded_upload_url_is_escaped_in_event_but_kept_raw_in_composer() {
        let (mut app, mut result) = setup();
        app.state.set_active_buffer("test/#one");
        let url = "https://example.invalid/my%20file%25.png";
        result.result = Ok(url.into());
        app.finish_upload(result);
        assert_eq!(app.input.value, url);
        assert_eq!(app.state.buffers["test/#one"].messages.back().unwrap().text,
            "File uploaded: https://example.invalid/my%%20file%%25.png");
    }

}
