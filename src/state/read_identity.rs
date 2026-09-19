use super::AppState;
use super::buffer::Message;

#[derive(Default)]
pub(super) struct ReadIdentity {
    current_nick: String,
    current_since: Option<i64>,
    previous: Vec<(String, Option<i64>, i64)>,
}

impl AppState {
    pub(crate) fn record_read_nick_change(&mut self, conn_id: &str, old: &str, new: &str, at: i64) {
        if !self
            .connections
            .get(conn_id)
            .is_some_and(super::connection::Connection::server_owns_history)
        {
            return;
        }
        let identity = self.read_identities.entry(conn_id.to_string()).or_default();
        let since = if identity.current_nick.eq_ignore_ascii_case(old) {
            identity.current_since
        } else {
            None
        };
        identity.previous.push((old.to_string(), since, at));
        identity.current_nick = new.to_string();
        identity.current_since = Some(at);
    }

    pub(super) fn is_own_history_message(
        &self,
        buffer_id: &str,
        message: &Message,
        own_nick: Option<&str>,
    ) -> bool {
        let Some((conn_id, _)) = buffer_id.split_once('/') else {
            return false;
        };
        let Some(nick) = message.nick.as_deref() else {
            return false;
        };
        if let Some(account) = message
            .tags
            .as_ref()
            .and_then(|tags| tags.get("account"))
            .filter(|account| !account.is_empty() && *account != "*")
        {
            let own_account = own_nick.and_then(|own| {
                self.buffers
                    .values()
                    .filter(|buffer| buffer.connection_id == conn_id)
                    .flat_map(|buffer| buffer.users.values())
                    .filter(|entry| entry.nick.eq_ignore_ascii_case(own))
                    .find_map(|entry| entry.account.as_deref())
            });
            if let Some(own_account) = own_account {
                return account.eq_ignore_ascii_case(own_account);
            }
        }
        let timestamp = message.timestamp.timestamp_millis();
        if let Some(identity) = self.read_identities.get(conn_id) {
            if identity.previous.iter().any(|(previous, since, until)| {
                previous.eq_ignore_ascii_case(nick)
                    && since.is_none_or(|since| timestamp >= since)
                    && timestamp <= *until
            }) {
                return true;
            }
            if identity.current_nick.eq_ignore_ascii_case(nick) {
                return identity
                    .current_since
                    .is_none_or(|since| timestamp >= since);
            }
        }
        own_nick.is_some_and(|own| nick.eq_ignore_ascii_case(own))
    }
}
