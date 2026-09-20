# Bouncer account and network lifecycle

The pinned provider scenario runs one Repartee App with two accounts and two
networks per account. Account labels and conversation names are deliberately
identical. Both providers allocate network IDs globally, so actual network IDs
are distinct; socket/unit scope tests cover overlapping IDs from separate
accounts/endpoints. Lurker fixtures additionally reuse message IDs across
networks. Each history body identifies its owning account and network.

Run against the revisions in `BOUNCER_SUPPORT.md`:

```sh
python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju --account-matrix
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --account-matrix
```

The scenario verifies:

- Four independently scoped generated connections and two history conversations
  per network, each initially containing 200 server rows.
- Concurrent BEFORE and BETWEEN requests on every connection. Older pages reach
  300 rows; each separate range view receives exactly its own three expected
  rows, with no cross-account or cross-network results.
- A real additional IRC client receives one network's read marker. Other
  networks retain their own unread state and do not acquire that marker.
- Closing and reconnecting one account's control transport preserves the other
  account's connection generations, scopes and history.
- Provider-side rename preserves the child connection and scope. Delete removes
  the child and all its buffers. Recreating the same name yields a new identity
  and does not inherit deleted history; other connections remain unchanged.
- Terminating and restarting the actual provider process with its existing
  database restores both accounts and four networks on the same endpoint.
  Restored scopes remain stable and visible history contains no cross-network
  rows or duplicates.
- Conversation and search rows never enter Repartee's local log queue. This
  queue assertion supplements the separate disk-backed history fixtures; this
  scenario does not inspect an actual Repartee SQLite writer.

This run exposed a persistence defect on both providers: after provider restart,
an AWAY status numeric displayed in the active special search view entered the
local log queue. The bouncer logging exclusion now covers special buffers as
well as server/channel/query buffers. DCC and direct-IRC logging retain their
existing behavior.

The provider processes, authentication, history, read markers, registry updates
and process restart are real. Soju networks are disabled upstreams; Lurker uses
its upstream harness. This does not prove real upstream offline/online traffic,
combined presence/metadata isolation, browser rendering or OS focus changes.
Those remain separate completion evidence requirements.
