use super::App;
use crate::state::buffer::make_buffer_id;

impl App {
    fn live_batch_messages(
        &self,
        conn_id: &str,
        batch: &crate::irc::batch::BatchInfo,
        clean_end: bool,
    ) -> Vec<(u64, ::irc::proto::Message)> {
        if batch.batch_type != "DRAFT/MULTILINE" {
            return batch.ordered_messages();
        }
        match crate::irc::batch::build_multiline_message(&self.state, conn_id, batch, clean_end) {
            crate::irc::batch::MultilineOutcome::Message(message) => {
                vec![(batch.message_order.first().copied().unwrap_or(0), *message)]
            }
            crate::irc::batch::MultilineOutcome::Replay if clean_end => batch.ordered_messages(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn receive_completed_batch(
        &mut self,
        conn_id: &str,
        batch: &crate::irc::batch::BatchInfo,
    ) {
        if batch.is_live()
            && let Some(parent) = batch.parent_ref()
        {
            let messages = self.live_batch_messages(conn_id, batch, true);
            if !self.batch_trackers.get_mut(conn_id).is_some_and(|tracker| {
                let folded = tracker.fold_messages(parent, messages, batch.dropped_messages);
                tracker.retain_redactions(parent, &batch.redaction_refs);
                tracker.refresh_redactions(parent, &mut self.state, conn_id);
                folded
            }) {
                tracing::warn!(
                    conn_id,
                    parent,
                    "discarding nested live batch without its parent"
                );
            }
        } else {
            self.dispatch_completed_batch(conn_id, batch, true);
        }
    }

    pub(crate) fn dispatch_expired_batch_set(
        &mut self,
        mut batches: Vec<(String, String, crate::irc::batch::BatchInfo)>,
    ) {
        while let Some(index) = batches.iter().position(|(conn_id, reference, batch)| {
            batch.is_live()
                && batch.parent_ref().is_some()
                && !batches.iter().any(|(child_conn, _, child)| {
                    child_conn == conn_id
                        && child.is_live()
                        && child.parent_ref() == Some(reference.as_str())
                })
        }) {
            let (conn_id, _, batch) = batches.remove(index);
            let parent = batch.parent_ref().unwrap();
            let messages = self.live_batch_messages(&conn_id, &batch, false);
            if let Some((_, _, parent_batch)) = batches
                .iter_mut()
                .find(|(id, reference, _)| id == &conn_id && reference == parent)
            {
                parent_batch.extend_messages(messages, batch.dropped_messages);
                parent_batch.retain_redactions(&batch.redaction_refs);
                parent_batch.refresh_redactions(&mut self.state, &conn_id);
            } else if !self
                .batch_trackers
                .get_mut(&conn_id)
                .is_some_and(|tracker| {
                    let folded = tracker.fold_messages(parent, messages, batch.dropped_messages);
                tracker.retain_redactions(parent, &batch.redaction_refs);
                    tracker.refresh_redactions(parent, &mut self.state, &conn_id);
                folded
                })
            {
                tracing::warn!(
                    conn_id,
                    parent,
                    "discarding expired nested live batch without its parent"
                );
            }
        }
        for (conn_id, _, batch) in batches {
            if batch.is_live() && batch.parent_ref().is_some() {
                tracing::warn!(conn_id, "discarding cyclic expired live batch");
            } else {
                self.dispatch_completed_batch(&conn_id, &batch, false);
            }
        }
    }

    pub(crate) fn dispatch_completed_batch(
        &mut self,
        conn_id: &str,
        batch: &crate::irc::batch::BatchInfo,
        clean_end: bool,
    ) {
        if batch.batch_type == "CHATHISTORY" && self.receive_search_context(conn_id, batch, clean_end) { return; }
        match batch.batch_type.as_str() {
            "SOJU.IM/SEARCH" => self.receive_search_results(conn_id, batch, clean_end),
            "DRAFT/CHATHISTORY-TARGETS" if clean_end => {
                self.receive_history_targets(conn_id, batch);
            }
            "CHATHISTORY" | "NETSPLIT" | "NETJOIN" | "DRAFT/CHATHISTORY-TARGETS" => {
                if let Some(cont) = crate::irc::batch::process_completed_batch(
                    &mut self.state,
                    conn_id,
                    batch,
                    clean_end,
                ) {
                    self.request_chathistory(
                        conn_id,
                        &cont.target,
                        crate::irc::chathistory::Direction::After,
                        Some((cont.anchor_msgid, cont.anchor_ms)),
                    );
                }
            }
            "DRAFT/MULTILINE" => {
                match crate::irc::batch::build_multiline_message(
                    &self.state,
                    conn_id,
                    batch,
                    clean_end,
                ) {
                    crate::irc::batch::MultilineOutcome::Message(message) => {
                        self.dispatch_live_irc_message(conn_id, &message);
                    }
                    crate::irc::batch::MultilineOutcome::Replay => {
                        for message in &batch.messages {
                            self.dispatch_live_irc_message(conn_id, message);
                        }
                    }
                    crate::irc::batch::MultilineOutcome::Empty => {}
                }
            }
            _ => {
                for message in &batch.messages {
                    self.dispatch_live_irc_message(conn_id, message);
                }
            }
        }
        self.drain_pending_web_events();
    }

    pub(crate) fn dispatch_live_irc_message(&mut self, conn_id: &str, msg: &::irc::proto::Message) {
        if self.handle_bouncer_metadata(conn_id, msg) { return; }
        if self.handle_server_search(conn_id, msg) { self.drain_pending_web_events(); return; }
        let previous = self.state.irc_reply_buffer.take();
        self.state.irc_reply_buffer = self.labeled_response_buffer(conn_id, msg);
        let acknowledgement = matches!(&msg.command, ::irc::proto::Command::Raw(command, _) if command.eq_ignore_ascii_case("ACK"));
        if !acknowledgement {
            self.dispatch_live_irc_message_inner(conn_id, msg);
        }
        self.state.irc_reply_buffer = previous;
        if !msg
            .tags
            .as_ref()
            .is_some_and(|tags| tags.iter().any(|tag| tag.0 == "batch"))
            && let Some(label) = crate::irc::labels::message_label(msg)
        {
            self.finish_labeled_response(conn_id, label);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch_live_irc_message_inner(&mut self, conn_id: &str, msg: &::irc::proto::Message) {
        self.observe_bouncer_presence(conn_id, msg);
        if self.observe_monitor(conn_id, msg) {
            return;
        }
        if self.handle_read_marker(conn_id, msg) {
            return;
        }
        // Normal message processing

        // Extract channel from RPL_ENDOFNAMES (for auto-WHO/MODE batch).
        let endofnames_channel = if let ::irc::proto::Command::Response(
            ::irc::proto::Response::RPL_ENDOFNAMES,
            ref args,
        ) = msg.command
        {
            args.get(1).cloned()
        } else {
            None
        };

        // Extract target from RPL_ENDOFWHO (for batch completion).
        let endofwho_target = if let ::irc::proto::Command::Response(
            ::irc::proto::Response::RPL_ENDOFWHO,
            ref args,
        ) = msg.command
        {
            args.get(1).cloned()
        } else {
            None
        };

        // Update conn.nick from RPL_WELCOME — args[0] is our confirmed nick
        // after any ERR_NICKNAMEINUSE retries by the irc crate.
        if let ::irc::proto::Command::Response(::irc::proto::Response::RPL_WELCOME, ref args) =
            msg.command
            && let Some(confirmed_nick) = args.first()
        {
            if let Some(conn) = self.state.connections.get_mut(conn_id) {
                conn.nick.clone_from(confirmed_nick);
                // Drop any handle from a previous registration —
                // the server may assign a different ident/host/cloak
                // on (re)connect. The fresh self-USERHOST below
                // re-seeds it; until then DM E2E waits rather than
                // keying under a stale @<old_handle>.
                conn.own_handle = None;
            }

            // Seed our own ident@host (the recipient-keyed DM E2E
            // context) with a ONE-SHOT self-USERHOST, gated to
            // RPL_WELCOME so it fires exactly once per registration —
            // NOT per message (the RPL_USERHOST reply is itself a
            // message, which would otherwise loop). Sent early so the
            // reply lands before the end-of-MOTD CHATHISTORY gap-fill
            // that decrypts DM backlog under it. echo-message + CHGHOST
            // keep it current afterwards. E2E-only.
            if self.state.e2e_manager.is_some()
                && let Some(handle) = self.irc_handles.get(conn_id)
            {
                let _ = handle.sender().send(::irc::proto::Command::Raw(
                    "USERHOST".to_string(),
                    vec![confirmed_nick.clone()],
                ));
            }
        }

        // Emit to scripts before default handling. Suppress semantics:
        //
        //   non-state-mutating (PRIVMSG, NOTICE, INVITE, ...)
        //     → early return; nothing displayed, nothing mutated
        //   state-mutating (JOIN/PART/QUIT/KICK/NICK/MODE/TOPIC/
        //                   ACCOUNT/AWAY/CHGHOST)
        //     → handler always runs so the nicklist/topic/modes
        //       stay in sync with the server, but the event line
        //       (MessageType::Event) is hidden via
        //       state.suppress_event_display so the script's
        //       "hide JOIN spam" intent is preserved.
        //
        // Mirrors weechat: WEECHAT_RC_OK_EAT only hides display,
        // the core protocol handler still mutates state.
        let state_mutating = matches!(
            msg.command,
            ::irc::proto::Command::JOIN(..)
                | ::irc::proto::Command::PART(..)
                | ::irc::proto::Command::QUIT(..)
                | ::irc::proto::Command::KICK(..)
                | ::irc::proto::Command::NICK(..)
                | ::irc::proto::Command::ChannelMODE(..)
                | ::irc::proto::Command::UserMODE(..)
                | ::irc::proto::Command::TOPIC(..)
                | ::irc::proto::Command::ACCOUNT(..)
                | ::irc::proto::Command::AWAY(..)
                | ::irc::proto::Command::CHGHOST(..)
        );
        let state_mutating = state_mutating
            || matches!(&msg.command, ::irc::proto::Command::Raw(command, _) if command.eq_ignore_ascii_case("SETNAME"));
        let script_suppressed = self.emit_irc_to_scripts(conn_id, msg);
        if script_suppressed && !state_mutating {
            // Display suppressed — still keep auxiliary tracking in sync.
            //
            // A translated send holds a place in its buffer's
            // reorder queue for the reflection it is expecting.
            // Eating the PRIVMSG here means that reflection never
            // reaches the handler that would fill it, so the place
            // has to be given up — the message is gone, but the
            // rest of the conversation must not wait for it.
            crate::irc::events::release_suppressed_own_echo(&mut self.state, conn_id, msg);
            if let Some(channel) = endofnames_channel {
                self.queue_channel_query(conn_id, channel);
            }
            if let Some(ref target) = endofwho_target {
                self.handle_who_batch_complete(conn_id, target);
            }
            return;
        }
        let suppress_display = script_suppressed && state_mutating;

        // Intercept DCC CTCP before normal IRC handling.
        // DCC messages arrive as CTCP inside PRIVMSG; events.rs ignores
        // non-ACTION CTCPs, so we must consume them here to avoid them
        // appearing as garbled text in the chat view.
        if let ::irc::proto::Command::PRIVMSG(_, ref text) = msg.command
            && text.starts_with('\x01')
            && text.ends_with('\x01')
            && text.len() > 2
        {
            let inner = &text[1..text.len() - 1];
            if let Some(dcc_msg) = crate::dcc::protocol::parse_dcc_ctcp(inner) {
                let (nick, ident, host) =
                    crate::irc::formatting::extract_nick_userhost(msg.prefix.as_ref());

                // A passive DCC response from the peer looks like:
                //   DCC CHAT CHAT <peer_ip> <peer_port> <our_token>
                // where port > 0 and passive_token matches what we sent.
                // We find our pending record by token and connect to the peer.
                if let Some(token) = dcc_msg.passive_token
                    && dcc_msg.port > 0
                {
                    let matching_id = self
                        .dcc
                        .records
                        .iter()
                        .find(|(_, r)| r.passive_token == Some(token))
                        .map(|(id, _)| id.clone());

                    if let Some(id) = matching_id {
                        // Update the record to point at the peer's real address.
                        if let Some(rec) = self.dcc.records.get_mut(&id) {
                            rec.addr = dcc_msg.addr;
                            rec.port = dcc_msg.port;
                            rec.state = crate::dcc::types::DccState::Connecting;
                        }

                        let (line_tx, line_rx) = tokio::sync::mpsc::channel(256);
                        self.dcc.chat_senders.insert(id.clone(), line_tx);

                        let task_id = id.clone();
                        let event_tx = self.dcc.dcc_tx.clone();
                        let timeout_dur = std::time::Duration::from_secs(self.dcc.timeout_secs);
                        let peer_addr = std::net::SocketAddr::new(dcc_msg.addr, dcc_msg.port);

                        tracing::debug!(
                            "passive DCC response from {nick}: \
                             connecting to {peer_addr} (token={token})"
                        );

                        tokio::spawn(async move {
                            crate::dcc::chat::connect_for_chat(
                                task_id,
                                peer_addr,
                                timeout_dur,
                                event_tx,
                                line_rx,
                            )
                            .await;
                        });

                        // Don't fall through to normal IRC handling.
                        if let Some(channel) = endofnames_channel {
                            self.queue_channel_query(conn_id, channel);
                        }
                        if let Some(ref target) = endofwho_target {
                            self.handle_who_batch_complete(conn_id, target);
                        }
                        return;
                    }
                }

                // Otherwise this is a fresh incoming DCC CHAT offer.
                self.handle_dcc_event(crate::dcc::DccEvent::IncomingRequest {
                    nick,
                    conn_id: conn_id.to_string(),
                    addr: dcc_msg.addr,
                    port: dcc_msg.port,
                    passive_token: dcc_msg.passive_token,
                    ident,
                    host,
                });

                // Don't pass to normal IRC handler — the CTCP is consumed.
                if let Some(channel) = endofnames_channel {
                    self.queue_channel_query(conn_id, channel);
                }
                if let Some(ref target) = endofwho_target {
                    self.handle_who_batch_complete(conn_id, target);
                }
                return;
            }
        }

        // Snapshot buffer count so we can detect newly created buffers
        // and feed them with chat history from the log database.
        let buffers_before = self.state.buffers.len();
        let names_request = crate::irc::names::request_after_join(&self.state, conn_id, msg);
        let own_handle_before = self
            .state
            .connections
            .get(conn_id)
            .and_then(|c| c.own_handle.clone());

        if suppress_display {
            self.state.suppress_event_display = true;
        }
        crate::irc::events::handle_irc_message(&mut self.state, conn_id, msg);
        if suppress_display {
            self.state.suppress_event_display = false;
        }

        // Migrate config keyed by a buffer id that just moved —
        // a query re-keyed because the peer changed nick. The
        // state-side maps moved with it already; this is the half
        // the App owns, and without it the next
        // `sync_translate_from_config` re-derives the mirror from
        // the stale key and translation stops for that
        // conversation.
        if let Some(command) = names_request
            && let Some(handle) = self.irc_handles.get(conn_id)
            && let Err(error) = handle.sender().send(command)
        {
            let label = &self.state.connections[conn_id].label;
            let buffer_id = make_buffer_id(conn_id, label);
            self.add_event_to_buffer(
                &buffer_id,
                format!("Could not request NAMES: {error}").replace('%', "%%"),
            );
        }
        self.drain_pending_buffer_rekeys();
        // Drain pending web events and broadcast + auto-record mentions.
        self.drain_pending_web_events();
        // Drain queued RPE2E NOTICE sends (handshake replies,
        // auto-KEYREQ on MissingKey) produced by the handlers.
        self.drain_pending_e2e_sends();

        // Drain post-handshake gap-fills: a KEYRSP that just
        // installed a session queued its conversation here so the
        // message that triggered the handshake (transient
        // "[E2E: awaiting session with …]" placeholder) is
        // re-fetched via CHATHISTORY and decrypted for real. A
        // gap-fill suppressed by an in-flight CHATHISTORY for the
        // same target is re-queued: its batch END is itself an
        // IRC event, so the retry fires exactly when the conflict
        // clears (dropping it instead would strand the
        // placeholder until reconnect).
        let e2e_gapfills = std::mem::take(&mut self.state.pending_e2e_gapfills);
        for gapfill in e2e_gapfills {
            if !self.regapfill_conversation_after_session(&gapfill.connection_id, &gapfill.target)
                && !self.state.pending_e2e_gapfills.contains(&gapfill)
            {
                self.state.pending_e2e_gapfills.push(gapfill);
            }
        }

        // If we just learned our own ident@host (the recipient-keyed
        // DM context), re-run the query gap-fill: a gap-fill that ran
        // at end-of-MOTD before the self-USERHOST reply arrived would
        // have skipped encrypted DM backlog (and the batch completes,
        // so it is never retried). The retry must release the one-shot
        // gap-fill claim the first request took, or it would be
        // suppressed — see the helper.
        if own_handle_before.is_none()
            && self.state.e2e_manager.is_some()
            && self
                .state
                .connections
                .get(conn_id)
                .is_some_and(|c| c.own_handle.is_some())
        {
            self.regapfill_queries_after_own_handle(conn_id);
        }

        // Load backlog for any buffers created by handle_irc_message
        // (e.g. query buffer on first PRIVMSG from a new nick)
        if self.state.buffers.len() > buffers_before {
            let new_ids: Vec<String> = self
                .state
                .buffers
                .keys()
                .skip(buffers_before)
                .cloned()
                .collect();
            for buf_id in &new_ids {
                self.load_backlog(buf_id);
            }
        }

        // ── DCC: track nick renames ──────────────────────────────────
        // When a user renames on IRC their DCC record and buffer must
        // follow, since buffers are named after the peer's nick (=Nick).
        if let ::irc::proto::Command::NICK(ref new_nick) = msg.command
            && let Some(::irc::proto::Prefix::Nickname(ref old_nick, _, _)) = msg.prefix
        {
            let renames = self.dcc.update_nick(old_nick, new_nick);
            for (_old_id, _new_id, old_buf_suffix, new_buf_suffix) in renames {
                let old_buf_id = crate::state::buffer::make_buffer_id(conn_id, &old_buf_suffix);
                let new_buf_id = crate::state::buffer::make_buffer_id(conn_id, &new_buf_suffix);

                if let Some(mut buf) = self.state.buffers.shift_remove(&old_buf_id) {
                    buf.id.clone_from(&new_buf_id);
                    buf.name = format!("={new_nick}");
                    self.state.buffers.insert(new_buf_id.clone(), buf);
                    self.state.rekey_activity(&old_buf_id, &new_buf_id);

                    // Keep active selection consistent.
                    if self.state.active_buffer_id.as_deref() == Some(&old_buf_id) {
                        self.state.active_buffer_id = Some(new_buf_id);
                    }
                }
            }
        }

        // ── DCC: ERR_NOSUCHNICK cleanup ──────────────────────────────
        // If the IRC server reports that a nick does not exist,
        // cancel any pending DCC request to that nick so it doesn't
        // sit in the queue until timeout.
        if let ::irc::proto::Command::Response(::irc::proto::Response::ERR_NOSUCHNICK, ref args) =
            msg.command
            && let Some(target_nick) = args.get(1)
            && let Some(record) = self.dcc.close_by_nick(target_nick)
        {
            crate::commands::helpers::add_local_event(
                self,
                &format!("DCC CHAT to {} cancelled: no such nick", record.nick),
            );
        }

        // Queue channel for batched WHO + MODE after join.
        if let Some(channel) = endofnames_channel {
            // NAMES completing confirms the server has us in the
            // channel, so a membership-gated CHATHISTORY won't be
            // rejected. Run the active channel's reconnect gap-fill
            // here rather than at end-of-MOTD, which races the JOIN.
            self.gapfill_active_channel_on_join(conn_id, &channel);
            self.queue_channel_query(conn_id, channel);
        }

        // Check if a WHO batch completed.
        if let Some(ref target) = endofwho_target {
            self.handle_who_batch_complete(conn_id, target);
        }

        // Reconnect gap-fill for an active QUERY buffer (deferred
        // from RPL_WELCOME): by end-of-MOTD the 005 ISUPPORT lines are
        // parsed, so CHATHISTORY uses the correct limit/ref types. A
        // query needs no channel membership, so this timing is safe.
        // Active CHANNEL buffers gap-fill from the end-of-NAMES path
        // instead (membership is confirmed there; firing here would
        // race the auto-JOIN). Fires on the MOTD terminator (376) or
        // its absence (422).
        if matches!(
            msg.command,
            ::irc::proto::Command::Response(
                ::irc::proto::Response::RPL_ENDOFMOTD | ::irc::proto::Response::ERR_NOMOTD,
                _
            )
        ) {
            self.gapfill_active_buffer_on_connect(conn_id);
            self.start_history_discovery(conn_id);
        }
    }
}
