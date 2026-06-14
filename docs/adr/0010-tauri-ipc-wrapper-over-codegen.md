# 10. A Hand-Written `invoke` Wrapper (not codegen) as the IPC Seam

Status: accepted

Every Tauri `invoke` call goes through one typed wrapper, `src/lib/tauri.ts`, rather than calling `invoke('command_name', …)` raw. The wrapper is hand-written (one thin function per `#[command]`, typed with the generated wire types from ADR-0009), and adoption is enforced by a drift test (`tests/unit/tauri-ipc-seam.test.ts`) with a shrinking allowlist — not by generating the bindings with `tauri-specta`.

## Context

A renamed or removed `#[command]` is this project's #1 runtime-failure mode (see `CLAUDE.md`: "New `#[command]` … must be added to the `lib.rs` handler list, or they fail with 'command not found' at runtime"). `invoke('foo', …)` is a string literal TypeScript cannot check, so a backend rename breaks silently in every component that still passes the old name.

The wrapper `src/lib/tauri.ts` already existed but was **dead**: ~28 functions were defined and *zero* call sites imported them — ~63 raw `invoke` sites across stores and components called the backend directly, 14 commands were invoked both ways, and several hand-declared their own request/response shapes instead of using the generated types. The seam existed on paper but enforced nothing.

ADR-0009 generates the *wire types* (`ts-rs`) but explicitly left the *call surface* out of scope ("specta + tauri-specta … Rejected for this first cut"). This ADR closes that gap for the command layer.

## Decision

Adopt the hand-written wrapper everywhere and enforce it:

- Each `#[command]` gets one thin wrapper function in `src/lib/tauri.ts`, typed with the generated types from `src/types/generated/`. The command-name string lives in exactly one place.
- Consumers import the wrapper (`import * as api from '../lib/tauri'`), never `invoke` from `@tauri-apps/api/core`.
- A drift test enumerates files importing raw `invoke` and asserts they are within a declared allowlist of not-yet-migrated files. The allowlist is the remaining to-do list; it shrinks as files migrate. A *new* raw-`invoke` file fails the test immediately; when the allowlist is empty the seam is fully enforced.
- Migration is staged, stores first (the highest-leverage chokepoint, holding the commands that were invoked both ways), then components.

## Considered options

- **`tauri-specta` (generate the `invoke` bindings).** The strongest guarantee — a renamed/removed `#[command]` fails at codegen/compile, not at lint/test time. Rejected: it is a **new dependency** on top of the project's already-fragile Windows build (`CLAUDE.md`: no new deps beyond the task), and it overlaps ADR-0009's `ts-rs` machinery — adopting it means running two generators or migrating type-gen too. The marginal gain over "one lint-/test-enforced wrapper + generated types" is compile-time vs test-time detection of a rename, which is a narrow win for a wide cost. *A future architecture review will predictably re-suggest "generate the bindings like you generate the types" — this is why the rejection is recorded.* Revisit only if the project decides to consolidate on `specta` and reopen ADR-0009 deliberately.
- **An ESLint `no-restricted-imports` rule.** The natural enforcement, and what the original plan called for. Rejected because the project has no ESLint setup (no config, no `lint` script, no dependency); adding ESLint solely for this rule is scope creep. The repo already enforces conventions through tests (the token-exists test, the IPC contract test) and the `guard-antipatterns.mjs` hook — a drift test fits that grain and, unlike the edit-blocking hook, doesn't disrupt editing not-yet-migrated files mid-sweep.
- **Delete the wrapper, keep raw `invoke`.** Cheapest, and honest about the then-current reality (nothing used the wrapper). Rejected: it abandons the only defense against the command-name failure mode. The generated types protect the *payload shape*, never the *command-name string*, which is the actual fragility.

## Consequences

- A command rename now changes one wrapper function; TypeScript flags every caller. The command-name string has a single home.
- Adoption ratchets: the allowlist only shrinks, and CI fails if a new file reaches for raw `invoke`.
- The wrapper stays a **pure typing seam** for now — it adds the typed command name and nothing else. Central IPC error logging (via the existing `frontendLog` bridge) is a deliberate follow-up the single chokepoint now makes possible; it is intentionally not bundled with this migration.
- Adding a `#[command]` still means hand-adding a wrapper function. The drift test makes the omission visible the moment a consumer tries to call it raw, but it does not generate the function — that is the residual cost of choosing hand-written over codegen, accepted above.
