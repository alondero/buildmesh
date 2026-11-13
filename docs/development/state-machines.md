# State-machine inventory

Status: current source audit, 2026-11-09. Audience: contributors and coding
agents locating transition ownership before changing behavior.

Use this map to find the module that owns a workflow, the facts that constrain
its transitions, and the regression surface to exercise. State machines already
exist throughout Buildmesh; adding a framework everywhere would increase the
interface contributors must learn without establishing better ownership.

## Scope and reading key

This audit covers desktop React/Zustand, the Rust application, the mobile web
client, and native Android. It groups repeated instances by owner and behavior;
it does not count every checkbox as a separate machine. Build scripts and the
development harness are outside the application inventory. Discovery combined
the architecture owner documents with source searches for unions, reducers,
status writes, paired flags, pending promises, timers, cancellation, generations,
and revisions. Search hits alone are not proof of a machine or of a bug.

- **Implicit (I):** legal transitions are encoded by flags, nullable values,
  collections, control flow, or resource presence.
- **Hybrid (H):** named states exist, but request authority, effects, or recovery
  still depend on another implicit lifecycle.
- **Explicit (E):** a union, reducer, or transition module already owns the main
  lifecycle. This is a reference, not a request to replace it.

Arrows below summarize normal paths and recovery, not every legal edge. Names
without code formatting describe conceptual states, not wire literals. Each
row's invariant is the property a change must preserve or explicitly verify;
the inventory is not proof that every interleaving currently satisfies it.
This is a best-effort source audit, not a formal completeness or correctness
proof, and does not establish behavior in an installed desktop app or phone.

## Desktop application and shared frontend owners

Paths in this table are relative to `src/`.

