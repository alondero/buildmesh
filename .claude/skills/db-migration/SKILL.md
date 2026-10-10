---
name: db-migration
description: Add or change a SQLite schema migration. Use when bumping SCHEMA_VERSION, editing db/migrations.rs, or writing a migration test.
---

# Change the database schema

`SCHEMA_VERSION` lives in `src-tauri/src/db/migrations.rs`. `src-tauri/src/db/mod.rs` owns the connection pool and `init` only. Do not declare a second version constant.

1. Bump `SCHEMA_VERSION` and add the step in `src-tauri/src/db/migrations.rs`. Pass the `&Connection` you were given into an `_inner` helper. Do not call `read_conn()` or `write_conn()` from code that already holds a connection.
2. Do not do filesystem, git, or process work while that connection or the writer mutex is held. Prepare under the lock, do the I/O after releasing it, then write back under the lock.
3. New columns need a default or must be nullable, so existing rows still read. Match the steps already in `src-tauri/src/db/migrations.rs`. Do not add a second migrator.
4. Add a test in `src-tauri/src/db/migration_tests.rs` that runs the production initialiser on a fresh in-memory database and on a legacy schema that already has rows. Assert the preserved rows, the new columns or indexes, and that a second initialisation changes nothing. A test that uses `db::get()` installs its database with `db::test_support::isolated()` and holds the guard for the whole body.
5. Update `src-tauri/src/db/schema_dump.txt` in the same change. `init_schema_dump_matches_committed_snapshot` in `src-tauri/src/db/seam_tests.rs` fails when the fresh schema drifts from that file.

From `src-tauri/`:

`cargo test --lib migration_tests`

`cargo test --lib init_schema_dump_matches_committed_snapshot`
