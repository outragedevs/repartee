# Bounded history provider acceptance

This is provider wire-protocol evidence for G4, not completed Repartee range retrieval. The structured client request state, native/web entry points, cancellation and storage-exclusion acceptance remain to be implemented.

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

## Remaining G4 acceptance

Add a two-bound request to Repartee, expose a native/web retrieval path, and verify real-provider bounds and limits through that path. Cover cancellation, failed requests, delayed batches, connection/target isolation and absence of bouncer-history writes to local storage. This probe establishes expected provider behavior only and does not close G4.
