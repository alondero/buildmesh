# Domain Docs

Single-context layout: one `CONTEXT.md` and `docs/adr/` at the repo root.

## Consumer Rules

| Skill | Reads |
|-------|-------|
| `improve-codebase-architecture` | `CONTEXT.md`, `docs/adr/*.md` |
| `diagnose` | `CONTEXT.md` |
| `tdd` | `CONTEXT.md` |

## CONTEXT.md

Defined at the repo root — see [CONTEXT.md](../../CONTEXT.md). It holds the project's domain language: what a **Mesh** is, what an **Agent Node** is, how they relate, and the canonical *avoid* terms (e.g. "session"/"pane" for an Agent Node).

## docs/adr/

Architecture Decision Records live at [docs/adr/](../adr/). Add a new ADR there when a decision is consequential enough that future contributors (human or AI) will need to know the *why*.

## Layout

```
buildmesh/
├── CONTEXT.md          # Project domain language and mental model
└── docs/
    └── adr/            # Architecture Decision Records
        └── *.md
```
