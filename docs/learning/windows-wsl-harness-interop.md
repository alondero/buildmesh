---
name: windows-wsl-harness-interop
description: Windows and WSL harness interoperability feasibility and live evidence
metadata:
  type: reference
  date: 2026-09-10
---

# Windows and WSL harness interoperability

Investigation base: `9fe9115b42492e5c5f15bc57fe4e9b62cfb11df0`.

Both directions are technically possible. A mesh's repository location need not determine its harness runtime. Windows Buildmesh can run a Linux harness through `wsl.exe` against a mounted Windows repository. A Windows harness can access a WSL repository through its Windows host path. A Linux Buildmesh process inside WSL can invoke Windows executables when interoperability is enabled. This conclusion combines Microsoft's documented process/file interoperability with the live probes below; it does not certify every harness feature. [Microsoft filesystem interoperability](https://learn.microsoft.com/en-us/windows/wsl/filesystems)

## Platform boundaries

| Concern | Findings and implication |
|---|---|
| Launch | WSL accepts an explicit distribution and working directory. Discover installed distributions; keep the distribution used for discovery, launch, and session storage consistent. [WSL commands](https://learn.microsoft.com/en-us/windows/wsl/basic-commands) |
| Arguments | WSL passes arguments without converting embedded paths. Windows processes require Windows path arguments; Linux processes require Linux paths. Windows batch entrypoints require a Windows command interpreter. [Filesystem interoperability](https://learn.microsoft.com/en-us/windows/wsl/filesystems) |
| Mounts | `/mnt/` is configurable, so runtime `wslpath` translation is preferable to assuming the default mount root. Windows access to Linux storage requires the correct distribution. [WSL configuration](https://learn.microsoft.com/en-us/windows/wsl/wsl-config) |
| Environment | `WSLENV` bridges explicitly named variables. `/p` translates a path; `/u` selects Windows-to-WSL propagation; `/w` selects WSL-to-Windows propagation. Preserve existing entries. Runtime home/configuration paths must follow the selected installation. [Filesystem interoperability](https://learn.microsoft.com/en-us/windows/wsl/filesystems#wslenv-flags) |
| Reverse discovery | WSL configuration can disable Windows interoperability or omit Windows PATH entries. A Linux binary detection result alone cannot establish availability of a Windows installation. [WSL configuration](https://learn.microsoft.com/en-us/windows/wsl/wsl-config#interop-settings) |
| Callbacks | Default WSL2 NAT requires Linux clients to reach Windows servers through the host IP; Windows loopback-only servers are insufficient. Mirrored networking permits IPv4 localhost both ways. Hook URLs and local provider proxies must use a reachable endpoint. [WSL networking](https://learn.microsoft.com/en-us/windows/wsl/networking) |
| Terminal | ConPTY provides UTF-8 input/output hosting for console processes. Launch success does not prove interactive rendering, resize, Ctrl+C, or descendant cleanup; those need real PTY tests with the selected harness. [Pseudoconsoles](https://learn.microsoft.com/en-us/windows/console/pseudoconsoles) |

Cross-filesystem workloads may be slower; filesystem case rules also differ. [Filesystem interoperability](https://learn.microsoft.com/en-us/windows/wsl/filesystems)

## Git worktrees are a separate compatibility boundary

Do not alternate shared `.git` metadata between Windows-absolute and Linux-absolute paths when two runtimes need the same worktree. The repository's existing `sanitize_git_worktree` rewrites both the forward `.git` link and admin `gitdir` backpointer for one environment. Runtime selection therefore needs a deliberate metadata policy. [Current implementation](../../src-tauri/src/git/worktree/mod.rs)

Modern Git supports `worktree --relative-paths`; enabling relative worktrees also enables `extensions.relativeWorktrees`, making the repository incompatible with older Git clients. The `commondir` file already supports relative paths. [Git worktree manual](https://git-scm.com/docs/git-worktree), [Repository layout](https://git-scm.com/docs/gitrepository-layout)

The installed Windows Git, including `C:/Program Files/Git/cmd/git.exe`, is `2.30.0.windows.2`; Ubuntu Git is `2.43.0`. In an isolated `.tmp/interop-research-probe` repository:

- A relative forward `.git` link allowed `git status --short` to succeed from both Windows and Ubuntu.
- Replacing the admin `gitdir` backpointer with a relative path made **both clients** report the live worktree removable with `git worktree prune --dry-run --verbose`. Windows `worktree list --porcelain` printed the unresolved relative path.
- Keeping a Windows-absolute backpointer also made Ubuntu's dry-run prune consider it missing.

No actual prune was performed. Git 2.43's `get_linked_worktree` reads the backpointer literally rather than resolving it against the admin directory, matching the experiment. [Git 2.43 worktree source](https://github.com/git/git/blob/v2.43.0/worktree.c)

Buildmesh's lockfile selects `libgit2-sys 0.18.7+1.9.6`. The locally installed vendored `libgit2/src/libgit2/repository.c` includes `relativeworktrees` in `builtin_extensions`; `worktree.c` resolves relative link paths. Thus bundled libgit2 support exists, but does not fix old external Git clients. [Lockfile](../../src-tauri/Cargo.lock), [libgit2 worktree source](https://github.com/libgit2/libgit2/blob/main/src/libgit2/worktree.c)

Implementation options inferred from that evidence: require compatible Git before using the full relative format, or retain a host-owned absolute backpointer with a relative forward link and an explicitly owned worktree lock protecting against foreign-runtime pruning. The latter also needs lock-aware cleanup and does not give old guest Git correct worktree administration. Never silently enable an incompatible repository extension.

## Muse primary evidence

Ubuntu contains `/home/alond/.local/bin/muse`. Live `muse --version` reports `Muse Code 1.1.1 (1.1.1-R2514.1)`. The following contract comes from that installed binary's `--help`, `resume --help`, and `exec --help`, not a third-party guide:

| Capability | Observed contract |
|---|---|
| Fresh interactive session | `muse [OPTIONS] [PROMPT]` |
| Resume | `muse resume <session-uuid>`, `muse resume --last`, or a workspace session picker with no argument |
| Resume options | Root options may appear on either side of `resume`; a follow-up positional prompt is not documented |
| Workspace | `--workspace PATH`; harness-managed worktree mode defaults to `off` |
| Model | `--model`, `--reasoning-effort` |
| Permissions | `--approval-mode`, `--permission-profile`, `--trust-workspace`, `--yolo`; the latter disables approvals and sandboxing |
| Headless | `muse exec --json`; `--session-id UUID` is documented for exec, not the interactive root |

The public Meta documentation URL returned a login page during this investigation, so no claim of official Windows or WSL support is inferred from it. [Meta documentation](https://dev.meta.ai/docs/muse-code)

### Muse session identity metadata

Follow-up inspection of the installed 1.1.1 CLI and local metadata established:

- Interactive root `--session-id UUID` is rejected with `unexpected argument '--session-id'`; it cannot assign IDs like the headless command.
- Session logs live at `~/.local/share/muse/sessions/YYYY/MM/DD/<UUID>/session.jsonl` on the observed Ubuntu installation.
- A JSONL line can be a direct event or a retained-frame envelope. The first observed permission envelope has string marker `retained_frame: "session_permission_transaction"` and a **top-level** `children` array; parse each `children[].record_json` string as a JSON object. The observed session metadata event is a **direct top-level record**, with `payload_type: "runtime.session.metadata"`, session UUID at `stream.id` with `stream.kind: "session"`, full working directory at `payload.record.workspace_root`, and timestamp at `recorded_at` (integer microseconds). It follows initial permission records, so reading only the first record misses it. A reader must handle direct events as well as envelope children.
- `session.opened.observed` also supplies `payload.record.session_id` and `recorded_at`, but not the workspace path.
- `~/.local/share/muse/session-index.db` has a `sessions` table with `session_id`, `session_log_path`, `workspace_root`, `workspace_key`, `created_at_us`, and `updated_at_us`. The existing observed row had all four workspace/time fields null. The JSONL metadata supplies the missing workspace and timestamp; capture must not require the index fields to be populated.
- The live registry `~/.local/share/muse/runtime/muse/sessions/<UUID>.json` contains `session_id`, `workspace_label` (a basename, not a full path), and `process_generation_hint`, but no full workspace path or creation timestamp.

Only metadata fields and CLI help/parser behavior were inspected; transcript text and credential values were not collected. This is an observed version-specific storage contract, not a published API guarantee. Resume help advertises UUID/`--last` plus root options; a follow-up prompt on the resume command remains undocumented.

Read-only platform probes succeeded:

```text
wsl.exe -d Ubuntu -- wslpath -u F:/src/buildmesh
/mnt/f/src/buildmesh

wsl.exe -d Ubuntu -- bash -lc 'cmd.exe /c ver'
Microsoft Windows [Version 10.0.28120.2912]
```

A live Muse headless echo probe from `/mnt/f/src/buildmesh` reported that workspace root and completed with `echo: interop-probe`. It used `--provider echo --no-session-log --disable-shell --disable-write --disable-web-tools --no-foreign-personal-context --max-model-steps 1 --json`. JSON also contained child-task failures because the echo provider does not support base instructions. This proves startup, Windows repository access, and the echo path; it is not evidence of authenticated Meta inference, interactive PTY behavior, or successful child agents. No paid model request or login was performed.

## Integration implications

The selected harness installation must own executable resolution, launch shell, environment variables, authentication/configuration homes, session discovery/resume, and callback addressing. Host file access must continue through `env::to_host_path`; repository/worktree ownership should remain separate from process runtime. Detection-only changes cannot provide those guarantees. These are design conclusions from the boundaries above and the existing [host-path module](../../src-tauri/src/env/host_path.rs) and [spawn wrapper](../../src-tauri/src/agent/spawn_environment.rs).

## Linux Buildmesh in WSL launching Windows harnesses

This reverse direction means the Buildmesh backend itself is a Linux process, not merely a Windows backend opening a WSL mesh. Windows execution through WSL interoperability remains possible, but host filesystem operations must now receive Linux-accessible paths while the launched harness receives Windows paths. Microsoft documents Windows executable invocation and unmodified argument passing from WSL. [Filesystem interoperability](https://learn.microsoft.com/en-us/windows/wsl/filesystems)

Live read-only probes on Ubuntu WSL2 established:

- A Linux Python process successfully launched Windows PowerShell. Adding `BUILDMESH_INTEROP_PROBE/w` to its child `WSLENV` propagated an inert test value. Adding `BUILDMESH_INTEROP_PATH/pw` converted `/mnt/f/src/buildmesh` to `F:\src\buildmesh` inside Windows. This confirms the reverse environment direction and path flag in the real subprocess boundary.
- Querying only Windows `USERPROFILE` returned `C:\Users\alond`; `wslpath -u` mapped it to `/mnt/c/Users/alond`. Windows `.claude`, `.codex`, and `.grok` directories were accessible from Linux. No configuration or credential contents were read. Implementation should resolve the Windows user's home through Windows, then convert it for Linux-side session/configuration I/O, rather than using Linux `HOME` for Windows installations.
- A temporary Linux HTTP server bound only to `127.0.0.1` on an ephemeral port was reached by Windows PowerShell with an exact generated nonce match. The listener was then stopped. This verifies Windows-to-Linux localhost callback reachability on this machine, consistent with Microsoft's documented default forwarding. [WSL networking](https://learn.microsoft.com/en-us/windows/wsl/networking)
- A Windows PowerShell process started from Linux `/home/alond` reported the matching WSL UNC working directory. `cmd.exe /c cd` from the same location instead warned that UNC current directories are unsupported and fell back to `C:\Windows`. Batch harness launch therefore requires an explicit solution such as `pushd` mapping; inherited cwd is insufficient.

Callback availability and ownership are separate. A Windows Buildmesh instance and a Linux Buildmesh instance can have different listeners; a reachable port number does not establish the intended server identity. The actual runtime port and authentication/correlation values must be propagated, and a connectivity check should verify the intended server rather than accepting any listener. This is an inference from the separate runtime boundary and Buildmesh's existing fallback-port allocation in [HTTP server code](../../src-tauri/src/http/mod.rs).

For a Linux-owned Git worktree, reverse the ownership policy discussed above: Linux filesystem operations and its Git administrator need Linux-resolvable metadata; Windows harnesses still need a portable forward gitfile. A Linux-absolute admin backpointer is not understood by older Windows Git administration. Relative backpointers require compatible Git clients, or an owned lock and host-only administration policy. The previous old-Git dry-run prune evidence applies symmetrically; changing the executing harness must not rewrite shared metadata into its private path syntax.

### Linux build validation prerequisites

Initial Ubuntu checks found `pkg-config`, GCC/G++, Node/npm, and WSLg, but no Rust toolchain at `~/.cargo/bin` or on the login PATH. `pkg-config` could not resolve `gtk+-3.0`, `webkit2gtk-4.1`, `javascriptcoregtk-4.1`, or `libsoup-3.0`. Therefore full Linux Tauri validation initially lacked build dependencies, not merely a display.

The authorized validation setup installs a minimal official Rust stable toolchain and Ubuntu development packages `build-essential`, `pkg-config`, `libssl-dev`, `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, and `patchelf`. Tauri's official Debian prerequisites identify the corresponding WebKitGTK, compiler, OpenSSL, indicator, and SVG development dependencies. [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)

Rust installation was verified directly as Cargo `1.98.1` and rustc `1.98.1` under `/home/alond/.cargo/bin`; it used rustup's minimal profile and did not modify shell PATH. Installation logs are in ignored `.tmp/wsl-rust-install.log` and `.tmp/wsl-tauri-deps-install.log`. Linux cargo validation should explicitly add that bin directory and set a Linux `CARGO_TARGET_DIR`, for example under `/tmp`, instead of sharing Windows build artifacts. No distro defaults, interoperability configuration, or user credentials were changed.

System dependency installation completed successfully. A subsequent `pkg-config --modversion` resolved GTK `3.24.41`, WebKitGTK/JavaScriptCoreGTK `2.52.6`, libsoup `3.4.4`, OpenSSL `3.0.13`, Ayatana indicator `0.5.90`, and librsvg `2.58.0`; `patchelf --version` reported `0.18.0`. The prerequisite layer is ready for a Linux build; these checks alone do not establish that Buildmesh compiles or its tests pass on Linux.


## Implemented scope

Windows Buildmesh discovers native installations and executable-backed installations in its default WSL distribution. Explicit Windows/WSL profiles keep execution runtime separate from mesh storage. WSL profiles retain the discovered distribution name; selecting an installation from another default distribution requires changing the default and restarting, rather than silently substituting an installation. This implementation does not enumerate nondefault distributions. A Linux-hosted Buildmesh inside WSL discovers Windows executables through PowerShell and launches them through Windows interoperability, translating host access and process paths separately.

The shared provider menu feeds desktop, mobile, and automation entrypoints. Menus prefer a currently installed native executable and show a foreign installation only when absent natively. Stored profiles are retained for old sessions, while current detection filters stale automatic menu entries. Canonical Circuit harness ids resolve the preferred installation. Node creation/provider changes persist runtime before database writes, and host path resolution remains independent of preferences. Session readers and capture use runtime homes. Muse adds fresh prompts, model/extra arguments, metadata-based session identification, and UUID resume. Muse transcript rendering and turn-completion callbacks remain unavailable.

Guest launches use `wsl.exe --exec`, with a login-shell PATH and positional arguments. A real probe demonstrated that plain `--` allowed the default zsh to reinterpret embedded quotes, `$HOME`, and backticks; `--exec` preserved them literally. Cross-runtime worktrees use relative forward links and locked host administration, with preparation on the blocking pool. Windows-side Git must trust WSL repositories; the Windows CLI must support network-share paths. Tests isolate fixture trust rather than changing the user's Git configuration.

Windows-hosted shell attention hooks prefer Windows curl for NAT-safe host loopback; Windows hooks under a Linux-hosted backend explicitly call curl in the owning WSL distribution. Grok uses native HTTP and therefore requires mirrored networking for attention callbacks. The Windows process sandbox rejects WSL launches because it cannot contain the guest process.

## Verification evidence

- Installed Muse Code 1.1.1: CLI help/parser and metadata schema checked; headless echo startup accessed the Windows repository. Authenticated Meta inference and interactive resume were not exercised.
- Real ConPTY test: Windows-to-WSL cwd, callback environment, and literal multiline arguments passed.
- Real default-distribution discovery: found Muse and verified cached mount conversion against `wslpath`.
- Real Git test: Windows and WSL Git both read worktrees on Windows storage and WSL storage; Buildmesh cleanup succeeded. WSL ownership trust was confined to a temporary test configuration.
- Real dev-profile WebView2: backend `list_providers` returned `Meta Muse (WSL: Ubuntu)` and explicit Windows profiles; the Windows mesh provider menu displayed Muse. Inspected screenshot: `.tmp/interop-menu-after.png` (local, unpublished). No agent was launched through that UI check.
- The initial frontend screenshot fixture failure was also reproduced at the investigation base. The fixture expected a failed Circuit run in Activity; selecting History repaired it, and the complete integration suite then passed.

- Real reverse ConPTY test: a Windows `.cmd` harness (script path containing spaces) launched through the production wrapper and wrote its output into a WSL-backed working directory.

Final checks on Windows:

| Check | Result |
|---|---|
| `scripts/check.ps1 rust -SerialRust` | 3,103 library tests + 18 integration tests passed; 18 ignored including the doc test. |
| `scripts/check.ps1 all-ts` | Desktop/mobile builds, 3,008 unit tests, 63 integration tests, agent checks, README checks, and bundle budget passed; one unit test skipped. |
| `cargo test --locked --manifest-path src-tauri/Cargo.toml live_wsl_ -- --ignored --nocapture --test-threads=1` | Four real Windows/WSL checks passed, including reverse `.cmd` launch. |
| `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets` | Completed; warnings remain on code outside the edited lines. |
| `npm run check:agent -- --base 9fe9115b42492e5c5f15bc57fe4e9b62cfb11df0` | Passed. |

Rust export tests regenerated the wire types, including previously stale generated preference/Circuit fields. `git diff --check` passes for authored files; ts-rs emits trailing spaces on wrapped generated fields. The new ignored reverse-launch test was added after the full Rust run, then compiled and executed in the four-check live run. Changes remain local; nothing was published.

## Reverse-host and menu follow-up

The Linux backend now distinguishes Windows interoperability from its legacy native-host runtime. Windows PowerShell resolves Windows binaries, `cmd` uses `pushd` for UNC workspaces, and host file readers translate Windows paths back into the current Linux distribution. Foreign-distribution UNC paths are not silently treated as local. Windows callbacks explicitly re-enter the owning distribution; a real test delivered Unicode JSON to a Linux listener. Workspace trust records the Windows process path in the Windows configuration home.

Automatic menu rows are filtered against executable observations from startup, then selected native-first. Historical preferences remain available for existing sessions. Windows npm shim directories are excluded from Linux-native detection and the native child PATH. Windows Command Code discovery probes `cmdc`, not the operating system's `cmd.exe`.

Reverse Windows discovery reads WSL's `/proc/mounts` once and accepts both
native `drvfs` entries and WSL's `9p` entries tagged with `aname=drvfs`; it
retains the mount point reported by WSL (including configured roots such as
`/drives/c`) and no longer launches one `wslpath` process per drive. Guest-home
probes emit a Buildmesh marker so login banners cannot become part of the
cached path. Codex state discovery follows the selected runtime's effective
home: guest-side `CODEX_HOME` is read inside WSL, while Windows interoperability
probes the Windows-side override; neither runtime inherits the other process's
host variable through `WSLENV`.

Follow-up checks:

- Windows full Rust suite: 3,106 library tests and 18 integration tests passed; 18 library tests and one doc test ignored. A stale temporary-repository collision was resolved by using a fresh temporary root outside the checkout.
- Desktop/mobile builds, 3,008 frontend unit tests and 63 integration tests passed; one unit test skipped.
- Windows Clippy completed with the same 13 library / 40 test warnings on unchanged lines. Agent diff checks passed.
- Real Linux backend PTY checks passed for Windows PowerShell and batch processes, each on Linux storage and Windows storage, including session environment propagation. Real Windows-to-Linux callback delivery passed with Unicode JSON.
- The broad Linux suite exposed existing platform failures. An isolated archive of `9fe9115b42492e5c5f15bc57fe4e9b62cfb11df0` reproduced ten remaining failures: OpenCode's filesystem-error whitelist, classifier child-output cleanup, a Windows launcher assertion, a file-manager fixture path, Unix backslash traversal, an HTTP listener race, CommandCode's separator assertion, and three Windows session-naming fixtures. These are not claimed green. Compiler PATH was pinned to the real Linux compiler because the user's `cc` command is a harness launcher.

Detailed local logs are under `.tmp/reverse-*`; nothing was published.

The final Windows dev-profile backend and visible menu were checked over WebView2 CDP: exactly one `mcode` / **MiniMax Code** entry appeared, and **Meta Muse (WSL: Ubuntu)** was the only WSL addition. Screenshot inspected locally: `.tmp/interop-menu-deduplicated.png`. No paid harness was launched by this check. The final targeted Linux run passed 26 Cursor tests and 14 workspace-trust tests, including the two new portable regressions added after the Windows full run.
