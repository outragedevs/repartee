use crate::irc::IrcEvent;

impl super::App {
    pub(crate) fn cancel_connection_attempt(&mut self, id: &str) {
        self.upstream_auth.remove(id);
        self.account_registration.remove(id);
        self.reset_server_search(id);
        *self.connection_attempts.entry(id.to_string()).or_default() += 1;
        if let Some(task) = self.forwarder_handles.remove(id) {
            task.abort();
        }
    }

    pub(crate) fn start_connection_attempt(
        &mut self,
        id: &str,
        config: crate::config::ServerConfig,
    ) {
        self.cancel_connection_attempt(id);
        let generation = self.connection_attempts[id];
        let tx = self.irc_tx.clone();
        let general = self.config.general.clone();
        let connection_id = id.to_string();
        let task = tokio::spawn(async move {
            let wrap =
                |event| IrcEvent::Attempt(connection_id.clone(), generation, Box::new(event));
            match crate::irc::connect_server(&connection_id, &config, &general).await {
                Ok((handle, mut events)) => {
                    if tx
                        .send(wrap(IrcEvent::HandleReady(Box::new(handle))))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    while let Some(event) = events.recv().await {
                        if tx.send(wrap(event)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = tx
                        .send(wrap(IrcEvent::Disconnected(
                            connection_id.clone(),
                            Some(error.to_string()),
                        )))
                        .await;
                }
            }
        });
        self.forwarder_handles.insert(id.to_string(), task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::connection::ConnectionStatus;

    #[tokio::test]
    async fn cancelled_attempt_discards_queued_events_and_drops_the_writer() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let connection = crate::state::events::tests::make_test_connection();
        let id = connection.id.clone();
        app.state.add_connection(connection);
        app.connection_attempts.insert(id.clone(), 1);
        app.cancel_connection_attempt(&id);
        let task = tokio::spawn(std::future::pending::<()>());
        let abort = task.abort_handle();
        let handle = crate::irc::IrcHandle::new(
            id.clone(),
            crate::irc::IrcSender::capturing(0),
            None,
            Some(task),
        );
        for event in [
            IrcEvent::HandleReady(Box::new(handle)),
            IrcEvent::Connected(id.clone(), ["stale-cap".into()].into(), None),
            IrcEvent::Disconnected(id.clone(), Some("old failure".into())),
            IrcEvent::Message(
                id.clone(),
                Box::new(":server 001 stale :Welcome".parse().unwrap()),
            ),
        ] {
            app.handle_irc_event(IrcEvent::Attempt(id.clone(), 1, Box::new(event)));
        }
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
        assert!(!app.irc_handles.contains_key(&id));
        assert!(app.state.connections[&id].enabled_caps.is_empty());
        assert!(app.state.connections[&id].error.is_none());
        app.handle_irc_event(IrcEvent::Attempt(
            id.clone(),
            2,
            Box::new(IrcEvent::Disconnected(id.clone(), None)),
        ));
        assert_eq!(
            app.state.connections[&id].status,
            ConnectionStatus::Disconnected
        );
    }

    #[tokio::test]
    async fn cancellation_aborts_the_pending_registration_task() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let task = tokio::spawn(std::future::pending::<()>());
        let abort = task.abort_handle();
        app.forwarder_handles.insert("pending".into(), task);
        app.cancel_connection_attempt("pending");
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
        assert!(!app.forwarder_handles.contains_key("pending"));
        assert_eq!(app.connection_attempts["pending"], 1);
    }
}
