# Notifications design prototypes

Design question: how should toast history and actionable worktree cleanup
problems appear behind a bell immediately to the left of Zoom?

The user selected D and approved production implementation. The fixture-only
primary source is preserved on the local branch
`prototype/notifications-probe-dusky-rare-drumlin`, commit
`0b8a380b1`. Check out that branch to run `npm run prototype:notifications`.
The preview opens at
`http://127.0.0.1:1436/?prototype=notifications&variant=D` and uses the real
desktop shell with browser-local fixture IPC. Notification history resets on
reload. Recovery actions operate on sample records in memory.

## Options to review

- **A — Compact dropdown:** a small chronological list, a quiet bell indicator,
  and a Resolve link beside each blocked worktree.
- **B — Attention first:** an amber count of unresolved worktree problems,
  pinned recovery cards, and routine activity below.
- **C — Activity drawer:** a wider panel, unread count, filters, and expandable
  details before opening recovery.
- **D — Notifications Probe (selected refinement):** C's side-panel placement
  with B's attention-first view, docked beside the workspace. It follows the
  Probe's header/context layout and resizes from 240px to 720px in memory.
  The bell's amber count represents unresolved worktrees. Read activity
  remains available; marking all read does not hide unresolved problems.

Use the floating review bar or Left/Right keys to switch variants. The URL
retains the selection. Use Samples, All read, and Empty to compare states;
the theme button compares dark and light. All read preserves unresolved
worktree problems. Clear read also preserves unresolved worktrees.

Show toast adds an actual app toast and records it in the prototype history.
Dismissing the toast leaves that history available. Resolve opens the existing
blocked-cleanup dialog. Retry and Keep worktree demonstrate completion with
fixture responses. The sample modal does not open automatically on a toast.

## Scope

The captured branch contains throwaway prototypes. They load only during
development when fixture IPC and the prototype query parameter are present.
The selected behavior is implemented in the Notifications Probe; current
architecture belongs in [Probe UI](../../development/probe-ui.md#notifications).
No real cleanup or process termination is
exercised by this preview.
