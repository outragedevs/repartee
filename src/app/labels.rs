use super::App;
use crate::irc::labels::message_label;

impl App {
    pub(crate) fn tick_labels(&mut self) {
        self.labeled_requests.retain(|id, labels| {
            labels.expire(std::time::Instant::now());
            self.state.connections.get(id).is_some_and(|conn| {
                conn.enabled_caps.contains("labeled-response")
                    && conn.enabled_caps.contains("batch")
                    && conn.status == crate::state::connection::ConnectionStatus::Connected
            })
        });
    }

    pub(crate) fn send_active_labeled_request(
        &mut self,
        command: irc::proto::Command,
    ) -> Result<(), String> {
        let buffer = self.state.active_buffer().ok_or("No active buffer")?;
        let id = buffer.connection_id.clone();
        let buffer_id = buffer.id.clone();
        let sender = self
            .irc_handles
            .get(&id)
            .ok_or("Not connected")?
            .sender()
            .clone();
        self.tick_labels();
        let enabled = self.state.connections.get(&id).is_some_and(|conn| {
            conn.enabled_caps.contains("labeled-response") && conn.enabled_caps.contains("batch")
        });
        let label = if enabled {
            self.labeled_requests
                .entry(id.clone())
                .or_default()
                .register(buffer_id, std::time::Instant::now())
        } else {
            None
        };
        let mut message: irc::proto::Message = command.into();
        if let Some(label) = &label {
            message.tags = Some(vec![irc::proto::message::Tag(
                "label".into(),
                Some(label.clone()),
            )]);
        }
        let result = sender.send(message);
        if result.is_err()
            && let Some(label) = label
        {
            self.labeled_requests.get_mut(&id).unwrap().remove(&label);
        }
        result.map_err(|error| error.to_string())
    }

    pub(crate) fn labeled_response_buffer(
        &mut self,
        id: &str,
        message: &irc::proto::Message,
    ) -> Option<String> {
        let label = message_label(message)?;
        self.tick_labels();
        let buffer = self
            .labeled_requests
            .get_mut(id)
            .and_then(|labels| labels.resolve(label, std::time::Instant::now()));
        let buffer = buffer.filter(|buffer_id| {
            self.state
                .buffers
                .get(buffer_id)
                .is_some_and(|buffer| buffer.connection_id == id)
        });
        buffer.or_else(|| {
            self.state
                .connections
                .get(id)
                .map(|conn| crate::state::buffer::make_buffer_id(id, &conn.label))
        })
    }

    pub(crate) fn finish_labeled_response(&mut self, id: &str, label: &str) {
        if let Some(labels) = self.labeled_requests.get_mut(id) {
            labels.finish(label);
        }
    }
}
