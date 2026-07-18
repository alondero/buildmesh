---
name: Bug report
about: Something in Buildmesh broke or behaves wrong
title: "[bug] "
labels: ["needs-triage"]
---

<!--
  Bug reports go through the triage workflow in
  docs/agents/triage-labels.md. The maintainer will relabel as they
  route this — your job is to give them enough to reproduce.
  For security issues, see SECURITY.md instead of filing here.
-->

## What happened

<!-- One-sentence summary of the symptom. -->

## Steps to reproduce

1.
2.
3.

## Expected

<!-- What you thought would happen. -->

## Actual

<!-- What actually happened. Include error text verbatim. -->

## Environment

- Buildmesh version (commit SHA, release tag, or installer date):
- OS / version (Windows 11, macOS 14, Ubuntu 24.04, …):
- Tauri runtime dev or release build (`scripts\run.ps1` vs
  `scripts\run-dev.ps1`):
- Provider(s) running when it happened (Anthropic, Codex, …):

## Logs / screenshots

<!--
  Attach `panic.log`, `panic_early.log`, or the dev-profile debug log.
  Inline is fine if the relevant slice is short; otherwise drag-drop.
-->

## Workaround

<!-- optional: anything that lets you keep working until this is fixed -->