# Buildmesh documentation

This is the documentation map for Buildmesh. Start with the document that
matches the job you are doing; the README is intentionally a landing page, not
the complete product manual.

## Choose a path

| Audience | Start here | You will find |
|---|---|---|
| New or returning user | [User guide](user-guide.md) | First session, meshes, Agent Nodes, harnesses, worktrees, settings, remote access, and review workflow |
| Troubleshooting a running install | [Troubleshooting](troubleshooting.md) | Symptoms, likely causes, safe recovery, and the information to include in a report |
| Installing or evaluating Buildmesh | [README](../README.md) | Supported platforms, downloads, prerequisites, security limitations, and a feature overview |
| Contributor | [CONTRIBUTING.md](../CONTRIBUTING.md) | Contribution contract, checks, issue flow, and PR evidence |
| Developer | [Development guide](development/README.md) | Repository map, test matrix, seams, generated types, and feature checklists |
| UI contributor | [DESIGN.md](../DESIGN.md) | Design tokens, typography, and component patterns shared by desktop and mobile |
| Maintainer | [Releasing](development/releasing.md) and [release notes](releases/README.md) | Release procedure, updater signing, and release-note discipline |
| AI coding agent | [CLAUDE.md](../CLAUDE.md) and [AI context](knowledge-primer.md) | Always-on rules, architecture, conventions, and anti-patterns |

## Source-of-truth order

- User-visible behavior belongs in the [user guide](user-guide.md), with
  recovery paths in [troubleshooting](troubleshooting.md).
- The root [README](../README.md) owns discovery, installation, support links,
  and concise limitations. It must not become a changelog or an issue tracker.
- [CONTEXT.md](../CONTEXT.md) owns domain vocabulary. Use **Mesh**, **Agent
  Node**, **Agent Harness**, **Model Provider**, and **Worktree** consistently.
- [Architecture Decision Records](adr/README.md) own durable decisions and
  their rationale. [Specs](specs/README.md) describe proposed or historical
  implementation contracts and may be superseded.
- [Development documentation](development/README.md) owns contributor-facing
  build, test, release, and maintenance procedures.
- Versioned [release notes](releases/README.md) own concise, user-visible
  changes for a specific release. GitHub Releases is the published history;
  the versioned files remain reviewable source records in the repository.
- [CLAUDE.md](../CLAUDE.md) and [knowledge-primer.md](knowledge-primer.md) are
  AI context, not a substitute for human-facing documentation.

## Documentation maintenance

Read [Documentation standards](documentation-standards.md) before adding or
substantially changing a document. Every documentation change should leave the
navigation map, local links, status labels, and affected user workflows
coherent. `npm run check:docs` is the fast local gate and runs in CI.

The folders below are deliberately separated by purpose:

- [`adr/`](adr/README.md) — accepted, proposed, and superseded decisions.
- [`agents/`](agents/engineering.md) — agent-aware engineering and issue
  workflow contracts.
- [`development/`](development/README.md) — current developer procedures and
  implementation notes.
- [`learning/`](learning/) and [`research/`](research/) — evidence captured
  while investigating a behavior or external integration.
- [`releases/`](releases/) — versioned drafts and historical release notes.
- [`specs/`](specs/README.md) — product and technical specifications.
- [`archive/`](archive/README.md) — retired material that is not current truth.
- [`brand/`](brand/README.md) — the mark, the wordmark, and how the raster
  assets are generated.
