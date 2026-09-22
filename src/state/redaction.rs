use irc::proto::{Command, Message as IrcMessage, Prefix};

use super::AppState;
use super::buffer::{BufferType, Message, MessageType, make_buffer_id};
use crate::irc::isupport::casefold;

fn msgid(message: &Message) -> Option<&str> {
    message
        .redaction_msgid
        .as_deref()
        .or_else(|| message.tags.as_ref()?.get("msgid").map(String::as_str))
}

fn replace_body(message: &mut Message, text: &str) {
    message.text = text.to_string();
    message.message_type = MessageType::Event;
    message.nick = None;
    message.nick_mode = None;
    message.highlight = false;
    message.event_key = None;
    message.event_params = None;
    message.wire_origin = None;
    message.translation_suffix_at = None;
    if let Some(tags) = &mut message.tags {
        tags.retain(|key, _| matches!(key.as_str(), "msgid" | "time") || key == super::buffer::SEARCH_TARGET_TAG.as_str() || key == super::buffer::SEARCH_SCOPE_TAG.as_str());
    }
}

impl AppState {
    pub(super) fn prune_redaction_scopes(&mut self) {
        let active: std::collections::HashSet<_> = self
            .connections
            .values()
            .filter(|connection| connection.server_owns_history())
            .map(super::connection::Connection::network_key)
            .collect();
        self.redaction_registry.retain_scopes(&active);
    }

    pub(crate) fn redaction_key(
        &self,
        buffer_id: &str,
        id: &str,
    ) -> Option<(String, String, String)> {
        let (conn_id, target) = buffer_id.split_once('/')?;
        let conn = self.connections.get(conn_id)?;
        if !conn.server_owns_history() {
            return None;
        }
        let mapping = conn.isupport_parsed.casemapping();
        Some((
            conn.network_key().to_string(),
            casefold(target, mapping),
            id.to_string(),
        ))
    }

    pub(super) fn mention_redaction_id(&self, buffer_id: &str, id: &str) -> Option<String> {
        serde_json::to_string(&self.redaction_key(buffer_id, id)?).ok()
    }

    pub(crate) fn retain_wire_redaction(
        &mut self,
        conn_id: &str,
        message: &IrcMessage,
    ) -> Option<std::sync::Arc<super::redaction_registry::Identity>> {
        let conn = self.connections.get(conn_id)?;
        if !conn.server_owns_history() {
            return None;
        }
        let (target, id) = match &message.command {
            Command::PRIVMSG(target, _) | Command::NOTICE(target, _) => {
                let id = message.tags.as_ref()?.iter()
                    .find(|tag| tag.0 == "msgid")?.1.as_deref()?;
                (target.as_str(), id)
            }
            Command::BATCH(reference, Some(kind), Some(params))
                if reference.starts_with('+') && kind.to_str().eq_ignore_ascii_case("draft/multiline") =>
            {
                let id = message.tags.as_ref()?.iter()
                    .find(|tag| tag.0 == "msgid")?.1.as_deref()?;
                (params.first()?.as_str(), id)
            }
            Command::Raw(command, args)
                if command.eq_ignore_ascii_case("REDACT") && (2..=3).contains(&args.len()) =>
            {
                (args[0].as_str(), args[1].as_str())
            }
            _ => return None,
        };
        if target.is_empty() || id.is_empty() {
            return None;
        }
        let mapping = conn.isupport_parsed.casemapping();
        let target = if casefold(target, mapping) == casefold(&conn.nick, mapping) {
            match message.prefix.as_ref()? {
                Prefix::Nickname(nick, _, _) => nick.as_str(),
                Prefix::ServerName(_) => return None,
            }
        } else {
            target
        };
        let key = self.redaction_key(&make_buffer_id(conn_id, target), id)?;
        Some(self.redaction_registry.track(key))
    }

    pub(crate) fn attach_redaction_ref(&mut self, buffer_id: &str, message: &mut Message) {
        if message.redaction_ref.is_none()
            && let Some(key) = msgid(message).and_then(|id| self.redaction_key(buffer_id, id))
        {
            message.redaction_ref = Some(self.redaction_registry.track(key));
        }
    }

