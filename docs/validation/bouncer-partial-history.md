# Partial bounded-history disconnect acceptance

## Scope

This fixture cuts an actual pinned-provider BETWEEN response after its opener and first PRIVMSG, before the batch closer. It exercises the real Repartee connection and App state, then reconnects and retries against the same provider. It is not a full daemon/browser fault test and does not cover a stalled connection, timeout, SEARCH batches or ordinary scrolling batches.

```sh
python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju --partial-history
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --partial-history
```

The pins, account and database creation are shared with the binding fixture. This standalone mode requires no Playwright installation. The ordinary broad fixture filter skips it because it needs the partial-batch proxy.

## Evidence required by the fixture

1. Initial server-owned history hydration settles before the proxy is armed.
2. The loopback proxy observes an actual BETWEEN request for the seeded peer. It forwards the corresponding actual provider batch opener and first message, then shuts down the transport and blocks new connections. It does not manufacture history or a batch terminator.
3. Repartee processes that message while the batch remains open and the search request is pending. No partial result appears in the search view.
4. Disconnection clears pending search and batch tracking. The ordinary conversation retains exactly its prior message IDs.
5. After proxy resume and an explicit connection attempt, a fresh request returns exactly messages 11, 12 and 13, with no duplicate partial row.
6. After the SQLite writer shuts down, no seeded history message exists on disk. The legacy row remains and a direct-IRC positive control is persisted. Connection diagnostics are allowed; the exclusion assertion targets the synthetic conversation content.

## Remaining fault acceptance

G4/G8 still need stalled/late batch and timeout behavior, cancellation during a real in-flight response, concurrent connection/target isolation, and the corresponding SEARCH and ordinary-history paths. The existing daemon TCP-cut fixture cuts after completed operations, so it is not substituted for those checks.
