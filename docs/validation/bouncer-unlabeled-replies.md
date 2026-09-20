# Unlabeled bouncer reply routing validation

Status: implemented on `fix/bouncer-unlabeled-reply-routing`; full Sol medium review clean.

## Verified defects

`active_or_server_buffer` selected the active buffer without checking its
connection when no label context existed. An unlabeled WHOIS arriving after
selecting another network could appear on that other network. A separate defect
allowed replies in bouncer server buffers into the local SQLite write queue.
Both regressions failed in `/tmp/repartee-unlabeled-check1-test.log`.

Fallback now uses an active buffer only on the originating connection, otherwise
that connection's server buffer. It does not change UI selection. Bouncer server
buffers are excluded from automatic local logging, in addition to conversational
bouncer buffers and correlated replies. The owner check uses the actual buffer's
connection. Direct IRC and DCC retain local history; the existing DCC regression
caught an overly broad initial storage exclusion, which was corrected before
review.

## Validation

`/tmp/repartee-unlabeled-check4` passed make clippy then make test with zero project
warnings, 2509 native tests, 142 web host tests and 10 ignored fixtures.
Regressions verify native/web destination after switching networks, unchanged
selection, bound/control server-buffer non-persistence, same-network behavior,
and positive controls for direct IRC logging and existing DCC history.

The real fixture now submits an unlabeled WHOIS, switches to another connection
before consuming the response, and requires native and web output in the source
server buffer with no SQLite log rows. It runs for both pinned Lurker and Soju,
as part of `scripts/test_bouncer_presence.py PROVIDER SOURCE --names`.
The fixture server's unique WHOIS realname proves the answer traveled through the
actual upstream and bouncer. Both fixtures passed:

- `/tmp/repartee-unlabeled-lurker2.log`
- `/tmp/repartee-unlabeled-soju2.log`

Full CLI review with explicitly pinned `gpt-5.6-sol`, `review_model=gpt-5.6-sol`
and medium reasoning completed without findings:
`/tmp/repartee-unlabeled-review1.log`.

## Remaining acceptance

This fixes reply destination and future automatic writes. It does not purge
historical local data or prove every older server-buffer backlog read path.
All remaining `BOUNCER_SUPPORT.md` rows and specialized label integration audits
remain required for full support.
