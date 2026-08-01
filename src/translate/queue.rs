//! Per-buffer reorder queue.
//!
//! Translation requests are dispatched the instant a line arrives and run
//! concurrently; this queue governs only WHEN a resolved line is allowed
//! onto the screen.
//!
//! That distinction is the whole point. A serial pipeline — line N waiting
//! for line N-1 to come back before being sent for translation — would make
//! latency cumulative: ten queued lines at ~900 ms each means nine seconds
//! for the last one, and on a live channel arrivals outpace the pipeline so
//! the backlog grows without bound. With concurrent dispatch and ordered
//! release, a line's delay is bounded by the slowest of its predecessors,
//! not by their sum.
//!
//! Ordering is by `Message::id`, which is monotonic. Timestamps are not
//! usable here: two lines within the same second have no defined order,
//! `@time` may be absent entirely, and a server clock is not guaranteed
//! monotonic. This repository already reached that conclusion when it added
//! the `ts_ms` column and made `id` the keyset tiebreaker — see
//! `CREATE_MESSAGES_SUBSECOND_IDX` in `src/storage/db.rs`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::UntranslatedReason;
use crate::state::buffer::{ActivityLevel, Message, WireOrigin};

/// A line awaiting its translation.
#[derive(Debug, Clone)]
pub struct PendingPayload {
    /// The message as it would have been delivered untranslated. On
    /// resolution its `text` is replaced by the composed display string and
    /// its `wire_origin` records what the wire actually carried.
    pub message: Message,
    pub activity: ActivityLevel,
    /// Whether the original is appended in brackets. Captured per line at
    /// dispatch, so flipping `/set translate.show_original_in` mid-flight
    /// cannot make a line render differently from how it was queued.
    pub show_original: bool,
}

#[derive(Debug)]
enum Slot {
    /// A place held for a row that does not exist yet — an outgoing echo
    /// whose translation is still running.
    ///
    /// An id alone does not preserve order: a later incoming line can create
    /// a queue, resolve, drain, and have the queue pruned before the echo
    /// arrives, leaving nothing to insert into. The barrier has to physically
    /// exist for the whole wait.
    Reserved { queued_at: Instant },
    Pending {
        original: String,
        payload: PendingPayload,
        queued_at: Instant,
    },
    Ready {
        message: Message,
        activity: ActivityLevel,
        reason: Option<UntranslatedReason>,
    },
}

#[derive(Debug)]
struct Entry {
    id: u64,
    /// `None` only momentarily, while [`TranslateQueue::resolve`] converts a
    /// pending slot into a ready one.
    slot: Option<Slot>,
}

/// A line cleared for delivery.
#[derive(Debug, Clone)]
pub struct ReadyEntry {
    pub id: u64,
    pub message: Message,
    pub activity: ActivityLevel,
    /// `None` when the line was translated; otherwise why it was not.
    pub reason: Option<UntranslatedReason>,
}

#[derive(Debug, Default)]
pub struct TranslateQueue {
    entries: VecDeque<Entry>,
}

impl TranslateQueue {
    #[cfg(test)]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    /// Reserve a place for a line that has been sent for translation.
    pub fn push_pending(&mut self, id: u64, original: String, payload: PendingPayload) {
        self.push_pending_at(id, original, payload, Instant::now());
    }

    /// Injectable-clock variant, so timeout behaviour is testable without
    /// sleeping.
    pub fn push_pending_at(
        &mut self,
        id: u64,
        original: String,
        payload: PendingPayload,
        now: Instant,
    ) {
        self.entries.push_back(Entry {
            id,
            slot: Some(Slot::Pending {
                original,
                payload,
                queued_at: now,
            }),
        });
    }