| Workflow / shape | Owning source | Transitions and invariant |
|---|---|---|
| Workspace boot (H) | [App](../../src/App.tsx), [boot sequence](../../src/lib/bootSequence.ts) | Loading → ready or error → retry. Readiness requires both authoritative snapshots and complete listener attachment; a resolved promise carrying a store error is still failure. |
| Agent listener attachment (E) | [agent-node store](../../src/stores/agentNodeStore.ts), [listener bundle](../../src/stores/agentNodeListeners.ts) | Idle → attaching → attached; failed partial registration rolls back before retry. StrictMode must not duplicate listeners. |
| Live stream attachment (E) | [stream subscription](../../src/lib/streams/streamSubscription.ts) | `idle` → `attaching` → `attached`; last release → `idle`. The registration owns notifications, success, and failure; a retired callback cannot change its replacement. |
| Mesh snapshot authority (H) | [mesh store](../../src/stores/meshStore.ts) | Read → apply, supersede with trailing read, or fail. Request identity, committed mutation revision, and pending optimistic reorder decide which snapshot may publish. |
| Agent snapshot and ownership authority (H) | [agent-node store](../../src/stores/agentNodeStore.ts), [reconciliation](../../src/stores/agentNode/reconciliation.ts) | Snapshot / live event / optimistic patch → reconcile. Older node or Circuit ownership snapshots must not overwrite a newer mutation or event. |
| Create/import and launch (H) | [creation slice](../../src/stores/agentNode/creationSlice.ts), [lifecycle slice](../../src/stores/agentNode/lifecycleSlice.ts) | Create → launch request → live status or error. Set spawning before awaiting; failed automatic launch stays error until an explicit retry. |
| Optimistic node mutations (I) | [mutation slice](../../src/stores/agentNode/mutationSlice.ts), [optimistic helpers](../../src/lib/optimistic.ts), [toggle](../../src/lib/optimisticToggle.ts) | Apply locally → acknowledge or roll back. A failed older mutation may not undo a later choice. |
| Close/archive/worktree decision (H) | [close slice](../../src/stores/agentNode/closeSlice.ts), [close policy](../../src/lib/worktreeClose.ts), [prompt store](../../src/stores/worktreeClosePromptStore.ts) | Inspect → prompt → cancel, keep, or remove. Replacing a prompt settles its old resolver; only the chosen close may continue. |
| Blocked cleanup recovery (I) | [cleanup store](../../src/stores/blockedCleanupStore.ts) | Open/read → diagnose or retry → refresh; release-process confirmation → action. Closing retires dialog reads; a failed queue read is not an empty queue. Process release requires the selected confirmation. |
| Exit/restart confirmation (I) | [exit prompt store](../../src/stores/exitPromptStore.ts), [window guard](../../src/components/WindowCloseGuard/WindowCloseGuard.tsx) | Policy lookup → prompt → keep working or exit → retry on failure. Prompt mode chooses exit versus relaunch; cancellation/failure retracts expected-exit marking. |
| Updater (H) | [updater store](../../src/stores/updaterStore.ts), [updater adapter](../../src/lib/updater.ts) | Check → available → download → install → ready to restart; failures retain retry context. Sequence fences stale work; Keep Working preserves the restart affordance. |
| Companion task and initialization (H) | [Companion store](../../src/stores/companionStore.ts) | Attach listeners/read task → start → running/confirmation/cancellation → terminal result. Task generation and revision order events; palette visibility does not own execution. |
| Mesh settings save session (H) | [settings store](../../src/stores/meshSettingsStore.ts) | Hydrate → edit → saving → saved/error → idle. Mesh identity, draft revisions, and serialized writes prevent a previous Mesh's completion from changing the current draft. |
| Scratch Pad persistence (I) | [settings store](../../src/stores/meshSettingsStore.ts), [Scratch Pad](../../src/components/Probe/ScratchpadTab.tsx) | Load → dirty draft → debounce/flush → persist. The outgoing draft is written to its own Mesh on switch or unmount. |
| View mode and scope (H) | [UI store](../../src/stores/uiStore.ts), [view derivation](../../src/lib/viewModes.ts), [navigation](../../src/lib/navigation.ts) | Mesh/Pinned/All Nodes ↔ Single with remembered return scope. All Nodes clears Mesh selection at the setter; search narrows the chosen scope. |
| Probe context, destination, and working set (H) | [UI store](../../src/stores/uiStore.ts), [Probe context](../../src/lib/probeContext.ts), [working set](../../src/lib/probeWorkingSet.ts) | Follow selection ↔ pinned context; open/focus/evict destination. Context ownership and resource identity travel together; tab insertion order differs from eviction recency. |
| Node activity/group selection (I) | [activity store](../../src/stores/nodeActivityStore.ts), [activity derivation](../../src/lib/nodeActivities.ts) | Agent ↔ utility ↔ run overview; group/ungroup/prune. Selection remains valid when a representative or grouped member disappears. |
| Grid layout and gestures (I) | [layout store](../../src/stores/gridLayoutStore.ts), [resize hook](../../src/hooks/useResizable.ts), [grid splitter](../../src/components/AgentNodeView/GridSplitter.tsx) | Rest → drag/resize → commit/cancel. Saved layout belongs to its scope; pointer cancellation releases capture/listeners. |
| Path-keyed query/cache lifecycle (I) | [path cache](../../src/lib/pathInvalidatedCache.ts), [query hook](../../src/hooks/usePathInvalidatedQuery.ts) | Cold → fetching → fresh/stale/error → trailing refresh → inactive/evicted. Coalescing, invalidation, retention and active-subscriber ownership are distinct. |
| Probe resource lifecycle (I) | [resource](../../src/lib/probeResource.ts), [path reads](../../src/lib/probePathReads.ts), [tab adapter](../../src/lib/probeTabAdapter.ts) | Inactive → loading → loaded/error → refreshing → deactivate. Cancellation and context identity fence publication; failed refresh follows the resource's stale-data policy. |
| Settings resource loading (H) | [resource hook](../../src/components/AppSettings/useSettingsResources.ts), [data context](../../src/components/AppSettings/SettingsDataContext.tsx) | Per-resource loading → loaded/failed → retry. Request IDs own each resource; preference writes serialize around the same snapshot. |
| Global shortcut registration (I) | [shortcut hook](../../src/hooks/useGlobalShortcuts.ts), [dispatch](../../src/lib/shortcutDispatch.ts) | Focus → register → active → unregister on blur/unmount. Late registration must be cleaned up; one keystroke follows one guarded dispatch policy. |
| Toast/notification lifetime (I) | [toast store](../../src/stores/toastStore.ts), [input notifications](../../src/lib/agentInputNotifications.ts), [worktree notices](../../src/lib/worktreeNotifications.ts) | New → visible → dismissed/expired or replaced. Notice identity determines deduplication; durable backend cleanup state outlives a toast. |

