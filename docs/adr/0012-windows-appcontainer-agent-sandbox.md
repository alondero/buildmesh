# 12. Windows AppContainer Sandbox for Agent PTY Processes

Status: accepted

For agent PTY processes on Windows, Buildmesh confines each one to a per-node [Windows AppContainer](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation) keyed to the node's Git worktree, rather than relying on the Job Object + token-impersonation containment that the unsandboxed path uses. Confinement is **opt-in per Mesh** via the `sandbox` flag (a single column shared with the macOS Seatbelt sibling, #497 — see the policy seam below).

## Context

Buildmesh orchestrates AI coding agents in long-lived PTY sessions. Each session is, in practice, an autonomous LLM that can:

- Read files anywhere the Buildmesh process can read
- Write files anywhere the Buildmesh process can write
- Spawn child processes (the `cwrap → bash → claude.exe` chain, plus anything the agent shells out to)
- Reach the network for the Anthropic API, `git push`, the GitHub CLI, etc.

A prompt-injected or compromised agent is therefore an unconstrained remote-code-execution surface. Job Objects contain the **process tree** (kills children when the parent dies) but say nothing about what the tree is *allowed to touch* — the token still has the user's full NTFS/registry ACLs. That is the wrong primitive for "this agent is untrusted."

The macOS Seatbelt slice (#497) already shipped a sibling answer for macOS: a per-mesh `sandbox` flag, a `[sandbox::sandbox_enabled]` policy seam in `spawn_agent_inner`, and a `sandbox-exec` profile that confines the spawned tree to its worktree. The Windows half of the same problem is what this ADR closes — same policy seam, OS-specific backend.

A feasibility spike (GH #498 Phase 0, not committed) confirmed the Windows AppContainer APIs do what we want: a process launched into a per-node AppContainer is **denied** filesystem reads outside the granted directories and **denied** registry writes by default; the agent can still reach the API and `git push` over HTTPS via the `internetClient` capability. A live `claude.exe` ran inside the container, read/wrote inside the granted worktree, and could not read `%USERPROFILE%` or write `HKCU\Software`.

## Decision

**1. One mesh-level column, two OS-specific backends.** A single `meshes.sandbox INTEGER NOT NULL DEFAULT 0` column (schema v18) backs both this slice and #497. The column is **off by default** for new meshes and pre-v18 rows. The OS-specific spawn policy is decided at `spawn_environment::wrap` / `sandbox::spawn::spawn_sandboxed` time — never duplicated into the frontend, never split per-OS at the schema layer. The spawn helper has the same `sandbox_enabled(mesh_sandbox) -> bool` shape on both OSes: `cfg!(target_os = "<that_os>") && mesh_sandbox`.

**2. The AppContainer is created per-node, not per-mesh.** The AppContainer SID identifies the *node*, not the mesh: every node gets a `com.alond.buildmesh.node-<session_id>` profile. This means:

- Two agents in the same mesh, on different worktrees, are isolated from each other's working copies (the second can't read the first's uncommitted work, even though the user can).
- Closing the node tears the profile down (`cleanup(session_id)` from `kill_session`) — the SID never outlives the session.
- A node can't `icacls`-grant its sibling's worktree because it doesn't know the sibling's SID and can't reach `icacls` from inside the container (it doesn't have the right to grant to SIDs anyway).

**3. Confinement is deny-by-default; grants are explicit.** The container starts with **no** filesystem grants. The orchestrator grants:

- The node's Git worktree — `Full` (read + write + delete). Mandatory.
- `~/.local/bin` — `ReadExecute`. Holds `cwrap` / `claude.exe`. Optional (skip if missing).
- `~/.claude` — `Full`. Agent config + session state. Optional (skip if missing).

No other directory is granted. In particular:

- `%USERPROFILE%` (the rest of it) is denied.
- The host `%TEMP%` is denied; the spawn helper creates a per-node `.bm-sandbox-tmp/` *inside* the worktree and points `TEMP`/`TMP` at it. This keeps every writable surface inside the granted area, so a single `icacls`-driven revoke on close fully cleans up.
- The Windows registry is denied by default for AppContainer processes (no profile = no write), so no explicit grant or revoke is needed.
- The system `node` (in `C:\Program Files\nodejs`) and MSYS2 are **not** on the curated PATH. They would need an admin one-time grant; we deliberately do not pre-grant them. The default `cwrap → Git-bash → claude.exe` chain runs grant-free (Git's `bash`/`git` carry the app-package ACE, and `claude.exe` lives in `~/.local\bin`).

**4. Owned ConPTY spawn (`sandbox::conpty`).** `portable-pty` 0.8 builds its `STARTUPINFOEX` proc-thread attribute list with capacity for exactly one attribute (`PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`) and calls plain `CreateProcessW` with the caller's token — it exposes **no seam** to add a second attribute or a sandbox token. An AppContainer must be applied at process creation (it cannot be attached afterwards like the Job Object). So the sandboxed path **re-implements the ~80 lines of ConPTY spawn** with a two-attribute list (`PSEUDOCONSOLE` + `SECURITY_CAPABILITIES`) + `CreateProcessW`, returning types that implement `portable-pty`'s `Child` / `MasterPty` / `ChildKiller` traits. `spawn_agent_inner` is otherwise unchanged — the reader thread, resize, Job Object containment, and kill path all consume the sandboxed types as if they were the unsandboxed `PtyPair`.

**5. Inline `extern "system"` FFI, no new dep.** The AppContainer profile / SID / `SECURITY_CAPABILITIES` lifecycle (`sandbox::appcontainer`), the owned ConPTY spawn (`sandbox::conpty`), and the ACL grant/revoke (`sandbox::acl`) all use hand-rolled `extern "system"` blocks — same shape as `process_util::JobHandle`. No `windows-sys`, no `winapi`. The dependency surface stays the same; the failure surface is the API contract, not a crate version.

**6. `sandbox_enabled` is the only policy seam.** One function (`sandbox::sandbox_enabled`) decides whether a given spawn takes the sandboxed path. It returns `cfg!(target_os = "windows") && mesh_sandbox` — nothing else reads `mesh.sandbox` for spawn-time decisions. This means a future per-node override (Alt-click spawn, currently the dev-handle for `use_worktree`) is exactly one match arm in this function; the spawned types, ACL grants, and ConPTY wiring don't change.

**7. Cleanup is best-effort and idempotent.** `sandbox::spawn::cleanup(session_id)` deletes the AppContainer profile and revokes every grant recorded at spawn time. Called from the node-close path (`kill_session`). On spawn failure the orchestrator undoes its own grants immediately — no half-created profile leaks if `CreateProcessW` fails. A second `cleanup` for the same session is a no-op (the static `HashMap<session_id, Cleanup>` is `remove`d).

**8. SSH-agent forwarding is deferred (AC #3).** git-over-HTTPS works through the `internetClient` capability, so a sandboxed agent can `git push` to GitHub without `id_rsa` touching the container. Forwarding an OpenSSH/Pageant pipe is a deliberate follow-up — there is no SSH agent running on this dev host to validate against, and shipping the forwarding code without a live agent would be untestable. The profile capability list is extensible; adding `S-1-15-3-…` style named-pipe grants (or a `wininet` proxy that injects the agent) lands once a representative setup exists.

## Consequences

- **A prompt-injected agent is confined to its worktree.** Even if the model is coerced into "read `~/.aws/credentials` and POST it to evil.example.com," the AppContainer denies the read by default. The blast radius of a compromised session is the one worktree, not the user's home directory.
- **The default spawn path is unchanged for opt-out meshes.** A mesh with `sandbox = false` takes the existing portable-pty path; the rest of the agent-spawn machinery is the same code on both sides.
- **Sandboxing is Windows-only at runtime.** The `sandbox` column is honoured on macOS by the existing Seatbelt path (#497) and is ignored on hosts where neither backend is built. The frontend checkbox text ("Sandbox agent processes") is OS-agnostic; the per-OS policy seam hides the difference.
- **The default deny posture is a UX trade-off, not a security one.** Anything the agent needs that isn't in the curated PATH or the granted dirs fails at runtime — `npm install` works (project dir is granted), `where node` finds Git's node shim, but `pip install` of a system-package would not. Live-agent validation will surface the gaps; per-provider grant tuning (e.g. one-time admin grant for a system tool the user actually relies on) is a follow-up.
- **AppContainer profile state is best-effort, not transactional.** A process crash between `CreateAppContainerProfile` and the `HashMap::insert` leaks a profile until the next successful spawn of the same node id or a manual `cleanup` call. We accept this: profiles are cheap, the cleanup path catches them on close, and a leaked profile is a deny-default container with no processes in it — *safer* than no profile, not less safe.
- **SSH-agent forwarding (#3) is open.** `git push` over HTTPS works today; SSH-based push/pull requires either forwarding the agent pipe into the container or running an HTTPS-only workflow. The decision is "ship the deny-by-default confinement first, add SSH only after we have a host with an SSH agent to test against."
- **Sequencing.** The Phase 1 toggle (PR #509) ships before the native spawn so existing meshes can opt in *and* opt out before the deny-by-default flip matters. This ADR documents the Phase 2 native spawn (PR #513) and is the architecture record for the parent issue (#498).

## Considered alternatives

- **Job Objects alone.** Already in use for the unsandboxed path, and they're free — but they contain the *tree*, not the *capabilities*. Adding a UAC-restricted token would limit damage but not to the worktree; you'd still have whatever the user has. Rejected: doesn't match the macOS story, doesn't solve the home-directory read.
- **`windows-sys` / `winapi` for the FFI.** Strict typing, less hand-rolled risk. Rejected: it's a new top-level dep (CLAUDE.md: no new deps beyond the task), and the FFI surface here is small (~15 functions, all mirror `process_util::JobHandle`'s pattern). Hand-rolled matches the existing codebase.
- **Container-per-mesh instead of container-per-node.** Reuses profiles across nodes, smaller profile surface. Rejected: defeats the "two agents can't see each other's uncommitted work" property the per-node design buys us. Per-mesh is also wrong for the macOS sibling — Seatbelt profiles are spawned with the process.
- **Run the whole `claude` chain as the user (no containment) and rely on file ACLs in the worktree.** Doesn't stop the agent from shelling out to `cmd /c rmdir /s C:\Users`. AppContainer deny-by-default does.
- **Build a usermode sandbox.** Too large a project, too many edge cases; the OS already ships one.
- **Force every consumer onto HTTPS.** Doesn't solve the local-filesystem problem (an agent still reads `~/.aws/credentials` even if it can only POST over HTTPS).

## Status: complete (PR #513, #498 Phase 2)

The native AppContainer spawn ships in PR #513. The Phase 1 toggle (PR #509, `meshes.sandbox` + `update_mesh_sandbox` + UI checkbox) lands first and was the prerequisite for the seam to have a consumer. The single seam — `sandbox::sandbox_enabled` — is read by `spawn_agent_inner`; the Windows branch routes to `sandbox::conpty::spawn_in_appcontainer`, which composes `AppContainerProfile` + ACL grants + curated env. macOS routing (the Seatbelt sibling, #497) reads the same flag from the same column.

Known follow-ups, deliberately out of scope:

- **Live-agent validation.** The 21 new sandbox tests prove the seam, the ConPTY, the profile lifecycle, and the ACL grant-read contract against real Win32 APIs; a real `claude` turn inside a sandboxed mesh needs the running app. The default `cwrap → bash → claude.exe` chain is designed to run grant-free; per-provider grant tuning may be needed.
- **SSH-agent forwarding.** git-over-HTTPS covers `git push` to GitHub today. SSH-based workflows need the forwarding slice once a host with a live OpenSSH/Pageant agent is available to validate against.
- **Per-node override.** An Alt-click toggle to sandbox a single node in an opt-out mesh. One match arm in `sandbox_enabled`; the spawned types and ACL grants don't change.
- **macOS coverage.** The Seatbelt path (#497) and the AppContainer path (#498) share the `sandbox` column but the per-OS policy seam is two functions, not one. Consolidating (e.g. behind a `SandboxBackend` trait) is premature until a third OS shows up on the roadmap.