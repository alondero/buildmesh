# Documentation standards

Documentation is a product surface. A feature is not fully delivered when it
works in code but a user cannot discover it, configure it, recover from its
failure, or understand its security boundary.

## Required content

Choose the smallest document that can answer the reader's question, then cover
the applicable items below:

- **Audience and status:** say who the document is for and whether it is
  current, proposed, experimental, superseded, or historical.
- **Outcome first:** begin with what the reader can accomplish, not internal
  implementation history.
- **Prerequisites:** list supported OS/runtime, required CLIs, credentials,
  permissions, ports, versions, and whether a step is optional.
- **Executable examples:** prefer copyable commands, concrete UI labels, and a
  visible expected result. Keep examples small and say where they run.
- **Boundaries:** document defaults, platform differences, unsupported cases,
  data locations, network exposure, credential handling, and destructive or
  irreversible actions.
- **Failure and recovery:** give the first diagnostic signal, a safe recovery
  action, and the escalation path. Never tell users to paste secrets or an
  unredacted log.
- **Navigation:** link to the next task, the relevant source of truth, and the
  support/reporting path. Do not leave a document discoverable only by grep.

## Document type rules

| Type | Owns | Does not own |
|---|---|---|
| `README.md` | What Buildmesh is, install, supported platforms, first-run prerequisites, high-level features, limitations, support | Long procedures, internal issue history, architecture policy |
| `docs/user-guide.md` | Task-oriented workflows and product concepts | Unreleased designs or implementation-only details |
| `docs/troubleshooting.md` | Symptom → cause → recovery → report guidance | A vague list of error messages without actions |
| `CONTRIBUTING.md` / `docs/development/` | Developer setup, tests, evidence, release, extension checklists | End-user onboarding |
| `CONTEXT.md` | Canonical domain language | File paths, UI gestures, or historical rationale |
| `docs/adr/` | One consequential decision, alternatives, consequences, and status | A chronological bug diary or a general tutorial |
| `docs/specs/` | Proposed behavior, acceptance criteria, and implementation contract | Current user promises unless marked implemented and kept in sync |
| `docs/research/` / `docs/learning/` | Evidence, source links, and conclusions from an investigation | Unverified claims presented as product behavior |
| `docs/archive/` | Retired material with a reason it is no longer authoritative | A live link from the user guide as if it were current |

## Update contract

Use this impact map when changing behavior:

| Change | Documentation and evidence to consider |
|---|---|
| New or changed user workflow | README discovery link, user guide procedure, troubleshooting entry if failure modes changed, and the current versioned release note in `docs/releases/` |
| New harness, provider, setting, shortcut, or platform | User guide capability/setup table, README summary or drift source, platform limitations, and a live/tested source of truth |
| Security, auth, data storage, network, or destructive behavior | User-facing warning and recovery steps, `SECURITY.md` if reporting scope changes, and an ADR |
| Tauri command, HTTP route, IPC payload, or generated type | Developer guide/API documentation, Rust source docs, contract tests, and regenerated bindings where applicable |
| Architectural boundary or trade-off | ADR with status; update `CONTEXT.md` or `knowledge-primer.md` only when vocabulary or durable AI guidance changes |
| Build, lint, hook, CI, or release change | Developer guide, `CONTRIBUTING.md`, and the relevant command/check documentation |
| Documentation-only correction | The affected document and its navigation links; no release-note entry is required unless users need to know about the correction |

If a behavior change needs no user-facing update, make that decision explicit in
the PR evidence or commit message with `docs: none — <reason>`. Do not hide a
missing document behind a checkbox.

## Markdown and link style

- Use one H1 per document and a predictable heading hierarchy.
- Use sentence-case headings, short paragraphs, tables for exact mappings, and
  fenced code blocks with a language where practical.
- Refer to UI controls by their visible label and distinguish host, WSL, and
  remote device steps.
- Prefer relative links for repository files and absolute links for external
  services. Check that local links resolve before review.
- Avoid internal issue numbers, ADR numbers, and implementation symbols in
  user-facing prose unless they are necessary to complete the task.
- Mark uncertainty and time-sensitive facts. Do not turn an experiment or a
  stale PRD into a current promise.
- Never include API keys, pairing tokens, cookies, private certificates,
  personal paths, or unredacted logs in examples or screenshots.

## Review checklist

Before requesting review, answer:

- Can a new reader find this from [docs/README.md](README.md) or the root
  README?
- Does the procedure work from a clean install or clearly state its starting
  state?
- Are platform/version/prerequisite differences explicit?
- Are defaults, limitations, security implications, and recovery steps clear?
- Did the change update the right source of truth rather than duplicate it?
- Did `npm run check:docs` and the scope-appropriate product checks run?