## Terminal and process pipelines

The visible node status is only one machine. Terminal ownership, output delivery,
and input acceptance have separate lifetimes and must not be collapsed into it.

| Workflow / shape | Owning source | Transitions and invariant |
|---|---|---|
| Desktop terminal creation/retention (I) | [terminal registry](../../src/components/Terminal/TerminalRegistry.ts), [terminal view](../../src/components/Terminal/Terminal.tsx) | Absent → pending creation → retained → attached/detached → deleted. Each await can lose creation ownership; React remount does not dispose the singleton terminal. |
| Build/Run terminal ownership (I) | [Build/Run registry](../../src/components/Terminal/BuildRunTerminalRegistry.ts), [backend registry](../../src-tauri/src/commands/build_run.rs) | Create/launch → attached/running → exit/relaunch → close. Node ID alone is insufficient: process generation fences readers and exit events. |
| Terminal output batching (I) | [writer](../../src/components/Terminal/TerminalWriter.ts), [resize scheduler](../../src/components/Terminal/TerminalResizeScheduler.ts) | Queued → frame scheduled → writing → acknowledged/drained or unregistered. Completion-aware buffers and per-frame budgets must preserve byte order and release old work. |
| Renderer allocation (I) | [WebGL pool](../../src/components/Terminal/WebglRendererPool.ts) | Software → acquire renderer → active → evicted/context lost → software. Renderer disposal and terminal disposal have different ownership. |
| Terminal input backpressure (I) | [input queue](../../src/lib/terminalInputQueue.ts), [backend input](../../src-tauri/src/agent/terminal_input.rs) | Enqueued → writing → accepted, paced retry, stalled, or cancelled. One lane per node preserves input order; stall/retry does not duplicate accepted bytes. |
| Backend PTY output attachment (E) | [output sink](../../src-tauri/src/pty/sink.rs), [PTY lifecycle](../../src-tauri/src/pty/lifecycle.rs) | Buffered → live delivery; detach retains appropriate buffered output. Subscription changes must not reorder output. |
| Agent process registration/exit (H) | [process registry](../../src-tauri/src/agent/process/registry.rs), [spawn orchestrator](../../src-tauri/src/agent/spawn/orchestrator.rs), [reader](../../src-tauri/src/agent/spawn/reader.rs) | Prepare → registered/spawning → running → natural exit, kill, or recovery. A retired process may not reap or promote its replacement. |
| Initial prompt and unusable resume (I) | [prompt delivery](../../src-tauri/src/agent/spawn/startup_prompt.rs), [resume guard](../../src-tauri/src/agent/spawn/resume_guard.rs) | Prepared prompt → readiness → delivery; early unusable resume → invalidate resume identity. A prewritten session UUID does not prove a usable conversation. |
| Node lifecycle/attention (E) | [session lifecycle](../../src-tauri/src/agent/session_lifecycle/mod.rs), [status vocabulary](../../src-tauri/src/models/agent.rs) | Launch, turn start/end, user input, interrupt, suspend, archive, and exit route through guarded transitions. Hook delivery cannot revive stopped/terminal nodes. |
| Hook and transcript observation (I) | [hook state](../../src-tauri/src/agent/hook_state.rs), [attention ordering](../../src-tauri/src/http/routes/attention/ordering.rs), [transcript readers](../../src-tauri/src/services/transcript_reader/readers/) | Observe tool/turn/session → accumulate evidence → publish lifecycle/report. Session continuation and retired-process mismatch require different evidence. |
| Session-ID capture parser (I) | [chunk capture](../../src-tauri/src/session_capture.rs) | Buffer incomplete bytes → find session identity → latch/pass through. Before capture, the retained suffix spans chunk boundaries; released prefixes preserve byte order without duplication. |
| Node naming reservation/retry (I) | [naming engine](../../src-tauri/src/session_naming/engine.rs) | Eligible turn → reserve generation → read dialogue/infer → rename, retry, or retire. Concurrent turns share a reservation; a manual rename or reset invalidates older work and its cleanup. |
| Interruption continuation (I) | [interruption recovery](../../src-tauri/src/services/interruption_recovery.rs) | Observe interruption → pending continuation → recheck evidence → deliver or retire. New user/process/turn evidence invalidates older continuation authority. |
| Terminal fanout and scrollback (I) | [WebSocket fanout](../../src-tauri/src/http/ws.rs) | Known node → retained channel → replay/live stream; process restart clears bytes, deletion retires channel. A deleted row cannot recreate a channel through reconnect. |
| Diff work cancellation (I) | [cancel ownership](../../src-tauri/src/diff/cancel.rs), [diff engine](../../src-tauri/src/diff/engine.rs) | Start → compute → publish or supersede/cancel → release. An older guard must not release a newer cancellation token. |
| Terminal link press and touch scrolling (I) | [link router](../../src/components/Terminal/TerminalLinkRouter.ts), [touch pan](../../src/components/Terminal/attachTouchPan.ts) | Hover → press → matched release/activate or cancel; touch start → accumulate row deltas → end/cancel. Link activation has one owner; web mobile and Android's terminal bundle share the touch-scroll implementation. |

