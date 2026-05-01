# Triage Labels

This repo uses the canonical five-label triage vocabulary.

| Label | Purpose |
|-------|---------|
| `needs-triage` | Maintainer needs to evaluate the issue |
| `needs-info` | Waiting on reporter for more information |
| `ready-for-agent` | Fully specified, AFK-ready for an agent to pick up |
| `ready-for-human` | Needs human implementation |
| `wontfix` | Will not be actioned |

## Consumer Rules

- The `triage` skill applies these labels as issues move through the triage state machine
- No custom label mappings are configured — defaults are used
- Skills that create issues (e.g. `to-issues`) do not apply labels automatically
