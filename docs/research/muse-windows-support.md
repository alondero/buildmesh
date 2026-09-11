# Muse Windows support and configuration alternatives

Investigated 2026-09-11 against installed Muse Code `1.1.1 (1.1.1-R2514.1)`.

## Finding

The available Muse distribution requires Linux or macOS. Running it through WSL from a Windows application is viable; invoking the installed executable through Git Bash is not. A launch-time solution for Windows Git symlink placeholders is more promising than moving project context into Muse-specific copies.

Meta's official launch announcement explicitly offers installation on macOS or Linux. The installed first-party launcher implements exactly four platform selections: macOS ARM64/x86-64 and Linux ARM64/x86-64. Every other `uname` result takes its unsupported-platform branch. These facts establish support for this distribution; they do not prove that Meta has no internal Windows work. [Official announcement](https://research.meta.ai/blog/introducing-muse-code-and-muse-spark-1-2), installed `/home/alond/.local/bin/muse`, `detect_platform()`.

## Executed evidence

All commands below used the existing binary directly where practical, avoiding launcher update behavior. No credentials were read or printed, and no installation or project context files were modified.

| Check | Result |
| --- | --- |
| `file /home/alond/.local/bin/muse-bin-1.1.1-R2514.1` inside Ubuntu WSL | Statically linked x86-64 ELF executable, not a Windows PE executable. |
| Windows Git Bash executes `//wsl.localhost/Ubuntu/home/alond/.local/bin/muse-bin-1.1.1-R2514.1 --version` | `cannot execute binary file: Exec format error`. |
| WSL binary `--version` | `Muse Code 1.1.1 (1.1.1-R2514.1)`. |
| `skills list --source project --workspace <worktree> --trust-workspace --json` | Exit 1, `.agents/skills: Not a directory (os error 20)`. |
| `exec --provider echo --no-session-log --no-foreign-personal-context --workspace <worktree> --trust-workspace PROBE` | Exit 1; runtime host startup fails with the same ENOTDIR error. |
| `exec --provider echo --no-session-log --workspace <worktree> PROBE` | Exit 0, `echo: PROBE`; explicitly skips project rules and skills because the workspace is untrusted; auto delegation unavailable. |

The worktree was `/mnt/f/src/buildmesh/.claude/worktrees/sleepy-adrift-coin`. Its existing Windows Git symlink placeholders were left intact. Echo mode is startup evidence only, not an authenticated model completion or filesystem-tool validation.

## Configuration possibilities

The installed `muse --help` and `muse exec --help` expose workspace and trust controls but no switch to replace the project rules filename or project skill search path. `--no-foreign-personal-context` concerns foreign personal rules/skills; the live failing probe confirms it does not bypass the project `.agents/skills` placeholder. `skills list` and `skills inspect` expose source filtering, but this is a catalog-query option, not an advertised runtime discovery override. Source: installed binary command help and executed probes above.

`muse skills import --from claude|codex [--scope user]` is advertised for user-level imports. That does not resolve a malformed project skill location, and making every project's context personal would change scope and precedence. No import was executed. Source: installed `muse skills --help`.

An untrusted run can bypass the crash, but the observed warnings show why this does not meet the intended behavior: project guidance and skills disappear, and automatic delegation is unavailable. Adding the project guidance to the prompt cannot restore skill discovery or trust semantics. Source: executed untrusted echo probe.

Binary strings contain skill activation/configuration and enterprise policy concepts, but those are not sufficient evidence for a supported path override. No undocumented environment flag or global configuration change was used. The public developer documentation URL tested returned a login-required page, so absence from accessible documentation is not proof that no private option exists. [Developer documentation endpoint tested](https://dev.meta.ai/docs/muse-code).

## Implications for the implementation plan

1. Keep the Windows UI and orchestrator, execute Muse in WSL, and solve placeholder handling at the launch boundary.
2. Do not describe a `.cmd`/PowerShell wrapper around `wsl.exe` as native Windows support: the harness and its shell tools still run on Linux.
3. Do not choose Git Bash/MSYS as a replacement runtime for the available ELF binary; the actual execution check failed.
4. Do not use untrusted mode or personal skill imports as a project portability fix; they change the context available to the agent.
5. Retest CLI discovery behavior when updating Muse. A future documented project-context override or native Windows package could change the best option.

The main investigation owns the comparative filesystem experiments and Buildmesh fresh/resume launch analysis; this note only establishes Windows runtime and configuration evidence.

## Independent prototype safety checks

The follow-up `.tmp/muse-investigation/safety_probe.py` exercised the main investigation's unchanged `repair.py` in twelve independent repositories under `.tmp/muse-investigation/safety/39af575f8ce945cfacb5265663478b3d`. Final execution: **12 cases passed, 0 failed, exit 0**.

Preservation cases: authored exact pointer indexed as `100644`; modified placeholder indexed as `120000`; existing directory; existing unexpected/dangling symlink; missing target; rules target of directory type; skills target of file type; three unmerged index stages; unexpected indexed target; symlinked `.agents` parent. Successful repair and repeat idempotence were checked separately for rules and skills. The first suite attempt stopped after ten passes because the fixture builder referenced a blob absent from that fixture's object database; the fixture was corrected and all twelve cases reran. This was a fixture error, not a passing repair test.

These cases validate the prototype's preservation decisions, not production readiness. Inspection identified: no check that a rules target is specifically a regular file (a special non-directory target passes its current predicate); predictable sibling temporary name and no collision recovery; no concurrency lock or final filesystem/index revalidation; unstructured failure on Git errors; and possible partial repair before a later failure. The production implementation must address these separately. No claim of race safety follows from the sequential fixture suite.

Windows `fsutil reparsepoint query` on the repaired rules fixture reported native symbolic-link tag `0xa000000c`; Windows `Get-Content` read `canonical rules`. Thus WSL-created relative links were Windows-readable on this machine. Do not generalize that result to every host: Microsoft's current WSL tests distinguish NT and Linux symlinks and explicitly test VM-dependent behavior. Its historical release notes describe conditions under which DrvFs creates native links and the Linux-link fallback. [Current Microsoft WSL tests](https://github.com/microsoft/WSL/blob/master/test/windows/DrvFsTests.cpp), [official release notes, Build 17046](https://github.com/MicrosoftDocs/wsl/blob/main/WSL/release-notes.md).

Git documents that `core.symlinks=false` checks symlinks out as small plain files containing the link text while preserving the recorded symlink type. That supports using the index mode and indexed blob as independent evidence before touching a placeholder; clean status on both Windows and WSL must still be validated on the actual replacement type. [Official Git configuration reference](https://git-scm.com/docs/git-config#Documentation/git-config.txt-coresymlinks).
