# Bounded history provider acceptance

This records provider wire-protocol evidence and the Repartee bounded-history command for G4. Real-provider interruption during an in-flight range remains open.

## Reproduction

Use the unchanged provider revisions in `scripts/test_bouncer_binding.py` (Soju `82e8b7adfb2ab64ec3b88807d29b8b6940236008`, Lurker `be42a04e73d6f337e76734684deb457cb5dcdb5f`), with their existing fixture build dependencies installed:

```sh
python3 scripts/test_bouncer_binding.py soju /path/to/soju --history-range
python3 scripts/test_bouncer_binding.py lurker /path/to/lurker --history-range
```

The runner creates a disposable account, network and database, seeds 300 messages in each of a query and channel, starts the actual provider, and deletes the fixture on exit. The probe authenticates with SASL, binds the exact network, negotiates batch/server-time/message-tags/history, and verifies `BOUNCER_NETID` and `MSGREFTYPES`. It does not run Repartee or test either client UI.

## Verified behavior

The numbered messages have timestamps one second apart, beginning at 2024-01-01T00:00:00.000Z. Both pinned providers passed all of the following independently for the query and channel:

| Request bounds (seconds), limit | Returned message indices |
| --- | --- |
| 10 to 20, 3 | 11, 12, 13 |
| 20 to 10, 3 | 17, 18, 19 |
| 10 to 20, 100 | 11 through 19 |
| 10 to 10, 100 | Empty batch |
| 400 to 500, 100 | Empty batch |

Both bounds are exclusive. Descending bounds select the newest rows in the interval, but the response still arrives oldest-first. Empty results retain proper batch framing. A wildcard is rejected in either bound with `FAIL CHATHISTORY INVALID_PARAMS BETWEEN`.

Both providers advertise **timestamp-only references**. Lurker emits message IDs when message-tags is negotiated, but still rejects those actual returned IDs as BETWEEN bounds with `INVALID_MSGREFTYPE`. Soju rejects `msgid=` bounds with `INVALID_PARAMS`. Presence of message IDs is therefore not evidence that a provider accepts them as history anchors; the client must honor `MSGREFTYPES`.

## Repartee command and storage acceptance

`/bsearch between <target> <first RFC3339 time> <last RFC3339 time> [1..1000]` sends a tracked BETWEEN request, honoring the server limit. It uses the ephemeral search view, independent of the `soju.im/search` capability. Existing search cancellation, labels, timeout quarantine and reconnect cleanup also apply. Results outside the requested interval are discarded.

```sh
NODE_PATH=/path/to/playwright/node_modules python3 scripts/test_bouncer_binding.py soju /path/to/soju --test-filter pinned_bouncer_bounded_history
NODE_PATH=/path/to/playwright/node_modules python3 scripts/test_bouncer_binding.py lurker /path/to/lurker --test-filter pinned_bouncer_bounded_history
```

Both passed against actual providers. The App fixture submits the ascending request through native `handle_submit`, descending and equal bounds through `WebCommand::RunCommand`, and compares exact returned rows. The fixture also serves the actual WASM frontend to WebKit, submits both range directions through the input, checks exact rendered rows and reloads the browser without duplicating results. Install Playwright and its WebKit runtime before running it. It shuts down the actual SQLite writer and checks the database: the pre-existing legacy row and direct-IRC positive control remain, with no bouncer-history rows. Only the two known away/unaway diagnostic texts in the server or special search buffer are additionally allowed; browser presence triggers those existing local events.

Deterministic tests cover limit clamping without SEARCH support, cancellation and discarded late results, exclusive-bound response validation, invalid selectors and errors releasing request state. Existing context tests cover the shared labelled batch routing and timeout lifecycle.

## Remaining G4 acceptance

Interrupt an actual provider while its range batch is in flight. Extend coverage of concurrent connection/target isolation and delayed range replies. These checks are still required before closing G4.
