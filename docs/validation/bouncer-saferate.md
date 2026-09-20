# SAFERATE transport integration

The pinned Soju source advertises the bare `soju.im/SAFERATE` ISUPPORT token
(`downstream.go`, `doc/ext/saferate.md`). The pinned Lurker source does not.
Repartee retains its configured flood protection unless a valid bare token is
present. A valued token, including an empty assignment, does not enable bypass.
Token removal restores the configured limit. Valid extended ISUPPORT batches
apply the change atomically at their end; malformed or incomplete batches do not.

The daemon's connection-specific sender controls both the actual IRC transport
and whether typing notifications require conservative flood headroom. The same
sender is used by native and web commands. Sender clones share the setting;
other connections and a configured zero threshold retain their own behavior.
The typing budget continues charging during bypass, so removing SAFERATE cannot
underestimate messages still queued for the transport. This is deliberately
conservative when already-sent traffic cannot be distinguished from queued work.

## Transport dependency

Repartee uses the published irc-repartee 1.5.2 registry dependency. Its
`Sender::set_flood_protection_enabled` API is connection-local and shared by
sender clones. Each writer poll uses one state snapshot; rapid updates may
coalesce. This is connection state, not a per-message bypass or a queue fence.
The implementation uses no Cargo patch, Git dependency or vendor copy.

## Verification

- Socket tests exercise an already-delayed message, unthrottled delivery,
  restoration, cloned senders, separate connections, configured zero, and rapid
  state changes.
- An application-to-TCP test verifies batch-end activation, removal, rejection
  of valued tokens and typing-budget behavior.
- Pinned Soju integration passed: registration advertised SAFERATE, and six PING
  replies arrived within two seconds while configured flood protection was on.
- Pinned Lurker integration passed: no SAFERATE token, and the same six-message
  probe retained configured throttling and eventually delivered every reply.

The registry-backed implementation passed `make clippy` and `make test`:
2700 native tests and 146 web tests, with 32 provider fixtures intentionally
ignored by the default suite. Both pinned provider SAFERATE fixtures passed
separately. Soju releases the burst promptly; Lurker retains configured throttling.

Cargo packaged and verified the normalized release source against registry
dependencies. Installing that package source with `cargo install --path ...
--locked --debug` into a disposable prefix succeeded, and the installed binary
reported its version. These commands were invoked through a temporary Makefile.
No application release was published by this verification.

Cargo reports pre-existing yanked lockfile entries for `chacha20` 0.10.1 and
`spin` 0.9.8; packaging and the locked installation still succeeded. Dependency
updates are outside this transport change.
