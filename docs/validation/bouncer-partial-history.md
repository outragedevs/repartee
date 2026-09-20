# Partial bounded-history disconnect acceptance

## Scope

This fixture cuts an actual pinned-provider BETWEEN response after its opener and first PRIVMSG, before the batch closer. It exercises the real Repartee connection and App state, then reconnects and retries against the same provider. It is not a full daemon/browser fault test. The stalled-response variants below extend coverage to range timeout and cancellation; SEARCH batches and ordinary scrolling batches remain separate.

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

G4/G8 still need late replies after batch/request expiry, concurrent connection/target isolation, and the corresponding SEARCH and ordinary-history paths. The existing daemon TCP-cut fixture cuts after completed operations, so it is not substituted for those checks.

## Stalled response and explicit cancellation

```sh
python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju --history-stall timeout
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --history-stall timeout
python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju --history-stall cancel
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --history-stall cancel
```

These modes forward the actual opener and first row, then hold the remaining upstream bytes while leaving the transport open. Timeout mode waits for the real 30-second search deadline, running the App's history maintenance methods without changing clocks or pending timestamps. Cancel mode submits `/bsearch cancel` after the App consumes the partial row.

Both modes require the connection to stay connected and a second request to be refused while the old response is unresolved. The proxy then releases the original bytes, including the terminal batch reply. The fixture checks that no partial/late results are displayed, the ordinary conversation is unchanged, and a new request completes with exactly the expected rows on the same connection. SQLite excludes seeded conversation content and retains legacy/direct-IRC controls.

This covers range-search timeout and cancellation before the 60-second batch purge. It does not prove behavior after the batch itself expires or after the 90-second generic history-request timeout; those longer-delay paths and actual-daemon fault acceptance remain open.
