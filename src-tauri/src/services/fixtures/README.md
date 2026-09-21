# Cline `sessions.db` schema fixture

The fixture in this directory was captured against a real
`~/.cline/data/db/sessions.db` on Cline 3.0.x during the issue
#1769 / #1774 investigation. It pins the production schema so
`services::cline_session::tests::open_test_db` (the in-memory
test fixture) cannot drift from reality without a reviewer
noticing the diff.

## Files

- `sessions.schema.sql` — captured `CREATE TABLE sessions (...)`
  from the production database, byte-identical to the live schema
  (issue #1769).
- `sessions.sample.sql` — captured `INSERT INTO sessions ...` rows
  representative of a real conversation in flight. The id shape,
  cwd, started_at format, and interactive flag all match the live
  store.

## When to update

Re-run `sqlite3 ~/.cline/data/db/sessions.db ".schema sessions"`
and `... ".dump sessions"` whenever a Cline release changes the
schema (column added, type widened, default altered). The
`open_test_db` fixture in `services/cline_session.rs` is a
hand-written subset that captures only the columns the
capture/recovery code reads; expand it when new columns become
load-bearing.

The capture poller's freshness gate (`session_id_epoch_ms`) and
the SQL `started_at` predicate both rely on the embedded epoch ms
in `session_id` and the ISO 8601 format of `started_at`. If Cline
ever changes either — e.g. prefixing the id with a random tag or
switching `started_at` to an integer ms column — every test in
`cline_session.rs` will fail loud, which is the contract this
fixture pins.
