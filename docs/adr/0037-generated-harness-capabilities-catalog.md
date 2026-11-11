# 37. Generate the per-harness capability catalog from Rust

Status: accepted

Launch Configuration amendment (#1857): provider endpoints, model choices, and
model effort restrictions are also Rust-owned static metadata. The editor
consumes generated wire types and backend-filtered launch targets. Effective
effort is the intersection of model metadata and adapter capabilities; clients
do not maintain an independent compatibility catalogue.

Per-harness capability *values* (and Inspector/docs labels) are owned by the
Rust adapters. `cargo test` emits a committed TypeScript/JSON snapshot under
`src/types/generated/`. A hand-written TypeScript table that copies those
values is a defect. This is an application of [ADR-0009](0009-shared-rust-ts-types-via-ts-rs.md)
to catalog *data*, not only wire *shapes*.

## Context

`HarnessCapabilities` is already ts-rs generated — but ts-rs emits types, not
values. The Circuits Inspector is an authoring form and must answer "what can
harness X do?" for any selectable harness, including ones the user has not
installed. `list_providers` / `available_providers()` cannot answer that: they
are driven by detection and configured accounts.

The previous answer was `src/components/Circuits/harnessCapabilities.ts`, a
hand-copied table, plus a vitest that asserted that table against *another*
hand-typed literal list. Two independent copies cannot detect drift between
themselves. Three harnesses drifted silently.

## Decision

1. Rust owns a total catalog: [`builtin_harness_catalog()`](../../src-tauri/src/agent/harness_catalog.rs)
   enumerates [`Provider::all()`](../../src-tauri/src/models/agent.rs) and
   records `capabilities_for(adapter)` plus its Inspector/docs label.
2. `cargo test` writes `src/types/generated/HarnessCapabilitiesTable.ts` and
   `.json` (same `TS_RS_EXPORT_DIR` as ts-rs). CI's existing
   `git diff --exit-code src/types/generated` gate covers both files.
3. The Inspector and docs gates consume the generated artifact. Lookup helpers
   (`getCapabilitiesFor`, `effortAllowedFor`) stay in TypeScript because they
   are logic, not data.
4. The Harness Profile id `claude` is not a `Provider` variant. The catalog
   exports `HARNESS_PROFILE_ALIASES` (`claude` → `anthropic`) so that mapping
   is deliberate. Completeness tests require
   `BUILTIN_HARNESS_IDS == catalog keys ∪ aliases`.
5. Docs: every generated label must appear as a `| <label> |` row in
   `docs/user-guide.md` and `docs/learning/harness-capabilities-matrix.md`.
6. The catalogue is emitted as two artifacts from the same Rust rows: the full
   `HarnessCapabilitiesTable.ts` for the Inspector, and the narrow
   `HarnessReviewEligibility.ts` — the id vocabulary, the alias table and the
   four review-circuit eligibility flags — for the surfaces that are rendered
   on first paint. The split exists because the eager entry chunk has to
   answer "may this harness start a review circuit?" without carrying the
   whole catalogue (issue #2757). A Rust test asserts the two artifacts agree
   flag for flag. The **TypeScript declaration** of the id vocabulary and the
   alias table is in the narrow artifact, and the full catalogue re-exports it;
   the full catalogue's *payload* still carries a copy of the id list, because
   the Rust and docs gates read it from the emitted JSON.

### Authoritative declarations and launch dispatch (#2524)

Each adapter declares one immutable `HarnessDescriptor`. Capability readers use
that descriptor; `capabilities_for` projects it into `HarnessCapabilities` for
the menu, validation, configuration resolver and generated catalogue. Attention
installation derives from the structured attention capability, effort support
from its vocabulary, and default permission mode from the first declared mode.
These derived facts have no independently overridable adapter methods.

Command construction calls the selected adapter's object-safe `prepare_launch`.
Shared option assembly is private implementation reuse behind that dispatch;
tests call the same interface as production. Routing resolves the pairing,
credential reference, executable and runtime. The adapter spells the resolved
contribution into arguments and environment changes. The OS wrapper arranges
the process. In particular, Codex owns its profile arguments, route model
fallback, scoped credential and login home rules.

Session preparation remains behavior: OpenCode 2 can mint `ses_` identities and
return a fork identity, while Cline self-assigns and contributes no assignment
arguments. A descriptor does not replace these operations with a generic flag.

## Alternatives considered

- **(A) New `list_harness_capabilities` command.** Always runtime truth, no
  codegen. Rejected for this slice: the Inspector is a synchronous form, the
  command would need a failure fallback, and labels would still need a home.
  `list_providers` cannot be reused — see the detection caveat above.
- **(B) Generated static table (chosen).** Synchronous, no IPC, reuses the
  binding drift gate, and a new `Provider` variant either fails a Rust test
  or appears in the table with no frontend data edit.
- **(C) Hybrid: generated table overlaid with live `ProviderInfo.capabilities`
  when a detected row exists.** Useful if per-instance variation ever appears.
  Declined until a harness actually advertises instance-specific capabilities;
  overlaying today would hide catalog bugs behind detection.

## Consequences

- Adding a harness means: `Provider` variant, adapter, `BUILTIN_HARNESS_IDS`
  entry, `inspector_label` match arm (compile error if omitted), then
  `cargo test` to refresh the snapshot. Frontend data tables are not edited.
- Capability semantics still change only in the adapter. This ADR does not
  authorise "fixing" a flag in TypeScript.
- Inspector dropdown order follows `Provider::all()`, including Meta Muse,
  which the previous hand list omitted.
- README / docs gates parse the generated JSON rather than scraping source
  text, so a regex drift cannot false-pass coverage.
- The review-circuit eligibility predicate
  (`src/components/Circuits/harnessReviewEligibility.ts`) reads the narrow
  generated artifact; the Inspector's lookup helpers read the full one. Both
  are generated data — neither may be hand-copied.
