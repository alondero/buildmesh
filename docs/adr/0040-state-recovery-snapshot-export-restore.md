# ADR 0040: State recovery — snapshot, export, integrity check, and staged restore

Status: accepted.

Issue: [#1537](https://github.com/alondero/buildmesh/issues/1537)
Date: 2026-10-04

## Context

Buildmesh's durable state is a profile-scoped `buildmesh.db` (SQLite),
`preferences.json` beside it, and a `tls/` directory holding the LAN root CA
**private key**. Before this ADR there was no user-facing way to move, back up,
or recover any of it, and three specific hazards:

1. **Upgrades were one-way.** `db::migrations::evolve_to` runs column ALTERs,
   one-shot backfills, and an always-run safety-net pass against the live
   database during `db::init`. A bug in any phase left the user with damaged
   state and no pre-upgrade copy to return to.
2. **A naive folder copy leaks.** Zipping the app-data directory would sweep in
   `tls/ca.key.der` (the private key for the HTTPS identity every paired device
   trusts) and the cleartext `remote_access_token` — the Admin-role bearer token,
   stored cleartext pending the Keychain work in #495.
3. **Corruption had no non-destructive exit.** There was no way to find out a
   database was damaged without risking the data, and no supported way to
   recover once you knew.

## Decision

`services::state_recovery` owns snapshot, validate, export, restore, and
retention. Four invariants carry the design.

### 1. Snapshots use `VACUUM INTO`, not file copies

A WAL-mode database's committed content may still live only in the `-wal`
sidecar, so copying `buildmesh.db` yields a torn image. `VACUUM INTO` reads
through the WAL inside one SQLite read transaction and emits a single
self-contained file with no sidecars.

The capture uses a **private connection**, not `db::read_conn()`. The project
rule is that filesystem I/O must never run while a pooled or shared DB handle is
held, and `VACUUM INTO` writes its destination file. A private connection also
means a snapshot can never deadlock against the writer mutex it is capturing.

A pre-migration snapshot is taken in `run_profile_startup`, before `db::init`,
by opening its own read-only connection to read `schema_version`. The layering
stays `commands → services → db` — nothing in `db::` grows a dependency on
`services::`, and the hook is testable without the process-global `DB`.

### 2. Exports are a versioned container, not a folder copy

A `.bmsnap` file is `magic | u32 header_len | JSON header | payload blob`. The
header names each section with `(offset, length, sha256)`. Three properties a
folder copy cannot give:

- **Version rejection.** A bundle whose `format_version` is newer is refused
  outright, so an older build cannot half-apply a newer format.
- **Per-section checksums.** A truncated or edited file is detected *before*
  restore touches anything.
- **Explicit absences.** "This export contains no TLS keys" becomes a checkable
  statement about the section list rather than an assumption about what the
  exporter happened to include.

Payloads are staged in a temp file before the header is serialized, because a
section's offset cannot be known before the header's own length is. The final
file is header + a streaming copy of the payload.

### 3. Redaction runs against the copy, never the original

`build_bundle` captures to a staging file first, then redacts **that**:

- `app_settings` rows `remote_access_token`, `coordinator_read_token`,
  `coordinator_drive_token` are deleted, and `device_sessions` is cleared, so a
  redacted export cannot inherit another machine's logged-in devices.
- `preferences.json` is rewritten as `serde_json::Value`, dropping
  `minimax_api_key` and each `provider_accounts[].api_key`.

`preferences.json` redaction deliberately does **not** round-trip through the
`AppPreferences` struct. An export must survive a preferences file written by a
newer build; going through today's struct would silently discard every field it
does not know about. Unknown fields are preserved verbatim.

`tls/ca.key.der` is never in a bundle and has **no "include secrets" toggle**.
Anyone holding that key can impersonate the HTTPS identity paired devices
already trust, and that is not a risk a checkbox should offer. A user who wants
a full profile copy copies their own data directory.

Terminal transcripts are absent because durable state never held any: scrollback
is xterm.js state in the frontend, and agent transcripts live in harness session
directories outside the profile.

### 4. Restore is staged, and applied before `db::init`

`stage_restore` runs in three ordered steps:

1. **Verify.** Magic, header length, header JSON, format version, section
   bounds, then every section's SHA-256. A failure here returns before any
   write, so a bad bundle changes nothing.
2. **Rollback.** The current state is snapshotted in full fidelity, so a restore
   is always reversible.
3. **Stage.** Sections are extracted and fsynced into `pending-restore/`, and
   only then is the marker written. A crash before the marker leaves an inert
   directory — there is no state in which a partial restore can apply.

`apply_pending_restore` runs from `run_profile_startup` **before** `db::init`,
where no connection, reader pool, circuit worker, or PTY exists. "Close
workers/connections safely" is satisfied structurally rather than by a shutdown
race, and the user's restart is the confirmation step.

It deletes `buildmesh.db-wal` and `-shm` before moving the restored file into
place. This is not optional: SQLite would otherwise replay the *old*
database's WAL frames onto the restored file and corrupt it — exactly the
partial-state failure staging exists to prevent. It also re-runs both
`quick_check` and `integrity_check` on the staged payload, because the bytes on
disk may have changed since staging.

## Never silently reset

- A failed `quick_check` before migration still writes a snapshot. When
  `VACUUM INTO` cannot run on a damaged file, it falls back to a **raw byte
  copy**, labelled `pre-migration-raw` (and `manual-raw`), with a
  `RecoveryNotice` naming the path. The original bytes are preserved before
  anything else runs. A raw copy is not WAL-consistent, which is why it is
  labelled distinctly and the user is warned.
- A rejected bundle writes nothing and stages nothing — asserted byte-for-byte
  in `assert_rejected_without_side_effects`.
- An integrity check only ever reports. Nothing in this module rewrites a
  database to make a check pass.

## Retention

`SNAPSHOT_RETENTION = 3`, ordered by the timestamp slug in the filename with
mtime as a tiebreak. Three covers "the upgrade before this one" and "the one
that broke" with a spare. Manual snapshots share the cap: a bound the user
cannot exceed is a bound they cannot be surprised by.

## Alternatives considered

- **Zip the app-data folder.** Rejected: leaks `ca.key.der` and the cleartext
  root token, and gives no version or checksum gate.
- **Apply the restore immediately, live.** Rejected: a live writer, reader
  pool, circuit workers, and session pollers would hold handles to a file being
  replaced.
- **rusqlite's `backup` feature.** Rejected in favour of `VACUUM INTO`, which
  needs no new cargo feature and emits a sidecar-free file — exactly the shape
  a container section wants.
- **Encrypting exports.** Rejected for this slice. The safe default (redacted,
  with the omissions stated) removes the need for a password the user could
  lose, which would be its own recovery problem. A future slice can add it
  without changing the container: the header is already self-describing.
- **Bumping `SCHEMA_VERSION` to track restore state.** Rejected: a restore
  deliberately restores an *older* schema and lets the existing migration
  runner evolve it forward. Forcing the version would create a state the
  migration registry has never seen.

## Consequences

- Every upgrade is now reversible without user action.
- A redacted export restores structure without credentials: users re-enter API
  keys, and the root token is re-minted so paired devices must re-authenticate.
  The restore plan says so before the user commits.
- The startup recovery hooks are non-fatal by design. A recovery failure must
  not stop launch, because the user needs the UI to reach Restore; failures are
  logged and surfaced as a `RecoveryNotice` in the pane.
- Windows Credential Manager entries (OpenCode OAuth, Antigravity) remain
  outside any exportable file. Credential-storage remediation is tracked
  separately in #830.

## Verification

`src-tauri/src/services/state_recovery/tests.rs` pins the issue's five
acceptance behaviours, each against real files and real migrations:

- `old_schema_is_snapshotted_before_migration`,
  `snapshot_survives_a_real_migration_and_restores_pre_upgrade_rows` (the
  migration phases run for real via `db::init_schema`)
- `snapshot_includes_committed_wal_content` (WAL still un-checkpointed at capture)
- `export_import_round_trips_non_secret_state`,
  `a_default_export_contains_no_credentials` (a raw byte scan of the whole
  bundle for every secret literal, not just the places we thought to look)
- `a_file_that_is_not_a_bundle_is_rejected`,
  `a_truncated_bundle_is_rejected`, `a_tampered_section_is_rejected_by_its_checksum`,
  `a_tampered_header_is_rejected`, `a_bundle_from_a_newer_build_is_rejected`,
  `a_bundle_without_a_database_section_is_rejected` — all through
  `assert_rejected_without_side_effects`, which asserts the live database and
  preferences are byte-identical afterwards and nothing was staged
- `a_corrupt_database_is_preserved_and_reported_never_reset`,
  `an_interrupted_stage_is_discarded_rather_than_half_applied`,
  `a_corrupted_staged_payload_is_refused_at_apply_time`

Frontend: `tests/unit/data-recovery-settings.test.tsx`.

## User documentation

- [User guide — Data, backup, and restore](../user-guide.md#data-backup-and-restore)
- [Troubleshooting — A stored-state check reports damage](../troubleshooting.md#a-stored-state-check-reports-damage)
