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

The initial history phase starts with Soju disabled upstreams and Lurker's
upstream harness. The extended phase then activates four real TCP upstream
connections, verifies separately routed outgoing echoes, network/account away
scope, and Soju query metadata isolation (Lurker does not advertise metadata).
One upstream is stopped and restarted, and its downstream resumes traffic.
Lurker closes a downstream whose upstream connection object was replaced; the
fixture pumps the normal Repartee reconnect controller for recovery.

Network recreation and provider restart restore real upstream connections.
After the provider restart, fresh incoming messages identify each owning
connection, and assertions reject delivery to any of the other three networks.
The upstream wire log also rejects unsolicited JOIN throughout the scenario,
including after account reconnect and provider restart.

Set `REPARTEE_MATRIX_BROWSER_SCRIPT` to the absolute path of
`scripts/fixtures/account-matrix-browser.cjs` to include the compiled web UI.
The browser opens each of four identically named Alice queries after provider
restart, checks that exactly the owning connection's fresh message is rendered,
and repeats after browser reload. Set `NODE_PATH` and
`PLAYWRIGHT_CHROMIUM_EXECUTABLE` for the local Playwright installation.

Presence in this combined scenario is driven through the App web-command
handler; actual browser events and native attach are covered separately in
[bouncer-presence.md](bouncer-presence.md). It does not establish OS terminal
focus emission. The combined scenario checks the log queue; actual SQLite and
TRACE exclusion remain covered by the real daemon scenarios.

The browser check exposed a production defect in live own-message echoes: a new
query used the peer as its buffer ID but the sender (our own nick) as its display
name. The common PRIVMSG handler now uses the resolved conversation target for
both. The combined fixture checks the native state name immediately after each
own echo and the actual browser label/content after provider and browser restart.
