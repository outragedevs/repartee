use std::collections::HashMap;

/// An event that flows through the event bus.
#[derive(Debug, Clone)]
pub struct Event {
    pub name: String,
    pub params: HashMap<String, String>,
}

/// Result of handling an event — scripts can suppress default behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventResult {
    /// Continue processing (default behavior runs).
    Continue,
    /// Suppress default behavior (like irssi `signal_stop` / kokoirc `stop()`).
    Suppress,
}

/// Priority levels for event handlers (higher = runs first).
/// Matches kokoirc's `EventPriority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Lowest = 0,
    Low = 25,
    Normal = 50,
    High = 75,
    Highest = 100,
}

// EventBus, Registration, and EventHandler are only used in tests.
// The Lua engine manages its own handler storage directly for performance.
// These types exist as a tested reference implementation of the event dispatch
// algorithm (priority ordering, once-handlers, suppress semantics).

#[cfg(test)]
mod tests {
    use super::*;

    struct Registration {
        event: String,
        handler: Box<dyn EventHandler>,
        priority: i32,
        once: bool,
        owner: String,
        id: u64,
    }

    trait EventHandler: Send + Sync {
        fn handle(&self, event: &Event) -> EventResult;
    }

    struct EventBus {
        handlers: Vec<Registration>,
        next_id: u64,
    }

    impl EventBus {
        const fn new() -> Self {
            Self {
                handlers: Vec::new(),
                next_id: 0,
            }
        }

        fn on(
            &mut self,
            event_name: &str,
            handler: Box<dyn EventHandler>,
            priority: i32,
            owner: &str,
        ) -> u64 {
            let id = self.next_id;
            self.next_id += 1;
            let reg = Registration {
                event: event_name.to_string(),
                handler,
                priority,
                once: false,
                owner: owner.to_string(),
                id,
            };
            self.insert(reg);
            id
        }

        fn once(
            &mut self,
            event_name: &str,
            handler: Box<dyn EventHandler>,
            priority: i32,
            owner: &str,
        ) -> u64 {
            let id = self.next_id;
            self.next_id += 1;
            let reg = Registration {
                event: event_name.to_string(),
                handler,
                priority,
                once: true,
                owner: owner.to_string(),
                id,
            };
            self.insert(reg);
            id
        }

        fn emit(&mut self, event: &Event) -> bool {
            let matching: Vec<(usize, u64, bool)> = self
                .handlers
                .iter()
                .enumerate()
                .filter(|(_, r)| r.event == event.name)
                .map(|(i, r)| (i, r.id, r.once))
                .collect();

            let mut suppressed = false;
            let mut remove_ids = Vec::new();

            for &(idx, id, once) in &matching {
                let result = self.handlers[idx].handler.handle(event);

                if once {
                    remove_ids.push(id);
                }

                if result == EventResult::Suppress {
                    suppressed = true;
                    break;
                }
            }

            self.handlers.retain(|r| !remove_ids.contains(&r.id));
            suppressed
        }

        fn remove(&mut self, id: u64) {
            self.handlers.retain(|r| r.id != id);
        }

        fn off(&mut self, event_name: &str) {
            self.handlers.retain(|r| r.event != event_name);
        }

        fn remove_all(&mut self, owner: &str) {
            self.handlers.retain(|r| r.owner != owner);
        }

        fn clear(&mut self) {
            self.handlers.clear();
        }

        const fn len(&self) -> usize {
            self.handlers.len()
        }

        fn insert(&mut self, reg: Registration) {
            let pos = self
                .handlers
                .iter()
                .position(|r| r.priority < reg.priority)
                .unwrap_or(self.handlers.len());
            self.handlers.insert(pos, reg);
        }
    }

    struct TestHandler {
        result: EventResult,
    }

    impl EventHandler for TestHandler {
        fn handle(&self, _event: &Event) -> EventResult {
            self.result
        }
    }

    fn make_event(name: &str) -> Event {
        Event {
            name: name.to_string(),
            params: HashMap::new(),
        }
    }

    #[test]
    fn emit_no_handlers_returns_false() {
        let mut bus = EventBus::new();
        assert!(!bus.emit(&make_event("test")));
    }

    #[test]
    fn emit_continue_handler_returns_false() {
        let mut bus = EventBus::new();
        bus.on(
            "test",
            Box::new(TestHandler {
                result: EventResult::Continue,
            }),
            Priority::Normal as i32,
            "test_script",
        );
        assert!(!bus.emit(&make_event("test")));
    }

