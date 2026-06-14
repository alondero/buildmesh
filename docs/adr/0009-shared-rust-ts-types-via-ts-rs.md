# 9. Generate Shared Rust↔TS Wire Types with ts-rs

Status: accepted

Wire types that cross the Tauri `invoke` boundary or the mobile HTTP server are generated from their Rust struct with [`ts-rs`](https://github.com/Aleph-Alpha/ts-rs) into `src/types/generated/`, committed to the repo, and drift-gated in CI. The Rust struct is the single source of truth; the hand-maintained TS interfaces that mirrored it are deleted. (Issue #359.)

## Context

The polyglot schema workflow was hand-maintained: a developer edited a Rust struct, then hand-edited a matching TS interface (often two — one for desktop `src/lib/tauri.ts`, one for mobile `src/mobile/api.ts`) and hoped they remembered every field. `tsc` cannot catch a missing or wrong field because `invoke<T>()` and `.json()` are *casts*, not validation — TypeScript erases types at runtime, so the declared shape is asserted, never checked against what Rust actually serialises. The result is silent drift that surfaces as a runtime crash deep in a render.

The drift was not hypothetical. Auditing the five most-edited wire types for this ADR found, in three independent hand-written copies each:

- **`SessionStatus`** serialised `AwaitingInput` as `"awaitinginput"` (serde `rename_all = "lowercase"`) while every consumer — DB column, desktop store, mobile screen — compared `"awaiting_input"`. It was masked only because the UI sets that status client-side, so the broken wire value never had to match. The "needs input" filter on both desktop and mobile silently never fired.
- **mobile `DiscoveredSession`** declared `cli_session_id`, `last_active_at`, and `provider` — none of which the wire sends (it sends `session_id` and `timestamp`, and no provider field). All three reads were permanently `undefined`; the Previous-Sessions screen keyed every card off `undefined`, expanding all of them at once and sorting by a field that was never present.
- **mobile `GitStatusEntry`** dropped the `additions`/`deletions` line counts the wire carries and mislabelled `status` as a porcelain code when the wire sends a word.
- **mobile `GitHubIssue`** declared `url`/`state`/`labels` slots the backend never serialised — the crash that produced a blank Issues page (#360).
- **desktop `Mesh`** declared 6 fields; the wire carries 14.

Each of these passed both `tsc` and `cargo` and would have continued to. Fixing them one at a time is whack-a-mole; the class needs a structural cut-off.

## Decision

Adopt **ts-rs** as the generator. On each wire struct/enum: add `TS` to the derive list and `#[ts(export, export_to = "Name.ts")]`. `cargo test` runs ts-rs's auto-generated `export_bindings_*` tests, which write the `.ts` files to `src/types/generated/` (dir set by `TS_RS_EXPORT_DIR` in `src-tauri/.cargo/config.toml`). The frontend imports the generated types; stores and the two API modules re-export them under the names call sites already use.

Conventions that make it safe:

- **64-bit ints get `#[ts(as = "i32")]`** (`Option<i64>` → `#[ts(as = "Option<i32>")]`). ts-rs defaults `i64`/`u64`/`usize` to `bigint`, but serde_json and `invoke` send them as JS numbers; the annotation makes the type say `number`. An un-annotated 64-bit field generates `bigint`, which fails the TS build — so the omission is caught, not shipped.
- **serde attributes are honoured** via ts-rs `serde-compat` (default on). The `SessionStatus` fix was to switch the enum to `#[serde(rename_all = "snake_case")]`, which both corrects the wire value and makes the generated union `"awaiting_input"`.
- **CI gate:** `.github/workflows/build.yml` now runs the frontend build, then `cargo test` (which regenerates bindings — and runs the Rust suite in CI for the first time), then `git diff --exit-code src/types/generated`. A Rust change not reflected in committed bindings is a red build.

Rolled out to the five types named in #359: `Mesh`, `AgentNode` (and the `EnvType`/`Provider`/`SessionStatus` enums it embeds), `GitHubIssue`, `DiscoveredSession`, `GitStatus`.

## Alternatives considered

- **specta + tauri-specta.** Generates fully-typed `invoke` wrappers (commands, not just types) — the richest IPC-contract fit. Rejected for this first cut: it adds a heavier macro layer and more version churn on top of the project's already-fragile Windows build, and it does not help the mobile HTTP transport, which is half the drift surface. ts-rs covers both transports because both serialise the identical Rust struct (verified: no `json!{}` remapping on either path).
- **Hand-maintained shared module + a locked field-set test** (issue #359 option 3). Cheapest, and closest to the project's existing "paired constants + a test on each side" convention for polyglot *defaults*. Rejected because TS interfaces have no runtime reflection: a test cannot enumerate an interface's keys, so it would need a hand-kept `['number','title',…]` array beside each type — a fourth copy to drift. That convention fits *values* (which each side may legitimately diverge on); type *shapes* are pure structure with a single correct answer, which is exactly what codegen is for.

## Consequences

- Wire-shape drift between Rust and TS is now a CI failure, not a runtime crash. Adding/removing/renaming a field on a generated struct forces the binding (and therefore every consumer) to update.
- The generated dir is committed and must never be hand-edited (files carry a "Do not edit" banner). Editing a wire type means editing the Rust struct and re-running `cargo test`.
- CI now compiles and runs the Rust test suite, which it previously never did.
- **Trade-off — widened unions lose TS-side narrowing.** `GitStatus.status` is a Rust `String` (the domain is `modified|added|deleted|renamed|untracked`), so the generated type is `status: string`, dropping the literal union the hand-written TS used to carry. A typo'd status now type-checks and renders no badge instead of erroring. The deeper fix is a Rust `enum` (or `#[ts(type = "...")]` literal union) at the source — a separate refactor, since `git.rs` builds the string from git2 status flags. Same applies to `Mesh.layout` (`'grid' | 'single'` → `string`).
- **Not yet migrated (follow-ups):** `src/lib/status.ts` holds a fourth, UI-config copy of `SessionStatus` (tolerant via `getStatusConfig`, missing `archived`); the `Diff*`/`FileNode`/`OpenPr`/`GitBranchStatus`/`GitSummary` types remain hand-written; and test fixtures (`tests/`) are outside the `tsc` `include`, so the new types don't yet guard fixture shapes (a factory or a tests typecheck step would close that). The natural next step is widening Rust `GitHubIssue` to actually serialise `html_url`/`labels` (the GitHub API already returns them; this is issue #358) so the generated type lights the mobile labels/link UI back up — the inverse of the band-aid this ADR removed.
