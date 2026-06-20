# 14. Pivot the Windows agent sandbox off AppContainer to a restricted-token primitive

Status: proposed (supersedes the primitive choice in [ADR-0012](0012-windows-appcontainer-agent-sandbox.md); the policy seam, per-node model, and ACL grant story carry over unchanged)

The Windows agent sandbox should confine each agent PTY process with a **restricted / low-integrity token + Job Object + worktree ACL grant**, applied through the existing owned-ConPTY spawn seam, rather than a per-node **AppContainer**. AppContainer's private object namespace and named-pipe denial are **fundamentally incompatible with `claude.exe`**, and that incompatibility is not fixable with grants or capabilities.

## Context

ADR-0012 shipped a per-node Windows AppContainer (#498 Phase 2, PR #513) as the confinement primitive: a process launched into a `com.alond.buildmesh.node-<session_id>` AppContainer, denied filesystem access outside the granted worktree, reaching the network via the `internetClient` capability. Its Phase 0 feasibility spike confirmed a live `claude.exe` *ran* inside the container and could not read `%USERPROFILE%`.

Live validation (the follow-up the ADR-0012 left open) then surfaced two **independent, fatal** blockers, both core to how Claude Code works:

1. **Child-process spawn hangs (#528).** A non-invasive `cdb` thread dump of a hung sandboxed `claude.exe` showed the main thread spinning in:
   `ntdll!NtCreateNamedPipeFile` ← `KERNELBASE!CreateNamedPipeW` ← `claude!uv_pipe` (libuv `uv__create_stdio_pipe_pair`) ← `claude!uv_spawn`.
   libuv is creating the stdio **named pipe** for a child process; the AppContainer denies named-pipe creation on `\Device\NamedPipe`; libuv's `uv__create_stdio_pipe_pair` retries the failure in a `for(;;)` loop forever and never reaches `CreateProcess`. The hung process showed **162s cumulative CPU** — a busy spin, not the "passive stall" originally recorded. Claude spawns children constantly (startup shell-snapshot, `git`, `ripgrep`, hooks, the Bash tool), so this fires on essentially every real turn. `claude.exe --version` works (no child spawn); `-p`/interactive hang.

2. **MSYS `bash` cannot initialize (#498 Blocker #1).** Git's `bash` dies with `STATUS_DLL_INIT_FAILED (0xC0000142)` inside an AppContainer — `msys-2.0.dll`'s runtime needs shared sections / named kernel objects under `\BaseNamedObjects` that the AppContainer confines to a private namespace. Claude's shell-snapshot and Bash tool depend on `bash`.

A third, lesser defect (#533): AppContainers block **loopback**, so a sandboxed agent's attention hook (`127.0.0.1:$BUILDMESH_PORT`) can never call home; no capability lifts this.

The decisive point: **#528 (1) and Blocker (2) are not missing grants.** Named-pipe creation and `\BaseNamedObjects` access are properties of AppContainer's object-namespace isolation, which is the *mechanism* of the isolation — you cannot grant past it without dismantling it. Even a hypothetical fix for the named pipe would still leave `bash` dead. **AppContainer is the wrong primitive for `claude.exe`.**

The ADR-0012 spike missed all of this because it validated `claude.exe` *starting*, not `claude.exe` *spawning a child* — the operation that breaks. This ADR therefore gates its own acceptance on a spike that tests the previously-untested path (see Decision §4).

## Decision

**1. Keep everything in ADR-0012 except the containment primitive.** The per-mesh `sandbox` column, the `sandbox::sandbox_enabled` policy seam, the per-node model, the deny-by-default worktree ACL grant story, the owned ConPTY spawn (`sandbox::conpty`), the curated env, and best-effort idempotent cleanup all carry over. This is a swap of *one layer*: the token / process-creation attribute, not the architecture.

**2. Confine with a restricted, low-integrity token instead of an AppContainer SID.** At process creation, replace the `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` attribute with a primary token built by `CreateRestrictedToken` plus a **low integrity level**, following the long-proven Chromium / `sandbox` model:
   - **Restricting SIDs** so the process passes access checks only against objects whose DACL grants the sandbox's own SID. We grant the worktree (and the same `~/.claude`, `~/.local/bin`, `~/.claude.json` set as ADR-0012) to that SID; everything else (the rest of `%USERPROFILE%`, host `%TEMP%`, the registry) fails the check by default — preserving the deny-by-default read protection that was the whole point of ADR-0012.
   - **Low integrity level** blocks writes to higher-integrity objects as a second layer.
   - Crucially, a restricted token does **not** create a private object namespace and does **not** sit behind AppContainer's NPFS denial — so libuv's named-pipe creation, MSYS `bash` init, and loopback all work. (This is exactly why Chromium's renderer sandbox uses restricted tokens, not AppContainers, for processes that must spawn/IPC.)

**3. Reuse the owned ConPTY seam; only the process-creation inputs change.** `sandbox::conpty::spawn_in_appcontainer` already re-implements the ~80-line ConPTY spawn precisely because a sandbox token must be applied at `CreateProcessW` time (the same reason an AppContainer must). The pivot swaps the `SECURITY_CAPABILITIES` attribute for `CreateProcessAsUserW` with the restricted token (and sets the integrity level on that token). `spawn_agent_inner`, the reader thread, resize, Job Object containment, and the kill path are untouched. Inline `extern "system"` FFI, no new dep — same posture as ADR-0012 §5.

**4. Acceptance is gated on a feasibility spike that tests the path ADR-0012 skipped.** Before this ADR moves to `accepted`, an ignored live test (mirroring `sandbox::spawn::tests::repro_anthropic_sandbox_direct_boots`) must prove, under the restricted token in an owned ConPTY, **all** of:
   - (a) creates a named pipe successfully (the #528 operation);
   - (b) spawns a child process successfully (`cmd.exe /c echo`);
   - (c) Git `bash --version` initializes (the Blocker-#1 operation);
   - (d) reading a file in `%USERPROFILE%` **outside** the worktree is **denied**;
   - (e) writing **outside** the worktree is **denied**;
   - (f) reaches `https://api.anthropic.com` **and** the hub on `127.0.0.1:$BUILDMESH_PORT`;
   - (g) a real `claude.exe` renders its prompt (> the 16-byte ConPTY handshake) and survives a turn that shells out.
   If (d)/(e) cannot be achieved *together with* (a)/(b)/(c) — i.e. if restricting SIDs can't deny home-dir reads without also denying the system objects child-spawn needs — fall back to the alternatives below rather than shipping a sandbox that either leaks reads or hangs.

**5. `CheckNetIsolation LoopbackExempt` is not needed and not used.** It is admin-only and machine-global; the restricted-token primitive doesn't block loopback in the first place, so #533 is resolved for free.

## Consequences

- **#528, #533, and Blocker #1 are all resolved by construction** — none of named-pipe creation, `bash` init, or loopback is restricted by a restricted/low-IL token.
- **Restricted-token sandboxes are subtle.** Getting restricting SIDs to deny user files while still admitting the system objects a process legitimately needs (CSRSS/`\Windows\ApiPort`, the window station/desktop, the NPFS root, the pseudoconsole pipes) is the hard part — it is why Chromium's sandbox is large. We mitigate by gating on the §4 spike and by keeping the grant surface identical to ADR-0012 (worktree + the three `~/.claude*` paths).
- **The read-protection guarantee must be re-proven, not assumed.** ADR-0012's "agent can't read `~/.aws/credentials`" property is re-established by spike criterion (d), not inherited. This is the criterion the original spike never tested for the spawn path; we do not repeat that.
- **The default-deny UX trade-off from ADR-0012 §Consequences is unchanged** — anything outside the curated PATH / granted dirs still fails; per-provider grant tuning remains a follow-up.
- **macOS Seatbelt (#497) is unaffected** — it reads the same `sandbox` column through the same seam; only the Windows backend changes.
- **Cleanup simplifies slightly** — no AppContainer profile to create/derive/delete; cleanup is the ACL revoke + token handle close. The `CLEANUP` map and `kill_session` call site stay.

## Considered alternatives

- **Keep AppContainer, fix the named pipe.** Not possible — the denial is the isolation mechanism, not a grant gap; and `bash` (Blocker #1) would still die. Rejected: doesn't make `claude` work.
- **AppContainer + force claude to not spawn children / not use bash.** Outside our control (claude's shell-snapshot, hooks, and Bash tool are core). Rejected.
- **Job Object + low integrity only, no restricting SIDs.** Simple and fixes #528/#533/Blocker-1, but Low IL blocks *writes-up*, not *reads* — a Low-IL agent can still read `~/.aws/credentials`. Rejected: defeats ADR-0012's core read-protection goal. (Acceptable only as an explicit "containment-lite" fallback if §4(d) proves unachievable, and only with that limitation documented.)
- **WSL-based confinement.** Run sandboxed agents inside WSL where Linux namespaces / `bubblewrap` confine cleanly and `claude` runs natively. Buildmesh already has hybrid WSL support and `env::to_host_path`. Strong long-term option; heavier prerequisite (requires WSL + a claude install inside the distro) and a different path-translation surface. Worth a parallel spike if the restricted-token §4 criteria prove hard to satisfy together; not the default because it changes the agent's runtime environment, not just its confinement.
- **No Windows sandbox (Job Object only, as today's opt-out path).** The honest fallback if neither restricted-token nor WSL can confine `claude` without breaking it: keep `sandbox = true` a no-op on Windows and document it. Rejected as the *plan*, retained as the floor.

## Status

`proposed` — pending the §4 feasibility spike. On a green spike this ADR moves to `accepted` and supersedes ADR-0012's primitive; ADR-0012 stays as the record of the AppContainer attempt and *why it failed* (the cdb root cause in #528). Tracking: #528 (root cause), #533 (loopback), #498 (parent).
