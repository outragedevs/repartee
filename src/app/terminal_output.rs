use std::io;

use crate::image_preview::PreviewStatus;
use crate::ui;

use super::App;

impl App {
    pub(crate) fn terminal_graphics_enabled(&self) -> bool {
        self.socket_output
            .as_ref()
            .is_none_or(crate::session::writer::SocketOutput::graphics_enabled)
    }

    pub(crate) fn render_terminal_frame(&mut self) -> bool {
        if self.terminal.is_none() {
            return true;
        }
        let output = self.socket_output.clone();
        if output.as_ref().is_some_and(|output| !output.begin_frame()) {
            return false;
        }
        let mut terminal = self.terminal.take().unwrap();
        let size = terminal
            .size()
            .map_or((self.cached_term_cols, self.cached_term_rows), |size| {
                (size.width, size.height)
            });
        let key = crate::image_preview::inline::frame_key(self, size);
        let clear_inline = self
            .inline_previews
            .prepare_frame(key, self.needs_full_redraw);
        if clear_inline {
            self.inline_previews.clear_graphics(
                terminal.backend_mut(),
                self.picker.protocol_type(),
                self.in_tmux,
            );
            self.emote_animator.clear();
        }
        let result = (|| -> io::Result<()> {
            if self.needs_full_redraw || clear_inline {
                terminal.clear()?;
                self.needs_full_redraw = false;
            }
            terminal.draw(|frame| ui::layout::draw(frame, self))?;
            self.inline_previews
                .write_direct(terminal.backend_mut(), self.picker.protocol_type());
            if let Some(output) = &output {
                output.finish_frame()?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                let key = crate::image_preview::inline::frame_key(self, size);
                self.inline_previews.finish_frame(key);
                self.terminal = Some(terminal);
                self.write_tmux_direct_image();
                self.mark_terminal_read();
            }
            Err(error) => {
                if let Some(output) = &output {
                    output.cancel_frame();
                }
                if output.is_some() && error.kind() == io::ErrorKind::WouldBlock {
                    tracing::warn!("terminal frame exceeded output limit: {error}");
                    if let PreviewStatus::Ready { url, .. } = &self.image_preview {
                        self.image_preview = PreviewStatus::Error {
                            url: url.clone(),
                            message: "Image exceeds the terminal output limit".into(),
                        };
                    } else if let Some(output) = &output {
                        output.disable_graphics();
                        self.image_preview = PreviewStatus::Error {
                            url: String::new(),
                            message: "Terminal output limit reached. Inline graphics disabled until reattach.".into(),
                        };
                    }
                    self.inline_previews.invalidate_protocols();
                    self.emote_animator.clear();
                    self.needs_full_redraw = true;
                    terminal.current_buffer_mut().reset();
                    self.terminal = Some(terminal);
                } else {
                    tracing::warn!("terminal draw failed, triggering detach: {error}");
                    self.should_detach = true;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ratatui_image::picker::{Picker, ProtocolType};
    use tokio::sync::mpsc;

    use crate::session::protocol::MainMessage;
    use crate::session::writer::{MAX_SOCKET_OUTPUT_QUEUE_BYTES, SocketWriter};

    use super::*;

    #[expect(
        deprecated,
        reason = "fixed font metrics reproduce the reported image output size"
    )]
    fn image_app(limit: usize) -> (App, mpsc::UnboundedReceiver<MainMessage>, Arc<AtomicUsize>) {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let writer = SocketWriter::new(tx, Arc::clone(&queued), limit);
        app.socket_output = Some(writer.output());
        app.terminal = Some(ui::setup_socket_terminal(Box::new(writer), 120, 60).unwrap());
        while let Ok(MainMessage::Output(bytes)) = rx.try_recv() {
            queued.fetch_sub(bytes.len(), Ordering::AcqRel);
        }
        app.is_socket_attached = true;
        app.picker = Picker::from_fontsize((20, 40));
        app.picker.set_protocol_type(ProtocolType::Kitty);
        app.image_preview = PreviewStatus::Ready {
            url: "https://example.org/test.png".into(),
            title: None,
            image: Box::new(
                app.picker
                    .new_resize_protocol(image::DynamicImage::new_rgba8(2000, 2000)),
            ),
            raw_png: Vec::new(),
            width: 102,
            height: 52,
            direct_written: false,
        };
        (app, rx, queued)
    }

    #[tokio::test]
    async fn large_kitty_image_survives_a_slow_output_receiver() {
        let (mut app, mut rx, queued) = image_app(MAX_SOCKET_OUTPUT_QUEUE_BYTES);
        assert!(app.render_terminal_frame());
        assert!(!app.should_detach);
        assert!(app.terminal.is_some());
        assert!(queued.load(Ordering::Acquire) > 16 * 1024 * 1024);
        assert!(matches!(app.image_preview, PreviewStatus::Ready { .. }));
        assert!(!app.render_terminal_frame());
        let MainMessage::Output(bytes) = rx.try_recv().unwrap() else {
            panic!("missing frame")
        };
        assert!(!bytes.is_empty());
        assert!(rx.try_recv().is_err());
        queued.fetch_sub(bytes.len(), Ordering::AcqRel);
        assert!(app.render_terminal_frame());
        assert!(!app.should_detach);
    }

    #[tokio::test]
    async fn oversized_image_shows_an_error_without_detaching_or_partial_output() {
        let (mut app, mut rx, queued) = image_app(64 * 1024);
        assert!(app.render_terminal_frame());
        assert!(!app.should_detach);
        assert!(app.terminal.is_some());
        assert!(matches!(app.image_preview, PreviewStatus::Error { .. }));
        assert!(app.needs_full_redraw);
        assert_eq!(queued.load(Ordering::Acquire), 0);
        assert!(rx.try_recv().is_err());
        assert!(app.render_terminal_frame());
        assert!(!app.should_detach);
        assert!(queued.load(Ordering::Acquire) > 0);
        assert!(app.terminal.is_some());
    }
}
