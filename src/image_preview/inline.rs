use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::hash::{Hash, Hasher};

use image::{DynamicImage, ImageReader};
use ratatui::{Frame, layout::Rect, widgets::Paragraph};
use ratatui_image::{StatefulImage, picker::Picker, protocol::StatefulProtocol};
use tokio::sync::mpsc;

use super::{ImagePreviewEvent, detect::UrlType};
use crate::config::ImagePreviewConfig;

pub const ROWS: u16 = 8;
const COLS: u16 = 48;
const CACHE_LIMIT: usize = 32;
const FETCH_LIMIT: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub buffer_id: String,
    pub message_id: u64,
    pub url: String,
}

impl ImageKey {
    pub fn for_message(buffer_id: &str, message: &crate::state::buffer::Message) -> Option<Self> {
        let url = super::detect::extract_urls(&message.text)
            .into_iter()
            .find(|url| url.url_type == UrlType::DirectImage)?;
        Some(Self {
            buffer_id: buffer_id.to_string(),
            message_id: message.id,
            url: url.url,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub key: ImageKey,
    pub rect: Rect,
    pub first_row: u16,
}

pub fn placements(rows: &[Option<(ImageKey, u16)>], area: Rect) -> Vec<Placement> {
    let mut result: Vec<Placement> = Vec::new();
    for (y, row) in rows.iter().take(usize::from(area.height)).enumerate() {
        let Some((key, first_row)) = row else {
            continue;
        };
        if let Some(last) = result.last_mut()
            && last.key == *key
            && last.first_row + last.rect.height == *first_row
            && usize::from(last.rect.bottom() - area.y) == y
        {
            last.rect.height += 1;
            continue;
        }
        result.push(Placement {
            key: key.clone(),
            rect: Rect::new(
                area.x,
                area.y + u16::try_from(y).unwrap_or(0),
                area.width.min(COLS),
                1,
            ),
            first_row: *first_row,
        });
    }
    result
}

#[derive(PartialEq)]
struct Geometry {
    cols: u16,
    rows: u16,
    first_row: u16,
    font: (u16, u16),
    protocol: ratatui_image::picker::ProtocolType,
    background: (u8, u8, u8),
    tmux_direct: bool,
}

struct CachedProtocol {
    geometry: Geometry,
    image: StatefulProtocol,
    png: Vec<u8>,
    direct_rect: Option<Rect>,
}

enum Status {
    Loading(u64),
    Ready {
        image: Box<DynamicImage>,
        protocol: Option<Box<CachedProtocol>>,
    },
    Error(String),
}

struct Entry {
    status: Status,
    used: u64,
}

#[derive(Default)]
pub struct InlinePreviews {
    entries: HashMap<ImageKey, Entry>,
    pending: HashSet<u64>,
    next_request: u64,
    clock: u64,
    client: Option<reqwest::Client>,
    pub visible: bool,
    direct: Vec<(ImageKey, Rect)>,
    frame_key: Option<u64>,
    invalidated: bool,
    cleanup_protocol: Option<ratatui_image::picker::ProtocolType>,
}

pub struct RenderContext<'a> {
    pub picker: &'a Picker,
    pub config: &'a ImagePreviewConfig,
    pub tx: &'a mpsc::Sender<ImagePreviewEvent>,
    pub background: (u8, u8, u8),
    pub suppressed: bool,
    pub tmux_direct: bool,
}

pub fn frame_key(app: &crate::app::App, size: (u16, u16)) -> u64 {
    if !app.config.image_preview.inline && !app.inline_previews.visible {
        return 0;
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    size.hash(&mut hash);
    app.state.active_buffer_id.hash(&mut hash);
    app.scroll_offset.hash(&mut hash);
    app.wrap_indent.hash(&mut hash);
    app.input.value.matches('\n').count().min(5).hash(&mut hash);
    app.picker.font_size().hash(&mut hash);
    format!("{:?}", app.picker.protocol_type()).hash(&mut hash);
    app.in_tmux.hash(&mut hash);
    app.is_socket_attached.hash(&mut hash);
    app.config.image_preview.enabled.hash(&mut hash);
    app.config.image_preview.inline.hash(&mut hash);
    app.config.sidepanel.left.visible.hash(&mut hash);
    app.config.sidepanel.left.width.hash(&mut hash);
    app.config.sidepanel.right.visible.hash(&mut hash);
    app.config.sidepanel.right.width.hash(&mut hash);
    app.emotes_graphical().hash(&mut hash);
    app.config.emotes.max_cols.hash(&mut hash);
    app.config.emotes.max_rows.hash(&mut hash);
    app.theme.colors.bg.hash(&mut hash);
    (!matches!(app.image_preview, super::PreviewStatus::Hidden)).hash(&mut hash);
    app.wizard.is_some().hash(&mut hash);
    app.emote_picker.is_open().hash(&mut hash);
    if let Some(buffer) = app.state.active_buffer() {
        app.state.connections.get(&buffer.connection_id).map(|connection| &connection.nick).hash(&mut hash);
        buffer.messages.len().hash(&mut hash);
        buffer.messages.front().map(|message| message.id).hash(&mut hash);
        buffer.messages.back().map(|message| message.id).hash(&mut hash);
        let count = app.chat_rows.as_ref().map_or(0, |rows| rows.message_count(&buffer.id));
        for message in buffer.messages.iter().rev().take(count) {
            message.id.hash(&mut hash);
            message.timestamp.hash(&mut hash);
            message.message_type.as_str().hash(&mut hash);
            message.text.hash(&mut hash);
            message.highlight.hash(&mut hash);
            message.nick.hash(&mut hash);
            message.nick_mode.hash(&mut hash);
            message.event_key.hash(&mut hash);
            message.event_params.hash(&mut hash);
        }
    }
    hash.finish()
}

impl InlinePreviews {
    pub fn prepare_frame(&mut self, key: u64, force: bool) -> bool {
        let clear = self.visible && (force || self.invalidated || self.frame_key != Some(key));
        self.frame_key = Some(key);
        self.invalidated = false;
        clear
    }

    pub const fn finish_frame(&mut self, key: u64) {
        self.frame_key = Some(key);
        self.invalidated = false;
    }

    pub const fn invalidate_layout(&mut self) {
        self.invalidated = true;
    }

    pub fn clear_graphics(
        &mut self,
        out: &mut impl std::io::Write,
        protocol: ratatui_image::picker::ProtocolType,
        tmux: bool,
    ) {
        let protocol = self.cleanup_protocol.take().unwrap_or(protocol);
        if protocol == ratatui_image::picker::ProtocolType::Kitty {
            let sequence: &[u8] = if tmux {
                b"\x1bPtmux;\x1b\x1b_Ga=d,d=A,q=2\x1b\x1b\\\x1b\\"
            } else {
                b"\x1b_Ga=d,d=A,q=2\x1b\\"
            };
            let _ = out.write_all(sequence);
            let _ = out.flush();
        }
        self.invalidate_protocols();
    }

    pub fn write_direct(
        &self,
        out: &mut impl std::io::Write,
        protocol: ratatui_image::picker::ProtocolType,
    ) {
        for (key, rect) in &self.direct {
            let Some(Entry {
                status:
                    Status::Ready {
                        protocol: Some(cached),
                        ..
                    },
                ..
            }) = self.entries.get(key)
            else {
                continue;
            };
            match protocol {
                ratatui_image::picker::ProtocolType::Kitty => {
                    super::tmux::write_kitty(out, &cached.png, *rect);
                }
                ratatui_image::picker::ProtocolType::Iterm2 => {
                    super::tmux::write_iterm2(out, &cached.png, *rect);
                }
                _ => {}
            }
        }
    }

    pub fn invalidate_protocols(&mut self) {
        self.invalidate_layout();
        for entry in self.entries.values_mut() {
            if let Status::Ready { protocol, .. } = &mut entry.status {
                *protocol = None;
            }
        }
    }

    pub fn purge(&mut self) {
        self.entries.clear();
        self.direct.clear();
        self.invalidate_layout();
    }

    pub fn accept(&mut self, request: u64, result: Result<Box<DynamicImage>, String>) {
        self.pending.remove(&request);
        if let Some(entry) = self
            .entries
            .values_mut()
            .find(|entry| matches!(entry.status, Status::Loading(id) if id == request))
        {
            self.invalidated = true;
            entry.status = match result {
                Ok(image) => Status::Ready {
                    image,
                    protocol: None,
                },
                Err(error) => Status::Error(error),
            };
        }
    }

    fn request(
        &mut self,
        key: &ImageKey,
        visible: &HashSet<&ImageKey>,
        context: &RenderContext<'_>,
    ) {
        if self.entries.contains_key(key) || self.pending.len() >= FETCH_LIMIT {
            return;
        }
        if self.entries.len() >= CACHE_LIMIT {
            let oldest = self
                .entries
                .iter()
                .filter(|(key, _)| !visible.contains(key))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { return };
            self.entries.remove(&oldest);
        }
        self.next_request = self.next_request.wrapping_add(1);
        let request = self.next_request;
        self.pending.insert(request);
        self.entries.insert(
            key.clone(),
            Entry {
                status: Status::Loading(request),
                used: self.clock,
            },
        );
        let url = key.url.clone();
        let config = context.config.clone();
        if self.client.is_none() {
            match crate::web::preview::public_image_client() {
                Ok(client) => self.client = Some(client),
                Err(error) => {
                    self.accept(request, Err(error.to_string()));
                    return;
                }
            }
        }
        let client = self
            .client
            .as_ref()
            .expect("image client initialized")
            .clone();
        let tx = context.tx.clone();
        tokio::spawn(async move {
            let result = match super::fetch_image_data(
                &url,
                &config,
                &client,
                Some(crate::web::preview::validate_url_shape_str),
                super::FetchMode::Inline,
            )
            .await
            {
                Ok((bytes, _)) => tokio::task::spawn_blocking(move || decode_thumbnail(&bytes))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(std::convert::identity),
                Err(error) => Err(error.to_string()),
            };
            let _ = tx.send(ImagePreviewEvent::Inline { request, result }).await;
        });
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        placements: &[Placement],
        context: &RenderContext<'_>,
    ) {
        self.visible = false;
        self.direct.clear();
        if !context.config.enabled || !context.config.inline {
            self.entries.clear();
            return;
        }
        if context.suppressed {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        let visible: HashSet<&ImageKey> =
            placements.iter().map(|placement| &placement.key).collect();
        for placement in placements {
            self.request(&placement.key, &visible, context);
            let Some(entry) = self.entries.get_mut(&placement.key) else {
                frame.render_widget(Paragraph::new("Image queued..."), placement.rect);
                continue;
            };
            entry.used = self.clock;
            match &mut entry.status {
                Status::Loading(_) => {
                    frame.render_widget(Paragraph::new("Loading image..."), placement.rect);
                }
                Status::Error(error) => frame.render_widget(
                    Paragraph::new(format!("Image unavailable: {error}")),
                    placement.rect,
                ),
                Status::Ready { image, protocol } => {
                    let geometry = Geometry {
                        cols: placement.rect.width,
                        rows: placement.rect.height,
                        first_row: placement.first_row,
                        font: context.picker.font_size(),
                        protocol: context.picker.protocol_type(),
                        background: context.background,
                        tmux_direct: context.tmux_direct,
                    };
                    if protocol
                        .as_ref()
                        .is_none_or(|cached| cached.geometry != geometry)
                    {
                        let cropped = crop_thumbnail(
                            image,
                            placement,
                            context.picker.font_size(),
                            context.background,
                        );
                        let mut png = Cursor::new(Vec::new());
                        if context.tmux_direct
                            && cropped.write_to(&mut png, image::ImageFormat::Png).is_err()
                        {
                            frame.render_widget(
                                Paragraph::new("Image encoding failed"),
                                placement.rect,
                            );
                            continue;
                        }
                        *protocol = Some(Box::new(CachedProtocol {
                            png: png.into_inner(),
                            direct_rect: None,
                            geometry,
                            image: context.picker.new_resize_protocol(cropped),
                        }));
                    }
                    if let Some(cached) = protocol {
                        if context.tmux_direct {
                            if cached.direct_rect != Some(placement.rect) {
                                self.direct.push((placement.key.clone(), placement.rect));
                                cached.direct_rect = Some(placement.rect);
                            }
                        } else {
                            frame.render_stateful_widget(
                                StatefulImage::default(),
                                placement.rect,
                                &mut cached.image,
                            );
                        }
                        self.visible = true;
                        self.cleanup_protocol = Some(context.picker.protocol_type());
                    }
                }
            }
        }
    }
}

fn decode_thumbnail(bytes: &[u8]) -> Result<Box<DynamicImage>, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|error| error.to_string())?;
    Ok(Box::new(image.thumbnail(768, 384)))
}

fn crop_thumbnail(
    image: &DynamicImage,
    placement: &Placement,
    font: (u16, u16),
    background: (u8, u8, u8),
) -> DynamicImage {
    let fw = u32::from(font.0.clamp(1, 64));
    let fh = u32::from(font.1.clamp(1, 64));
    let width = u32::from(placement.rect.width.max(1)) * fw;
    let height = u32::from(ROWS) * fh;
    let fitted = image.thumbnail(width, height);
    let mut canvas = image::RgbaImage::from_pixel(
        width,
        height,
        image::Rgba([background.0, background.1, background.2, 255]),
    );
    image::imageops::overlay(&mut canvas, &fitted.to_rgba8(), 0, 0);
    DynamicImage::ImageRgba8(canvas).crop_imm(
        0,
        u32::from(placement.first_row) * fh,
        width,
        u32::from(placement.rect.height) * fh,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn redaction_purges_native_images_and_rejects_late_fetch_results() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for (id, status) in [
            (1, Status::Ready { image: Box::new(thumbnail()), protocol: None }),
            (2, Status::Loading(42)),
        ] {
            let key = ImageKey { buffer_id: "net/oldnick".into(), message_id: id,
                url: "https://example.invalid/private.png".into() };
            app.inline_previews.direct.push((key.clone(), Rect::new(0, 0, 8, 8)));
            app.inline_previews.entries.insert(key, Entry { status, used: 0 });
        }
        app.inline_previews.pending.insert(42);
        app.inline_previews.visible = true;
        app.state.pending_web_events.push(crate::web::protocol::WebEvent::RedactMessage {
            buffer_id: "net/newnick".into(), msgid: "opaque".into(), text: "Message deleted".into(),
        });
        app.drain_pending_web_events();
        assert!(app.inline_previews.entries.is_empty());
        assert!(app.inline_previews.direct.is_empty());
        assert!(app.inline_previews.prepare_frame(0, false));
        assert!(app.inline_previews.pending.contains(&42));
        app.inline_previews.accept(42, Ok(Box::new(thumbnail())));
        assert!(app.inline_previews.entries.is_empty());
        assert!(app.inline_previews.pending.is_empty());
    }

    #[tokio::test]
    async fn oversized_inline_frame_recovers_as_text_for_the_attachment() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use crate::session::protocol::MainMessage;
        use crate::session::writer::SocketWriter;
        use crate::state::buffer::{Buffer, BufferType};
        use crate::state::events::tests::make_test_message;

        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.config.image_preview.inline = true;
        app.state.add_buffer(Buffer::for_test("net", BufferType::Channel, "#images"));
        app.state.set_active_buffer("net/#images");
        let message = make_test_message(&mut app.state, "https://example.invalid/picture.png");
        let image_key = ImageKey::for_message("net/#images", &message).unwrap();
        app.state.add_local_message("net/#images", message);
        app.inline_previews.entries.insert(image_key, Entry {
            status: Status::Ready { image: Box::new(thumbnail()), protocol: None },
            used: 0,
        });
        app.picker = Picker::halfblocks();
        app.picker.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let writer = SocketWriter::new(tx, Arc::clone(&queued), 64 * 1024);
        app.socket_output = Some(writer.output());
        app.terminal = Some(crate::ui::setup_socket_terminal(Box::new(writer), 120, 60).unwrap());
        app.is_socket_attached = true;
        while let Ok(MainMessage::Output(bytes)) = rx.try_recv() {
            queued.fetch_sub(bytes.len(), Ordering::AcqRel);
        }
        assert!(app.render_terminal_frame());
        assert!(!app.should_detach);
        assert!(!app.terminal_graphics_enabled());
        assert!(!app.emotes_graphical());
        assert_eq!(queued.load(Ordering::Acquire), 0);
        assert!(rx.try_recv().is_err());
        app.image_preview = super::super::PreviewStatus::Hidden;
        assert!(app.render_terminal_frame());
        assert!(!app.should_detach);
        assert!(queued.load(Ordering::Acquire) > 0);
        assert!(!app.inline_previews.visible);
        assert!(app.config.image_preview.inline);
    }

    fn key(id: u64) -> ImageKey {
        ImageKey {
            buffer_id: "net/#images".into(),
            message_id: id,
            url: format!("http://127.0.0.1/image-{id}.png"),
        }
    }

    fn thumbnail() -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            480,
            160,
            image::Rgba([7, 211, 61, 255]),
        ))
    }

    #[tokio::test]
    async fn frame_key_scans_only_messages_needed_for_the_viewport() {
        use crate::state::buffer::{Buffer, BufferType};
        use crate::state::events::tests::make_test_message;
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.config.image_preview.inline = true;
        app.state.add_buffer(Buffer::for_test("net", BufferType::Channel, "#images"));
        app.state.set_active_buffer("net/#images");
        for _ in 0..1000 {
            let message = make_test_message(&mut app.state, "history");
            app.state.add_local_message("net/#images", message);
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| crate::ui::chat_view::render(frame, frame.area(), &mut app)).unwrap();
        assert!(app.chat_rows.as_ref().unwrap().message_count("net/#images") < 30);
        let initial = frame_key(&app, (80, 20));
        app.state.buffers.get_mut("net/#images").unwrap().messages.front_mut().unwrap().text = "older history changed".into();
        assert_eq!(frame_key(&app, (80, 20)), initial);
        app.state.buffers.get_mut("net/#images").unwrap().messages.back_mut().unwrap().text = "visible message changed".into();
        assert_ne!(frame_key(&app, (80, 20)), initial);
        let recent = frame_key(&app, (80, 20));
        let message = make_test_message(&mut app.state, "new message");
        app.state.add_local_message("net/#images", message);
        assert_ne!(frame_key(&app, (80, 20)), recent);
    }

    #[test]
    fn frame_changes_track_layout_but_ignore_typing_and_clock_wakes() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.config.image_preview.inline = true;
        let initial = frame_key(&app, (80, 24));
        app.input.value = "draft".into();
        assert_eq!(frame_key(&app, (80, 24)), initial);
        assert_ne!(frame_key(&app, (81, 24)), initial);
        app.input.value.push('\n');
        assert_ne!(frame_key(&app, (80, 24)), initial);
        app.input.value.clear();
        app.scroll_offset = 1;
        assert_ne!(frame_key(&app, (80, 24)), initial);
        let mut previews = InlinePreviews { visible: true, ..InlinePreviews::default() };
        previews.finish_frame(initial);
        assert!(!previews.prepare_frame(initial, false));
        previews.invalidate_layout();
        assert!(previews.prepare_frame(initial, false));
        previews.finish_frame(initial);
        assert!(!previews.prepare_frame(initial, false));
        assert!(previews.prepare_frame(initial, true));
    }

    #[test]
    fn runtime_protocol_refresh_preserves_the_old_cleanup_protocol() {
        use ratatui_image::picker::ProtocolType;
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.shim_term_env = Some(HashMap::new());
        app.config.image_preview.protocol = "kitty".into();
        app.refresh_image_protocol();
        assert_eq!(app.picker.protocol_type(), ProtocolType::Kitty);
        app.inline_previews.cleanup_protocol = Some(ProtocolType::Kitty);
        app.config.image_preview.protocol = "halfblocks".into();
        app.refresh_image_protocol();
        assert_eq!(app.picker.protocol_type(), ProtocolType::Halfblocks);
        let mut cleanup = Vec::new();
        app.inline_previews.clear_graphics(&mut cleanup, app.picker.protocol_type(), false);
        assert!(String::from_utf8(cleanup).unwrap().contains("_Ga=d,d=A"));
    }

    #[test]
    fn tmux_images_use_passthrough_and_cleanup_forces_retransmission() {
        use ratatui_image::picker::ProtocolType;
        let config = ImagePreviewConfig {
            inline: true,
            ..ImagePreviewConfig::default()
        };
        let (tx, _rx) = mpsc::channel(16);
        let placement = Placement {
            key: key(1),
            rect: Rect::new(2, 3, COLS, ROWS),
            first_row: 0,
        };
        for protocol in [ProtocolType::Kitty, ProtocolType::Iterm2] {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(protocol);
            let context = RenderContext {
                picker: &picker,
                config: &config,
                tx: &tx,
                background: (0, 0, 0),
                suppressed: false,
                tmux_direct: true,
            };
            let mut previews = InlinePreviews::default();
            previews.entries.insert(
                key(1),
                Entry {
                    status: Status::Ready {
                        image: Box::new(thumbnail()),
                        protocol: None,
                    },
                    used: 0,
                },
            );
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
            for _ in 0..2 {
                terminal
                    .draw(|frame| {
                        previews.render(frame, std::slice::from_ref(&placement), &context);
                    })
                    .unwrap();
                let mut output = Vec::new();
                previews.write_direct(&mut output, protocol);
                let output = String::from_utf8(output).unwrap();
                assert!(output.contains("\x1bPtmux;"));
                assert!(output.contains("\x1b[4;3H"));
                if protocol == ProtocolType::Kitty {
                    assert!(output.contains("a=T,f=100"));
                } else {
                    assert!(output.contains("]1337;File=inline=1"));
                }
                previews.finish_frame(123);
                for _ in 0..20 {
                    assert!(!previews.prepare_frame(123, false));
                    terminal.draw(|frame| previews.render(frame, std::slice::from_ref(&placement), &context)).unwrap();
                    let mut unchanged = Vec::new();
                    previews.write_direct(&mut unchanged, protocol);
                    assert!(unchanged.is_empty());
                    previews.finish_frame(123);
                }
                assert!(previews.prepare_frame(456, false));
                let mut cleanup = Vec::new();
                previews.clear_graphics(&mut cleanup, protocol, true);
                if protocol == ProtocolType::Kitty {
                    assert!(String::from_utf8(cleanup).unwrap().contains("_Ga=d,d=A"));
                }
                assert!(matches!(
                    &previews.entries[&key(1)].status,
                    Status::Ready { protocol: None, .. }
                ));
            }
        }
    }

    #[test]
    fn clipped_rows_stay_within_the_chat_viewport() {
        let rows: Vec<_> = (3..ROWS).map(|row| Some((key(1), row))).collect();
        let found = placements(&rows, Rect::new(4, 6, 80, 3));
        assert_eq!(
            found,
            vec![Placement {
                key: key(1),
                rect: Rect::new(4, 6, COLS, 3),
                first_row: 3
            }]
        );
        let source = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(480, 160, |_, y| {
            image::Rgba([u8::try_from(y / 20).unwrap(), 0, 0, 255])
        }));
        let cropped = crop_thumbnail(&source, &found[0], (10, 20), (0, 0, 0)).to_rgba8();
        assert_eq!(cropped.dimensions(), (480, 60));
        assert_eq!(cropped.get_pixel(0, 0).0[0], 3);
        assert_eq!(cropped.get_pixel(0, 59).0[0], 5);
    }

    #[test]
    fn oversized_image_dimensions_are_rejected_before_rendering() {
        let oversized = DynamicImage::new_rgb8(8193, 1);
        let mut bytes = Cursor::new(Vec::new());
        oversized
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert!(decode_thumbnail(bytes.get_ref()).is_err());
        assert!(decode_thumbnail(b"not an image").is_err());
    }

    #[tokio::test]
    async fn requests_are_bounded_and_private_urls_never_reach_the_network() {
        let mut previews = InlinePreviews::default();
        let picker = Picker::halfblocks();
        let config = ImagePreviewConfig {
            inline: true,
            ..ImagePreviewConfig::default()
        };
        let (tx, mut rx) = mpsc::channel(16);
        let context = RenderContext {
            picker: &picker,
            config: &config,
            tx: &tx,
            background: (0, 0, 0),
            suppressed: false,
            tmux_direct: false,
        };
        for id in 0..8 {
            previews.request(&key(id), &HashSet::new(), &context);
        }
        assert_eq!(previews.pending.len(), FETCH_LIMIT);
        assert_eq!(previews.entries.len(), FETCH_LIMIT);
        for _ in 0..FETCH_LIMIT {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            let ImagePreviewEvent::Inline { request, result } = event else {
                panic!("expected inline event")
            };
            assert!(result.is_err());
            previews.accept(request, result);
        }
        assert!(previews.pending.is_empty());
        for id in 0..u64::try_from(CACHE_LIMIT).unwrap() {
            previews.entries.insert(
                key(id),
                Entry {
                    status: Status::Error("fixture".into()),
                    used: id,
                },
            );
        }
        previews.request(&key(100), &HashSet::new(), &context);
        assert_eq!(previews.entries.len(), CACHE_LIMIT);
        assert!(!previews.entries.contains_key(&key(0)));
        previews.accept(0, Ok(Box::new(thumbnail())));
        assert!(!previews.entries.contains_key(&key(0)));
    }

    #[tokio::test]
    async fn long_image_history_reaches_the_oldest_message() {
        use crate::state::buffer::{Buffer, BufferType};
        use crate::state::events::tests::make_test_message;
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.config.image_preview.inline = true;
        app.state.add_buffer(Buffer::for_test("net", BufferType::Channel, "#images"));
        app.state.set_active_buffer("net/#images");
        for index in 0..100 {
            let text = format!("OLDEST{index:03} {} https://example.invalid/{index}.png", "word ".repeat(130));
            let message = make_test_message(&mut app.state, &text);
            app.state.add_local_message("net/#images", message);
        }
        app.scroll_offset = usize::MAX;
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| crate::ui::chat_view::render(frame, frame.area(), &mut app)).unwrap();
        assert!(app.chat_scroll_at_top);
        let text: String = terminal.backend().buffer().content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("OLDEST000"));
    }

    #[tokio::test]
    async fn delayed_images_preserve_message_rows_and_buffer_switches_clear_them() {
        use crate::state::buffer::{Buffer, BufferType};
        use crate::state::events::tests::make_test_message;
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.config.image_preview.inline = true;
        app.state
            .add_buffer(Buffer::for_test("net", BufferType::Channel, "#images"));
        app.state
            .add_buffer(Buffer::for_test("net", BufferType::Channel, "#other"));
        app.state.set_active_buffer("net/#images");
        let before = make_test_message(&mut app.state, "BEFORE");
        let image = make_test_message(&mut app.state, "https://example.invalid/picture.png");
        let image_key = ImageKey::for_message("net/#images", &image).unwrap();
        let after = make_test_message(&mut app.state, "AFTER");
        for message in [before, image, after] {
            app.state.add_local_message("net/#images", message);
        }
        app.inline_previews.entries.insert(
            image_key.clone(),
            Entry {
                status: Status::Loading(777),
                used: 0,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| crate::ui::chat_view::render(frame, frame.area(), &mut app))
            .unwrap();
        let lines = |terminal: &Terminal<TestBackend>| -> Vec<String> {
            terminal
                .backend()
                .buffer()
                .content
                .chunks(80)
                .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect())
                .collect()
        };
        let pending = lines(&terminal);
        let image_row = pending
            .iter()
            .position(|row| row.contains("picture.png"))
            .unwrap();
        let after_row = pending
            .iter()
            .position(|row| row.contains("AFTER"))
            .unwrap();
        assert_eq!(after_row - image_row, usize::from(ROWS) + 1);
        let rows = app.chat_rows.as_ref().unwrap();
        let buffer = app.state.active_buffer().unwrap();
        assert_eq!(rows.preview_url(buffer, image_row + 3).as_deref(), Some("https://example.invalid/picture.png"));
        assert!(rows.preview_url(buffer, after_row).is_none());
        app.handle_preview_event(ImagePreviewEvent::Inline {
            request: 777,
            result: Ok(Box::new(thumbnail())),
        });
        terminal
            .draw(|frame| crate::ui::chat_view::render(frame, frame.area(), &mut app))
            .unwrap();
        assert_eq!(
            lines(&terminal)
                .iter()
                .position(|row| row.contains("AFTER")),
            Some(after_row)
        );
        assert!(app.inline_previews.visible);
        app.inline_previews.invalidate_protocols();
        assert!(matches!(
            &app.inline_previews.entries[&image_key].status,
            Status::Ready { protocol: None, .. }
        ));
        app.state.set_active_buffer("net/#other");
        terminal.clear().unwrap();
        terminal
            .draw(|frame| crate::ui::chat_view::render(frame, frame.area(), &mut app))
            .unwrap();
        assert!(!app.inline_previews.visible);
        assert!(
            !lines(&terminal)
                .iter()
                .any(|row| row.contains("picture.png"))
        );
    }
}
