# Redaction uses live ISUPPORT

Confirmed source defect: redaction read Connection.isupport, a legacy map without
production writers, while incoming numeric 005 updates isupport_parsed. Tests
previously populated the legacy map directly and therefore missed the defect.
Changing the existing custom-channel regression to ingest actual 005 messages
reproduced a failure before the implementation changed:
`/tmp/repartee-isupport-reproduction-test.log`.

All redaction CASEMAPPING and CHANTYPES reads now use the live parser, including
wire identity retention, deletion lookup and incoming private-message routing.
The regression covers custom '+' channels and ASCII, strict RFC1459 and RFC1459
case folding through actual numeric messages. Another regression verifies wire
identity folding, token removal returning defaults, and connection reset clearing
previous ISUPPORT. No state serialization or web asset changes are required.

Validation: make clippy has no project warnings; make test passes 2561 native and
144 web tests (12 provider fixtures ignored). Logs:
`/tmp/repartee-redaction-live-isupport-clippy.log` and
`/tmp/repartee-redaction-live-isupport-test.log`. Full Sol medium review is next.

Full Sol medium review round 1 is clean, including the full diff and new audit
file. All three model/review/reasoning overrides were pinned. Review log:
`/tmp/repartee-redaction-isupport-review1.log`.
