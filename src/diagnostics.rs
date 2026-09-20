use tracing::{Event, Metadata, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::layer::{Context, Filter};

pub struct IrcWireFilter;

fn permitted(metadata: &Metadata<'_>) -> bool {
    *metadata.level() != tracing::Level::TRACE
        || !["irc::client", "irc_repartee::client"].iter().any(|target| {
            metadata.target() == *target
                || metadata.target().strip_prefix(target).is_some_and(|suffix| suffix.starts_with("::"))
        })
}

impl<S: Subscriber> Filter<S> for IrcWireFilter {
    fn enabled(&self, metadata: &Metadata<'_>, _: &Context<'_, S>) -> bool {
        permitted(metadata)
    }

    fn event_enabled(&self, event: &Event<'_>, _: &Context<'_, S>) -> bool {
        event
            .normalized_metadata()
            .as_ref()
            .map_or_else(|| permitted(event.metadata()), permitted)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::prelude::*;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn raw_irc_trace_is_suppressed_even_with_explicit_trace_directives() {
        let _ = tracing_log::LogTracer::init();
        log::set_max_level(log::LevelFilter::Trace);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = Capture(bytes.clone());
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new(
                "trace,irc::client=trace,irc::client::transport=trace,irc_repartee::client=trace,irc_repartee::client::transport=trace",
            ))
            .with(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_ansi(false)
                    .with_writer(move || writer.clone())
                    .with_filter(super::IrcWireFilter),
            );
        tracing::subscriber::with_default(subscriber, || {
            log::trace!(target: "irc::client", "[SENT] BOUNCER CHANGENETWORK 1 pass=fixture-private-password");
            log::trace!(target: "irc::client::transport", "[SEND] BOUNCER CHANGENETWORK 1 pass=fixture-private-password");
            tracing::trace!(target: "irc::client", "fixture-private-password");
            log::trace!(target: "irc_repartee::client", "[SENT] WEBPUSH REGISTER synthetic-endpoint auth=synthetic-push-secret");
            log::trace!(target: "irc_repartee::client::transport", "[SEND] WEBPUSH REGISTER synthetic-endpoint auth=synthetic-push-secret");
            tracing::trace!(target: "irc_repartee::client", "synthetic-push-secret");
            log::debug!(target: "irc::client", "flood estimate updated");
            tracing::warn!("connection failed");
        });
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(!output.contains("fixture-private-password"));
        assert!(!output.contains("BOUNCER"));
        assert!(!output.contains("synthetic-push-secret"));
        assert!(!output.contains("synthetic-endpoint"));
        assert!(!output.contains("WEBPUSH"));
        assert!(output.contains("flood estimate updated"));
        assert!(output.contains("connection failed"));
    }
}
