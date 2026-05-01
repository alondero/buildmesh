# Domain Docs

Single-context layout: one `CONTEXT.md` and `docs/adr/` at the repo root.

## Consumer Rules

| Skill | Reads |
|-------|-------|
| `improve-codebase-architecture` | `CONTEXT.md`, `docs/adr/*.md` |
| `diagnose` | `CONTEXT.md` |
| `tdd` | `CONTEXT.md` |

## CONTEXT.md

Not yet created. The `diagnose` and `tdd` skills will read it when it exists.

## docs/adr/

Not yet created. Architectural Decision Records go here.

## Layout

```
buildmesh/
├── CONTEXT.md          # Project domain language and mental model
└── docs/
    └── adr/            # Architecture Decision Records
        └── *.md
```
