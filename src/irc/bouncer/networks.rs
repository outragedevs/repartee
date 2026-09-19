use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use irc::proto::{Command, Message};

use super::{NETWORKS_CAP, normalize_network_id};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Network {
    pub id: String,
    pub attributes: BTreeMap<String, String>,
}

impl Network {
    pub fn name(&self) -> &str {
        self.attributes.get("name").map_or(&self.id, String::as_str)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegistryEvent {
    Unrelated,
    Pending,
    Snapshot,
    Changed(String),
    Invalid,
}

struct Pending {
    tag: String,
    networks: BTreeMap<String, Network>,
    valid: bool,
    started: Instant,
}

#[derive(Default)]
pub struct NetworkRegistry {
    pub networks: BTreeMap<String, Network>,
    pub complete: bool,
    pending: Option<Pending>,
}

impl NetworkRegistry {
    pub fn expire(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.started.elapsed() > Duration::from_secs(30))
        {
            self.pending = None;
        }
    }

    pub fn handle(&mut self, message: &Message) -> RegistryEvent {
        self.expire();
        if let Command::BATCH(reference, kind, _) = &message.command {
            if let Some(tag) = reference.strip_prefix('+')
                && kind
                    .as_ref()
                    .is_some_and(|kind| kind.to_str().eq_ignore_ascii_case(NETWORKS_CAP))
            {
                self.pending = Some(Pending {
                    tag: tag.to_string(),
                    networks: BTreeMap::new(),
                    valid: true,
                    started: Instant::now(),
                });
                return RegistryEvent::Pending;
            }
            if let Some(tag) = reference.strip_prefix('-')
                && self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.tag == tag)
            {
                let pending = self.pending.take().unwrap();
                if !pending.valid {
                    return RegistryEvent::Invalid;
                }
                self.networks = pending.networks;
                self.complete = true;
                return RegistryEvent::Snapshot;
            }
            return RegistryEvent::Unrelated;
        }
        let Command::Raw(command, args) = &message.command else {
            return RegistryEvent::Unrelated;
        };
        if !command.eq_ignore_ascii_case("BOUNCER")
            || !args
                .first()
                .is_some_and(|arg| arg.eq_ignore_ascii_case("NETWORK"))
        {
            return RegistryEvent::Unrelated;
        }
        let batch = message
            .tags
            .as_ref()
            .and_then(|tags| tags.iter().find(|tag| tag.0 == "batch"))
            .and_then(|tag| tag.1.as_deref());
        if batch.is_some_and(|tag| {
            self.pending
                .as_ref()
                .is_none_or(|pending| pending.tag != tag)
        }) {
            return RegistryEvent::Pending;
        }
        let Some((id, attributes)) = parse_update(args) else {
            if batch.is_some()
                && let Some(pending) = self.pending.as_mut()
            {
                pending.valid = false;
            }
            return RegistryEvent::Invalid;
        };
        if let Some(pending) = self.pending.as_mut() {
            apply_update(&mut pending.networks, &id, attributes.as_ref());
        }
        if batch.is_some() {
            return RegistryEvent::Pending;
        }
        apply_update(&mut self.networks, &id, attributes.as_ref());
        RegistryEvent::Changed(id)
    }
}

type Attributes = BTreeMap<String, Option<String>>;

fn parse_update(args: &[String]) -> Option<(String, Option<Attributes>)> {
    if args.len() != 3 {
        return None;
    }
    let id = normalize_network_id(&args[1]).ok()?;
    if args[2] == "*" {
        return Some((id, None));
    }
    if args[2].is_empty() || args[2].chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let parsed: Message = format!("@{} TAGMSG *", args[2]).parse().ok()?;
    let attributes = parsed.tags?.into_iter().map(|tag| (tag.0, tag.1)).collect();
    Some((id, Some(attributes)))
}

fn apply_update(
    networks: &mut BTreeMap<String, Network>,
    id: &str,
    attributes: Option<&Attributes>,
) {
    let Some(attributes) = attributes else {
        networks.remove(id);
        return;
    };
    let network = networks.entry(id.to_string()).or_insert_with(|| Network {
        id: id.to_string(),
        attributes: BTreeMap::new(),
    });
    for (key, value) in attributes {
        if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
            network.attributes.insert(key.clone(), value.clone());
        } else {
            network.attributes.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receive(registry: &mut NetworkRegistry, line: &str) -> RegistryEvent {
        registry.handle(&line.parse().unwrap())
    }

    #[test]
    fn snapshots_commit_atomically_and_empty_snapshots_remove_networks() {
        let mut registry = NetworkRegistry::default();
        receive(&mut registry, "BOUNCER NETWORK 1 name=old");
        receive(&mut registry, "BATCH +a soju.im/bouncer-networks");
        receive(
            &mut registry,
            "@batch=a BOUNCER NETWORK 2 name=New\\sNetwork;state=connected",
        );
        assert_eq!(registry.networks.len(), 1);
        assert!(registry.networks.contains_key("1"));
        assert_eq!(receive(&mut registry, "BATCH -a"), RegistryEvent::Snapshot);
        assert_eq!(registry.networks.len(), 1);
        assert_eq!(registry.networks["2"].name(), "New Network");
        receive(&mut registry, "BATCH +b soju.im/bouncer-networks");
        receive(&mut registry, "BATCH -b");
        assert!(registry.networks.is_empty());
        assert!(registry.complete);
    }

    #[test]
    fn notifications_merge_attributes_and_clear_both_server_encodings() {
        let mut registry = NetworkRegistry::default();
        receive(
            &mut registry,
            "BOUNCER NETWORK 42 name=One\\:Two;state=connected;error=old;future=value",
        );
        receive(
            &mut registry,
            "BOUNCER NETWORK 42 state=disconnected;error;future=",
        );
        let attributes = &registry.networks["42"].attributes;
        assert_eq!(attributes["name"], "One;Two");
        assert_eq!(attributes["state"], "disconnected");
        assert!(!attributes.contains_key("error"));
        assert!(!attributes.contains_key("future"));
        receive(&mut registry, "BOUNCER NETWORK 42 *");
        assert!(registry.networks.is_empty());
    }

    #[test]
    fn malformed_and_abandoned_snapshots_cannot_delete_known_networks() {
        let mut registry = NetworkRegistry::default();
        receive(&mut registry, "BOUNCER NETWORK 1 name=known");
        receive(&mut registry, "BATCH +a soju.im/bouncer-networks");
        receive(&mut registry, "@batch=a BOUNCER NETWORK invalid name=bad");
        assert_eq!(receive(&mut registry, "BATCH -a"), RegistryEvent::Invalid);
        assert!(registry.networks.contains_key("1"));
        receive(&mut registry, "BATCH +b soju.im/bouncer-networks");
        registry.pending.as_mut().unwrap().started =
            Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
        receive(&mut registry, "@batch=b BOUNCER NETWORK 2 name=orphan");
        receive(&mut registry, "BATCH -b");
        assert!(registry.networks.contains_key("1"));
        assert!(!registry.networks.contains_key("2"));
    }
}