## Backend durable workflows and recovery

| Workflow / shape | Owning source | Transitions and invariant |
|---|---|---|
| Startup/profile/shutdown (H) | [startup](../../src-tauri/src/startup/), [instance guard](../../src-tauri/src/instance_guard.rs), [app entry](../../src-tauri/src/lib.rs) | Claim profile → initialize → ready/fatal; close → expected exit/suspend sweep. Profile ownership, startup failure, and crash recovery are separate facts. See [startup contract](startup-and-profiles.md). |
| Node create/start idempotency (E) | [create ledger](../../src-tauri/src/db/create_operation.rs), [create service](../../src-tauri/src/services/agent_node/create.rs) | Bind operation → claim start → record success/failure → replay. The operation key identifies one node; retry cannot silently spawn a second process. |
| Worktree warm pool (H) | [pool persistence](../../src-tauri/src/db/warm_pool.rs), [pool worker](../../src-tauri/src/services/warm_pool.rs), [backoff](../../src-tauri/src/services/warm_pool_backoff.rs) | Filling → available → claimed; refresh/reconcile/remove on drift or interrupted work. Persist the recovery marker before checkout; pool failure permits cold spawn. |
| Node delete and deferred cleanup (I) | [cleanup service](../../src-tauri/src/services/agent_node/cleanup.rs), [worktree blockers](../../src-tauri/src/worktree_blockers.rs) | Commit deletion/enqueue removal → drain → success, backoff, or keep worktree. Worktree ownership must be rechecked before removal; failed removal stays recoverable. |
| Circuit run/step execution (E) | [vocabulary](../../src-tauri/src/circuit/vocabulary.rs), [pure stepper](../../src-tauri/src/circuit/stepper.rs), [ledger](../../src-tauri/src/db/circuit/ledger.rs) | Queued/admitted → running/waiting/blocked/unverified → terminal; recovery creates a new attempt. Only verified evidence or an explicit operator outcome may advance the relevant step. |
| Circuit queue/capacity/lease (H) | [queue](../../src-tauri/src/db/circuit/queue.rs), [leases](../../src-tauri/src/db/circuit/leases.rs), [admission](../../src-tauri/src/services/circuit_worker/admission.rs) | Pending slot → claim/admit → heartbeat/renew → release/reclaim. Worker ownership and capacity constrain execution independently of run presentation. |
| Circuit effect execution/replay (H) | [effects](../../src-tauri/src/db/circuit/evidence/effects.rs), [dispatch](../../src-tauri/src/services/circuit_worker/dispatch.rs), [restart](../../src-tauri/src/services/circuit_worker/restart.rs) | Intent → external effect → committed receipt; restart reconciles uncertain outcomes. Never infer an external side effect did not happen merely because its receipt is absent; use [effect recovery policy](circuit-effect-recovery.md). |
| Circuit observation, readiness and report admission (H) | [observation](../../src-tauri/src/services/circuit_worker/observation.rs), [readiness](../../src-tauri/src/services/circuit_worker/readiness.rs), [report admission](../../src-tauri/src/circuit/report_admission.rs), [handoff](../../src-tauri/src/services/circuit_worker/result_handoff.rs) | Waiting for startup/turn/report → fresh evidence → admitted result or checkpoint. Old reports and quiet windows are not completion evidence. |
| Circuit operator/review/cancellation (H) | [control services](../../src-tauri/src/services/circuit_control/), [step recovery](../../src-tauri/src/db/circuit/step_recovery.rs), [review continuation](../../src-tauri/src/circuit/review_continuation.rs) | Pause/recheck/record outcome/retry/cancel/review again → new execution or terminal state. Operator outcomes do not constitute review approval; history remains intact. |
| Coordinator prompt driving (H) | [drive policy](../../src-tauri/src/coordinator/drive.rs), [drive ledger](../../src-tauri/src/db/drive.rs) | Claim idempotency key → validate/deliver → record verdict or release claim. Same key and different content is not a new authorized prompt. |
| Companion execution/confirmation (I) | [task owner](../../src-tauri/src/companion/mod.rs), [runner](../../src-tauri/src/companion/runner.rs) | Running → confirming → approved/declined, cancelling, or terminal result. Task generation/revision owns confirmation and tool effects; cancellation kills owned inference work. |
| Preferences health/save/recovery (H) | [storage](../../src-tauri/src/preferences/storage.rs), [store](../../src-tauri/src/preferences/store.rs), [recovery](../../src-tauri/src/preferences/recovery.rs) | Missing/healthy/corrupt → cached snapshot → atomic save, restore backup, or reset. Corruption is not empty defaults; source identity/revision invalidates derived menus. |
| State snapshot/restore (I) | [state recovery](../../src-tauri/src/services/state_recovery/mod.rs) | Inspect/snapshot → validate/plan → apply or refuse restore → restart as required. A restore plan is not permission to overwrite running state. |
| Provider discovery/menu/usage freshness (I) | [menu derivation](../../src-tauri/src/agent/provider_menu.rs), [usage cache](../../src-tauri/src/services/usage/cache.rs), [last-known usage](../../src-tauri/src/services/usage/last_known.rs) | Cold → probing → cached → invalidated/refresh; error may retain last-known data. Preference source identity/revision and detection epoch own menu freshness; usage freshness is explicit. |
| Provider login/verification (H) | [OAuth service](../../src-tauri/src/services/opencode_oauth.rs), [verification](../../src-tauri/src/services/provider_verification.rs), [preferences model](../../src-tauri/src/preferences/model.rs) | Device code/sign-in → pending → success, expiry, denial, or retry; pairing verification records capability evidence. A successful unrelated probe cannot verify the selected pairing. |
| Git synchronization/feed cache (I) | [sync policy](../../src-tauri/src/services/github/sync.rs), [Git sync lock](../../src-tauri/src/git/sync_lock.rs), [GitHub feeds](../../src-tauri/src/services/github/feeds.rs) | Read/cache → synchronize → apply/invalidate or error. Serialize relevant writes; partial or failed remote reads must not masquerade as authoritative empty results. |
| DB initialization/legacy retirement (I) | [migrations](../../src-tauri/src/db/migrations.rs), [retirement](../../src-tauri/src/db/legacy_retirement.rs) | Inspect version → evolve → compatible current schema; interrupted retirement → retry/reconcile. Existing data and migration repeatability constrain recovery. |