    /// Take a place for a row that needs no translation — a JOIN, a notice,
    /// our own echo.
    ///
    /// These still enter the queue whenever it is non-empty. Delivering them
    /// directly would let a JOIN render before the lines queued ahead of it,
    /// silently reordering the buffer's timeline: the same failure the queue
    /// exists to prevent, arriving from a different direction.
    pub fn push_resolved(&mut self, id: u64, message: Message, activity: ActivityLevel) {
        self.entries.push_back(Entry {
            id,
            slot: Some(Slot::Ready {
                message,
                activity,
                reason: None,
            }),
        });
    }

    /// Place a ready row at the position its id earns, rather than at the
    /// back.
    ///
    /// Used for a deferred outgoing echo: its id was allocated when the user
    /// pressed Enter, but it only becomes a row once translation returns —
    /// by which time lines that arrived DURING the wait are already queued.
    /// Appending would render the user's own message after replies to it.
    pub fn insert_resolved_in_order(
        &mut self,
        id: u64,
        message: Message,
        activity: ActivityLevel,
    ) {
        let pos = self
            .entries
            .iter()
            .position(|e| e.id > id)
            .unwrap_or(self.entries.len());
        self.entries.insert(
            pos,
            Entry {
                id,
                slot: Some(Slot::Ready {
                    message,
                    activity,
                    reason: None,
                }),
            },
        );
    }

    /// Hold a place for a row that will be built later.
    pub fn reserve(&mut self, id: u64) {
        self.reserve_at(id, Instant::now());
    }

    /// Injectable-clock variant.
    pub fn reserve_at(&mut self, id: u64, now: Instant) {
        self.entries.push_back(Entry {
            id,
            slot: Some(Slot::Reserved { queued_at: now }),
        });
    }

