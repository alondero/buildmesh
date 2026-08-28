# PRD: Per-Harness Default Configurations & Per-Mesh Overrides (Ship Parity)

Status: shipped (issue [#1210](https://github.com/alondero/buildmesh/issues/1210))
Spec wayfinder map: [#1142](https://github.com/alondero/buildmesh/issues/1142)
Spec ticket: [#1148](https://github.com/alondero/buildmesh/issues/1148)

## Problem Statement

Before this epic, Buildmesh stored one model and one effort value on each
Mesh (`meshes.model`, `meshes.effort`) and applied them loosely across Agent
Harnesses. That created four concrete failure modes:

1. **Ambiguous configuration.** The same Mesh column meant different things
   for Claude Code (`--model <name>`), Codex (`--model <name>`), and the
   Terminal harness (no equivalent). Users could not tell whether a saved
   value was honoured.
2. **Unsupported flags reaching binaries.** A harness that received an
   unsupported flag either silently dropped it or crashed; either way,
   the user had no signal that the value was meaningless.
3. **No application-level defaults.** Every Mesh had to repeat the same
   choice; no way to set "I always want Opus on Claude Code".
4. **Autopilot was unsafe to enable.** Autopilot polled and spawned whatever
   the Mesh's resolved default harness was, with no capability gate — so a
   Mesh pointing at a harness that couldn't drive the autopilot loop could
   schedule runs that always failed.

## Solution

Decouple "what configuration does Buildmesh forward" from "which harness
gets it" by giving every Agent Harness a declared **capability contract**.
Layer the user-facing surface into three sparse maps — one per scope —
and resolve every spawn through a single most-specific-wins cascade:

```
explicit Agent Node spawn argument   (highest priority — ad-hoc override)
  > per-Mesh harness override         (rare, explicit per-harness exception)
    > application harness default     (set once in App Settings → General)
      > harness native behaviour      (fallback; no synthetic flags)
```

Each layer only contributes a field it has a non-empty value for; empty /
whitespace-only values normalise to "absent" before resolution. The resolver
masks any value the resolved harness's capabilities don't accept, so stale
or malformed persisted data can't produce an invalid CLI argument.

The legacy `meshes.model` / `meshes.effort` columns are migrated once into
the Claude Code override entry on schema v32 → v33 (no overwrite of an
already populated new entry); the legacy columns remain physically present
for backward compatibility but are no longer read as active configuration.

## User Stories

Pulled forward from [#1148](https://github.com/alondero/buildmesh/issues/1148);
this PRD does not restate all 35 — they are mapped to merged PRs below.

## Shipped Slices

| Slice | PR | Closes | What it lands |
|---|---|---|---|
| Capability contract + spawn-config resolver | [#1153](https://github.com/alondero/buildmesh/pull/1153) | [#1149](https://github.com/alondero/buildmesh/issues/1149) | Rust-owned `HarnessCapabilities` + `EffortControlKind`; one pure resolver replaces ad-hoc per-call precedence. Derived `#[ts(TS)]` wire types; no hand-declared TS. |
| Application defaults per Harness | [#1154](https://github.com/alondero/buildmesh/pull/1154) | [#1150](https://github.com/alondero/buildmesh/issues/1150) | Sparse `harness_defaults` map in `AppPreferences`; `<HarnessDefaultsSection>` in App Settings, capability-gated, harness-specific effort vocabulary. |
| Cascade layer 1 (explicit Agent Node spawn arg) | [#1156](https://github.com/alondero/buildmesh/pull/1156) | [#1155](https://github.com/alondero/buildmesh/issues/1155) | Ad-hoc `Agent Node → Spawn Arguments → model/effort` wins over everything else. |
| Per-Mesh overrides + v32→v33 legacy migration | [#1158](https://github.com/alondero/buildmesh/pull/1158) | [#1151](https://github.com/alondero/buildmesh/issues/1151) | New `meshes.harness_configs` JSON column; one-shot migration of non-empty legacy `model`/`effort` into the Claude Code override; deprecated columns remain physically present. |
| `SpawnRequest::new()` + `resolve_spawn_config` seam | [#1159](https://github.com/alondero/buildmesh/pull/1159) | [#1157](https://github.com/alondero/buildmesh/issues/1157) | Single spawn-time resolver; every spawn surface (native, Proxied Provider, autopilot) consumes the same pure function. |
| AutoAutopilot compatibility gate | [#1160](https://github.com/alondero/buildmesh/pull/1160) | [#1152](https://github.com/alondero/buildmesh/issues/1152) | UI disables the master "Autopilot on" checkbox with a concrete reason when the resolved default harness lacks the required capabilities; backend rejects the same. |
| Capability coherence follow-up (mcode) | [#1185](https://github.com/alondero/buildmesh/pull/1185) | [#1179](https://github.com/alondero/buildmesh/issues/1179) | Adapter capabilities match the effective launch recipe (closes the documented gap that the contract alone exposed). |
| Grok Code `--effort` / `--reasoning-effort` into contract | [#1319](https://github.com/alondero/buildmesh/pull/1319) | [#1280](https://github.com/alondero/buildmesh/issues/1280) | Grok harness exposes model + effort in the same capability contract as Claude/Codex. |
| AGY `--effort` into contract | [#1310](https://github.com/alondero/buildmesh/pull/1310) | [#1286](https://github.com/alondero/buildmesh/issues/1286) | Antigravity harness exposes effort on the capability contract. |
| AGY detection + native `--sandbox` forwarding | [#1311](https://github.com/alondero/buildmesh/pull/1311) | [#1287](https://github.com/alondero/buildmesh/issues/1287) | Auto-detect + `config_dirs`, native `--sandbox` plays nicely with the resolver's "no synthetic flags for unsupported harnesses" rule. |
| Circuits walking skeleton | [#1213](https://github.com/alondero/buildmesh/pull/1213) | [#1206](https://github.com/alondero/buildmesh/issues/1206) | Manual `Trigger Now` on the Circuits Probe tab; per-circuit `enabled` toggle; live verification hook for [#1212](https://github.com/alondero/buildmesh/issues/1212). |

## Implementation Decisions

- **Capability declarations are Rust-owned, TS-generated.** Every new wire
  type derives `#[ts(TS)]` (issue #359). The frontend renders from the
  generated metadata instead of hard-coding harness names — adding a harness
  is a backend-only change.
- **The resolver is the only spawn seam.** Native, Proxied Provider,
  Autopilot — every spawn entry point funnels through `resolve_spawn_config`
  and consumes its output. The same capability mask applies at every seam.
- **Empty / whitespace-only values are absent.** The resolver normalises
  per field, so a stray `" "` in saved prefs cannot create a phantom
  override. Unknown harness ids are rejected at the write boundary, so
  corrupt configuration cannot leak into unrelated harnesses.
- **One-shot, idempotent migration.** Schema v32 → v33 copies non-empty
  legacy `model` / `effort` columns into the Claude Code entry of the new
  `harness_configs` map. Empty legacy values produce an empty map. Running
  the migration twice is a no-op.
- **Autopilot is gated, not redesigned.** The compatibility verdict is
  derived from the resolved default harness's declared requirements
  (worktree, prompt/prefill delivery, attention hooks). The verdict disables
  the UI control with a concrete reason and rejects the same enable request
  on the backend, so a backdoor caller cannot enable Autopilot for an
  incompatible Mesh.
- **Legacy columns remain physically present.** Dropping them would break
  any external tool reading the SQLite file. Deprecating the read path in
  code is sufficient — they stay row-compatible.

## Testing Decisions

- **Stable external-behavior tests** drive the resolver through every
  precedence layer, per-field fallback, empty normalisation, native fallback,
  and capability masking. Tests pin the recipe shape for representative
  harnesses (Claude Code, Codex, AGY, Grok, mcode) so adding a harness
  cannot silently change existing launches.
- **Database migration tests** start from v32 rows and verify preservation
  of non-empty legacy values, empty legacy values, already-populated new
  values, and repeat-safe one-shot behaviour.
- **Typed-command tests** cover add / update / remove of one Mesh override,
  reset-all, application-default updates, unknown harness ids, invalid
  effort values, and error propagation without nested DB locking.
- **Component tests** verify capability-gated fields in `<HarnessDefaultsSection>`,
  the sparse override list in `<MeshOverridesSection>` (Add / Edit / Reset /
  Reset all), and the disabled-with-reason autopilot enable control.
- **The green bar is the merge gate.** `scripts\check.ps1 all` (Windows /
  worktree) must pass: unit vitest + cargo lib tests + clippy `-D warnings`
  + tsc + vite builds.

## Out of Scope

Explicitly deferred from the [#1148](https://github.com/alondero/buildmesh/issues/1148)
spec — preserved here so they don't drift back in:

- Multi-tier model overrides per Mesh (small / fast, reasoning, Sonnet, Opus,
  or other alias tiers).
- Exporting, importing, or sharing Mesh harness-configuration presets.
- Per-Model Provider account defaults.
- Per-Harness + Proxied Provider pairing configuration matrices in Mesh
  Properties (the resolver still passes Proxied Provider `model_tiers`
  translation through unchanged).
- Workspace trust configuration.
- General-purpose arbitrary CLI flag editing.

## Follow-ups (deferred — tracked under separate tickets)

- [#1212](https://github.com/alondero/buildmesh/issues/1212) — Circuits
  walking-skeleton live verification + small follow-ups.
- [#1219](https://github.com/alondero/buildmesh/issues/1219) — Circuits
  editor follow-ups (per-step log capture, AST harness / timeout fields,
  layout persistence, Probe create-row simplification).

## Further Notes

- The umbrella ticket [#1210](https://github.com/alondero/buildmesh/issues/1210)
  was opened without a body (a `null` body, the same class of bug fixed in
  [#1216](https://github.com/alondero/buildmesh/pull/1216) for future issues).
  The shipped-slice table and the parity map are reproduced in its closing
  comment so the ticket is discoverable to `gh issue view` and to future
  readers of the repo.
- `docs/knowledge-primer.md` is the source of truth for the harness /
  provider / Proxied Provider vocabulary used throughout this doc; the
  resolver replaces several older "configuration" sections that used to live
  on Mesh Properties.
