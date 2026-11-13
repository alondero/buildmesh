# ADR 0058: PR repository identity and wire shapes live behind `services/github`

Status: accepted

## Context

Issue [#2236](https://github.com/alondero/buildmesh/issues/2236) reviewed the
GitHub pull-request surface and found the command layer owning knowledge that
services and circuits needed.

`src-tauri/src/commands/pr.rs` was the de facto owner of "which GitHub repo
does this path belong to". `services/circuit_triggers.rs` and
`services/circuit_worker/github.rs` reached up into the command module for
`resolve_github_owner_repo`, and `services/github/feeds.rs` and
`services/github/pr_workflow.rs` reached up for the wire DTOs the workflows
themselves build. A service importing the boundary that is supposed to be
above it is the dependency direction the module map calls out: the step is
invisible, the call compiles, and the ownership it skipped is simply missing.

The same module also carried three create-PR paths. Two of them derived their
source from the *project root* — the branch the mesh checkout happened to be
sitting on — which is how a `main -> main` PR, or a PR from an unrelated
feature branch, got published while the sheet showed the agent's own branch
(issue #2024 rank 4 / #1567). Neither had a call site left in the base tree:
the desktop `createPr` and `createPrForMesh` wrappers were exported but never
invoked, and the mobile client uses the node-scoped HTTP route.

## Decision

`services/github/` owns the PR domain's shapes and workflows:

- `services/github/repository.rs` — repository identity: which GitHub repo a
  path belongs to, the current branch, the configured remotes, and the
  `https://github.com/{owner}/{repo}` URL. It reads through `git::remote`,
  never a `Repository` handle.
- `services/github/wire.rs` — the issue and pull-request wire shapes
  (`GitHubIssue`, `GitHubPullRequest`, `PrSource`, `CreatePrResult`, `OpenPr`
  and the feed wrappers). They live beside the workflows that build them.
- `services/github/feeds.rs` and `services/github/pr_workflow.rs` — the feed
  queries and the create/open/merge workflows, unchanged in behaviour.

`commands/pr.rs` is a thin adapter: validate, offload, map the error to a
`String`. It imports the wire shapes from `services/github/wire.rs`. The
create workflow itself — the offload onto the blocking pool, the client
construction and the typed failure — lives in
`services/github/pr_workflow.rs::create_pr_for_node_source`, so the desktop
command and the mobile route are two adapters over one call. The
project-root create-PR commands are deleted rather than kept as a way to
publish the wrong branch again, and the desktop `createPr` /
`createPrForMesh` wrappers go with them.

`scripts/check-pr-creation-source-ownership.mjs` (with
`tests/agent-infra/pr-creation-source-ownership.test.mjs`) fails the build when:

- a production module under `services/` or `circuit/` reaches `commands::pr`;
- a production function that calls `create_pull_request_idempotent` names no
  source resolver, or reads a path's checked-out branch through `repo_info` —
  the read that produced the bug;
- the node-scoped resolver stops reading the agent node's worktree, or starts
  reading the mesh's root path;
- the removed project-root commands come back, on either side of the IPC
  boundary.

The guard's scope is `services/` and `circuit/`, which is where the upward
dependency was. The HTTP routes are exempt by design: they are the second
command boundary and call the *registered* commands by their async wrappers,
which `docs/development/rust-conventions.md` prescribes. The route that
reached past that boundary — `resolve_pr_source_for_node`, a helper rather
than a registered command — now imports the workflow from `services/github/`
like every other caller.

The accepted source resolvers are named explicitly, so a new create-PR path has
to say where its head branch comes from instead of defaulting to "the branch
the project root is sitting on".

## Alternatives and consequences

- **Leave the DTOs in the command layer and let the services import them.**
  That is what the code did, and it is why `services/github/feeds.rs` imported
  `commands::pr`. ADR 0047 granted that exception deliberately, for a
  different reason: keeping the declarations preserved the ts-rs export test
  names. It is not free — a workflow that has to name a wire shape then
  depends upward on the adapter that happens to serialise it.
- **Move the DTOs to `models/`.** `models/` is the shared DTO owner and would
  have kept them out of both layers, but `GitHubPageCompleteness` already
  lives with the pagination code it belongs to, and the PR shapes are read
  only by the GitHub workflows and their two adapters. A new `models/`
  domain for one module's shapes would spread the GitHub contract across two
  owners.
- **Keep the project-root create-PR commands and route them through the
  node-source resolution.** They had no callers, so there was nothing to
  route. Keeping them would keep a second way to publish a PR from the wrong
  branch alive.

The consequence is that the Rust test inventory moves with the code: the PR
suite moves from `commands::pr::tests` to `services::github::pr_tests`,
because the functions it exercises moved with the module that owns them. The
behaviours the deleted paths pinned — duplicate-create recovery, the
single-POST happy path, the non-422 propagation, the structured 422 detection
and the recovery GET's URL encoding — are pinned against the surviving
node-scoped path. The generated TypeScript bytes are unchanged: the export
file names come from `#[ts(export_to = …)]`, not from the module the struct
lives in.

## Verification

Compare the baseline and candidate `cargo test --lib -- --list` inventories and
the generated TypeScript bytes. Run the GitHub service, command and HTTP
suites, the shard-coverage gate, the ownership guard, and the repository
verification. The ownership guard must fail when a service module reaches
`commands::pr`, and pass on the candidate.

## Links

- [Probe UI](../development/probe-ui.md)
- [Rust backend module map](../development/module-map.md)
- [ADR 0047: Backend responsibility modules behind stable entrypoints](0047-backend-responsibility-modules.md)
  — this record partially supersedes ADR 0047's decision to keep the PR wire
  DTO declarations in the command layer; its decision to keep the *Circuit*
  wire DTOs there, and its rule that an adapter invokes no command, still
  stand.
