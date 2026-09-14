# Specifications

Specifications describe product intent, acceptance criteria, or a technical
contract. They are useful design history, but they are not automatically the
current behavior. Check the status line and then verify current behavior in the
user guide, source, tests, or an accepted ADR.

## Current index

| Document | Scope |
|---|---|
| [Build/run system](build-run-system.md) | Build, Run, and terminal utility flow; its old `mesh.toml` storage section is explicitly superseded in the document |
| [Git sync and changed files](prd-git-sync-and-changed-files.md) | Git synchronization and review surfaces |
| [Autopilot mode](prd-autopilot-mode.md) | Issue-driven autonomous Agent Node lifecycle |
| [Autopilot indicators](autopilot-node-indicators.md) | Current presentation contract for automated nodes |
| [Harness configuration](prd-harness-configuration.md) | Per-harness defaults and Mesh overrides |
| [Harness/provider separation](prd-harness-provider-separation.md) | Domain and configuration contract |
| [Remote access MVP](prd-remote-access.md) | Superseded early remote-access design; current security and pairing behavior is in [the user guide](../user-guide.md#remote-access) and [ADR 0034](../adr/0034-pairing-tickets-and-trusted-root-rotation.md) |
| [OpenAI/Codex usage](openai-codex-usage-meters.md) | Usage-meter design and provider contract |
| [Cursor usage](cursor-usage-meters.md) | Cursor usage-meter design and implementation notes |

## Status discipline

When a spec is implemented, add or update a concise status line and link the
current user/developer documentation. When it is no longer authoritative, add a
superseded banner naming the replacement. Keep historical acceptance criteria
when they explain why the code looks the way it does; do not make users read
them to learn how the product works today.