## Network and mobile clients

| Workflow / shape | Owning source | Transitions and invariant |
|---|---|---|
| HTTP listener and TLS realization (I) | [server](../../src-tauri/src/http/server.rs), [server state](../../src-tauri/src/http/state.rs), [TLS identity](../../src-tauri/src/http/tls/identity.rs) | Configure → bind → serve → rebind/stop; certificate generation/rotation changes trust. Desired exposure is not proof of a realized listener. |
| Pairing, sessions, WS tickets, pacing (I) | [pairing](../../src-tauri/src/http/pairing.rs), [HTTP owners](../../src-tauri/src/http/) | Invitation → exchange → device session; ticket mint → single bound upgrade or expiry/refusal. Authentication, authorization, one-use tickets, and rate limits have separate lifetimes. See [remote access](remote-access.md). |
| Web session recovery (H) | [mobile transport](../../src/mobile/api.ts) | Request 401 → shared refresh → replay once, rejected, or unavailable. Only rejected credentials permit re-pair; offline/rate-limit/error responses keep the credential. |
| Web navigation/auth recovery (H) | [mobile app](../../src/mobile/App.tsx), [launch visit](../../src/mobile/launchVisit.ts) | Bootstrap/connect → work/task/node screens; auth rejection → Connect. Visit identity owns launch navigation and sheets; backend work may finish after the initiating visit ends. |
| Web events connection (E) | [events hook](../../src/mobile/useWsEvents.ts) | `idle` → `minting` → `socket`; failure/deadline → `backoff`, 429 → `rate-limited`, auth rejection → `unauthorized`, cleanup → `disposed`. A 15-second deadline bounds the mint, including cookie recovery; retired work cannot publish or re-arm. |
| Web terminal connection (I) | [terminal screen](../../src/mobile/screens/TerminalScreen.tsx) | Mint → connect → live → bounded retry/countdown, rate-limited, auth failure, or user close. Explicit retry and foreground recovery cannot create parallel attempts; terminal input is sent only while open. |
| Web polling/snapshot reconciliation (I) | [visibility polling](../../src/mobile/useVisibilityPolling.ts), [node snapshot](../../src/mobile/components/nodeList/useNodeListSnapshot.ts) | Foreground refresh → settle → next tick; hidden/offline/unmount pauses or retires. Latest request and mutation revision own publication; a 503 preserves the last snapshot. |
| Web pull/swipe gestures (I) | [pull to refresh](../../src/mobile/usePullToRefresh.ts), [terminal screen](../../src/mobile/screens/TerminalScreen.tsx) | Touch start → direction/threshold tracking → trigger or cancel. Terminal scrollback owns its gestures; only the app bar owns swipe dismissal. |
| Android pairing and dashboard session (I) | [view model](../../android/app/src/main/java/dev/buildmesh/remote/BuildmeshViewModel.kt), [connection](../../android/app/src/main/java/dev/buildmesh/remote/Connection.kt), [session storage](../../android/app/src/main/java/dev/buildmesh/remote/SessionStore.kt) | Load/pair → verify trust/session → foreground refresh → pause, retry, or forget. Session generation fences old work; failed connectivity is distinct from device revocation. |
| Android remote socket (I) | [remote socket](../../android/app/src/main/java/dev/buildmesh/remote/RemoteSocket.kt) | Job starts → mint → socket/channel → reconnect loop → stop. One job owns its socket; generation prevents old cleanup from clearing a replacement. |
| Android shared event feed/watch (I) | [event feed](../../android/app/src/main/java/dev/buildmesh/remote/EventFeed.kt), [watch service](../../android/app/src/main/java/dev/buildmesh/remote/AgentWatchService.kt), [attention events](../../android/app/src/main/java/dev/buildmesh/remote/AttentionEvents.kt) | First listener → open/feed → reconnect; last detach/reload/revocation → stop. App and watch service share subscription ownership; stale sessions cannot revoke a newer one. |
| Android refresh and pull feedback (I) | [refresh controller](../../android/app/src/main/java/dev/buildmesh/remote/RefreshController.kt), [listing refresh](../../android/app/src/main/java/dev/buildmesh/remote/ListingRefresh.kt) | Idle → reading → publish/error → one queued read; stop/invalidate fences completion. Pull feedback settles for the requested read set. |
| Android navigation/actions/terminal bridge (I) | [app navigation](../../android/app/src/main/java/dev/buildmesh/remote/BuildmeshApp.kt), [terminal pane](../../android/app/src/main/java/dev/buildmesh/remote/TerminalPane.kt), [terminal input](../../android/app/src/main/java/dev/buildmesh/remote/TerminalKeyInput.kt) | Screen/sheet visit → action → success/error/dismiss; WebView loading → ready → streaming/disposed. Node/session identity owns action results, buffered output and input modes. |

