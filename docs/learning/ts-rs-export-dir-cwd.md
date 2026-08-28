# ts-rs export dir only applies when cargo's cwd is `src-tauri/`

Learned while regenerating `CircuitNodeKind.ts` for issue #1356.

`TS_RS_EXPORT_DIR` lives in `src-tauri/.cargo/config.toml` and points at
`../src/types/generated`. Cargo loads that file only when the process cwd
is `src-tauri/` (or a descendant). Two invocations look equivalent and
are not:

| Command | Where bindings land |
|---|---|
| `cd src-tauri; cargo test` (CI's `working-directory: src-tauri`) | `src/types/generated/` (committed) |
| `cargo test --manifest-path src-tauri/Cargo.toml` from the repo root (`scripts\check.ps1 rust`) | `src-tauri/bindings/` (gitignored fallback) |

Symptom: `export_bindings_*` tests pass, but `git diff src/types/generated`
is empty and the generated type is missing new fields. The tests wrote
the new file next to the crate, not next to the frontend.

Fix: regenerate with cwd `src-tauri/`. Do not copy out of `bindings/`
unless you have just confirmed that directory's timestamp is the run
you care about.
