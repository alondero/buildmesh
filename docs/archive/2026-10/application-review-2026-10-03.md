# Application bug and UX review - October 3, 2026

Status: review of baseline `3a5f5b4865c0cc2697f8c7c3a776769f88d84584`.
Audience: maintainers. Current usage belongs in the [user guide](../../user-guide.md).

## Scope and evidence

The review follows desktop workspace/navigation, agent lifecycle and terminals,
Project Settings, Notes, Repository cleanup, files/diffs, GitHub feeds, provider
settings, Circuits, startup/recovery, remote access and mobile task navigation.
Source inspection covers React state ownership, IPC calls, Rust command/HTTP
handlers and generated wire types. It supplements the earlier
[desktop UX audit](desktop-ux-audit-2026-10.md); already-corrected behavior is
not counted again merely because its historical issue remains open.

The ranking prioritizes directly reproducible lost edits and cross-project
destructive actions. Ranks 1-3 are the critical fixes in this PR; ranks 4-10
are follow-ups with concrete source evidence, owned by
[#2024](https://github.com/alondero/buildmesh/issues/2024) and the linked existing
tickets. An application-wide review is not proof of all possible runtime paths.
No paid harness, external PR creation/merge, production Circuit, WSL agent or
physical phone pairing was executed. Backend findings below are call-path
reviews, not claims of live GitHub/provider verification.

## Ranked top ten

| Rank | Priority | Bug or UX problem and trigger | Disposition |
|---|---|---|---|
| 1 | Critical | Project Settings loads Alpha, switches to Beta, and catches Beta's failed read by clearing only `loading`. Alpha's editable values then appear under Beta; blur can save them to Beta. | Fixed: associate the loaded form with its mesh, quarantine pending/failed reads, offer Retry and reject saves without a successful current load. Closes [#1575](https://github.com/alondero/buildmesh/issues/1575). |
| 2 | Critical | Notes accepts typing before loading; a late read replaces it, or a failed read permits a blank replacement. Concurrent saves can overtake each other; old rejection handlers evict newer cache entries and old save indicators bleed across meshes. | Fixed: gate editing on the current read, provide Retry, serialize writes per mesh, cache acknowledged results and fence cache/status updates by their owner. [#2023](https://github.com/alondero/buildmesh/issues/2023). |
| 3 | Critical | Repository manual/post-action refreshes commit without ownership checks. Switching projects retains old rows/confirmation while loading or failing; a late Alpha refresh can populate Beta's cleanup targets. | Fixed: scope the component's targets, selection and confirmation to one mesh; share refresh revisions and drop late mutation feedback after leaving. Disable selection/deletion during refresh. [#2023](https://github.com/alondero/buildmesh/issues/2023). |
| 4 | High | Mobile Changes displays the selected agent branch, but Create PR sends its mesh ID to the mesh-root endpoint. A root on another branch can publish the wrong work; a root on `main` generally refuses creation. | Existing [#1567](https://github.com/alondero/buildmesh/issues/1567), tracked by #2024. |
| 5 | High | Startup awaits `Promise.allSettled`, but the primary mesh/node fetch actions swallow their errors. A failed snapshot can fulfill initialization and render an empty workspace rather than the boot Retry flow. | Existing [#1524](https://github.com/alondero/buildmesh/issues/1524), tracked by #2024. |
| 6 | High | GitHub issues, PR summaries and PR files cap at one hundred without a complete/partial distinction. Busy repositories and large PRs hide work from review. | Existing [#1528](https://github.com/alondero/buildmesh/issues/1528), tracked by #2024. |
| 7 | Medium | Archive builds its list from discoverable harness transcripts. Durable archived rows without a CLI session ID or readable file, including Terminal sessions, disappear rather than displaying unavailable Resume. | Existing [#1065](https://github.com/alondero/buildmesh/issues/1065), tracked by #2024. |
| 8 | Medium | Project Settings awaits the mesh store's rename action, which catches a rejected IPC call. The shared save wrapper subsequently announces Saved while the sidebar retains the old name. | New follow-up in #2024. |
| 9 | Medium | Mobile Create PR disables Cancel during submission, but the sheet backdrop and browser Back still dismiss it; fields remain editable and late completion pops history after the sheet has gone. Dialog semantics/focus containment are also absent. | New follow-up in #2024. |
| 10 | Medium | Mobile Diff treats zero text hunks as “file matches HEAD.” Changed binaries and metadata-only changes also have zero hunks, and the node diff actually compares against its merge base. The mobile hand-declared type drops Rust's binary/status fields. | New follow-up in #2024. |

## Follow-up evidence and acceptance

- **Mobile PR target:** `src/mobile/App.tsx` passes `node.mesh_id` to
  `CreatePrSheet`; `api.createPr` posts to `/api/meshes/{id}/pr`;
  `src-tauri/src/http/routes/pr.rs` passes `mesh.path` to the creator. Compare
  that with `get_current_branch` resolving `env::node_working_path`. Verify a
  root on `main` and a root on another feature; only the displayed node branch
  may be submitted, with server-side node/mesh ownership checks.
- **Startup:** `meshStore.fetchMeshes` calls `loadMeshes(false)`;
  `agentNodeStore.fetchAgentNodes` catches the primary snapshot failure.
  Reject each read independently, verify the visible failure/Retry, then
  recover a populated workspace. Keep refresh and startup contracts explicit.
- **Pagination:** `services/github/issues.rs::list_issues_only` requests one
  `per_page=100` search response; `prs.rs` retains `PR_SUMMARY_CAP = 100` and
  `list_pr_files` requests one page. Test 101 items and second-page failure;
  distinguish partial results from a complete successful list.
- **Archive:** `services/agent_node_discovery.rs::discover` constructs entries
  from harness files; database rows only help exclude currently tracked IDs.
  Test archived rows with no session ID, a missing transcript and a readable
  transcript. Historical identity must survive unavailable resumability.
- **Rename:** `ProjectSettingsTab::saveName` calls `meshStore.updateMeshName`;
  its catch fulfills without propagating failure to `wrappedSave`. Reject the
  command and assert Save failed, preserved draft and unchanged sidebar, then
  prove successful retry updates both.
- **Mobile sheet:** `ui.tsx::Sheet` always enables backdrop dismissal;
  `App.tsx` pops a sheet without busy ownership; `CreatePrSheet` has no
  request/unmount fence. Defer submission and exercise backdrop/browser Back,
  then complete the request. Preserve the draft and avoid surprise navigation
  or duplicate publication. Verify dialog keyboard/focus behavior.
- **Mobile diff:** `DiffScreen::DiffBody` checks only hunks; compare the local
  `api.ts::FileDiff` declaration with generated `FileDiff.ts`. Test changed
  binary, rename-only, committed text and actually empty fixtures. Use the
  generated wire type and truthful baseline language.

## Regression and runtime verification

`tests/unit/probe-data-safety.test.tsx` exercises production components and
the IPC facade with controlled promises. In an isolated checkout of the
recorded baseline, **12 of 13 cases fail**; the existing late Alpha read fence
already passes. All 13 pass after the fixes. Coverage includes read failure
and Retry, save ordering across meshes, late read/write rejection, save-status
ordering, a return during an outstanding write, manual refresh crossing a
mesh switch, a cleanup confirmation crossing a pending/failed load, and
selection attempts during a failed refresh.

The existing Notes suite now uses separate mesh identities per test and waits
for loading before editing. Its previous cache reuse hid backend outcomes;
these changes preserve the real cache rather than replacing the production
boundary with an echo mock.

The complete frontend run (`VITEST_MAX_WORKERS=4 npx vitest run`) passed
**271 files / 3,774 tests**, with one skip. An earlier unconstrained gate
attempt timed out in version/child-process/UI readiness tests; their focused
reruns and the bounded full run passed without changing their assertions.
The canonical verification receipt and final gate results are recorded in
the PR evidence table; local harness receipts remain ignored.

Native acceptance used a separate `com.alond.buildmesh.review.dev` profile,
the baseline checkout and rebuilt working tree, and two temporary Git
repositories. Both `tauri build --no-bundle` release builds succeeded. CDP
assertions exercised real project creation, settings reads, Notes writes and
reads, project switching, cleanup selection/confirmation, and fixture removal.
The failure and delayed-read cases intercept only those IPC responses in the
real WebView: they prove frontend ownership, not an actual disk failure.

At **240px**, the baseline shows Alpha's editable form under Beta after a
rejected settings read; the fixed build shows a wrapping error and Retry.
Retry returns Beta's real configuration, with no old form. Notes is disabled
during the delayed read, then loads the persisted value; editing saves to
Rust, survives switching away/back, and leaves Beta's notes unchanged.
Notes enablement is a functional DOM assertion; its pixels alone do not
establish the disabled state.

The inspected [before](../../pr-screenshots/meager-hooded-dandy/settings-before.png)
and [after](../../pr-screenshots/meager-hooded-dandy/settings-after.png) captures
contain only review fixtures. Native log inspection found the injected
settings error and expected occupied-port/global-shortcut warnings from
running alongside existing instances; no panic files were created.

## Review and handoff

The finish review evaluates documented standards and the requested behavior
independently. The PR closes only the three implemented safety findings;
#2024 remains open for the seven follow-ups. Start with #1567, then startup
failure recovery and pagination. Keep fixes small and verify their actual
HTTP/IPC boundaries as well as their rendered controls.

Both independent reviews returned **APPROVE** after correcting row selection
during Repository refresh and wrapping unbroken error identifiers. Those
review discoveries are included in the fixes and acceptance evidence above.