## Local interaction families

These are real, smaller machines. Keeping them local is usually the deepest
interface: one view owns its draft, pending action, and recovery. Independent
preferences (for example enabling a provider and choosing a theme) are parallel
data, not mutually exclusive lifecycle states.

| Family / shape | Owning source and repeated instances | Transition contract to inspect |
|---|---|---|
| OpenCode account flow (E) | [account reducer](../../src/components/AppSettings/OpenCodeAccountCard.reducer.ts) and its views | Add/choose plan/device-code activation/poll/remove/error paths are already a discriminated reducer; keep effects tied to its current operation. |
| Account, provider and configuration editing (I) | [settings views](../../src/components/AppSettings/), [launch editor](../../src/components/Providers/LaunchConfigurationEditor.tsx), [launch list](../../src/components/Providers/LaunchConfigurations.tsx) | Clean → dirty → saving → acknowledged/error; delete confirmation is separate from save. Closing or changing the edited identity retires presentation authority. |
| Pairing invitation and certificate help (I) | [remote-access modal](../../src/components/RemoteAccess/RemoteAccessModal.tsx), [remote settings](../../src/components/AppSettings/RemoteAccessPane.tsx) | Discover host → mint invitation/QR → paired or expired → renew; trust generation → acknowledgment/reinstall. Renewing an invitation invalidates its old timers and QR result. |
| Mesh create/recovery/recolor (I) | [Mesh dialogs](../../src/components/Mesh/), [recovery hook](../../src/hooks/useMeshRecovery.ts) | Choose/edit → submit → acknowledged/error; recovery has independent restore/free operations. Failed writes retain the actionable form. |
| PR merge/check/spawn (H) | [PR Probe](../../src/components/Probe/GitPullRequestsTab.tsx), [PR pill](../../src/components/AgentNodeView/PrPill.tsx), [open PR hook](../../src/hooks/useOpenPr.ts), [mobile PRs](../../src/mobile/screens/PullRequests.tsx) | Confirm → merge → check/poll → merged, error, or exhausted; spawn is its own action. A response must belong to the selected repository/PR and operation. |
| Issue triage, archived resume and prompts (I) | [Probe views](../../src/components/Probe/), [prompt library](../../src/components/Prompts/), [web issues](../../src/mobile/screens/IssuesScreen.tsx), [web archive](../../src/mobile/screens/ArchivedNodesScreen.tsx), [Android task views](../../android/app/src/main/java/dev/buildmesh/remote/TaskScreens.kt) | Browse/filter/select → label/edit/delete/spawn/resume → success/error. An acknowledged launch remains discoverable after navigation; old reads cannot undo newer edits. |
| Circuit editor and intervention views (I) | [flow editor](../../src/components/Circuits/CircuitFlowEditor.tsx), [run overview](../../src/components/Circuits/CircuitRunOverview.tsx), [next steps](../../src/components/Circuits/RunNextStepsPanel.tsx), [web run](../../src/mobile/screens/CircuitRun.tsx), [Android runs](../../android/app/src/main/java/dev/buildmesh/remote/CircuitRuns.kt) | Edit/connect → dirty → save/discard; run action → pending → refreshed result/error. Rendered button state is not permission to advance the backend machine. |
| Mobile task sheets and request previews (I) | [create PR](../../src/mobile/screens/CreatePrSheet.tsx), [capture idea](../../src/mobile/screens/CaptureIdea.tsx), [start review](../../src/mobile/screens/StartReview.tsx), [node list views](../../src/mobile/components/nodeList/) | Draft → submit → acknowledged/error → dismiss. Sheet visit owns dismissal and result navigation; a previous sheet cannot close a new one. |
| On-demand reads and disclosure (I) | [Node Glance](../../src/hooks/useNodeGlance.ts), [history](../../src/components/Probe/AgentHistoryTab.tsx), [file tree](../../src/components/FileTree/FileTree.tsx), [mobile changes/diff](../../src/mobile/screens/ChangesScreen.tsx) | Closed/inactive → request → data/empty/error → close or switch identity. Stale completion must not reveal another node's content. |
| Menus, inline editing and modal dismissal (I) | [submenu hook](../../src/hooks/useSubmenu.ts), [ARIA menu](../../src/hooks/useAriaMenu.ts), [inline text](../../src/components/shared/InlineEditableText.tsx), [modal](../../src/components/shared/Modal.tsx), [omnibar](../../src/components/CommandOmnibar/CommandOmnibar.tsx) | Closed → open/focused → select/commit, escape, blur, or discard confirmation. Keyboard focus and pointer ownership must follow the rendered menu and its portal. |
| Copy/search/selection feedback (I) | [desktop terminal](../../src/components/Terminal/Terminal.tsx), [mobile terminal](../../src/mobile/screens/TerminalScreen.tsx), [copy payload](../../src/components/Circuits/CopyPromptPayload.tsx) | Select/search/copy → feedback → expire/close. Timed feedback belongs to its mounted view; scroll-to-tail follows user intent. |