    #[test]
    fn emit_suppress_handler_returns_true() {
        let mut bus = EventBus::new();
        bus.on(
            "test",
            Box::new(TestHandler {
                result: EventResult::Suppress,
            }),
            Priority::Normal as i32,
            "test_script",
        );
        assert!(bus.emit(&make_event("test")));
    }

    #[test]
    fn off_removes_handlers() {
        let mut bus = EventBus::new();
        bus.on(
            "test",
            Box::new(TestHandler {
                result: EventResult::Suppress,
            }),
            Priority::Normal as i32,
            "test_script",
        );
        bus.off("test");
        assert!(!bus.emit(&make_event("test")));
    }

    #[test]
    fn clear_removes_all_handlers() {
        let mut bus = EventBus::new();
        bus.on(
            "a",
            Box::new(TestHandler {
                result: EventResult::Suppress,
            }),
            Priority::Normal as i32,
            "test_script",
        );
        bus.on(
            "b",
            Box::new(TestHandler {
                result: EventResult::Suppress,
            }),
            Priority::Normal as i32,
            "test_script",
        );
        bus.clear();
        assert!(!bus.emit(&make_event("a")));
        assert!(!bus.emit(&make_event("b")));
    }

    #[test]
    fn multiple_handlers_first_suppress_wins() {
        let mut bus = EventBus::new();
        bus.on(
            "test",
            Box::new(TestHandler {
                result: EventResult::Continue,
            }),
            Priority::Normal as i32,
            "s1",
        );
        bus.on(
            "test",
            Box::new(TestHandler {
                result: EventResult::Suppress,
            }),
            Priority::Normal as i32,
            "s2",
        );
        bus.on(
            "test",
            Box::new(TestHandler {
                result: EventResult::Continue,
            }),
            Priority::Normal as i32,
            "s3",
        );
        assert!(bus.emit(&make_event("test")));
    }

    #[test]
    fn wrong_event_name_not_triggered() {
        let mut bus = EventBus::new();
        bus.on(
            "specific",
            Box::new(TestHandler {
                result: EventResult::Suppress,
            }),
            Priority::Normal as i32,
            "test_script",
        );
        assert!(!bus.emit(&make_event("other")));
    }

    #[test]
    fn higher_priority_runs_first() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct OrderHandler {
            expected_order: u32,
            counter: Arc<AtomicU32>,
        }

        impl EventHandler for OrderHandler {
            fn handle(&self, _event: &Event) -> EventResult {
                let got = self.counter.fetch_add(1, Ordering::SeqCst);
                assert_eq!(got, self.expected_order, "handler ran out of order");
                EventResult::Continue
            }
        }

        let counter = Arc::new(AtomicU32::new(0));
        let mut bus = EventBus::new();
        // Register low priority first, high priority second
        bus.on(
            "test",
            Box::new(OrderHandler {
                expected_order: 1,
                counter: Arc::clone(&counter),
            }),
            Priority::Low as i32,
            "s1",
        );
        bus.on(
            "test",
            Box::new(OrderHandler {
                expected_order: 0,
                counter: Arc::clone(&counter),
            }),
            Priority::High as i32,
            "s2",
        );

        bus.emit(&make_event("test"));
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn remove_all_by_owner() {
        let mut bus = EventBus::new();
        bus.on(
            "test",
            Box::new(TestHandler {
                result: EventResult::Suppress,
            }),
            Priority::Normal as i32,
            "script_a",
        );
        bus.on(
            "test",
            Box::new(TestHandler {
                result: EventResult::Continue,
            }),
            Priority::Normal as i32,
            "script_b",
        );

        bus.remove_all("script_a");
        // Only script_b's Continue handler remains
        assert!(!bus.emit(&make_event("test")));
        assert_eq!(bus.len(), 1);
    }

    #[test]
    fn once_handler_fires_once() {
        let mut bus = EventBus::new();
        bus.once(
            "test",
            Box::new(TestHandler {
                result: EventResult::Continue,
            }),
            Priority::Normal as i32,
            "s1",
        );

        assert_eq!(bus.len(), 1);
        bus.emit(&make_event("test"));
        assert_eq!(bus.len(), 0);
    }

    #[test]
    fn remove_by_id() {
        let mut bus = EventBus::new();
        let id = bus.on(
            "test",
            Box::new(TestHandler {
                result: EventResult::Suppress,
            }),
            Priority::Normal as i32,
            "s1",
        );

        assert!(bus.emit(&make_event("test")));
        bus.remove(id);
        assert!(!bus.emit(&make_event("test")));
    }
}
