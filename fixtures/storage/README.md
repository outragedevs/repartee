# SQLite compatibility fixture

`sqlite-039.db` contains synthetic history only. It was generated on
2026-10-15 with rusqlite 0.39.0, bundled-full, and libsqlite3-sys 0.37.0
(SQLite 3.51.3), before testing rusqlite 0.40.2 / SQLite 3.53.2.

The generator opened a new file with `rusqlite::Connection::open` and ran
`execute_batch` with the exact contents of `sqlite-039.sql`, then closed
its connection. Do not regenerate the binary with the current dependency:
it is a fixed old-engine input for the upgrade regression test.

The schema predates millisecond timestamps and other additive message
columns and retains the old global message-ID index. Its FTS5 index and
triggers were created by the old SQLite engine. The regression test copies
this file to a temporary directory and checks read-only legacy reads,
application migrations, message-ID backfill, FTS queries, transaction
rollback/commit, and read/write plus read-only reopen persistence.