## Decisions and follow-up priorities

The two shared event lifecycles were selected for implementation because their
failure modes were reproduced with controlled promises and public entrypoints.
They affect many consumers while allowing a small, reviewable change.

| Decision | UX | DX | AX |
|---|---|---|---|
| Explicit desktop subscription state and registration identity | Prevents retired events reaching replacement consumers and duplicate registrations after stale failures | One owner for attachment, failure, notification and teardown | Three legal states; regression tests demonstrate stale-success and stale-failure orderings |
| Explicit mobile event connection state covering ticket mint | Avoids duplicate sockets, excess ticket requests, and callbacks to a departed screen; a stalled mint times out and can retry | Socket, timer and pending mint occupy one lifecycle slot | Named terminal/pause states and one transition owner; no caller-specific reconnect guard |
| Retain existing domain machines; document their seams | Preserves launch, recovery, Circuit and editing workflows | Avoids a new global state-machine dependency or new wire/storage format | Owner links and invariants make changes local and explain which layer to test |

Next candidates, in order of impact, are **investigation priorities**, not
confirmed defects or authorization to change their semantics:

1. Exit/restart prompt: exercise concurrent provider lookups, Keep Working
   during preparation, repeated confirm, and lookup completion after a newer
   mode request. Consider a single preparing/confirming/exiting union only
   after locking down the return-value contract shared by both callers.
