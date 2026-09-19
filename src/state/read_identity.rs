use super::AppState;
use super::buffer::Message;

#[derive(Default)]
enum AccountIdentity {
    #[default]
    Unknown,
    Known(Option<String>),
}

#[derive(Default)]
pub(super) struct ReadIdentity {
    account: AccountIdentity,
    current_nick: String,
    current_since: Option<i64>,
    previous: Vec<(String, Option<i64>, i64)>,
}

impl AppState {
    pub(crate) fn record_read_account(&mut self, conn_id: &str, nick: &str, account: Option<&str>) {
        if self
            .connections
            .get(conn_id)
            .is_some_and(|conn| conn.server_owns_history() && conn.nick.eq_ignore_ascii_case(nick))
        {
            self.read_identities
                .entry(conn_id.to_string())
                .or_default()
                .account = AccountIdentity::Known(
                account
                    .filter(|account| !account.is_empty() && *account != "*")
                    .map(str::to_string),
            );
        }
    }

    pub(crate) fn record_read_account_message(&mut self, conn_id: &str, msg: &irc::proto::Message) {
        use irc::proto::{Command, Prefix};
        let (numeric, args) = match &msg.command {
            Command::Response(response, args) => (*response as u16, args),
            Command::Raw(command, args) => (command.parse::<u16>().unwrap_or(0), args),
            _ => return,
        };
        if matches!(msg.prefix.as_ref(), Some(Prefix::ServerName(name)) if name == "lurker.bouncer")
        {
            return;
        }
        if numeric == 900
            && let (Some(nick), Some(account)) = (args.first(), args.get(2))
        {
            self.record_read_account(conn_id, nick, Some(account));
        } else if numeric == 901
            && let Some(nick) = args.first()
        {
            self.record_read_account(conn_id, nick, None);
        }
    }

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
            if let Some(ReadIdentity {
                account: AccountIdentity::Known(account_state),
                ..
            }) = self.read_identities.get(conn_id)
            {
                return account_state
                    .as_deref()
                    .is_some_and(|own| account.eq_ignore_ascii_case(own));
            }
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