    pub(super) fn redaction_notice(
        &self,
        buffer_id: &str,
        message: &Message,
    ) -> Option<String> {
        let key = msgid(message).and_then(|id| self.redaction_key(buffer_id, id))?;
        message
            .redaction_ref
            .as_ref()
            .filter(|identity| identity.key.0 == key.0 && identity.key.2 == key.2)
            .and_then(|identity| identity.notice().map(str::to_string))
            .or_else(|| {
                self.redaction_registry
                    .get(&key)
                    .and_then(|identity| identity.notice().map(str::to_string))
            })
    }

    pub(crate) fn apply_redaction(&self, buffer_id: &str, message: &mut Message) -> bool {
        let Some(text) = self.redaction_notice(buffer_id, message) else {
            return false;
        };
        replace_body(message, &text);
        true
    }

    pub(crate) fn receive_redaction(&mut self, conn_id: &str, message: &IrcMessage) {
        let Command::Raw(command, args) = &message.command else {
            return;
        };
        if !command.eq_ignore_ascii_case("REDACT") || !(2..=3).contains(&args.len()) {
            return;
        }
        let target = &args[0];
        let id = &args[1];
        if target.is_empty() || id.is_empty() {
            return;
        }
        let Some(conn) = self.connections.get(conn_id) else {
            return;
        };
        if !conn.server_owns_history() || !conn.enabled_caps.contains("draft/message-redaction") {
            return;
        }
        let Some(prefix) = &message.prefix else {
            return;
        };
        let actor = match prefix {
            Prefix::Nickname(nick, _, _) | Prefix::ServerName(nick) => nick,
        };
        let mapping = conn.isupport_parsed.casemapping();
        let channel = target.starts_with(|c| conn.isupport_parsed.chan_types().contains(c));
        let peer = if channel || casefold(actor, mapping) == casefold(&conn.nick, mapping) {
            target
        } else if casefold(target, mapping) == casefold(&conn.nick, mapping) {
            actor
        } else {
            return;
        };
        let folded = casefold(peer, mapping);
        let buffer_id = self
            .buffers
            .values()
            .find(|buffer| {
                buffer.connection_id == conn_id
                    && matches!(buffer.buffer_type, BufferType::Channel | BufferType::Query)
                    && casefold(&buffer.name, mapping) == folded
            })
            .map_or_else(|| make_buffer_id(conn_id, peer), |buffer| buffer.id.clone());
        let matches_elsewhere = self.buffers.values().any(|buffer| {
            buffer.connection_id == conn_id
                && matches!(buffer.buffer_type, BufferType::Channel | BufferType::Query)
                && casefold(&buffer.name, mapping) != folded
                && buffer
                    .messages
                    .iter()
                    .any(|message| msgid(message) == Some(id))
        });
        if matches_elsewhere {
            return;
        }
        let Some(key) = self.redaction_key(&buffer_id, id) else {
            return;
        };
        let text = args.get(2).filter(|reason| !reason.is_empty()).map_or_else(
            || format!("Message deleted by {actor}"),
            |reason| format!("Message deleted by {actor}: {reason}"),
        );
        let text = crate::commands::helpers::escape_format(&text);
        let mention_id = serde_json::to_string(&key).ok();
        let identity = self.redaction_registry.redact(key, text);
        let text = identity.notice().unwrap_or_default();
        self.redact_visible_buffer(&buffer_id, id, text);
        let copies: Vec<_> = self.buffers.values().filter(|buffer| buffer.buffer_type == BufferType::Special
            && buffer.messages.iter().any(|message| message.redaction_ref.as_ref().is_some_and(|reference| reference.key == identity.key)))
            .map(|buffer| buffer.id.clone()).collect();
        for copy in copies { self.redact_visible_buffer(&copy, id, text); }
        if let Some(mention_id) = mention_id {
            self.redact_visible_buffer("_mentions", &mention_id, text);
        }
    }

