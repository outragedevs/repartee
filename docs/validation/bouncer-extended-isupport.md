# Extended ISUPPORT validation

The client negotiates `draft/extended-isupport` and applies complete
`draft/isupport` batches atomically. These are incremental updates, not replacement
snapshots: omitted keys remain, and `-KEY` removes a key. Earlier audit notes that
proposed wholesale replacement were incorrect and are superseded here.

Authoritative protocol definition:
https://ircv3.net/specs/extensions/extended-isupport

Pinned Soju `downstream.go:4104` emits this batch when negotiated, including the
registration parameters and explicit `ISUPPORT` responses. The pinned Lurker
endpoint does not advertise this capability and continues using ordinary 005.

Empty, malformed, truncated, timed-out and invalid nested batches do not apply
partial updates. An ISUPPORT batch within a valid labeled-response parent is
accepted; batches inside ISUPPORT itself violate its 005-only contract and are
rejected. Connection registration confirms BOUNCER_NETID only from a complete,
validated burst. Parameters received before welcome survive connection setup
when the extension is negotiated; disconnect still clears old parameters.
Ordinary 005 supports both its explanatory trailing parameter and token-only
replies. Shared connection state serves native and web consumers.

Regression coverage is in `src/app/isupport_tests.rs` and
`src/irc/bouncer/tests.rs`. The ignored `pinned_bouncer_extended_isupport` test
runs against either provider through:

```
python3 scripts/test_bouncer_binding.py soju <pinned-soju-directory> --test-filter pinned_bouncer_extended_isupport
python3 scripts/test_bouncer_binding.py lurker <pinned-lurker-directory> --test-filter pinned_bouncer_extended_isupport
```

Full checks and both pinned integrations pass. Full `gpt-5.6-sol` / `medium`
review round 3 found no actionable defects.
SAFERATE, account-required, ICON, channel-context routing and the full acceptance
matrix remain separate required work.

Review round 1 found two malformed-input gaps: negated tokens with values and
BATCH terminators tagged into an ISUPPORT parent. Both now invalidate the burst;
regressions cover each. Extension negotiation uses its own CAP REQ after SASL,
before CAP END, to keep the existing capability request below the wire limit and
avoid authentication waiters consuming early extended-ISUPPORT messages.

After the first review fixes, Clippy passes with no project warnings; 2664 native
and 145 web tests pass (`/tmp/repartee-isupport10-{clippy,test}.log`). The pinned
Soju and Lurker integration runs pass respectively in
`/tmp/repartee-isupport-soju4.log` and `/tmp/repartee-isupport-lurker3.log`.
The socket fixture proves SASL precedes the separate extension request and early
bursts are replayed. Native regressions also cover final label selection and
abandoning an open burst on disconnect. Full Sol medium review round 2 is running.

Review round 2: automatic NETWORK labels now retain provenance, follow later
updates and revert to the origin label on removal; explicit names, including
names containing dots, remain unchanged. The other finding (005 script
suppression) was checked against `App::emit_irc_to_scripts`: its command match
has no numeric Response arm and returns false for every 005. Thus no existing
005 script hook can suppress either route. The redundant no-op hook was removed;
no scripting API is changed or invented to satisfy that false positive.

Automatic label updates also preserve existing query/channel buffers when a new
network name collides, update browser selections and broadcast the rename. A
regression covers a query named `alice` while the network becomes `alice`.

The final source after label fixes passes Clippy without project warnings and
2666 native/145 web tests (`/tmp/repartee-isupport14-{clippy,test}.log`), including
case-only renames. Both pinned integrations pass again in
`/tmp/repartee-isupport-soju5.log` and `/tmp/repartee-isupport-lurker4.log`.

Final clean review evidence: `/tmp/repartee-isupport-review3.log`. All three
model/review-model/reasoning overrides were explicitly pinned.
