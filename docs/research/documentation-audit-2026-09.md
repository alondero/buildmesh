# Documentation audit — September 2026

## Scope and method

This audit compares the repository at the audit baseline with documentation
patterns in well-known, highly starred open-source utility projects. The
comparison set is deliberately mixed: command-line utilities demonstrate
excellent installation/usage documentation, while Ruff demonstrates a larger
developer and generated-documentation workflow.

Repository baseline: `ebecf0bf3ab732c2cb82144fe0d87eeb3c77169f`.
External pages were reviewed on 14 September 2026; star counts are intentionally
not treated as fixed facts because GitHub popularity changes continuously.

Primary sources reviewed:

- [ripgrep README](https://github.com/BurntSushi/ripgrep/blob/master/README.md),
  [ripgrep user guide](https://github.com/BurntSushi/ripgrep/blob/master/GUIDE.md),
  and [ripgrep contributing guide](https://github.com/BurntSushi/ripgrep/blob/master/CONTRIBUTING.md)
- [fd README](https://github.com/sharkdp/fd/blob/master/README.md) and
  [fd contributing guide](https://github.com/sharkdp/fd/blob/master/CONTRIBUTING.md)
- [fzf README](https://github.com/junegunn/fzf/blob/master/README.md) and its
  [man page](https://github.com/junegunn/fzf/blob/master/man/man1/fzf.1)
- [jq README](https://github.com/jqlang/jq/blob/master/README.md)
- [bat contributing guide](https://github.com/sharkdp/bat/blob/master/CONTRIBUTING.md)
- [Ruff contributing guide](https://github.com/astral-sh/ruff/blob/main/CONTRIBUTING.md)
- [just README](https://github.com/casey/just/blob/master/README.md)

These projects do not share one template. The useful common standard is a
clear front door, a deeper task reference, explicit limitations, a contributor
path, and automated checks for the facts that are easy to let drift.

## What the comparison shows

| Pattern in the comparison set | Buildmesh baseline | Action |
|---|---|---|
| A short product pitch followed by install and quick links | Present, but mixed with a long manual and developer internals | Added a docs hub and a first-session path; kept README as the landing page |
| Detailed task documentation separate from the README | No dedicated user guide | Added `docs/user-guide.md` |
| FAQ/troubleshooting that starts from user symptoms | Support existed, but recovery advice was scattered across README and internal notes | Added `docs/troubleshooting.md` |
| Install matrix and explicit prerequisites | Strong Windows/WSL coverage; provider setup and runtime boundaries were easy to miss | Made the workflow and runtime distinction prominent in the user guide and troubleshooting guide |
| Examples, screenshots, demos, or a reference manual | README had a wordmark and feature prose but no guided workflow evidence | Added executable first-session steps and identified screenshots/browser evidence as a review standard; existing PR screenshots remain developer evidence |
| Build/test/contributor path | Present in `CONTRIBUTING.md`, `engineering.md`, and scripts, but fragmented | Added `docs/development/README.md` and linked the verification matrix |
| Changelog/release notes | No versioned release-note source; release guide existed | Added a release-notes index, a versioned draft, and release/contribution expectations |
| Explicit project documentation map | Missing | Added `docs/README.md`, ADR index, and spec index |
| Docs kept accurate by automation | README provider/platform checks existed; Markdown was excluded from ESLint and local links were not gated | Added `npm run check:docs` and its tests to CI/local checks |
| AI/automation contribution policy | Agent rules and hooks existed, but no documentation-impact rule | Added documentation standards, PR evidence, and a commit hook requiring docs or an explicit `docs: none — reason` decision for behavior-sensitive commits |

## Findings from the local audit

### Strengths to preserve

- The README already documents supported platforms, first-run prerequisites,
  data locations, logs, updater signatures, sandbox limitations, shortcuts, and
  support routes.
- `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, issue templates, and
  an evidence-oriented PR template are already present.
- The repository has unusually strong architecture history: domain vocabulary,
  ADRs, specs, research notes, an AI knowledge primer, and scope-specific
  engineering guidance.
- CI already prevents several kinds of drift, including generated Rust↔TS
  bindings, README provider coverage, lint configuration fixtures, and bundle
  budgets.

### Gaps and risks

1. **Discovery gap (high):** readers had to infer which of roughly one hundred
   Markdown files was current. There was no docs landing page or audience-based
   route.
2. **Task gap (high):** the first successful user loop, harness setup, worktree
   review, remote pairing, and recovery procedures were not collected in one
   user-facing guide.
3. **Support gap (high):** common failures such as runtime-specific CLI
   detection, non-resumable Terminal nodes, certificate trust, and no-realized
   LAN binding lacked a symptom-first recovery path.
4. **Release communication gap (medium):** there was no changelog contract even
   though the release process and updater were documented.
5. **Developer navigation gap (medium):** verification knowledge existed but was
   distributed across `CONTRIBUTING.md`, `CLAUDE.md`, the engineering contract,
   and `scripts/check.ps1`.
6. **Accuracy gap (medium):** the early remote-access PRD described a plaintext,
   shared-token MVP while current code uses opt-in TLS, pairing invitations,
   device sessions, and trusted-root management. Two contributor-facing links
   also resolved to the wrong GitHub path, and two UI setup links targeted a
   missing README anchor.
7. **Enforcement gap (medium):** docs were intentionally outside ESLint, but
   there was no equivalent link, heading, required-document, or documentation
   impact check.

## Implemented changes

- Added the docs hub and source-of-truth map in `docs/README.md`.
- Added user onboarding and feature semantics in `docs/user-guide.md`.
- Added symptom-first recovery and safe report guidance in
  `docs/troubleshooting.md`.
- Added the contributor/developer map in `docs/development/README.md`.
- Added the standards and behavior-to-document impact map in
  `docs/documentation-standards.md`.
- Added the release-notes index and initial versioned release draft, plus ADR and specs indexes.
- Added a repository-native documentation checker and regression tests for
  local Markdown links, case-sensitive anchors, required documents, one-H1
  structure, ADR/spec status, image alt text, and the docs index.
- Added a Claude commit guard, PR documentation-impact prompts, and canonical
  agent guidance so documentation is part of the normal change loop.
- Wired the impact check into the PR workflow with the same dependency-free
  script, using the PR base commit so non-Claude contributors receive the gate.
- Marked the early remote-access PRD as superseded and recorded the current
  pairing/trusted-root decision in ADR 0034.
- Corrected the README prerequisites anchor used by the provider and canvas
  setup links.
- Added public-key/fingerprint guidance, published SHA-256 checksums, and a
  release workflow gate requiring matching versioned release notes.

## Deliberately not added

- No heavyweight Markdown toolchain or site generator: the repository is a
  desktop app, and a small dependency-free gate is enough for the current
  failure modes.
- No invented provider installation commands: those CLIs change independently;
  Buildmesh documents detection, runtime, and configuration behavior while the
  provider's own setup guide remains authoritative.
- No generic “every file changed means docs changed” heuristic. CI applies the
  narrower impact map to behavior-sensitive source, tooling, hook, workflow,
  and release changes; a documentation update or explicit `docs: none — reason`
  is required, while maintainers still judge whether the decision is correct.

## Follow-up backlog

- Add a small set of maintained user screenshots or a short screen recording
  once the visual workflow stabilizes.
- Publish platform-specific release artifacts for macOS/Linux, then update the
  support matrix and installation instructions from the release workflow rather
  than memory.
- Consider a generated provider capability table once the backend inventory has
  a stable user-facing schema; until then, the README drift gate and user guide
  are intentionally reviewed together.
