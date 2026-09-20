use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use irc::proto::{Command, Message, Prefix};

const MAX_TARGETS: usize = 1000;

#[derive(Clone, Default)]
pub struct Peer {
    pub nick: String,
    pub online: Option<bool>,
    pub account: Option<String>,
    pub away: Option<bool>,
    pub ident: Option<String>,
    pub host: Option<String>,
    pub realname: Option<String>,
}

struct Snapshot {
    names: BTreeMap<String, String>,
    started: Instant,
    valid: bool,
    expired: bool,
}

#[derive(Clone)]
pub enum Change {
    Add(Vec<String>),
    Remove(Vec<String>),
    Clear,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Support {
    Available,
    Unavailable,
}

pub struct MonitorState {
    pub scope: String,
    pub peers: BTreeMap<String, Peer>,
    wanted: Option<BTreeMap<String, String>>,
    confirmed: Option<BTreeMap<String, String>>,
    pending_changes: Vec<Change>,
    submitted: Option<Change>,
    snapshot: Option<Snapshot>,
    mapping: String,
    retry: Option<Instant>,
    blocked: bool,
    support: Support,
    metadata_caps: [bool; 5],
    pub show_list: bool,
    pub request_status: bool,
}

pub fn fold(nick: &str, mapping: &str) -> String {
    nick.chars()
        .map(|c| match c {
            '[' if mapping != "ascii" => '{',
            ']' if mapping != "ascii" => '}',
            '\\' if mapping != "ascii" => '|',
            '^' if mapping == "rfc1459" => '~',
            _ => c.to_ascii_lowercase(),
        })
        .collect()
}

pub fn targets(input: &[String]) -> Result<Vec<String>, &'static str> {
    let names: Vec<_> = input
        .iter()
        .flat_map(|arg| arg.split(','))
        .map(str::to_string)
        .collect();
    if names.is_empty()
        || names.len() > MAX_TARGETS
        || names.iter().any(|nick| {
            nick.is_empty()
                || nick.len() > 200
                || nick
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace() || ",!@:*?".contains(c))
                || nick.starts_with(['#', '&'])
        })
    {
        return Err("Supply valid nicknames separated by spaces or commas (at most 1000)");
    }
    Ok(names)
}

impl MonitorState {
    pub const fn new(scope: String, mapping: String) -> Self {
        Self {
            scope,
            mapping,
            peers: BTreeMap::new(),
            wanted: None,
            confirmed: None,
            pending_changes: Vec::new(),
            submitted: None,
            snapshot: None,
            retry: None,
            blocked: false,
            support: Support::Unavailable,
            metadata_caps: [false; 5],
            show_list: false,
            request_status: false,
        }
    }

    pub fn set_supported(&mut self, supported: bool) {
        let support = if supported {
            Support::Available
        } else {
            Support::Unavailable
        };
        if self.support == support {
            return;
        }
        self.support = support;
        self.snapshot = None;
        self.submitted = None;
        self.retry = None;
        self.confirmed = None;
        self.blocked = !supported;
        for peer in self.peers.values_mut() {
            *peer = Peer {
                nick: peer.nick.clone(),
                ..Peer::default()
            };
        }
    }

    pub fn set_metadata_caps(&mut self, caps: &HashSet<String>) {
        let active = [
            caps.contains("account-notify"),
            caps.contains("away-notify"),
            caps.contains("chghost"),
            caps.contains("setname"),
            caps.contains("extended-monitor") || caps.contains("draft/extended-monitor"),
        ];
        let lost_extended = self.metadata_caps[4] && !active[4];
        for peer in self.peers.values_mut() {
            if lost_extended || (self.metadata_caps[0] && !active[0]) {
                peer.account = None;
            }
            if lost_extended || (self.metadata_caps[1] && !active[1]) {
                peer.away = None;
            }
            if lost_extended || (self.metadata_caps[2] && !active[2]) {
                peer.ident = None;
                peer.host = None;
            }
            if lost_extended || (self.metadata_caps[3] && !active[3]) {
                peer.realname = None;
            }
        }
        self.metadata_caps = active;
    }

    pub fn reset_transport(&mut self) {
        self.submitted = None;
        self.confirmed = None;
        self.snapshot = None;
        self.retry = None;
        self.blocked = false;
        for peer in self.peers.values_mut() {
            *peer = Peer {
                nick: peer.nick.clone(),
                ..Peer::default()
            };
        }
    }

