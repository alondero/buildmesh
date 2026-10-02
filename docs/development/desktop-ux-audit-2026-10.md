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

## Next design pass

Start with the interaction polish in #2003 and the onboarding sequence in
#1538. Use #2004 as the checklist, preserving the existing design tokens and
the grouped tool menu. More delight here comes from clear next actions,
comfortable targets, visible recovery, useful empty states and reliable focus
before it comes from new animation or decoration.