    /// Fill a reserved place with the row it was held for.
    ///
    /// Returns `false` when the id is unknown — the reservation was already
    /// released by a timeout, a flush, or a buffer close.
    pub fn fill_reserved(&mut self, id: u64, message: Message, activity: ActivityLevel) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        if !matches!(entry.slot, Some(Slot::Reserved { .. })) {
            return false;
        }
        entry.slot = Some(Slot::Ready {
            message,
            activity,
            reason: None,
        });
        true
    }

    /// Drop a reservation whose row will never arrive — a refused send.
    /// Leaving it would block everything queued behind it until the timeout.
    pub fn release_reserved(&mut self, id: u64) {
        self.entries
            .retain(|e| !(e.id == id && matches!(e.slot, Some(Slot::Reserved { .. }))));
    }

    /// Fold an outcome into the matching entry.
    ///
    /// Returns `false` when the id is unknown or already resolved — a late
    /// outcome for a line the queue has already released. That is expected
    /// (a provider answering after the timeout fired) and must not panic.
    pub fn resolve(&mut self, id: u64, outcome: Result<String, UntranslatedReason>) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        if !matches!(entry.slot, Some(Slot::Pending { .. })) {
            return false;
        }
        let Some(Slot::Pending {
            original, payload, ..
        }) = entry.slot.take()
        else {
            unreachable!("guarded by the matches! above")
        };
        let PendingPayload {
            mut message,
            activity,
            show_original,
        } = payload;
        let reason = match outcome {
            Ok(translated) => {
                let (text, offset) = super::compose_display(&translated, &original, show_original);
                message.text = text;
                message.wire_origin = Some(WireOrigin {
                    text: original,
                    suffix_at: offset,
                });
                None
            }
            Err(reason) => {
                // The original is what the user sees when translation did
                // not happen — never a guess, never a partial translation —
                // but a GAP is also marked, so it cannot be mistaken for a
                // line the broker correctly decided to leave alone.
                let (text, offset) = super::mark_untranslated(&original, &reason);
                message.text = text;
                message.wire_origin = Some(WireOrigin {
                    text: original,
                    suffix_at: offset,
                });
                Some(reason)
            }
        };
        entry.slot = Some(Slot::Ready {
            message,
            activity,
            reason,
        });
        true
    }

    /// Pop from the head for as long as the head is ready.
    pub fn drain_ready(&mut self) -> Vec<ReadyEntry> {
        let mut out = Vec::new();
        while matches!(
            self.entries.front().and_then(|e| e.slot.as_ref()),
            Some(Slot::Ready { .. })
        ) {
            let entry = self.entries.pop_front().expect("front matched above");
            let Some(Slot::Ready {
                message,
                activity,
                reason,
            }) = entry.slot
            else {
                unreachable!("front matched above")
            };
            out.push(ReadyEntry {
                id: entry.id,
                message,
                activity,
                reason,
            });
        }
        out
    }

    /// Force every entry that has been pending for at least `timeout` to
    /// resolve as [`UntranslatedReason::Timeout`]. Returns how many were
    /// forced.
    ///
    /// Without this a single stuck line holds its whole channel: everything
    /// behind it is ready but unreleasable.
    pub fn expire(&mut self, now: Instant, timeout: Duration) -> usize {
        let expired: Vec<u64> = self
            .entries
            .iter()
            .filter_map(|e| match e.slot.as_ref() {
                Some(Slot::Pending { queued_at, .. } | Slot::Reserved { queued_at })
                    if now.duration_since(*queued_at) >= timeout =>
                {
                    Some(e.id)
                }
                _ => None,
            })
            .collect();
        for id in &expired {
            // A reservation has no row to resolve INTO, so it is dropped.
            // Its message either arrives later and is delivered directly, or
            // was refused; either way it must stop blocking the queue.
            self.release_reserved(*id);
            self.resolve(*id, Err(UntranslatedReason::Timeout));
        }
        expired.len()
    }

    /// Force the oldest pending entries out until the queue fits `max`.
    /// Returns how many were forced.
    ///
    /// A dead provider must not turn this queue into an unbounded memory
    /// leak with a frozen channel behind it — past the ceiling the channel
    /// keeps flowing untranslated instead of stalling.
    pub fn enforce_ceiling(&mut self, max: usize) -> usize {
        if self.entries.len() <= max {
            return 0;
        }
        let excess = self.entries.len() - max;
        let ids: Vec<u64> = self
            .entries
            .iter()
            .filter(|e| matches!(e.slot, Some(Slot::Pending { .. } | Slot::Reserved { .. })))
            .take(excess)
            .map(|e| e.id)
            .collect();
        for id in &ids {
            self.release_reserved(*id);
            self.resolve(*id, Err(UntranslatedReason::Timeout));
        }
        ids.len()
    }

    /// Release everything, in order, untranslated where still pending.
    ///
    /// Used on buffer close, `/part`, disconnect, quit, detach, and
    /// `/translate delin|delout`. Pending lines must be released rather than
    /// dropped — the user already saw them arrive on the network.
    pub fn flush_all(&mut self) -> Vec<ReadyEntry> {
        let pending: Vec<u64> = self
            .entries
            .iter()
            .filter(|e| matches!(e.slot, Some(Slot::Pending { .. } | Slot::Reserved { .. })))
            .map(|e| e.id)
            .collect();
        for id in pending {
            self.release_reserved(id);
            self.resolve(id, Err(UntranslatedReason::Timeout));
        }
        self.drain_ready()
    }

    /// Release every line still waiting on an INCOMING translation, leaving
    /// outgoing reservations — and the order they enforce — untouched.
    ///
    /// For `/translate delin`, which stops incoming translation on a buffer
    /// whose outgoing sends may still be in flight. Those sends were already
    /// dispatched and their outcomes are still coming, so their reservations
    /// must survive: dropping one lets the rows queued behind it render
    /// first, and the user's own message then appears below the replies to
    /// it.
    pub fn flush_pending(&mut self) -> Vec<ReadyEntry> {
        let pending: Vec<u64> = self
            .entries
            .iter()
            .filter(|e| matches!(e.slot, Some(Slot::Pending { .. })))
            .map(|e| e.id)
            .collect();
        for id in pending {
            self.resolve(id, Err(UntranslatedReason::Timeout));
        }
        self.drain_ready()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total entries, pending or ready. Drives `/translate status`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// How many entries are still awaiting an outcome. Drives
    /// `/translate status`.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.slot, Some(Slot::Pending { .. } | Slot::Reserved { .. })))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::MessageType;
    use chrono::Utc;

    /// A resolved line must always record what the wire carried — that is
    /// what keys it against its own CHATHISTORY replay.
    fn orig(message: &Message) -> &WireOrigin {
        message
            .wire_origin
            .as_ref()
            .expect("a resolved line records its wire text")
    }

    fn message(id: u64, text: &str) -> Message {
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Message,
            nick: Some("alice".to_string()),
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
        }
    }

    fn payload(id: u64, text: &str) -> PendingPayload {
        PendingPayload {
            message: message(id, text),
            activity: ActivityLevel::Activity,
            show_original: false,
        }
    }

    fn ids(ready: &[ReadyEntry]) -> Vec<u64> {
        ready.iter().map(|e| e.id).collect()
    }

    #[test]
    fn resolved_head_drains_immediately() {
        let mut q = TranslateQueue::new();
        q.push_pending(1, "hola".into(), payload(1, "hola"));
        assert!(q.drain_ready().is_empty(), "a pending head blocks");
        assert!(q.resolve(1, Ok("czesc".into())));
        let ready = q.drain_ready();
        assert_eq!(ids(&ready), vec![1]);
        assert_eq!(ready[0].message.text, "czesc");
        assert!(q.is_empty());
    }

    #[test]
    fn later_line_waits_for_the_earlier_one() {
        // THE case this queue exists for: line 5 comes back before line 4.
        let mut q = TranslateQueue::new();
        q.push_pending(4, "vier".into(), payload(4, "vier"));
        q.push_pending(5, "fuenf".into(), payload(5, "fuenf"));
        q.resolve(5, Ok("piec".into()));
        assert!(q.drain_ready().is_empty(), "5 must not overtake 4");
        q.resolve(4, Ok("cztery".into()));
        let ready = q.drain_ready();
        assert_eq!(ids(&ready), vec![4, 5], "both release, in arrival order");
        assert_eq!(ready[0].message.text, "cztery");
        assert_eq!(ready[1].message.text, "piec");
    }

    #[test]
    fn non_translated_row_does_not_overtake_pending_lines() {
        // A JOIN arriving while line 4 translates must not render first.
        let mut q = TranslateQueue::new();
        q.push_pending(4, "vier".into(), payload(4, "vier"));
        q.push_resolved(5, message(5, "bob has joined"), ActivityLevel::None);
        assert!(q.drain_ready().is_empty(), "the JOIN waits its turn");
        q.resolve(4, Ok("cztery".into()));
        assert_eq!(ids(&q.drain_ready()), vec![4, 5]);
    }

    #[test]
    fn timeout_releases_a_stuck_head_and_drains_its_followers() {
        let mut q = TranslateQueue::new();
        let t0 = Instant::now();
        q.push_pending_at(4, "vier".into(), payload(4, "vier"), t0);
        q.push_pending_at(5, "fuenf".into(), payload(5, "fuenf"), t0);
        q.resolve(5, Ok("piec".into()));
        let expired = q.expire(t0 + Duration::from_millis(5001), Duration::from_millis(5000));
        assert_eq!(expired, 1, "only the still-pending entry expires");
        let ready = q.drain_ready();
        assert_eq!(ids(&ready), vec![4, 5]);
        assert_eq!(ready[0].reason, Some(UntranslatedReason::Timeout));
        assert_eq!(
            ready[0].message.text, "vier [untranslated: timeout]",
            "a timed-out line shows its original, marked so the gap is visible"
        );
        assert_eq!(ready[1].reason, None);
    }

    #[test]
    fn expire_leaves_entries_that_are_still_inside_the_budget() {
        let mut q = TranslateQueue::new();
        let t0 = Instant::now();
        q.push_pending_at(1, "a b".into(), payload(1, "a b"), t0);
        let expired = q.expire(t0 + Duration::from_millis(4999), Duration::from_millis(5000));
        assert_eq!(expired, 0);
        assert_eq!(q.pending_len(), 1);
    }

    #[test]
    fn ceiling_releases_oldest_first_and_bounds_growth() {
        let mut q = TranslateQueue::new();
        for id in 1..=10 {
            q.push_pending(id, format!("l{id}"), payload(id, "x"));
        }
        let forced = q.enforce_ceiling(4);
        assert_eq!(forced, 6, "six oldest forced out");
        let ready = q.drain_ready();
        assert_eq!(ids(&ready), vec![1, 2, 3, 4, 5, 6]);
        assert!(
            ready
                .iter()
                .all(|e| e.reason == Some(UntranslatedReason::Timeout))
        );
        assert_eq!(q.len(), 4, "the queue is bounded afterwards");
    }

    #[test]
    fn ceiling_is_a_no_op_below_the_limit() {
        let mut q = TranslateQueue::new();
        q.push_pending(1, "a".into(), payload(1, "a"));
        assert_eq!(q.enforce_ceiling(4), 0);
        assert_eq!(q.pending_len(), 1);
    }

    #[test]
    fn resolving_an_unknown_id_is_a_no_op() {
        let mut q = TranslateQueue::new();
        q.push_pending(1, "a".into(), payload(1, "a"));
        assert!(!q.resolve(999, Ok("x".into())), "a late outcome is dropped");
        assert_eq!(q.pending_len(), 1);
    }

    #[test]
    fn resolving_twice_is_a_no_op() {
        // A provider answering after the timeout already fired.
        let mut q = TranslateQueue::new();
        q.push_pending(1, "a b".into(), payload(1, "a b"));
        assert!(q.resolve(1, Err(UntranslatedReason::Timeout)));
        assert!(!q.resolve(1, Ok("late".into())), "the second is ignored");
        let ready = q.drain_ready();
        assert_eq!(ready[0].message.text, "a b [untranslated: timeout]");
        assert_eq!(ready[0].reason, Some(UntranslatedReason::Timeout));
    }

    #[test]
    fn a_reservation_survives_a_queue_that_would_otherwise_be_pruned() {
        // The exact scenario: outgoing 10 is still translating; incoming 11
        // arrives, resolves and drains. Without a physical barrier the queue
        // would be empty (and pruned), and 10 would later be appended AFTER
        // 11 — the user's message below the reply to it.
        let mut q = TranslateQueue::new();
        q.reserve(10);
        q.push_pending(11, "later".into(), payload(11, "later"));
        q.resolve(11, Ok("translated later".into()));

        assert!(
            q.drain_ready().is_empty(),
            "11 must not drain past the place held for 10"
        );
        assert!(!q.is_empty(), "the queue survives, so nothing prunes it");

        assert!(q.fill_reserved(10, message(10, "my own line"), ActivityLevel::None));
        assert_eq!(ids(&q.drain_ready()), vec![10, 11]);
    }

    #[test]
    fn releasing_a_reservation_unblocks_what_is_behind_it() {
        // A refused send writes no echo, so the barrier must be given back
        // or everything behind it waits out the timeout.
        let mut q = TranslateQueue::new();
        q.reserve(10);
        q.push_pending(11, "later".into(), payload(11, "later"));
        q.resolve(11, Ok("translated later".into()));
        q.release_reserved(10);
        assert_eq!(ids(&q.drain_ready()), vec![11]);
    }

    #[test]
    fn a_stale_reservation_expires_rather_than_wedging_the_buffer() {
        let mut q = TranslateQueue::new();
        let t0 = Instant::now();
        q.reserve_at(10, t0);
        q.push_pending_at(11, "later".into(), payload(11, "later"), t0);
        q.resolve(11, Ok("translated later".into()));
        q.expire(t0 + Duration::from_millis(5001), Duration::from_millis(5000));
        assert_eq!(ids(&q.drain_ready()), vec![11]);
        assert!(q.is_empty());
    }

    #[test]
    fn filling_an_unknown_reservation_reports_it_rather_than_panicking() {
        let mut q = TranslateQueue::new();
        assert!(!q.fill_reserved(10, message(10, "x"), ActivityLevel::None));
    }

    #[test]
    fn a_reserved_id_lands_ahead_of_lines_that_arrived_later() {
        // The outgoing echo's id was allocated at submission; lines 5 and 6
        // arrived while it was translating. It belongs before them.
        let mut q = TranslateQueue::new();
        q.push_pending(5, "later one".into(), payload(5, "later one"));
        q.push_pending(6, "later two".into(), payload(6, "later two"));
        q.insert_resolved_in_order(4, message(4, "my own line"), ActivityLevel::None);

        q.resolve(5, Ok("t5".into()));
        q.resolve(6, Ok("t6".into()));
        assert_eq!(
            ids(&q.drain_ready()),
            vec![4, 5, 6],
            "the echo renders before the replies to it"
        );
    }

    #[test]
    fn a_reserved_id_after_everything_lands_at_the_back() {
        let mut q = TranslateQueue::new();
        q.push_pending(1, "a".into(), payload(1, "a"));
        q.insert_resolved_in_order(9, message(9, "mine"), ActivityLevel::None);
        q.resolve(1, Ok("t".into()));
        assert_eq!(ids(&q.drain_ready()), vec![1, 9]);
    }

    #[test]
    fn flush_all_releases_everything_in_order() {
        let mut q = TranslateQueue::new();
        q.push_pending(1, "a".into(), payload(1, "a"));
        q.push_resolved(2, message(2, "bob has joined"), ActivityLevel::None);
        q.push_pending(3, "c".into(), payload(3, "c"));
        let ready = q.flush_all();
        assert_eq!(ids(&ready), vec![1, 2, 3]);
        assert!(q.is_empty());
    }

    #[test]
    fn resolve_composes_the_original_when_show_original_is_set() {
        let mut q = TranslateQueue::new();
        let mut p = payload(1, "blabla");
        p.show_original = true;
        q.push_pending(1, "blabla".into(), p);
        q.resolve(1, Ok("albalb".into()));
        let ready = q.drain_ready();
        assert_eq!(ready[0].message.text, "albalb [blabla]");
        assert_eq!(orig(&ready[0].message).suffix_at, Some(6));
    }

    #[test]
    fn a_gap_is_marked_and_the_marker_is_dimmable() {
        let mut q = TranslateQueue::new();
        q.push_pending(1, "hola que tal".into(), payload(1, "hola que tal"));
        q.resolve(1, Err(UntranslatedReason::NoProvider));
        let ready = q.drain_ready();
        assert_eq!(ready[0].message.text, "hola que tal [untranslated: no provider]");
        assert_eq!(
            orig(&ready[0].message).suffix_at,
            Some("hola que tal".len()),
            "the marker carries an offset so the renderer dims it"
        );
    }

    #[test]
    fn a_filtered_line_shows_its_original_without_brackets() {
        let mut q = TranslateQueue::new();
        let mut p = payload(1, "moin");
        p.show_original = true;
        q.push_pending(1, "moin".into(), p);
        q.resolve(1, Err(UntranslatedReason::Filtered));
        let ready = q.drain_ready();
        assert_eq!(ready[0].message.text, "moin");
        assert_eq!(orig(&ready[0].message).suffix_at, None);
        assert_eq!(ready[0].reason, Some(UntranslatedReason::Filtered));
        assert!(
            !ready[0].reason.as_ref().unwrap().is_gap(),
            "Filtered renders clean, with no marker"
        );
    }
}
