# Architecture Decision Records

ADRs capture decisions whose rationale should survive the implementation that
created them. Each record must state its status. A proposed or superseded ADR
must not be used as a current product promise without checking the live code
and the current user/developer guide.

## Current decision areas

| Area | Relevant records |
|---|---|
| Domain, IPC, and shared contracts | [ADR 0009](0009-shared-rust-ts-types-via-ts-rs.md), [ADR 0010](0010-tauri-ipc-wrapper-over-codegen.md), [ADR 0013](0013-rename-ipc-surface-to-node-mesh.md), [ADR 0037](0037-generated-harness-capabilities-catalog.md) |
| Git, worktrees, and sync | [ADR 0001](0001-auto-sync-mesh-on-node-spawn.md), [ADR 0002](0002-allow-branched-worktree-creation-on-dirty-mesh.md), [ADR 0003](0003-buildmesh-owns-worktree-creation.md), [ADR 0004](0004-optimistic-node-close-deferred-worktree-removal.md), [ADR 0005](0005-diff-against-merge-base.md), [ADR 0006](0006-mesh-health-recovery-ux.md), [ADR 0007](0007-extract-git-module.md), [ADR 0020](0020-background-mesh-sync-and-spawn-fetch-ttl.md), [ADR 0022](0022-fetch-always-gate-only-the-pull.md), [ADR 0033](0033-ff-pull-blocked-only-by-overlapping-local-changes.md) |
| Agent process and harness architecture | [ADR 0011](0011-autopilot-session-wrapup.md), [ADR 0012](0012-windows-appcontainer-agent-sandbox.md), [ADR 0014 sandbox pivot](0014-pivot-windows-sandbox-off-appcontainer.md), [ADR 0014 harness/provider split](0014-separate-harnesses-from-providers.md), [ADR 0016](0016-spawn-menu-harness-grouped-multi-harness-providers.md), [ADR 0019](0019-pre-spawn-worktree-pool.md), [ADR 0024](0024-assign-session-ids-not-capture-and-reuse-worktrees-on-resume.md) |
| Remote access and security | [ADR 0008](0008-coordinator-control-api.md), [ADR 0015](0015-two-tier-api-roles-and-header-auth.md), [ADR 0017](0017-opt-in-lan-exposure-and-self-signed-tls.md), [ADR 0018](0018-persistent-device-sessions-and-revocation.md), [ADR 0023](0023-ws-ticket-per-caller-rate-cap.md), [ADR 0025](0025-provider-credential-vs-pairing-endpoint.md), [ADR 0034](0034-pairing-tickets-and-trusted-root-rotation.md) |
| UI, usage, and circuits | [ADR 0026](0026-openai-and-codex-usage-meters.md), [ADR 0027](0027-circuit-canvas-editor-on-react-flow.md), [ADR 0028](0028-circuit-run-capacity-contract.md), [ADR 0029](0029-probe-context-lenses.md), [ADR 0030](0030-titlebar-navigation-on-demand-inspector.md), [ADR 0031](0031-palette-tool-discovery.md), [ADR 0032](0032-probe-working-set-tabs.md), [ADR 0037](0037-usage-last-known-fallback.md) |
| Window chrome | [ADR 0035](0035-native-windows-caption-button-affordances.md), [ADR 0036](0036-macos-traffic-light-affordances.md) |
| Release and distribution | [ADR 0021](0021-auto-updater-and-github-releases.md) |

## Adding an ADR

Use the next numeric filename and include:

1. Status (`proposed`, `accepted`, `superseded`, or `rejected`).
2. Context and the problem being decided.
3. The decision and its important invariants.
4. Alternatives considered and why they were rejected.
5. Consequences, migration/compatibility notes, and verification evidence.
6. Links to the current user or developer documentation when the decision is
   user-visible.
