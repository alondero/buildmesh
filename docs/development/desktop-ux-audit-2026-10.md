# Desktop UX/UI audit — October 2026

Status: historical audit of baseline `7932f3f4`, performed October 2, 2026.
Audience: desktop maintainers and designers. Current behavior is documented in
the [user guide](../user-guide.md) and [design system](../../DESIGN.md).

## Coverage and evidence

Reviewed workspace layouts, sidebar/agent actions, session creation, the
command palette and grouped tool menu, all Settings panes, Remote Access,
Project Files and the diff overlay, and Probe destinations. Source inspection
covered recovery, loading/error/empty states, tokens, keyboard contracts,
scroll ownership and information architecture. Native walkthroughs used the
real Tauri frontend and Rust backend, an empty profile and a local Git fixture.
The audit considers opportunities for a calmer, modern interface as well as
functional defects; proposed regroupings are design judgments.

Windows WebView2 ran from this checkout with CDP 9224 and the isolated
`com.alond.buildmesh.uxaudit.dev` identifier. CSS viewport checks exercise
900, 1100, 1280, 1400, 1786 and 1920px widths in both themes. They do not prove
macOS caption behavior or native Windows Snap hit testing. GitHub/automation
surfaces were inspected in available empty/error states and in source; no paid
agent, external PR merge, or production Circuit was run. This is not a claim
that every provider or remote phone flow was tested live.

## Ranked top fifteen