2. Mobile terminal connection: characterize its mint, countdown, exhaustion,
   rate-limit and auth behavior before replacing paired refs. Its bounded
   reconnect policy differs from the events stream; do not unify the two
   policies just because both use WebSockets.
3. Blocked cleanup dialog: check close/reopen during diagnose, retry, dismiss
   and confirmed process release, including completion against a new dialog
   visit. Keep durable queue mutation distinct from dialog publication.
4. Companion confirmation: exercise duplicate decisions and errors arriving
   after a newer task revision. Preserve the backend's revision/generation
   authority instead of making a button's disabled state the guard.
5. Local PR and configuration forms: use a local discriminated operation when
   mutually exclusive flags obscure a reproducible failure. Keep independent
   saves independent; an app-wide generic form machine would widen coupling.

For every candidate, establish both the displayed state and the permitted side
effects. Check failure/retry, cancellation, remount, stale success **and** stale
failure, event-versus-snapshot ordering, and external effects that outlive their
screen. A pure reducer cannot establish backend ownership or delivery by itself.

## Reproducing discovery and verification

Run from the repository root; generated bindings are descriptions of backend
types, not transition owners.

```powershell
rg -n 'useState|useReducer|inFlight|generation|revision|setTimeout|AbortController' src --glob '!**/generated/**'
rg -n 'enum .*State|enum .*Status|AtomicBool|AtomicU|status =|status:|cancel|claim' src-tauri/src --glob '!**/*tests*' --glob '!**/test*'
rg -n 'sealed|mutableStateOf|generation|Job\?|cancel|revision' android/app/src/main/java
$env:NODE_ENV = 'test'
npx vitest run tests/unit/stream-subscription-lifecycle.test.ts tests/unit/live-event-streams.test.ts tests/unit/live-event-stream-ownership.test.ts tests/unit/mobile-events-reconnect.test.tsx
npm run harness -- check
npm run verify
```

The focused suite exercises attachment/release, replacement, current failure
recovery, stale notifications, synchronous re-entry, ticket-mint overlap,
backoff, stalled-mint deadlines, auth rejection, rate limiting, and disposal. These are deterministic
frontend tests with fake sockets and backend registrations. They do not prove
native Tauri event delivery, server ticket validation, or real mobile network
recovery. Broader owner-specific tests and runtime evidence remain required
when changing those layers; see [the engineering contract](../agents/engineering.md).
