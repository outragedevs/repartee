use tokio::sync::broadcast;

use super::protocol::WebEvent;

/// Fan-out channel for broadcasting events to all connected web clients.
///
/// Uses `tokio::sync::broadcast` which supports multiple receivers.
/// If no clients are connected, events are silently dropped.
pub struct WebBroadcaster {
    tx: broadcast::Sender<WebEvent>,
}

impl WebBroadcaster {
    /// Create a new broadcaster with the given channel capacity.
    ///
    /// When the channel is full, oldest unread events are dropped for slow
    /// receivers (they'll get a `Lagged` error on next recv).
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Broadcast an event to all connected clients.
    ///
    /// Returns the number of receivers that will receive the event.
    /// Returns 0 if no clients are connected (event is silently dropped).
    #[must_use]
    pub fn send(&self, event: WebEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Number of active receivers (connected web clients).
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Subscribe to receive events. Each WebSocket session calls this on connect.
    pub fn subscribe(&self) -> broadcast::Receiver<WebEvent> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broadcast_to_multiple_receivers() {
        let bc = WebBroadcaster::new(16);
        let mut rx1 = bc.subscribe();
        let mut rx2 = bc.subscribe();

        let event = WebEvent::BufferClosed {
            buffer_id: "test/chan".into(),
        };
        let count = bc.send(event);
        assert_eq!(count, 2);

        let ev1 = rx1.recv().await.unwrap();
        let ev2 = rx2.recv().await.unwrap();
        assert!(matches!(ev1, WebEvent::BufferClosed { .. }));
        assert!(matches!(ev2, WebEvent::BufferClosed { .. }));
    }

    #[test]
    fn broadcast_no_receivers_does_not_panic() {
        let bc = WebBroadcaster::new(16);
        let count = bc.send(WebEvent::BufferClosed {
            buffer_id: "x".into(),
        });
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn dropped_receiver_does_not_block() {
        let bc = WebBroadcaster::new(16);
        let rx = bc.subscribe();
        drop(rx);

        // Should not panic — dropped receivers are cleaned up.
        let count = bc.send(WebEvent::BufferClosed {
            buffer_id: "y".into(),
        });
        assert_eq!(count, 0);
    }
}
