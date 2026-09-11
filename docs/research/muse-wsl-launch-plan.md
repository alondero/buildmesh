# Muse on Windows: validation and implementation plan

Investigation date: 2026-09-11. Base: `6e3f703b5a2c75f6933650a3f7f27e53bcaa6bac`, branch `sleepy-adrift-coin`. Runtime: Windows, Ubuntu WSL, Muse Code `1.1.1 (1.1.1-R2514.1)`, WSL Git 2.43.0. The implementation described here is now present in this worktree, but is not yet committed or published.

## Recommendation

Keep the project's committed symlinks and canonical Claude context. Have Buildmesh restore exact Git symlink placeholders before launching Muse in WSL. This requires no project migration, copied context, synchronization script, additional project commits, or global Git configuration change. It does change two local checkout entries into the links Git already records.

Use Windows-compatible links and verify them from both runtimes. The handoff's unconditional assumption that WSL-created links are Windows-readable is too broad: on this machine they are native NT symbolic links, but Microsoft's tests distinguish NT and Linux-only link types. The local implementation uses native Windows link APIs and preserves the original files if eligibility or link creation fails. [Microsoft WSL filesystem tests](https://github.com/microsoft/WSL/blob/master/test/windows/DrvFsTests.cpp), [supporting investigation](muse-windows-support.md).

The zero-checkout-change alternative tested here uses private bind mounts. It starts Muse and exposes the instructions successfully, but creates a false modified `AGENTS.md` in Git's view inside Muse. That is a poor default for a coding agent, whose decisions depend on Git status. Keep it as research rather than treating startup alone as sufficient validation.

## Options compared

| Option | Executed result | Project impact | Decision |
| --- | --- | --- | --- |
| Guarded WSL symlink repair | Muse startup works; authenticated Meta response reads the context marker; Windows and WSL Git are clean; Windows reads both paths | Local link restoration only | Recommended, with Windows compatibility and concurrency safeguards |
| Windows-native symlink creation | Both file and directory link creation succeeded; Muse echo startup succeeds; Windows Git clean | Same local restoration; depends on host capability | Prefer this construction mechanism on Windows where available; keep filesystem ownership in Buildmesh |
| Private Bubblewrap bind-mount view | Echo startup, authenticated rule marker and skill-marker completion succeed; outer checkout stays clean; inner Git reports ` M AGENTS.md` | No original files changed, but agent sees misleading Git state | Not a transparent replacement; do not ship this prototype |
| `--no-foreign-personal-context` | Trusted startup still fails with `.agents/skills: Not a directory` | None | Does not address project context |
| Run workspace untrusted | Echo starts, with explicit warnings that project context and automatic delegation are skipped | None | Loses required functionality |
| Native Muse through Git Bash | Installed ELF fails with `Exec format error`; launcher supports Linux/macOS | None | Not supported by the inspected distribution |
| Committed regular mirrors, as PR #1700 proposes | Prior handoff reports authenticated and fresh-checkout success; not revalidated here | Every adopting project must maintain copies | Valid fallback, but does not meet the desired maintenance cost |
| Enable native symlink checkout globally / move all repositories into WSL | Not applied or validated as a migration | Changes machine-wide or workspace conventions; existing checkouts still need handling | Unnecessary for the recommended approach |

Official Meta material offers macOS/Linux installation. A Windows shortcut or launcher can invoke `wsl.exe`, but the executable and its shell tools still run in Linux. No supported native Windows package or documented project-context path override was found in the accessible primary sources. This is a statement about the inspected distribution, not a claim that private or future support is impossible. [Official announcement](https://research.meta.ai/blog/introducing-muse-code-and-muse-spark-1-2), [detailed native-runtime probes](muse-windows-support.md).

## Reproduction and evidence

Project-context mutations were confined to disposable fixtures under ignored `.tmp/muse-investigation/`. This investigation also created local research documents, test programs and Muse probe session records. Existing project context, installations, Git defaults, PR #1700, and issue #1697 were not changed. The fixture repositories have actual Git commits with mode `120000` for both entries, `core.symlinks=false`, and regular files containing exactly `CLAUDE.md` and `../.claude/skills`. This models the checkout behavior described by [Git's `core.symlinks` documentation](https://git-scm.com/docs/git-config#Documentation/git-config.txt-coresymlinks).

| Probe | Observed result |
| --- | --- |
| Original placeholder fixture, trusted `muse exec --provider echo` | Runtime host startup fails with ENOTDIR at `.agents/skills` |
| Same fixture, WSL-created relative links | `echo: PROBE_WSL_LINK`, exit 0 |
| Windows-created relative file/directory links | `echo: PROBE_WINDOWS_LINK`, exit 0 |
| Repaired fixture, authenticated `muse exec`, shell/write tools disabled | `CONTEXT_REACHED_7319`, exit 0; the marker is in canonical `CLAUDE.md` |
| Repaired fixture, `muse skills list --source project --trust-workspace --json` | Exit 0; exactly one active `portability-probe` skill at `.agents/skills/probe/SKILL.md`; no diagnostics |
| Production Buildmesh WSL wrapper and ConPTY spawn, prototype repair before Muse | Fresh headless session returns `echo: PROBE_RESUME_SEED`, child exit 0; repair messages precede runtime startup |
| Same production wrapper/PTY, placeholders recreated before resume | Real `muse resume <UUID>` restores the seed conversation and completes a new input with `echo: PROBE_RESUME_LIVE`; interactive child deliberately stopped after observing completion |
| Windows filesystem access to repaired fixture | Node reads `AGENTS.md` contents and enumerates `.agents/skills/probe`; PowerShell agrees |
| Link type on this machine | `fsutil reparsepoint query` reports native symbolic link tag `0xa000000c` |
| Windows and WSL `git status --porcelain` after repair | Empty; experiment backup files were moved outside each fixture repository before the final check |
| Independent repair eligibility suite | 12 cases pass, process exit 0; details below |
| Private bind-mount view, authenticated instruction probe | `CONTEXT_REACHED_7319`, exit 0 |
| Private bind-mount view, authenticated skill-reading request | Terminal completion `SKILL_REACHED_4281`, exit 0; this proves completion, not by itself successful execution of every requested shell action |
| Git inside versus outside private view | Inside: ` M AGENTS.md`; outside: empty, original placeholders unchanged |

An initial authenticated invocation through nested `bash -lc` returned an unrelated greeting rather than the requested marker. It is not counted as context evidence. The successful repeat passed the prompt directly through `wsl.exe --exec` to Muse. Production already uses positional shell arguments to avoid prompt interpolation; retain that behavior.

The private-view prototype uses Bubblewrap with the original filesystem bound into a private mount namespace, the canonical rules file bound over `AGENTS.md`, and a temporary `.agents` directory containing the proper skill link. It does not copy project source. Binding the rules file exposes file contents rather than the indexed symbolic link, which explains the inner Git mismatch. The small fixture also does not establish preservation of arbitrary additional `.agents` entries; a production view would have to preserve those. [Bubblewrap primary documentation](https://github.com/containers/bubblewrap), local `.tmp/muse-investigation/overlay.sh`.

The 12 safety cases cover independently authored exact pointer text committed as a regular file; changed pointer contents under a symlink index entry; a directory replacing a link; an existing unexpected symlink; missing targets; wrong target types for both paths; repeated repair for both paths; unresolved index stages; an unexpected committed target; and a symlinked `.agents` parent. The first suite invocation failed because its conflict fixture omitted a Git object; the corrected fixture suite passed without changing the repair prototype. [Safety details and limitations](muse-windows-support.md).

The Python repair is a disposable eligibility experiment, not the proposed deployment dependency. It uses Git index mode, stage, blob target, exact working-file bytes, parent type and target type checks, then a sibling symlink and rename. It still needs production hardening: regular-file target checks must reject special files, temporary names must be unique, concurrent changes must be fenced, and partial repairs and Git failures need structured reporting.

### Buildmesh boundary test and verification limits

The temporary Rust integration test called the actual Muse adapter recipe/resume methods, `spawn_environment::wrap`, `open_pty_pair` and `spawn_child`. It wrapped the recipe with the isolated Python preflight, rather than changing the shipped provider or application. Windows Git `checkout-index --force` recreated the two placeholders immediately before both fresh and resume launches. Both runs printed both repair messages before Muse initialized.

Command: `cargo test --locked --manifest-path src-tauri/Cargo.toml --test muse_wsl_investigation -- --ignored --nocapture --test-threads=1`, with the existing Windows Cargo target directory. Final result: **1 executed test passed**, command exit 0, test execution 3.45 seconds. Fresh Muse exited 0; the resumed TUI was stopped by the test after its new echo response appeared. Do not describe the interactive child as having exited naturally with status 0.

The first run failed after 60 seconds because text plus Enter in one PTY write was interpreted as pasted multiline input. It did restore the conversation, but did not submit the new turn. The corrected driver used bracketed paste and submitted Enter after observing the rendered input; it passed without changing repair or production logic. An unused test import was removed before the successful compilation. The final log is `.tmp/muse-investigation/pty-result.log`; the test source was moved to `.tmp/muse-investigation/muse_wsl_investigation.rs` after execution so the deliverable does not add an environment-specific test to the product suite.

The local implementation now performs the preflight in provisioning, and the direct production-wrapper test exercises that operation before a real WSL `muse exec` process. The earlier disposable PTY probe also established fresh and resume feasibility at the Windows-to-WSL process boundary. This does **not** establish Tauri command/DB orchestration, UI launch, linked-worktree repair, arbitrary project layouts, hosts without symlink privilege, or normal shell write operations. Authenticated rule reading and project skill discovery were validated separately from the echo PTY lifecycle test. The focused Rust tests, Clippy, agent check, and diff checks have been run; a full WebView2 UI smoke test remains outside this validation.

The checked-in Windows regression suite is `cargo test --locked --manifest-path src-tauri/Cargo.toml git::ai_context_runtime::tests::windows_tests --no-fail-fast`: ten tests pass and the live WSL test is ignored by default. The live test was executed separately with `--ignored --nocapture`; it passed by repairing a disposable Windows checkout, invoking the actual `spawn_environment::wrap` command builder used by Buildmesh, running `muse exec --provider echo`, and confirming both links plus clean Git status afterward. This is real harness startup evidence, not authenticated Meta inference; the authenticated marker result is recorded separately in the table above. The repository gate `scripts/check.ps1 rust -SerialRust` also passed; Clippy exits successfully and the repository emits warnings outside the touched module.

## Production ownership and ordering

The relevant existing code paths are:

- [Workspace provisioning](../../src-tauri/src/agent/spawn/provision.rs): completes worktree creation/adoption/reuse, then prepares cross-runtime Git metadata on the blocking pool, provider routing, trust and hooks.
- [Launch phase](../../src-tauri/src/agent/spawn/launch.rs): builds the process command and opens the PTY after provisioning.
- [Command composition](../../src-tauri/src/agent/spawn/command.rs): both fresh and resume session modes compose the provider's launch contribution and call the same environment wrapper.
- [Environment wrapper](../../src-tauri/src/agent/spawn_environment.rs): pins the WSL distribution and working directory, preserves argument boundaries, and crosses into the guest through `wsl.exe --exec sh -lc`.
- [Muse adapter](../../src-tauri/src/agent/provider/adapters/muse.rs): owns the `muse` binary recipe and `resume <UUID>` arguments.

The implementation performs a fallible preflight in provisioning, after cross-runtime Git preparation and before launch. It keeps filesystem I/O off the pure command composer and the async executor thread, and does not hold a database connection while inspecting Git. The final command wrapper retains its existing quoting and environment behavior. This slightly refines the handoff: being before the runtime host scans is necessary; embedding mutation in a shell string is not necessary.

## Detailed implementation plan

### 1. Retain canonical context and narrow the change

Start from this investigation base or the current integration base that still has the original links. Keep `AGENTS.md -> CLAUDE.md` and `.agents/skills -> ../.claude/skills`. Do not add committed mirrors, a sync command, or mirror drift tests. Preserve independently authored AGENTS files and skill directories.

Treat #1697 and #1700's mirror direction as superseded only when implementing the replacement: their current text explicitly asks for mirrors and would otherwise conflict with the new acceptance criteria. Update their scope and description together with the replacement implementation; this investigation has not edited either external object.

### 2. Add one bounded context-readiness operation

Place the Git inspection and restoration logic under the existing `git/` ownership boundary, for example `git/ai_context_runtime.rs`. Provide one operation returning per-path outcomes such as unchanged, repaired, preserved-with-reason, or failed. Keep it specific to the two established aliases initially. Invoke it for Muse guest launches; do not silently turn it into a general repair of every repository symlink or every provider.

Use the resolved host path for Windows filesystem/Git access. Use `env`'s existing path conversion and selected distribution for guest validation. Locate the actual Git worktree root, so a nested starting directory does not lead to testing the wrong `AGENTS.md`; never cross into unrelated ancestors merely because a pathname matches. Linked worktree `.git` compatibility must already be prepared.

### 3. Prove eligibility before touching either entry

For each candidate, require exactly one stage-zero index entry with mode `120000`. Its index blob must be the exact allowed relative target. Require the checkout entry to be a regular file containing exactly those bytes. Reject authored regular index entries, conflicts, directories, existing links/reparse points, differing bytes, submodules, and symlinked intermediate parents. Require the canonical target to exist and be a regular file for rules or a directory for skills; validate resolved paths remain within the intended workspace.

Already valid links, authored context, and absent aliases are no-ops; do not create context aliases in projects that never declared them or resurrect deleted files. Unknown malformed context remains untouched and receives an actionable diagnostic; do not claim success by dropping trust or disabling context discovery. Do not require the entire repository to be clean: unrelated user changes must remain usable and unchanged.

### 4. Restore links with rollback and host validation

Prefer explicit Windows-native relative file/directory symlink APIs on Windows storage, since these cannot silently substitute Linux-only links. The Windows-native construction probe succeeded on this host. If WSL is used for construction, validate the candidate from Windows before replacement. Do not assume Developer Mode or symlink privilege; do not enable it or change Git configuration automatically.

Create unique temporary candidates in the same filesystem and with the correct relative-target interpretation. Confirm target text and Windows/guest traversal before installation. Coordinate concurrent Buildmesh launches for the same worktree. Recheck original identity, bytes and index facts immediately before replacement; retain a recoverable original until both runtime checks pass. Detect an unexpected concurrent change and preserve it. A process-local lock alone does not protect against external editors or Git commands; the filesystem replacement strategy must retain any displaced file for inspection/recovery rather than discard it blindly.

If a post-install check fails, fail closed: never delete an installed path based on a separate pathname check. Leave an expected link, its backup and the transaction manifest for the next recovery pass; restore a backup only into an absent path. This preserves an external replacement even across a check/delete race.

After installing, verify both runtimes resolve the paths and compare the affected paths' Git status against their preflight state. Never use a blanket checkout/reset to hide a diff. On failure, restore only entries still identifiable as this operation's changes, preserve concurrent edits, and report the reason through the existing provider error path. If compatible links cannot be created, retain the placeholders and explain the capability requirement. The private mount prototype is not a silent fallback because its Git semantics are different.

Make interrupted replacement recoverable too: retain enough operation identity with each backup to recognize it on the next launch, and recover before attempting another repair. Fault-test termination between backup, replacement and validation. Do not delete an unmatched backup or assume it still belongs to the current index entry.

### 5. Cover all lifecycle entrypoints

Call readiness after workspace provisioning for fresh, resumed, restarted and warm-pool sessions, including root nodes using an existing checkout. Run before any Muse command that initializes the workspace runtime. Resume must repeat the check because a Git checkout between sessions can recreate placeholders. A successful previous launch is not a permanent cache of filesystem validity.

Keep the current provider recipe and `resume <UUID>` construction. Do not modify WSL session-home discovery, callbacks, trust semantics or prompt handling. Run the preflight on the blocking pool outside database connections and return an ordinary launch error when known incompatible context remains.

### 6. Add meaningful regression and live tests

Port the 12 eligibility cases to the owning Rust module using real Git index entries. Add staged deletion/rename, non-Git directory, nested cwd, special-file targets, target escape, existing reparse parent, symlink creation denied, temporary-name collision, simultaneous launches, external-file replacement, failure after one repair, and rollback that must preserve a concurrent edit.

The current Windows regression suite now explicitly covers cached-index refresh, missing skills targets, directory-symlink candidate cleanup after a failed pre-install, blocked rollback recovery with backup preservation, and concurrent launch serialization. The broader fault matrix above remains release work where the host does not provide a deterministic capability seam (for example denied symlink privilege and external replacement).

Exercise Windows Git checkout with `core.symlinks=false`, not only handwritten placeholder files. Assert unchanged index/tree IDs and unchanged unrelated edits; verify Windows and WSL status. Test a linked worktree through the existing cross-runtime preparation function.

At the production process boundary, assert fresh and resume commands receive the same prerequisite. Run real Muse with a fixture-only rule marker and skill, prove startup and authenticated context use, then recreate placeholders and resume the same session. Confirm read/write shell operations on ordinary fixture source files under Muse's normal sandbox policy. Include a host without symlink creation privilege in the release matrix; assert a preserved checkout and useful error, not a Linux-only link left behind.

### 7. Verify and deliver the replacement

Run focused Rust tests first, then `scripts/check.ps1 rust -SerialRust`, Clippy for touched Rust code, and `npm run check:agent -- --base <implementation-base>`. Run frontend checks only if the implementation changes UI/types; a real dev WebView2 fresh/resume smoke check is required before claiming the complete app workflow is validated. Keep compile-only evidence separate from executed tests and echo startup separate from authenticated inference.

Document the behavior as automatic restoration of committed context links for Muse WSL launches. Explain that no context is duplicated and that manual Muse invocations bypass Buildmesh's preflight. A future user-level Windows wrapper can call the same readiness operation, but should not be added as a second independent repair implementation.

## Release acceptance

An existing Windows project whose Git index declares these two legacy context aliases starts Muse through Buildmesh without a project commit or manual repair. Rules and skills load. Windows tools still traverse both entries. Windows and WSL Git show no new changes. Independent context and absent aliases are preserved. Resume repeats readiness after placeholder recreation. Unsupported host capability leaves the checkout intact and reports a clear error. The published change contains runtime support, not mandatory context mirrors.
