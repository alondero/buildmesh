# Worktree default location research, 14 October 2026

Historical decision research, not the current contract. See [Agent Node worktree support](../../development/agent-nodes.md#worktree-support-git2-based) for maintained behavior.

## Recommendation

Use `<mesh>/.worktrees/<node-name>` as BuildMesh's fallback for newly created worktrees. Keep mesh and application overrides, saved checkout paths, and existing `.claude/worktrees` checkouts working. This is a product choice for BuildMesh's repository-local workflow, not a claim that Git developers universally use `.worktrees`.

The short, provider-neutral name tells Codex, Claude Code and other users what the directory contains. It groups worktrees with their mesh, avoids adding directories beside repositories the user selected, and requires no reorganization of an ordinary clone. The existing configurable-directory contract makes an external container available to users who prefer it. These are the reasons to select it over sibling directories for BuildMesh.

The tradeoff is real: IntelliJ IDEA currently warns that worktrees nested inside the current project can be misidentified as multi-root projects, breaking its worktree integration. Recommend an **absolute external container**, for example `/home/me/src/myrepo-worktrees` or `C:\src\myrepo-worktrees`, for IDEA users. Renaming `.claude/worktrees` does not resolve that existing nesting limitation. [JetBrains worktree documentation](https://www.jetbrains.com/help/idea/use-git-worktrees.html)

## What the primary sources establish

Git requires a destination path; its examples use sibling locations such as `../hotfix` and `../temp`. They establish a documented workflow rather than a built-in default directory or a measurement of popularity. Git also supports bare repositories with linked working trees. [Git worktree manual](https://git-scm.com/docs/git-worktree)

Outside agent workflows, ThePrimeagen's Neovim worktree plugin recommends a bare repository and allows paths relative to the Git root or absolute paths. A separate Neovim plugin, `afonsofrancof/worktrees.nvim`, defaults to the parent of the Git common directory: for a normal clone that is the working root, while a bare repository makes it the bare directory's parent. This distinction matters when interpreting a tool's apparent "parent directory" convention. [git-worktree.nvim README](https://github.com/ThePrimeagen/git-worktree.nvim), [worktrees.nvim configuration](https://github.com/afonsofrancof/worktrees.nvim#configuration-options)

Another non-agent-specific editor plugin, `awerebea/git-worktrees.nvim`, defaults to `<repo>/.git/wt/<branch>` for ordinary repositories and documents external containers as an alternative. That is evidence of varied tool choices, not a reason for BuildMesh to put user working files inside Git's administration directory. [git-worktrees.nvim base paths](https://github.com/awerebea/git-worktrees.nvim#worktree-base-paths)

Worktrunk defaults to sibling `<repo>.<branch>` directories and documents repository-local `.worktrees`, centralized `~/worktrees/<repo>/<branch>`, and bare-repository layouts as alternatives. Its own description explicitly targets parallel AI agent workflows, so it is relevant comparative product evidence but does **not** establish non-agent prevalence. [Worktrunk configuration](https://worktrunk.dev/config/#worktree-path-template), [Worktrunk purpose](https://worktrunk.dev/)

There is no prevalence survey in these sources. The defensible conclusion is that sibling destinations are prominent in Git's examples and layouts vary across tools.

## Options compared

The advantages and costs below are design judgments based on those layouts.

| Option | Advantages for BuildMesh | Costs | Decision |
| --- | --- | --- | --- |
| `<mesh>/.worktrees/<name>` | Provider-neutral, short, grouped with the selected mesh, existing relative-directory settings model | Needs ignore hygiene; nested projects can confuse editors and inherit parent dependencies | Best default for BuildMesh, with documented external alternative |
| `../<repo>-worktrees/<name>` or `../<repo>.<name>` | Matches Git's sibling examples; avoids a worktree inside the source checkout; external IDE integration | Writes beside the selected mesh; requires parent-directory access and repository-derived naming; generic `../<branch>` can collide across repos | Best explicit external alternative |
| `<project>/.git` bare repository plus `<project>/main`, `<project>/<name>` | Symmetric branch directories, no privileged main checkout | Requires users to adopt a different clone layout; migration is much larger than a default change | Support existing layouts through configuration; do not require conversion |
| Global application storage or cache, grouped by repository identity | Main repository stays clean; centrally managed | Less discoverable; duplicate clone names need stable identities; storage lifecycle must preserve unfinished work | Poor default; a durable external directory is preferable to treating checkouts as cache |
| `<mesh>/worktrees/<name>` | Visible in directory listings and easy to name | Adds visible working files to the project; otherwise has the same nesting costs as `.worktrees` | Reasonable user preference, no clear advantage as the fallback |

Nested placement can also affect Node.js dependency isolation. Node searches ancestor `node_modules` directories when a bare module specifier is unresolved locally. **Inference:** a checkout at `<repo>/.worktrees/<name>` can therefore resolve a missing dependency from `<repo>/node_modules`; a sibling checkout removes that particular ancestor. Worktrees isolate Git files, not every runtime lookup. [Node.js module resolution](https://nodejs.org/api/modules.html#loading-from-node_modules-folders)

## Friction and compatibility work

1. **Ignore before creating the checkout.** For automatic local provisioning, use the repository's shared `info/exclude` so ordinary node creation does not modify tracked `.gitignore`. Git recommends this file for repository-specific personal workflow files, while committed `.gitignore` is for rules distributed to collaborators. Outside-root containers need no rule in the source checkout. [Git ignore documentation](https://git-scm.com/docs/gitignore)

2. **Resolve Git metadata, not `<mesh>/.git/info/exclude`.** A linked worktree has a `.git` file pointing at its administrative directory. Shared metadata lives at the common directory. Keep BuildMesh's existing `repo.commondir()` behavior and test with an actual linked checkout, including `git check-ignore` and clean parent `git status`. [Git repository layout](https://git-scm.com/docs/gitrepository-layout)

3. **Update context portability separately.** A user-requested portability commit can include `.worktrees/` as a shared ignore rule; keeping `.claude/worktrees/` ignored protects legacy and manually created Claude worktrees without ignoring the canonical `.claude/skills` directory. Do not commit absolute custom containers or machine-specific paths. The current [portability template](../../../src-tauri/src/commands/ai_context_gitignore.rs) recognizes its header alone: extending the template must also repair older blocks and change the status check, or previously portable repositories remain falsely complete. Preserve existing bytes and line endings, avoid duplicating the old block, and make a second application a no-op.

4. **Change all new-path consumers together.** Cold provisioning, warm-pool creation, backend and frontend path resolution, effective-directory display, settings placeholders, filesystem event filtering, and documentation should agree. Retain explicit legacy fallback handling for records created before saved worktree paths existed. Do not replace every `.claude/worktrees` string mechanically: legacy compatibility tests and provider-native examples can still need it.

5. **Do not move existing worktrees automatically.** Existing sessions and pending cleanup own concrete paths; a folder move also affects Git's registration links. Git provides `worktree move` and `repair` for intentional relocations. A fallback change should affect new checkout creation only, preserving overrides, resumed nodes, imported sessions, and cleanup records. [Git worktree move and repair](https://git-scm.com/docs/git-worktree#_commands)

6. **Keep ignored-file hydration provider-neutral.** BuildMesh already creates worktrees itself and applies `.worktreeinclude` to newly materialized checkouts. Preserve that behavior regardless of directory name, and never copy the worktree container recursively as an ignored context directory. [Current worktree provisioner contract](../../development/agent-nodes.md#worktree-support-git2-based)

## Evidence boundary

Browsed primary sources on 14 October 2026. Repository observations use base `c5f0dd4216dfdc19150f71f72a48025330c4dff7` in `chilled-worried-catacomb`. This note records the selection rationale and implementation considerations; it does not claim the implementation or its checks have completed.
