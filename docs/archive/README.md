# Archive

Retired material that is **not current truth**. Nothing here is a contract: it is
kept so a past decision, incident, or investigation stays reviewable after the
code it describes has moved on.

## Layout

- `2026-09/`, `2026-10/`, … — dated investigations and run write-ups, filed under
  the month they describe. These used to sit in `docs/development/`, where they
  competed with current contracts for the same `rg circuit docs/development`.
- `Product-Vision-PRD.md` — predates the month folders (see below).

## What belongs here

A document moves here when it records something that happened rather than
something that holds:

- an incident or failure investigation (`circuit-failures-2026-09-26.md`)
- a single run's write-up or review verdict (`circuit-run-104-watchdog.md`)
- an audit against a recorded base commit (`circuit-autonomy-audit.md`)
- an acceptance record superseded by a later change
  (`circuit-reliability-acceptance.md`)

What **stays** in `docs/development/` is a current contract, and it says so: a
`Status:` line near the top is required there by `npm run check:docs`, and this
directory is exempt.

## How to move something here

Use `git mv` so the file keeps its history, file it under the month it
describes, and prefer a real Markdown link over a backticked path when pointing
at it from a live document. A backticked path is invisible to the link checker;
`check:docs` catches the ones that name a file which has since moved, because
that file still exists somewhere else under `docs/`.

Inside a moved document, its relative links need re-aiming — everything shifts
one directory deeper. `npm run check:docs` verifies every local link and anchor,
so it will find them.

## Vocabulary note

- **`Product-Vision-PRD.md`** — Written when the product was still branded "Conductor"
  (with a proposed `conductor://` URL scheme and `conductor.json` config). It uses
  pre-vocabulary language ("project", "session", "base branch") that no longer
  matches the canonical terms in `CONTEXT.md` (Mesh, Agent Node, Base Ref). Kept
  here for historical reference only — do not treat as a current spec.