P1 means a broken affordance, possible lost session, or an inaccessible
essential control. P2 means substantial discoverability, navigation or
consistency work. P3 is polish. All six P1 findings are implemented in this
session; the other nine are tracked in [#2004](https://github.com/alondero/buildmesh/issues/2004).

| Rank | Priority | Finding and evidence | Resolution |
|---|---|---|---|
| 1 | P1 | A root session bypasses the worktree-risk gate, so one close click can kill a live agent. The store also cancels scheduled input before the user accepts closure. | The shared close-prompt owner now confirms active root sessions; Cancel preserves the node, process and scheduled prompt. Cancel receives initial focus. [#1254](https://github.com/alondero/buildmesh/issues/1254). |
| 2 | P1 | The shared modal trap counts controls in mounted hidden Settings panes. The discard prompt includes the underlying form in its keyboard sequence. | Recompute visible, enabled, non-inert tabbable descendants on each Tab, respect positive tab order, and contain the discard layer. [#1573](https://github.com/alondero/buildmesh/issues/1573). |
| 3 | P1 | At 900px in Filtered mode, the Close button measured x=930..976 and was entirely outside the viewport. | Compact chrome yields space to window controls, abbreviates branding, and constrains both search fields. Regular wide layouts retain viewport centering. [#1572](https://github.com/alondero/buildmesh/issues/1572). |
| 4 | P1 | The active Remote Access dialog measured 805px high at 900x600 (top=-103); expanded instructions measured 1180px at 1280x800 (top=-190). | Bound the dialog to the viewport, pin the title/close row, and give its body one vertical scroller and responsive square QR images. [#1571](https://github.com/alondero/buildmesh/issues/1571). |
| 5 | P1 | A fresh profile reports “LAN exposure is on” when access is disabled. That sends a normal state into TLS troubleshooting. | Render off/loading/enabling/active/failed states with Enable, Retry, real TLS/interface diagnostics and readable hierarchy. [#1574](https://github.com/alondero/buildmesh/issues/1574). |
| 6 | P1 | A changed file in Project Files falls through to the external editor; the same file in Changed Files opens a diff. | Wire the tree's changed callback and open the overlay after the diff succeeds. Failed reads retain truthful state and a Review error; unchanged files retain editor behavior. [#1570](https://github.com/alondero/buildmesh/issues/1570). |
| 7 | P2 | First-run empty content presents ten shortcut lines before establishing repository/harness readiness. | A short, skippable setup sequence and calm next-action hierarchy. Existing [#1538](https://github.com/alondero/buildmesh/issues/1538), [#823](https://github.com/alondero/buildmesh/issues/823). |
| 8 | P2 | Settings default choices wait for expensive provider/WSL probes unrelated to their basic configuration. | Separate inexpensive choices from capability probes while preserving failed-load safety. Existing [#1936](https://github.com/alondero/buildmesh/issues/1936). |
| 9 | P2 | Search is visible, but filtering/sorting state lacks a complete control and reset surface. | Active chips, counts, provider/status choices, sort/direction and Clear all. Existing [#997](https://github.com/alondero/buildmesh/issues/997), [#988](https://github.com/alondero/buildmesh/issues/988). |
| 10 | P2 | Mesh grouping forces users to search each repository for waiting/failed agents. | An attention-first view with explicit mesh identity and recovery. Existing [#1373](https://github.com/alondero/buildmesh/issues/1373). |
| 11 | P2 | Related code, GitHub, automation and historical work still requires destination hopping. The grouped tool menu is already an improvement. | Build on that shipped grouping with shared context and lifecycle navigation; do not recreate the obsolete permanent rail. Existing [#1457](https://github.com/alondero/buildmesh/issues/1457), [#1458](https://github.com/alondero/buildmesh/issues/1458), [#1459](https://github.com/alondero/buildmesh/issues/1459), [#1462](https://github.com/alondero/buildmesh/issues/1462). |
| 12 | P2 | Probe body scroll ownership and horizontal containment vary; FileTree has unbounded error prose in a fixed-height row. | A consistent body/toolbar/error contract, tested with long content at 240px. Existing [#1464](https://github.com/alondero/buildmesh/issues/1464). |
| 13 | P2 | Settings uses a vertical tablist without arrow navigation, roving tabindex or tab/panel IDs. | Complete the advertised keyboard contract. [#2003](https://github.com/alondero/buildmesh/issues/2003). |
| 14 | P2 | Mesh colour targets are about 10px; sidebar Resume/Close relies on small hover glyphs. | Larger hit areas within compact rows and visible recovery actions. [#2003](https://github.com/alondero/buildmesh/issues/2003). |
| 15 | P3 | White-alpha hover fills and faded essential text remain; primary action hierarchy varies between surfaces. | Align elevations and action hierarchy with existing tokens; use predictable, reduced-motion-aware feedback. [#2003](https://github.com/alondero/buildmesh/issues/2003), existing contrast inventory [#740](https://github.com/alondero/buildmesh/issues/740). |

## Validation and limitations

The focused tests exercise hidden/inert/disabled controls, switching panes,
discard-layer traversal, root-session Cancel/Confirm, cancellation of scheduled
input, changed/unchanged tree routing and rejected diff loads. Browser checks
establish actual bounds and scrolling rather than relying on class assertions.
The native driver passed 943 assertions across the titlebar matrix, visible
Settings pane traversal, Remote Access enable/bounds/scrolling, and a real Git
diff opened from the tree at 240px. A separate real-PTY check verified Cancel
kept the process running and Confirm stopped it and deleted its node. The
tree callback carries both absolute selection and repository-relative action
paths; the overlay must receive the latter, including for Stage/Revert.
Controlled requests cover both completion orders, stale failures, leaving the
tree's owner, and alternating list/tree clicks. Enable/Retry keep focus on a
stable dialog control, and the native manual-pairing disclosure retains normal
Tab progression.
Screenshots are under [the audit evidence directory](../pr-screenshots/muscled-ritual-marigold/).

The initial frontend check passed 3,625 unit tests (one skipped) and 69
integration tests, compilation and lint, then failed the bundle budget at
1.53 MB raw/397.2 kB gzip. An isolated checkout at baseline `7932f3f4`
reproduced the budget failure (1.56 MB/397.0 kB). Later builds of the reviewed
changes passed that gate at 1.02 MB/319.0 kB without changing its limits.
Build-to-build chunk composition needs further investigation in the existing
open follow-up [#1750](https://github.com/alondero/buildmesh/issues/1750);
[#1568](https://github.com/alondero/buildmesh/issues/1568) introduced the gate.
The two changed-tree regression tests also fail on the original commit: the
editor opens instead of a diff, and a failed diff read never reports an error.

The final `scripts/check.ps1 all-ts` run passed all gates: 3,646 unit tests
(one skipped), 69 integration tests, compilation, lint, lint fixtures,
documentation and bundle checks. The final entry JS measured 1.02 MB raw /
319.1 kB gzip. The committed-diff agent check also passed.

## Standards

The first finish review identified one documented-standard breach: a diff
prefetch lacked request ownership. The corrected implementation invalidates
stale success/failure on newer selections, context changes and unmount, and
shares an abort owner across list/tree entrypoints. Controlled tests exercise
those transitions. No actionable code-smell findings were reported.

## Spec

The finish review identified five requirements to tighten within the six P1
fixes: require the changed-file callback for a badged tree, suppress stale diff
loads, preserve focus during Enable/Retry, include native disclosures in Tab
traversal, and share selection ownership across the list/tree. These are
implemented and covered by unit or real-WebView checks.

Review totals: one Standards and five Spec findings addressed; the worst
Standards concern was request ownership, and the worst Spec concern was
keyboard traversal. Both final reviews report zero remaining findings.

## Original follow-up plan

Start with the interaction polish in #2003 and the onboarding sequence in
#1538. Use #2004 as the checklist, preserving the existing design tokens and
the grouped tool menu. More delight here comes from clear next actions,
comfortable targets, visible recovery, useful empty states and reliable focus
before it comes from new animation or decoration.

## October 3 follow-up: findings 7–15

The implementation for [#2004](https://github.com/alondero/buildmesh/issues/2004)
uses base `d87596814c4701177013daa5115f4501838b130c`. The preceding sections
record the original audit and its six P1 fixes; the evidence below belongs to
this later follow-up. All nine remaining findings now have production changes,
behavior checks and inspected screenshots. Verification prerequisites remain
open as described below, so the PR remains a draft.

| Rank | Implemented outcome | Behavior evidence and inspected screenshots |
|---|---|---|
| 7 | Three readiness steps replace the shortcut wall. Skip/restore persists, Help retains advanced shortcuts, and runtime/login guidance opens Settings → Providers. The guide also appears in a selected empty repository while other repositories have agents; Terminal inherits its worktree setting. A repository permits a Terminal while harness detection is pending, failed or empty. Starting is single-flight and failures allow retry. | Controlled tests cover skip/restore and double clicks followed by failed spawn/retry. Real Windows and Ubuntu WSL Terminal PTYs started from readiness. [Before](../pr-screenshots/gh2004/readiness-before.png), [clean](../pr-screenshots/gh2004/readiness-after.png), [partial without a harness](../pr-screenshots/gh2004/readiness-partial-after.png), [offline at 900px](../pr-screenshots/gh2004/readiness-offline-900-after.png), [WSL readiness](../pr-screenshots/gh2004/readiness-wsl-after.png), [WSL Terminal](../pr-screenshots/gh2004/first-wsl-terminal-after.png). |
| 8 | A cheap, registered routing-catalog command reads saved preferences and cached startup detection independently of full provider probes. Unverified routes stay unavailable. Defaults require successfully loaded preferences; each resource owns pending/error/retry state and request fencing. Account, route and Launch Configuration mutations refresh both catalogs; Anthropic choices obey the live compatibility predicate and configuration errors keep specific remediation. | Controlled tests exercise stale success/failure, retry and unmount. Native IPC fault injection holds/rejects probes, proves four pickers stay enabled with real saved routing choices, then rejects preferences and proves all four disable. The original passive zero-write assertions did not establish write safety; the revision below attempts all four disabled controls and exercises successful saves after Retry. [Pending](../pr-screenshots/gh2004/settings-pending-after.png), [probe failure](../pr-screenshots/gh2004/settings-probe-error-after.png), [preferences failure](../pr-screenshots/gh2004/settings-preferences-error-after.png). |
| 9 | Filtered mode exposes persisted provider/status filters, sort/direction, matching counts, removable chips, popover Clear all and a one-action header reset. Compact controls preserve the caption-button budget. | Production-store tests cover combined filters, sorting and reset. Native checks cover zero results, debounced-search persistence after reload and header bounds in both themes at 900/1280/1920px. [Dark zero-results state](../pr-screenshots/gh2004/filters-after.png), [light at 900px](../pr-screenshots/gh2004/filters-light-900-after.png). |
| 10 | Attention consolidates failed, lost, waiting and suspended agents across Meshes. Full names wrap, repository identity stays visible, and Open terminal/Retry/Resume use existing cross-repository focus and spawn behavior. Workspace preserves its grouping and order. | Tests cover lost agents, recovery failures and reveal from Mesh/Pinned/Filtered modes. Native checks use two repositories and long names, then assert keyboard traversal from Open terminal to Resume. [Fleet](../pr-screenshots/gh2004/attention-after.png), [keyboard focus](../pr-screenshots/gh2004/attention-keyboard-after.png). |
| 11 | Files/Changes and Issues/Pull Requests share group slots and subview controls. Stable destination IDs preserve commands, deep links and per-destination pins; switching siblings retains the four-group working set. Agent History becomes a Host lifecycle finder with repository/status/search filters, archived rows and explicit Reopen versus Resume. Circuits remains the automation destination; removed Policies is not recreated. | ADR 0039 defines selection, pinning, breadcrumbs and migration. Reducer/rail tests cover sibling replacement and the group cap. The revision below adds actual restart restoration, projection, subview controls, keyboard navigation, baselines and per-view pin coverage. Native archived-history Reopen preserves node/session identity, returns Suspended and starts no process; revised tests return an empty collection or reject the actual subsequent history read and verify the adopted row remains visible. Failed initial reads show unavailable rather than an invented empty history. [Files group](../pr-screenshots/gh2004/files-expanded-240-after.png), [GitHub group](../pr-screenshots/gh2004/github-240-after.png), [Host history](../pr-screenshots/gh2004/history-240-after.png), [archived work](../pr-screenshots/gh2004/history-archived-after.png), [idle Circuits](../pr-screenshots/gh2004/automation-240-after.png). |
| 12 | The Probe shell owns layout; bodies explicitly contain horizontal overflow. Project Files owns one body scroller below its path header and bounded error/recovery region. FileTree reports its owned error/retry to that pinned region, retains fenced directory reads, and bounds errors in other hosts. Changes also keeps bounded recovery above its list scroller. Notes retains its mesh-aware implementation. | Native checks measure a 240 CSS-pixel panel, loading/empty/error states, long unspaced errors, Retry success and a nested expanded folder. The final Files check proves Retry is inside the visible panel before any scroll. PR search and Refresh remain unobstructed at 240px. Notes remains reachable. [Changes loading](../pr-screenshots/gh2004/changes-loading-240-after.png), [long Changes error](../pr-screenshots/gh2004/changes-error-240-after.png), [Changes empty](../pr-screenshots/gh2004/changes-empty-240-after.png), [pinned Files error and Retry](../pr-screenshots/gh2004/files-error-240-after.png), [expanded tree](../pr-screenshots/gh2004/files-expanded-240-after.png). |
| 13 | Settings has one tabbable selected tab, Up/Down wrapping, Home/End navigation and explicit tab/panel relationships. Hidden panes stay mounted to retain draft forms. | Behavior tests and native focus assertions cover the roving contract; the full Settings regression suite preserves dirty-form behavior. [Before](../pr-screenshots/gh2004/settings-before.png), [visible selected-tab focus](../pr-screenshots/gh2004/settings-light-after.png). |
| 14 | Sidebar colour, Resume, Close and spawn disclosure controls provide at least 24×24 CSS-pixel targets. Recovery actions remain visible without hovering; accessible names identify actions and sessions. FileTree disclosures/rows retain the same minimum. | Native checks measure the actual targets and focusability, plus the Attention keyboard sequence. [Workspace before](../pr-screenshots/gh2004/workspace-before.png), [visible recovery](../pr-screenshots/gh2004/attention-keyboard-after.png), [expanded disclosure](../pr-screenshots/gh2004/files-expanded-240-after.png). |
| 15 | The touched hover surfaces and essential text use existing elevation/text/status tokens. Empty-state primary actions use solid accent with inverse text; compact secondary actions follow DESIGN.md. Shared transitions respect the existing reduced-motion rule. | Native checks compare both themes at 900×600, 1280×800 and 1920×1080 and assert reduced-motion transition duration. The screenshot pairs above show the hierarchy and theme changes; no palette or dependency was added. |

### Follow-up validation boundaries

The real Windows WebView2 runtime used CDP 9334 and the isolated
`com.alond.buildmesh.ux2004.dev` app-data profile. The original `7d27edba` implementation
passed 99 functional/bounds/focus assertions across the fleet, Settings,
readiness, history and narrow-inspector walkthroughs. Local Git fixtures,
archived database fixtures and Windows/Ubuntu PTYs exercised the registered
Rust commands. All fixture Meshes and their processes were removed afterward.
The [27 inspected screenshots](../pr-screenshots/gh2004/) include three baseline
images and the resulting surfaces. Some native captures include WebView2's
1.5 device-pixel ratio; panel widths and target sizes were asserted in CSS
pixels, not inferred from PNG dimensions.

Controlled native `fetch` interception of Tauri IPC supplied delayed provider
responses, failed preferences, absent harnesses and long Files/Changes errors.
Successful routing choices, directory Retry, lifecycle Reopen and Terminal
spawn used the real backend. Offline evidence disables browser networking
while leaving local IPC available. It does not simulate every provider login,
WSL distribution, macOS caption layout or Windows Snap hit-test behavior.
GitHub is covered in the available no-remote state and Circuits in its idle
state; no paid harness, external PR merge or automation run was executed.

| Executed check | Result | Boundary |
|---|---|---|
| `npm test -- --maxWorkers=4` | 3,773 passed, one skipped, 270 files passed | Original follow-up full unit/integration run, including pinned Files recovery; revised evidence is recorded below. An earlier default-worker run had one browser-startup timeout; isolated browser retry passed four tests, then complete runs passed. |
| `cargo test --locked --lib -- --test-threads=1` | 4,033 passed, 27 ignored | Full Rust library suite, serial process-global fixtures. After test-only lint cleanup, 42 provider-menu tests also passed. |
| `cargo clippy --locked --all-targets` | Passed command; existing warnings remain | No new warning in the touched files; unrelated warning cleanup is separate work. |
| Playwright `verify-smoke`, isolated Vite port 1443 | Five passed | Mock IPC, real browser/xterm. Distinct from native backend evidence above. |
| Production `npm run build` then `npm run check:bundle` | Passed; entry JS 1.03 MB raw / 324.4 kB gzip | Unchanged budget. This is separate from the harness build environment. |
| `npm run verify` | Failed bundle gate; 14 of 20 planned gates executed | Agent/documentation/README/process-spawn checks, infrastructure tests, ESLint/fixtures and frontend compilation passed before the bundle failure. Later planned gates did not run in this command. |
| `cargo fmt --all -- --check` | Failed on change and unchanged base | Repository-wide pre-existing formatting drift, reproduced against `d87596814c4701177013daa5115f4501838b130c`; tracked in [#1543](https://github.com/alondero/buildmesh/issues/1543). |

The harness forces `NODE_ENV=test` into build subprocesses. On the unchanged
base, that bundle fails at 1.62 MB raw / 400.1 kB gzip; the same base built with
Vite's production environment passes at 1.02 MB / 320.3 kB. The changed tree
also passes the production budget. [#2013](https://github.com/alondero/buildmesh/issues/2013)
tracks the harness environment defect without relaxing the budget. The draft
PR records the failed harness receipt and does not claim a completed harness
task. Formatting and harness environment are the remaining verification
prerequisites.

### Follow-up review

Initial independent Standards and Spec reviews found three and seven actionable
findings respectively. The final pass also caught unbounded Files recovery
and missing 1920×1080 evidence. Files recovery is now pinned and verified
without scrolling, and both themes passed twelve additional assertions at
1920×1080. The implementation fences request ownership,
enforces group identity in the working-set reducer, prevents duplicate
Terminal starts, adopts reopened rows, refreshes cheap choices after mutations,
contains actual body overflow, preserves visible retry controls and reveals
cross-Mesh selections. Those approvals were superseded by the independent REQUEST_CHANGES review of `7d27edba`, which identified the behavior and evidence gaps addressed below.
The durable contract is recorded in [ADR 0039](../adr/0039-desktop-audit-navigation-and-readiness.md),
[the user guide](../user-guide.md), [DESIGN.md](../../DESIGN.md), the
[architecture primer](../knowledge-primer.md) and
[Probe checklist](probe-ui-checklist.md).

### Independent review correction round

The independent review of `7d27edba` requested changes. Its ten findings were
valid and are addressed in this revision; the earlier approvals above are
historical evidence. No additional review was initiated in this fix round.

| Review finding | Correction and evidence |
|---|---|
| 1: Terminal worktree override | The readiness action omits the override, inheriting `mesh.use_worktree` like the other spawn surfaces. A controlled test invokes the actual store/IPC adapter; native creation retains `use_worktree=true`, starts its PTY and `pwd` confirms the worktree. Creation errors use App's store-error toast once. |
| 2: invented empty history | Failed initial reads display **Agent history unavailable**, with the error and Retry. A rendered failure/Retry test and native 240px capture distinguish failure from a successful empty result. |
| 3: overwritten remediation | Pending runtime guidance is applied only when a row has no specific unavailability reason. Rust regressions preserve missing-credential, disabled-account and incompatible-model errors. |
| 4: incompatible cheap choices | Anthropic routes use the live menu's pure launchability predicate, including compatibility and nonblank credentials. A Rust catalog regression covers available, incompatible and blank-key routes without probing. |
| 5: stale Launch Configuration catalogs | The Settings owner refreshes both catalogs after save/delete. The actual editor/IPC-adapter test and real native save/delete exercise both mutations while live provider probes are held; cheap choices update independently. |
| 6: setup lands on General | Settings accepts an initial pane through the UI store and TitleBar; readiness requests Providers. Modal and TitleBar tests pin the chain, and native clicks open Providers directly. Ordinary opens reset to General. |
| 7: vacuous write assertions | Removed the passive assertions. All four disabled pickers receive attempted interactions; after preference Retry, all four perform actual setting IPC writes in the rendered test. Native attempted clicks open no menus and issue no default-setting writes. |
| 8: nonexistent refresh failure | Reopen tests now return `[]` or reject the second actual history read. Both retain the adopted Suspended row and saved session without spawning. |
| 9: global empty-state gate | Readiness uses the current canvas scope. A rendered test selects an empty Mesh while another has work; native pending/failure checks keep Terminal available in that state at 900×600. |
| 10: missing ADR coverage | Tests cover a fresh store's saved Files subview, the storage key and invalid values, grouped projection, rendered Files/GitHub subview controls, Arrow/Home/End keys, selected state, comparison baselines, independent pins, and real readiness pending/failure/Retry transitions. |

Additional corrections let Escape close filters while focus remains on the
trigger, remove the stale onboarding comment, scope the readiness list assertion,
give group slots a distinct type, and use destination metadata as the fallback
for subview labels. The cheap catalog now reads the already observed default
WSL distribution without initiating cold discovery; a regression verifies both
cold and populated cache behavior. Provider IPC passes its successfully read
preferences into menu composition; an unreadable-file test distinguishes its
error from internal discovery's tolerant fallback.

Skipping the guide remains an intentional application-level preference; changing
repositories does not undo a user's choice. A test proves Show setup guide and
Start Terminal remain available on another empty repository. No storage version
change is needed for these same three steps. The pre-existing unified Diff row's
horizontal containment is outside this revision; this PR changed only its tokens.
It is tracked separately in [#2015](https://github.com/alondero/buildmesh/issues/2015).

The native revision passed **42 assertions** in an isolated Windows WebView2
runtime over CDP 9334. History failure, held/rejected provider probes and failed
preferences were controlled IPC faults. Terminal creation/spawn, `pwd`, history
Retry, and Launch Configuration save/delete used registered Rust commands.
All created Meshes, configurations and PTYs were removed afterward.

| Before review fixes | After review fixes |
|---|---|
| [History read failure showing an empty-history claim](../pr-screenshots/gh2004/review-history-before.png) | [History unavailable with visible Retry at 240px](../pr-screenshots/gh2004/review-history-after.png) |
| [Setup action landing on General](../pr-screenshots/gh2004/review-setup-before.png) | [Setup action landing on Providers while checks run](../pr-screenshots/gh2004/review-setup-after.png) |

The [empty selected Mesh with another live repository at 900px](../pr-screenshots/gh2004/review-empty-mesh-900-after.png)
and [Terminal reporting its worktree](../pr-screenshots/gh2004/review-terminal-worktree-after.png)
were also inspected. These six captures supplement the original 27, rather than
replacing their historical evidence.

The final full frontend suite passed **3,786 tests, one skipped, 271 files**;
the final 73 focused regressions also passed after strengthening the subsequent
history-read assertions. The full serial Rust library suite passed **4,037 tests, 27 ignored**. The final
provider-menu run also passed **46 tests** after test-fixture lint cleanup.
All-target Clippy passed with no new warnings; existing environment documentation
and unrelated warnings remain. TypeScript and ESLint passed. The production
build and unchanged bundle budget passed at **1.03 MB raw / 324.4 kB gzip**.
The canonical harness's test-mode bundle defect (#2013) and repository formatting
drift (#1543) remain the previously established validation prerequisites; the PR
stays a draft pending those prerequisites and independent review.

The isolated runtime produced no panic files. Its log includes the injected
read/probe failures and existing startup resize, pre-spawn Git summary,
missing-origin and duplicate-global-shortcut messages; these are not treated as
a clean-log claim.