    pub fn set_mapping(&mut self, mapping: &str) {
        if self.mapping == mapping {
            return;
        }
        self.mapping = mapping.to_string();
        if let Some(wanted) = &mut self.wanted {
            *wanted = wanted
                .values()
                .map(|name| (fold(name, mapping), name.clone()))
                .collect();
        }
        if let Some(confirmed) = &mut self.confirmed {
            *confirmed = confirmed
                .values()
                .map(|name| (fold(name, mapping), name.clone()))
                .collect();
        }
        self.peers = std::mem::take(&mut self.peers)
            .into_values()
            .map(|peer| (fold(&peer.nick, mapping), peer))
            .collect();
        if let Some(snapshot) = &mut self.snapshot {
            snapshot.names = snapshot
                .names
                .values()
                .map(|name| (fold(name, mapping), name.clone()))
                .collect();
        }
    }

    pub fn change(&mut self, change: Change) -> Result<(), &'static str> {
        if let Some(wanted) = &mut self.wanted {
            apply(wanted, &change, &self.mapping)?;
        } else {
            let count: usize = self
                .pending_changes
                .iter()
                .map(|change| match change {
                    Change::Add(names) | Change::Remove(names) => names.len(),
                    Change::Clear => 1,
                })
                .sum();
            let additional = match &change {
                Change::Add(names) | Change::Remove(names) => names.len(),
                Change::Clear => 1,
            };
            if count + additional > MAX_TARGETS {
                return Err("Waiting for the initial MONITOR list; queued change limit reached");
            }
            self.pending_changes.push(change);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn synchronized(&self) -> bool {
        !self.blocked
            && self.snapshot.is_none()
            && self.submitted.is_none()
            && self
                .confirmed
                .as_ref()
                .zip(self.wanted.as_ref())
                .is_some_and(|(confirmed, wanted)| confirmed.keys().eq(wanted.keys()))
    }

    pub fn next_command(&self) -> Option<Command> {
        if self.blocked
            || self.snapshot.is_some()
            || self
                .retry
                .is_some_and(|time| time.elapsed() < Duration::from_secs(5))
        {
            return None;
        }
        let Some(confirmed) = &self.confirmed else {
            return Some(Command::MONITOR("L".into(), None));
        };
        let Some(wanted) = &self.wanted else {
            return Some(Command::MONITOR("L".into(), None));
        };
        let removed = confirmed
            .iter()
            .filter(|(key, _)| !wanted.contains_key(*key))
            .map(|(_, name)| name);
        let added = wanted
            .iter()
            .filter(|(key, _)| !confirmed.contains_key(*key))
            .map(|(_, name)| name);
        for (operation, names) in [
            ("-", removed.collect::<Vec<_>>()),
            ("+", added.collect::<Vec<_>>()),
        ] {
            let mut packed = String::new();
            for name in names {
                if packed.len() + name.len() + 1 > 480 {
                    break;
                }
                if !packed.is_empty() {
                    packed.push(',');
                }
                packed.push_str(name);
            }
            if !packed.is_empty() {
                return Some(Command::MONITOR(operation.into(), Some(packed)));
            }
        }
        if self.show_list {
            return Some(Command::MONITOR("L".into(), None));
        }
        if self.request_status {
            return Some(Command::MONITOR("S".into(), None));
        }
        None
    }

    pub fn sent(&mut self, command: &Command, success: bool) {
        self.retry = Some(Instant::now());
        if !success {
            return;
        }
        self.retry = None;
        if let Command::MONITOR(operation, names) = command {
            match operation.as_str() {
                "L" => {
                    self.snapshot = Some(Snapshot {
                        names: BTreeMap::new(),
                        started: Instant::now(),
                        valid: true,
                        expired: false,
                    });
                }
                "S" => self.request_status = false,
                "+" | "-" => {
                    let names = names
                        .as_deref()
                        .unwrap_or_default()
                        .split(',')
                        .map(str::to_string)
                        .collect();
                    self.submitted = Some(if operation == "+" {
                        Change::Add(names)
                    } else {
                        Change::Remove(names)
                    });
                    self.confirmed = None;
                }
                _ => self.confirmed = None,
            }
        }
    }

    pub fn expire(&mut self) -> bool {
        if let Some(snapshot) = &mut self.snapshot
            && !snapshot.expired
            && snapshot.started.elapsed() >= Duration::from_secs(30)
        {
            snapshot.expired = true;
            return true;
        }
        false
    }

    pub fn receive(&mut self, message: &Message) -> (bool, Vec<String>) {
        let (number, args) = match &message.command {
            Command::Response(number, args) => (*number as u16, args),
            Command::Raw(command, args) => (command.parse::<u16>().unwrap_or(0), args),
            _ => return (false, Vec::new()),
        };
        if matches!(number, 421 | 461)
            && args
                .get(1)
                .is_some_and(|arg| arg.eq_ignore_ascii_case("MONITOR"))
        {
            self.blocked = true;
            self.snapshot = None;
            self.confirmed = None;
            return (
                true,
                vec!["MONITOR is currently unavailable; targets are retained for reconnect".into()],
            );
        }
        if !(730..=734).contains(&number) {
            return (false, Vec::new());
        }
        if self.support != Support::Available {
            return (true, Vec::new());
        }
        let mut output = Vec::new();
        match number {
            730 | 731 => {
                self.receive_status(number == 730, args, &mut output);
            }
            732 => {
                if let Some(snapshot) = &mut self.snapshot
                    && !snapshot.expired
                {
                    if let Some(names) = args
                        .get(1)
                        .and_then(|names| targets(std::slice::from_ref(names)).ok())
                    {
                        for name in names {
                            if snapshot.names.len() >= MAX_TARGETS
                                && !snapshot.names.contains_key(&fold(&name, &self.mapping))
                            {
                                snapshot.valid = false;
                                break;
                            }
                            snapshot.names.insert(fold(&name, &self.mapping), name);
                        }
                    } else {
                        snapshot.valid = false;
                    }
                }
            }
            733 => {
                if let Some(snapshot) = self.snapshot.take() {
                    if !snapshot.valid || snapshot.expired {
                        self.confirmed = None;
                        self.blocked = true;
                        output.push(
                            "Incomplete MONITOR list ignored; reconnect to synchronize safely"
                                .into(),
                        );
                    } else {
                        if self.wanted.is_none() {
                            let mut wanted = snapshot.names.clone();
                            for change in std::mem::take(&mut self.pending_changes) {
                                if let Err(error) = apply(&mut wanted, &change, &self.mapping) {
                                    output.push(error.into());
                                }
                            }
                            self.wanted = Some(wanted);
                        }
                        self.verify_change(&snapshot.names, &mut output);
                        self.confirmed = Some(snapshot.names);
                        self.peers.retain(|key, _| {
                            self.wanted
                                .as_ref()
                                .is_some_and(|wanted| wanted.contains_key(key))
                        });
                        if let Some(wanted) = &self.wanted {
                            for (key, nick) in wanted {
                                self.peers.entry(key.clone()).or_insert_with(|| Peer {
                                    nick: nick.clone(),
                                    ..Peer::default()
                                });
                            }
                        }
                        if self.show_list {
                            output.extend(self.rows());
                            self.show_list = false;
                        }
                    }
                }
            }
            734 => {
                self.receive_full(args, &mut output);
            }
            _ => {}
        }
        (true, output)
    }

    fn verify_change(&mut self, names: &BTreeMap<String, String>, output: &mut Vec<String>) {
        match self.submitted.take() {
            Some(Change::Add(added)) => {
                for nick in added {
                    let key = fold(&nick, &self.mapping);
                    if !names.contains_key(&key)
                        && self
                            .wanted
                            .as_mut()
                            .is_some_and(|wanted| wanted.remove(&key).is_some())
                    {
                        output.push(format!(
                            "MONITOR: {nick} was not accepted by the server; target removed"
                        ));
                    }
                }
            }
            Some(Change::Remove(removed))
                if removed
                    .iter()
                    .any(|nick| names.contains_key(&fold(nick, &self.mapping))) =>
            {
                self.blocked = true;
                output.push("MONITOR removal was not accepted; reconnect before retrying".into());
            }
            _ => {}
        }
    }

    fn receive_full(&mut self, args: &[String], output: &mut Vec<String>) {
        if let Some(names) = args.get(2) {
            for nick in names.split(',') {
                let key = fold(nick, &self.mapping);
                if let Some(wanted) = &mut self.wanted {
                    wanted.remove(&key);
                }
                self.peers.remove(&key);
                output.push(format!("MONITOR: {nick} rejected because the list is full"));
            }
        }
    }

    fn receive_status(&mut self, online: bool, args: &[String], output: &mut Vec<String>) {
        if let Some(names) = args.get(1) {
            for mask in names.split(',') {
                let (nick, handle) = mask
                    .split_once('!')
                    .map_or((mask, None), |(nick, handle)| (nick, Some(handle)));
                let key = fold(nick, &self.mapping);
                if !self
                    .wanted
                    .as_ref()
                    .is_some_and(|wanted| wanted.contains_key(&key))
                {
                    continue;
                }
                let peer = self.peers.entry(key).or_insert_with(|| Peer {
                    nick: nick.into(),
                    ..Peer::default()
                });
                if peer.online != Some(online) {
                    output.push(format!(
                        "MONITOR: {nick} is {}",
                        if online { "online" } else { "offline" }
                    ));
                }
                if !online {
                    *peer = Peer {
                        nick: nick.into(),
                        ..Peer::default()
                    };
                }
                peer.online = Some(online);
                if let Some((ident, host)) = handle.and_then(|handle| handle.split_once('@')) {
                    peer.ident = Some(ident.into());
                    peer.host = Some(host.into());
                }
            }
        }
    }

    pub fn observe_extended(&mut self, message: &Message) {
        if self.support != Support::Available {
            return;
        }
        let Some(Prefix::Nickname(nick, _, _)) = &message.prefix else {
            return;
        };
        let key = fold(nick, &self.mapping);
        if !self
            .wanted
            .as_ref()
            .is_some_and(|wanted| wanted.contains_key(&key))
        {
            return;
        }
        let peer = self.peers.entry(key).or_insert_with(|| Peer {
            nick: nick.clone(),
            ..Peer::default()
        });
        match &message.command {
            Command::ACCOUNT(account) => peer.account = (account != "*").then(|| account.clone()),
            Command::AWAY(reason) => peer.away = Some(reason.is_some()),
            Command::CHGHOST(ident, host) => {
                peer.ident = Some(ident.clone());
                peer.host = Some(host.clone());
            }
            Command::Raw(command, args)
                if command.eq_ignore_ascii_case("SETNAME") && args.len() == 1 =>
            {
                peer.realname = Some(args[0].clone());
            }
            _ => {}
        }
    }

    pub fn rows(&self) -> Vec<String> {
        let Some(wanted) = &self.wanted else {
            return vec!["MONITOR list has not been synchronized yet".into()];
        };
        let mut names = wanted.clone();
        if let Some(confirmed) = &self.confirmed {
            names.extend(confirmed.clone());
        }
        if names.is_empty() {
            return vec!["MONITOR list is empty".into()];
        }
        names
            .iter()
            .map(|(key, nick)| {
                let unknown = Peer {
                    nick: nick.clone(),
                    ..Peer::default()
                };
                let peer = self.peers.get(key).unwrap_or(&unknown);
                let subscription = if !wanted.contains_key(key) {
                    "removing"
                } else if self
                    .confirmed
                    .as_ref()
                    .is_some_and(|list| list.contains_key(key))
                {
                    "subscribed"
                } else {
                    "pending"
                };
                let online = match peer.online {
                    Some(true) => "online",
                    Some(false) => "offline",
                    None => "unknown",
                };
                let away = match peer.away {
                    Some(true) => "away",
                    Some(false) => "present",
                    None => "away unknown",
                };
                format!(
                    "{}: {subscription}, {online}, {away}; account {}; {}@{}; real name {}",
                    peer.nick,
                    peer.account.as_deref().unwrap_or("unknown"),
                    peer.ident.as_deref().unwrap_or("?"),
                    peer.host.as_deref().unwrap_or("?"),
                    peer.realname.as_deref().unwrap_or("unknown")
                )
            })
            .collect()
    }
}

fn apply(
    wanted: &mut BTreeMap<String, String>,
    change: &Change,
    mapping: &str,
) -> Result<(), &'static str> {
    match change {
        Change::Add(names) => {
            let mut updated = wanted.clone();
            for name in names {
                updated.insert(fold(name, mapping), name.clone());
            }
            if updated.len() > MAX_TARGETS {
                return Err("MONITOR target limit is 1000");
            }
            *wanted = updated;
        }
        Change::Remove(names) => {
            for name in names {
                wanted.remove(&fold(name, mapping));
            }
        }
        Change::Clear => wanted.clear(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MonitorState {
        let mut state = MonitorState::new("network/account".into(), "rfc1459".into());
        state.set_supported(true);
        state
    }

    fn receive(state: &mut MonitorState, wire: &str) -> Vec<String> {
        state.receive(&wire.parse::<Message>().unwrap()).1
    }

    fn send(state: &mut MonitorState, operation: &str, names: Option<&str>) {
        let command = state.next_command().expect("expected MONITOR command");
        assert_eq!(
            command,
            Command::MONITOR(operation.into(), names.map(str::to_string))
        );
        state.sent(&command, true);
    }

    fn snapshot(state: &mut MonitorState, names: &str) {
        send(state, "L", None);
        if !names.is_empty() {
            receive(state, &format!(":server 732 me :{names}"));
        }
        receive(state, ":server 733 me :End of MONITOR list");
    }

    #[test]
    fn initial_list_preserves_existing_targets_and_requires_mutation_barriers() {
        let mut state = state();
        state.change(Change::Add(vec!["Bob".into()])).unwrap();
        assert!(state.rows()[0].contains("not been synchronized"));
        snapshot(&mut state, "Alice");
        assert!(
            state
                .rows()
                .iter()
                .any(|row| row.starts_with("Alice: subscribed"))
        );
        assert!(
            state
                .rows()
                .iter()
                .any(|row| row.starts_with("Bob: pending"))
        );
        send(&mut state, "+", Some("Bob"));
        send(&mut state, "L", None);
        receive(&mut state, ":server 732 me :Alice");
        assert!(state.next_command().is_none());
        receive(&mut state, ":server 732 me :Bob");
        receive(&mut state, ":server 733 me :End");
        assert!(state.next_command().is_none());
        state.change(Change::Remove(vec!["ALICE".into()])).unwrap();
        assert!(
            state
                .rows()
                .iter()
                .any(|row| row.starts_with("Alice: removing"))
        );
        send(&mut state, "-", Some("Alice"));
        snapshot(&mut state, "Bob");
        assert_eq!(state.rows().len(), 1);
        assert!(state.rows()[0].starts_with("Bob: subscribed"));
    }

    #[test]
    fn failed_list_send_does_not_repeat_an_accepted_add() {
        let mut state = state();
        snapshot(&mut state, "");
        state.change(Change::Add(vec!["Bob".into()])).unwrap();
        send(&mut state, "+", Some("Bob"));
        let list = state.next_command().unwrap();
        state.sent(&list, false);
        assert!(state.next_command().is_none());
        state.retry = Some(Instant::now().checked_sub(Duration::from_secs(6)).unwrap());
        snapshot(&mut state, "Bob");
        assert!(state.next_command().is_none());
    }

    #[test]
    fn expired_and_invalid_snapshots_cannot_drive_removals() {
        for invalid in [false, true] {
            let mut state = state();
            snapshot(&mut state, "Alice");
            state.show_list = true;
            send(&mut state, "L", None);
            if invalid {
                receive(&mut state, ":server 732 me :#invalid");
            } else {
                state.snapshot.as_mut().unwrap().started =
                    Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
                assert!(state.expire());
                assert!(!state.expire());
                assert!(state.next_command().is_none());
            }
            assert!(receive(&mut state, ":server 733 me :End")[0].contains("ignored"));
            assert!(state.confirmed.is_none());
            assert!(state.next_command().is_none());
            state.reset_transport();
            snapshot(&mut state, "");
            send(&mut state, "+", Some("Alice"));
        }
    }

    #[test]
    fn omitted_add_and_ignored_remove_do_not_repeat_forever() {
        let mut state = state();
        snapshot(&mut state, "Alice");
        state
            .change(Change::Add(vec!["RejectedNickname".into(), "Bob".into()]))
            .unwrap();
        send(&mut state, "+", Some("Bob,RejectedNickname"));
        snapshot(&mut state, "Alice,Bob");
        assert!(state.next_command().is_none());
        assert!(
            !state
                .wanted
                .as_ref()
                .unwrap()
                .contains_key("rejectednickname")
        );
        state.change(Change::Remove(vec!["Bob".into()])).unwrap();
        send(&mut state, "-", Some("Bob"));
        snapshot(&mut state, "Alice,Bob");
        assert!(state.next_command().is_none());
        assert!(state.blocked);
    }

    #[test]
    fn full_list_rejection_keeps_accepted_subset_without_retrying_rejected_nicks() {
        let mut state = state();
        snapshot(&mut state, "");
        state
            .change(Change::Add(vec!["Alice".into(), "Bob".into()]))
            .unwrap();
        send(&mut state, "+", Some("Alice,Bob"));
        receive(&mut state, ":server 734 me 1 Bob :Monitor list is full");
        snapshot(&mut state, "Alice");
        assert!(state.next_command().is_none());
        assert_eq!(state.rows().len(), 1);
        assert!(state.rows()[0].starts_with("Alice:"));
    }

    #[test]
    fn support_withdrawal_discards_pending_list_and_late_presence() {
        let mut state = state();
        snapshot(&mut state, "Alice");
        state.change(Change::Add(vec!["Bob".into()])).unwrap();
        send(&mut state, "+", Some("Bob"));
        send(&mut state, "L", None);
        receive(&mut state, ":server 732 me :Alice");
        state.set_supported(false);
        for wire in [
            ":server 730 me :Alice!u@h",
            ":server 731 me :Bob",
            ":server 732 me :Bob",
            ":server 733 me :End",
        ] {
            assert!(receive(&mut state, wire).is_empty());
        }
        state.observe_extended(&":Alice!u@h ACCOUNT old-account".parse::<Message>().unwrap());
        assert_eq!(state.peers["alice"].online, None);
        assert!(state.peers["alice"].account.is_none());
        assert!(state.snapshot.is_none());
        assert!(state.submitted.is_none());
        state.set_supported(true);
        snapshot(&mut state, "Alice");
        send(&mut state, "+", Some("Bob"));
        snapshot(&mut state, "Alice,Bob");
        assert!(state.synchronized());
    }

    #[test]
    fn reconnect_and_support_loss_invalidate_observations_but_keep_intent() {
        let mut state = state();
        snapshot(&mut state, "Alice");
        receive(&mut state, ":server 730 me :Alice!user@host");
        assert_eq!(state.peers["alice"].online, Some(true));
        state.set_supported(false);
        assert_eq!(state.peers["alice"].online, None);
        assert!(state.next_command().is_none());
        state.set_supported(true);
        snapshot(&mut state, "");
        send(&mut state, "+", Some("Alice"));
        snapshot(&mut state, "Alice");
        receive(&mut state, ":server 730 me :Alice!user@host");
        state.reset_transport();
        assert_eq!(state.peers["alice"].online, None);
        assert!(state.peers["alice"].host.is_none());
        snapshot(&mut state, "");
        send(&mut state, "+", Some("Alice"));
    }

    #[test]
    fn extended_notifications_update_monitored_peers_without_channel_membership() {
        let mut state = state();
        snapshot(&mut state, "Alice");
        let mut caps: HashSet<String> = [
            "account-notify",
            "away-notify",
            "chghost",
            "setname",
            "draft/extended-monitor",
            "extended-monitor",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        state.set_metadata_caps(&caps);
        for wire in [
            ":Alice!u@h ACCOUNT alice",
            ":Alice!u@h AWAY :Busy",
            ":Alice!u@h CHGHOST new host",
            ":Alice!u@h SETNAME :Alice Example",
        ] {
            state.observe_extended(&wire.parse::<Message>().unwrap());
        }
        let peer = &state.peers["alice"];
        assert_eq!(peer.account.as_deref(), Some("alice"));
        assert_eq!(peer.away, Some(true));
        assert_eq!(peer.ident.as_deref(), Some("new"));
        assert_eq!(peer.host.as_deref(), Some("host"));
        assert_eq!(peer.realname.as_deref(), Some("Alice Example"));
        state.observe_extended(&":Stranger!u@h ACCOUNT other".parse::<Message>().unwrap());
        assert_eq!(state.peers.len(), 1);
        caps.remove("extended-monitor");
        state.set_metadata_caps(&caps);
        assert_eq!(state.peers["alice"].account.as_deref(), Some("alice"));
        caps.remove("draft/extended-monitor");
        state.set_metadata_caps(&caps);
        assert!(state.peers["alice"].account.is_none());
        assert!(state.peers["alice"].away.is_none());
        assert!(state.peers["alice"].host.is_none());
        assert!(state.peers["alice"].realname.is_none());
    }

    #[test]
    fn nick_casemapping_and_queued_limits_are_bounded() {
        assert_eq!(fold("A[]\\^", "rfc1459"), "a{}|~");
        assert_eq!(fold("A[]\\^", "strict-rfc1459"), "a{}|^");
        assert_eq!(fold("A[]\\^", "ascii"), "a[]\\^");
        let mut state = state();
        state
            .change(Change::Add(vec!["Alice".into(); 999]))
            .unwrap();
        assert!(state.change(Change::Add(vec!["Bob".into(); 2])).is_err());
        snapshot(&mut state, "");
        assert_eq!(state.wanted.as_ref().unwrap().len(), 1);
        assert!(targets(&["Alice,,Bob".into()]).is_err());
        assert!(targets(&["Alice\r\nQUIT".into()]).is_err());
        assert!(targets(&["Alice,Bob".into()]).is_ok());
    }
}