    fn redact_visible_buffer(&mut self, buffer_id: &str, id: &str, text: &str) {
        let mut changed = Vec::new();
        if let Some(buffer) = self.buffers.get_mut(buffer_id) {
            for message in &mut buffer.messages {
                if msgid(message) == Some(id) {
                    changed.push(message.id);
                    replace_body(message, text);
                }
            }
        }
        if let Some(activity) = self.read_activity.get_mut(buffer_id) {
            let mut unread_changed = false;
            for message_id in changed {
                if let Some(entry) = activity.unread.get_mut(&message_id) {
                    entry.1 = super::buffer::ActivityLevel::Events;
                    unread_changed = true;
                }
            }
            if unread_changed {
                self.refresh_read_activity(buffer_id);
            }
        }
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::RedactMessage {
                buffer_id: buffer_id.to_string(),
                msgid: id.to_string(),
                text: text.to_string(),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> crate::app::App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for id in ["one", "two"] {
            let config = toml::from_str(&format!("label='{id}'\naddress='localhost'\nport=1\ntls=false\nchannels=[]\nnick='me'\nbouncer_network_id='1'")).unwrap();
            app.setup_connection(id, &config);
            let conn = app.state.connections.get_mut(id).unwrap();
            conn.nick = "me".into();
            conn.enabled_caps.insert("draft/message-redaction".into());
            for channel in ["#a", "#b"] {
                app.state.add_buffer(super::super::buffer::Buffer::for_test(
                    id,
                    BufferType::Channel,
                    channel,
                ));
            }
        }
        app
    }

    fn add(app: &mut crate::app::App, buffer: &str, id: &str) -> Message {
        let mut message =
            super::super::events::tests::make_test_message(&mut app.state, "original secret");
        message.tags = Some(std::collections::HashMap::from([
            ("msgid".into(), id.into()),
            ("+draft/reply".into(), "sensitive-reference".into()),
        ]));
        message.highlight = true;
        message.translation_suffix_at = Some(8);
        app.state.add_message_unshrunk(buffer, message.clone());
        message
    }

    #[test]
    fn deletion_is_scoped_and_late_delivery_cannot_restore_body() {
        let mut app = setup();
        let late = add(&mut app, "one/#a", "opaque-ID");
        add(&mut app, "two/#a", "opaque-ID");
        app.state.receive_redaction(
            "one",
            &":alice!u@h REDACT #A opaque-ID :withdrawn".parse().unwrap(),
        );
        let deleted = app.state.buffers["one/#a"].messages.back().unwrap();
        assert_eq!(deleted.text, "Message deleted by alice: withdrawn");
        assert!(!deleted.highlight);
        assert!(deleted.translation_suffix_at.is_none());
        assert_eq!(deleted.tags.as_ref().unwrap().len(), 1);
        assert_eq!(
            app.state.buffers["two/#a"].messages.back().unwrap().text,
            "original secret"
        );
        app.state.add_message_unshrunk("one/#a", late);
        assert_eq!(
            app.state.buffers["one/#a"].messages.back().unwrap().text,
            "Message deleted by alice: withdrawn"
        );
        assert!(
            app.state
                .pending_web_events
                .iter()
                .any(|event| matches!(event,
            crate::web::protocol::WebEvent::RedactMessage { buffer_id, msgid, .. }
                if buffer_id == "one/#a" && msgid == "opaque-ID"))
        );
    }

    #[test]
    fn retained_deletion_survives_peer_rename_before_delayed_delivery() {
        use crate::translate::queue::{ReadyDelivery, ReadyEntry, ReadyOrigin};
        let mut app = setup();
        crate::irc::events::handle_irc_message(
            &mut app.state,
            "one",
            &"@msgid=pending :alice!u@h PRIVMSG me :original secret".parse().unwrap(),
        );
        let delayed = app.state.buffers["one/alice"].messages.back().unwrap().clone();
        app.state.receive_redaction(
            "one", &":alice!u@h REDACT me pending".parse().unwrap(),
        );
        crate::irc::events::handle_irc_message(
            &mut app.state, "one", &":alice!u@h NICK alicia".parse().unwrap(),
        );
        app.state.deliver_ready("one/alicia", vec![ReadyEntry {
            id: delayed.id,
            message: delayed.clone(),
            activity: super::super::buffer::ActivityLevel::Mention,
            origin: ReadyOrigin::Translated,
            delivery: ReadyDelivery::Logged,
        }]);
        assert_eq!(app.state.buffers["one/alicia"].messages.back().unwrap().text,
            "Message deleted by alice");
        let mut different_account = delayed;
        assert!(!app.state.apply_redaction("two/alicia", &mut different_account));
        assert_eq!(different_account.text, "original secret");
    }

    #[test]
    fn known_message_in_other_target_is_not_deleted_or_remembered() {
        let mut app = setup();
        add(&mut app, "one/#b", "opaque-ID");
        app.state
            .receive_redaction("one", &":alice!u@h REDACT #a opaque-ID".parse().unwrap());
        assert_eq!(app.state.redaction_registry.deletion_count(), 0);
        assert_eq!(
            app.state.buffers["one/#b"].messages.back().unwrap().text,
            "original secret"
        );
    }

    #[test]
    fn message_ids_are_case_sensitive_and_scope_changes_discard_effect() {
        let mut app = setup();
        let original = add(&mut app, "one/#a", "ID");
        app.state
            .receive_redaction("one", &":alice!u@h REDACT #a id".parse().unwrap());
        assert_eq!(
            app.state.buffers["one/#a"].messages.back().unwrap().text,
            "original secret"
        );
        app.state
            .receive_redaction("one", &":alice!u@h REDACT #a ID".parse().unwrap());
        app.state.connections.get_mut("one").unwrap().network_scope =
            Some("different-account".into());
        let mut original = original;
        assert!(!app.state.apply_redaction("one/#a", &mut original));
    }
    #[test]
    fn history_deletion_is_applied_before_original_is_exposed() {
        let mut app = setup();
        let batch = crate::irc::batch::BatchInfo {
            message_order: Vec::new(),
            redaction_refs: Vec::new(),
            batch_type: "CHATHISTORY".into(),
            params: vec!["#a".into()],
            started_at: std::time::Instant::now(),
            opener_tags: None,
            dropped_messages: 0,
            messages: vec![
                "@msgid=original;time=2024-01-01T00:00:01.000Z :alice!u@h PRIVMSG #a :original secret".parse().unwrap(),
                ":alice!u@h REDACT #a original".parse().unwrap(),
            ],
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        app.state.log_tx = Some(tx);
        app.state.pending_web_events.clear();
        crate::irc::batch::process_completed_batch(&mut app.state, "one", &batch, true);
        assert_eq!(
            app.state.buffers["one/#a"].messages.back().unwrap().text,
            "Message deleted by alice"
        );
        assert!(rx.try_recv().is_err());
        for event in &app.state.pending_web_events {
            if let crate::web::protocol::WebEvent::InsertMessage { message, .. } = event {
                assert_eq!(message.msgid.as_deref(), Some("original"));
                assert!(!message.text.contains("original secret"));
            }
        }
    }
    #[test]
    fn aggregate_mentions_are_scoped_and_deferred_fanout_stays_redacted() {
        use crate::translate::queue::{ReadyDelivery, ReadyEntry, ReadyOrigin};
        let mut app = setup();
        let mut mentions =
            super::super::buffer::Buffer::for_test("", BufferType::Mentions, "_mentions");
        mentions.id = "_mentions".into();
        app.state.add_buffer(mentions);
        for connection in ["one", "two"] {
            crate::irc::events::handle_irc_message(
                &mut app.state,
                connection,
                &"@msgid=shared-ID :alice!u@h PRIVMSG #a :me original secret"
                    .parse()
                    .unwrap(),
            );
        }
        let late = app.state.buffers["one/#a"].messages.back().unwrap().clone();
        assert_eq!(app.state.buffers["_mentions"].messages.len(), 2);
        let mention_id = app.state.buffers["_mentions"].messages[0]
            .tags
            .as_ref()
            .unwrap()["msgid"]
            .clone();
        assert_ne!(
            mention_id,
            app.state.buffers["_mentions"].messages[1]
                .tags
                .as_ref()
                .unwrap()["msgid"]
        );
        app.state.pending_web_events.clear();
        app.state
            .receive_redaction("one", &":alice!u@h REDACT #a shared-ID".parse().unwrap());
        assert_eq!(
            app.state.buffers["_mentions"].messages[0].text,
            "Message deleted by alice"
        );
        assert!(
            app.state.buffers["_mentions"].messages[1]
                .text
                .contains("original secret")
        );
        assert!(
            app.state
                .pending_web_events
                .iter()
                .any(|event| matches!(event,
            crate::web::protocol::WebEvent::RedactMessage { buffer_id, msgid, .. }
                if buffer_id == "_mentions" && msgid == &mention_id))
        );
        app.state.deliver_ready(
            "one/#a",
            vec![ReadyEntry {
                id: late.id,
                message: late,
                activity: super::super::buffer::ActivityLevel::Mention,
                origin: ReadyOrigin::Translated,
                delivery: ReadyDelivery::Logged,
            }],
        );
        assert_eq!(app.state.buffers["_mentions"].messages.len(), 2);
        assert_eq!(
            app.state.buffers["one/#a"].messages.back().unwrap().text,
            "Message deleted by alice"
        );
    }
    #[test]
    fn encrypted_placeholder_keeps_redaction_identity_without_history_dedup_identity() {
        let mut app = setup();
        crate::irc::events::handle_irc_message(
            &mut app.state,
            "one",
            &"@msgid=encrypted-ID :alice!u@h PRIVMSG me :+RPE2E01 c=@me@host m=ciphertext"
                .parse()
                .unwrap(),
        );
        let placeholder = app.state.buffers["one/alice"].messages.back().unwrap();
        assert!(placeholder.tags.is_none());
        assert_eq!(placeholder.redaction_msgid.as_deref(), Some("encrypted-ID"));
        assert_eq!(
            crate::web::snapshot::message_to_wire(placeholder, None)
                .msgid
                .as_deref(),
            Some("encrypted-ID")
        );
        app.state
            .receive_redaction("one", &":alice!u@h REDACT me encrypted-ID".parse().unwrap());
        assert_eq!(
            app.state.buffers["one/alice"].messages.back().unwrap().text,
            "Message deleted by alice"
        );
        let mut decrypted =
            super::super::events::tests::make_test_message(&mut app.state, "decrypted secret");
        decrypted.tags = Some(std::collections::HashMap::from([(
            "msgid".into(),
            "encrypted-ID".into(),
        )]));
        app.state.apply_redaction("one/alice", &mut decrypted);
        app.state.surface_history_rows("one/alice", vec![decrypted]);
        assert_eq!(app.state.buffers["one/alice"].messages.len(), 1);
    }
    #[test]
    fn channel_targets_follow_custom_chantypes_and_irc_casemapping() {
        for (mapping, stored, target, matches) in [
            ("rfc1459", "+Test[", "+TEST{", true),
            ("ascii", "+Test[", "+TEST{", false),
            ("strict-rfc1459", "+Test^", "+TEST~", false),
            ("rfc1459", "+Test^", "+TEST~", true),
        ] {
            let mut app = setup();
            crate::irc::events::handle_irc_message(&mut app.state, "one",
                &format!(":server 005 me CASEMAPPING={mapping} CHANTYPES=+ :are supported").parse().unwrap());
            app.state.add_buffer(super::super::buffer::Buffer::for_test(
                "one",
                BufferType::Channel,
                stored,
            ));
            let buffer_id = make_buffer_id("one", stored);
            add(&mut app, &buffer_id, "ID");
            app.state.receive_redaction(
                "one",
                &format!(":alice!u@h REDACT {target} ID").parse().unwrap(),
            );
            assert_eq!(
                app.state.buffers[&buffer_id]
                    .messages
                    .back()
                    .unwrap()
                    .text
                    .starts_with("Message deleted"),
                matches
            );
        }
    }

    #[test]
    fn redaction_identity_follows_live_isupport_removal_and_reconnect_reset() {
        let mut app = setup();
        let announce = ":server 005 me CASEMAPPING=ascii CHANTYPES=+ :are supported";
        crate::irc::events::handle_irc_message(&mut app.state, "one", &announce.parse().unwrap());
        let original = app.state.redaction_key("one/+Test[", "id").unwrap();
        assert_eq!(original.1, "+test[");
        let identity = app.state.retain_wire_redaction("one",
            &"@msgid=wire :alice!u@h PRIVMSG +Test[ :body".parse().unwrap()).unwrap();
        assert_eq!(identity.key.1, "+test[");
        crate::irc::events::handle_irc_message(&mut app.state, "one",
            &":server 005 me -CASEMAPPING -CHANTYPES :are supported".parse().unwrap());
        assert_eq!(app.state.redaction_key("one/+Test[", "id").unwrap().1, "+test{");
        assert_eq!(app.state.connections["one"].isupport_parsed.chan_types(), "#&");
        crate::irc::events::handle_irc_message(&mut app.state, "one", &announce.parse().unwrap());
        crate::irc::events::handle_connected(&mut app.state, "one");
        assert_eq!(app.state.redaction_key("one/+Test[", "id").unwrap().1, "+test{");
        assert_eq!(app.state.connections["one"].isupport_parsed.chan_types(), "#&");
    }

    #[test]
    fn private_redactions_resolve_peer_for_incoming_and_own_echo() {
        for (actor, target) in [("Alice", "ME"), ("ME", "Alice")] {
            let mut app = setup();
            app.state.add_buffer(super::super::buffer::Buffer::for_test(
                "one",
                BufferType::Query,
                "Alice",
            ));
            add(&mut app, "one/alice", "ID");
            app.state.receive_redaction(
                "one",
                &format!(":{actor}!u@h REDACT {target} ID").parse().unwrap(),
            );
            assert!(
                app.state.buffers["one/alice"]
                    .messages
                    .back()
                    .unwrap()
                    .text
                    .starts_with("Message deleted")
            );
        }
        let mut app = setup();
        app.state.add_buffer(super::super::buffer::Buffer::for_test(
            "one",
            BufferType::Query,
            "Alice",
        ));
        add(&mut app, "one/alice", "ID");
        app.state
            .receive_redaction("one", &":Eve!u@h REDACT Alice ID".parse().unwrap());
        assert_eq!(app.state.redaction_registry.deletion_count(), 0);
        assert_eq!(
            app.state.buffers["one/alice"].messages.back().unwrap().text,
            "original secret"
        );
    }
    #[test]
    fn reconnect_preserves_deletions_but_removed_or_replaced_scopes_are_pruned() {
        let mut app = setup();
        for id in ["one", "two"] {
            app.state
                .receive_redaction(id, &":alice!u@h REDACT #a ID".parse().unwrap());
        }
        assert_eq!(app.state.redaction_registry.deletion_count(), 2);
        let original = app.state.connections["one"].clone();
        app.state.add_connection(original.clone());
        assert_eq!(app.state.redaction_registry.deletion_count(), 2);
        let mut replacement = original;
        replacement.network_scope = Some("different-account".into());
        app.state.add_connection(replacement);
        assert_eq!(app.state.redaction_registry.deletion_count(), 1);
        app.state.remove_connection("two");
        assert_eq!(app.state.redaction_registry.deletion_count(), 0);
    }
    #[test]
    fn delayed_copy_keeps_confirmation_after_registry_cache_pressure() {
        let mut app = setup();
        add(&mut app, "one/#a", "pending");
        let pending = app.state.buffers["one/#a"].messages.back().unwrap().clone();
        assert!(pending.redaction_ref.is_some());
        app.state
            .receive_redaction("one", &":alice!u@h REDACT #a pending".parse().unwrap());
        for id in 0..10_000 {
            app.state.redaction_registry.redact(
                ("pressure".into(), "#a".into(), id.to_string()),
                "deleted".into(),
            );
        }
        app.state.add_message_unshrunk("one/#a", pending);
        assert_eq!(
            app.state.buffers["one/#a"].messages.back().unwrap().text,
            "Message deleted by alice"
        );
    }
    #[test]
    fn aggregate_retains_deletion_after_source_trim_and_cache_pressure() {
        let mut app = setup();
        let mut mentions =
            super::super::buffer::Buffer::for_test("", BufferType::Mentions, "_mentions");
        mentions.id = "_mentions".into();
        app.state.add_buffer(mentions);
        crate::irc::events::handle_irc_message(
            &mut app.state,
            "one",
            &"@msgid=retained :alice!u@h PRIVMSG #a :me original secret".parse().unwrap(),
        );
        let mut replay = app.state.buffers["one/#a"].messages.back().unwrap().clone();
        replay.redaction_ref = None;
        app.state.buffers.get_mut("one/#a").unwrap().messages.clear();
        app.state.receive_redaction(
            "one", &":alice!u@h REDACT #a retained".parse().unwrap(),
        );
        for id in 0..10_000 {
            app.state.redaction_registry.redact(
                ("pressure".into(), "#a".into(), id.to_string()), "deleted".into(),
            );
        }
        assert!(app.state.apply_redaction("one/#a", &mut replay));
        assert_eq!(replay.text, "Message deleted by alice");
        app.state.fan_out_mention(
            "one", "#a", "alice", "me original secret",
            (replay.timestamp, Some("retained")),
        );
        assert_eq!(app.state.buffers["_mentions"].messages.len(), 1);
        assert_eq!(app.state.buffers["_mentions"].messages[0].text, "Message deleted by alice");
    }

    #[test]
    fn surfaced_history_retains_deletion_for_later_replay() {
        let mut app = setup();
        let mut tracker = crate::irc::batch::BatchTracker::default();
        tracker.start_batch("history", "chathistory", vec!["#a".into()], None);
        tracker.add_message("@batch=history;msgid=history-ID;time=2026-09-19T10:00:00Z :alice!u@h PRIVMSG #a :original secret".parse().unwrap());
        let batch = tracker.end_batch("history").unwrap();
        crate::irc::batch::process_completed_batch(&mut app.state, "one", &batch, true);
        let mut replay = app.state.buffers["one/#a"].messages.back().unwrap().clone();
        assert!(replay.redaction_ref.is_some());
        replay.redaction_ref = None;
        app.state.receive_redaction("one", &":alice!u@h REDACT #a history-ID".parse().unwrap());
        for id in 0..10_000 {
            app.state.redaction_registry.redact(
                ("pressure".into(), "#a".into(), id.to_string()), "deleted".into(),
            );
        }
        assert!(app.state.apply_redaction("one/#a", &mut replay));
        assert_eq!(replay.text, "Message deleted by alice");
    }

    #[test]
    fn queued_wire_identities_share_channel_and_private_deletions() {
        let mut app = setup();
        for (original, deletion) in [
            ("@msgid=ID :alice!u@h PRIVMSG #A :secret", ":alice!u@h REDACT #a ID"),
            ("@msgid=ID :alice!u@h NOTICE me :secret", ":alice!u@h REDACT me ID"),
            ("@msgid=ID :me!u@h PRIVMSG Alice :secret", ":me!u@h REDACT Alice ID"),
        ] {
            let original = app.state.retain_wire_redaction("one", &original.parse().unwrap()).unwrap();
            let deletion = app.state.retain_wire_redaction("one", &deletion.parse().unwrap()).unwrap();
            assert!(std::sync::Arc::ptr_eq(&original, &deletion));
            let foreign = app.state.retain_wire_redaction("two", &"@msgid=ID :alice!u@h PRIVMSG #A :secret".parse().unwrap()).unwrap();
            assert!(!std::sync::Arc::ptr_eq(&original, &foreign));
        }
        assert!(app.state.retain_wire_redaction("one", &":alice!u@h PRIVMSG #a :without ID".parse().unwrap()).is_none());
    }

    #[test]
    fn batch_retention_discards_same_id_from_unretained_target() {
        let mut app = setup();
        let retained: IrcMessage = "@batch=test;msgid=ID :alice!u@h PRIVMSG #A :retained".parse().unwrap();
        let dropped: IrcMessage = "@msgid=ID :alice!u@h PRIVMSG #b :dropped".parse().unwrap();
        let kept = app.state.retain_wire_redaction("one", &retained).unwrap();
        let discarded = app.state.retain_wire_redaction("one", &dropped).unwrap();
        let mut tracker = crate::irc::batch::BatchTracker::default();
        tracker.start_batch("test", "chathistory", vec!["#a".into()], None);
        tracker.add_message(retained);
        tracker.retain_redactions("test", &[kept.clone(), discarded]);
        tracker.refresh_redactions("test", &mut app.state, "one");
        let batch = tracker.end_batch("test").unwrap();
        assert_eq!(batch.redaction_refs.len(), 1);
        assert!(std::sync::Arc::ptr_eq(&batch.redaction_refs[0], &kept));
    }

    #[test]
    fn redaction_capability_is_selected_only_for_bound_bouncer_networks() {
        let mut app = setup();
        let caps = crate::irc::cap::ServerCaps::parse("draft/message-redaction message-tags");
        assert!(crate::irc::cap::bouncer_network_caps(&caps).contains(&"draft/message-redaction".into()));
        assert!(!crate::irc::cap::DESIRED_CAPS.contains(&"draft/message-redaction"));
        for (bound, control, expected) in [(true, false, true), (true, true, false), (false, false, false)] {
            let conn = app.state.connections.get_mut("one").unwrap();
            conn.origin_config.bouncer_network_id = bound.then(|| "1".into());
            conn.origin_config.bouncer_control = control;
            conn.enabled_caps.remove("draft/message-redaction");
            let requested = crate::irc::events::handle_cap_new(
                &mut app.state, "one", Some("draft/message-redaction"), None,
            );
            assert_eq!(requested.contains(&"draft/message-redaction".into()), expected);
        }
    }

    #[test]
    fn deletion_reclassifies_unread_mention_and_broadcasts_activity() {
        let mut app = setup();
        app.state.connections.get_mut("one").unwrap().enabled_caps.insert("draft/read-marker".into());
        crate::irc::events::handle_irc_message(&mut app.state, "one",
            &"@msgid=unread;time=2026-09-20T01:00:00Z :alice!u@h PRIVMSG #a :me secret".parse().unwrap());
        assert_eq!(app.state.buffers["one/#a"].activity, super::super::buffer::ActivityLevel::Mention);
        app.state.pending_web_events.clear();
        app.state.receive_redaction("one", &":alice!u@h REDACT #a unread".parse().unwrap());
        assert_eq!(app.state.buffers["one/#a"].activity, super::super::buffer::ActivityLevel::Events);
        assert_eq!(app.state.buffers["one/#a"].unread_count, 1);
        assert!(app.state.pending_web_events.iter().any(|event| matches!(event,
            crate::web::protocol::WebEvent::ActivityChanged { buffer_id, activity: 1, unread_count: 1, .. } if buffer_id == "one/#a")));
    }

    #[tokio::test]
    async fn redaction_removes_volatile_mention_and_reopened_aggregate_copy() {
        let mut app = setup();
        crate::irc::events::handle_irc_message(&mut app.state, "one",
            &"@msgid=volatile :alice!u@h PRIVMSG #a :me original secret".parse().unwrap());
        app.drain_pending_web_events();
        assert_eq!(app.volatile_mentions.len(), 1);
        app.create_mentions_buffer();
        assert!(app.state.buffers["_mentions"].messages.iter().any(|message| message.text.contains("original secret")));
        app.state.receive_redaction("one", &":alice!u@h REDACT #a volatile".parse().unwrap());
        app.drain_pending_web_events();
        assert!(app.volatile_mentions.is_empty());
        assert!(!app.state.buffers["_mentions"].messages.iter().any(|message| message.text.contains("original secret")));
        app.state.buffers.shift_remove("_mentions");
        app.create_mentions_buffer();
        assert!(app.state.buffers["_mentions"].messages.is_empty());
    }

    #[tokio::test]
    async fn deletion_before_dispatch_suppresses_queued_mention_alert() {
        let mut app = setup();
        let mut events = app.web_broadcaster.subscribe();
        crate::irc::events::handle_irc_message(&mut app.state, "one",
            &"@msgid=pending-alert :alice!u@h PRIVMSG #a :me secret".parse().unwrap());
        app.state.receive_redaction("one", &":alice!u@h REDACT #a pending-alert".parse().unwrap());
        app.drain_pending_web_events();
        assert!(app.volatile_mentions.is_empty());
        assert!(!std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(event,
            crate::web::protocol::WebEvent::MentionAlert { .. })));
    }

}
