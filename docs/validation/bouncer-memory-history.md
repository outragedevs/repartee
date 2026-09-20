# Soju memory-store daemon acceptance

This scenario runs the pinned, unmodified Soju revision
`82e8b7adfb2ab64ec3b88807d29b8b6940236008` with `message-store memory`, a
real TCP upstream fixture, the production daemon in a disposable container, and
the compiled web UI in Chromium. It complements the database-backed history
scenario in [bouncer-daemon-history.md](bouncer-daemon-history.md).

Build the daemon image from a clean archive of the revision under test using
`scripts/fixtures/daemon-history.Dockerfile`, as described in that document.
With Docker/OrbStack and Playwright configured, run:

```sh
python3 scripts/test_bouncer_presence.py soju /path/to/pinned/soju \
  --memory-history --daemon-image your-daemon-image
```

The standalone scenario verifies:

- A direct `CAP LS 302` probe reaches Soju and does not advertise
  `draft/chathistory`.
- The browser displays the explicit no-CHATHISTORY limitation in the server
  buffer.
- A browser command reaches the upstream through Soju and triggers an incoming
  private message. An outgoing private message reaches the upstream, which
  records it and returns its echo through Soju.
- Reloading the browser retrieves both messages from daemon memory. The response
  has `has_more = false`; it does not promise unavailable server history.
- Between daemon processes, while the first daemon is stopped, the upstream
  emits another private message and a PING barrier. Only after Soju acknowledges
  the barrier does the runner start the second daemon. That browser must display
  the offline message before sending any new traffic, and retain it after reload.
- Two separate daemon processes use the same isolated runtime directory, with
  real logging enabled and `RUST_LOG=trace`. After each clean shutdown, SQLite
  contains only the expected local startup events. Neither incoming nor outgoing
  message bodies occur in the diagnostic file.

Only synthetic accounts and messages are used. The provider remains running
across both daemon processes. The existing container runner inspects copied
SQLite files and removes containers and temporary data afterwards.

This proves live incoming/outgoing traffic, offline replay and memory-only browsing
across two process lifecycles. It does not claim provider restart retention,
terminal rendering, or full TARGETS enumeration. Those remain separate acceptance
cases. The fixture does not combine `--memory-history` with other feature modes.
