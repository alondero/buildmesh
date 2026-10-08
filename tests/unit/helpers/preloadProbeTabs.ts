/**
 * Load every `React.lazy` tab chunk of `ProbePanel` before a test renders it.
 *
 * `ProbePanel` pulls each tab in with a dynamic `import()`, which goes to disk
 * the first time. Tests that render a tab and then look for its content either
 * query synchronously after one `await act(async () => {})` flush or use
 * `waitFor`'s 1 s default; both only hold if that import settles quickly, and on
 * a loaded machine (the full suite runs ~23 workers) it did not, so the tab's
 * content was "not found". With the modules already loaded, the later lazy
 * resolution is a microtask whatever the disk is doing.
 *
 * Keep this list in step with the `lazy(() => import(...))` block in
 * `src/components/Probe/ProbePanel.tsx`.
 *
 * Usage: `beforeAll(preloadProbeTabs, 60_000);`
 */
export async function preloadProbeTabs(): Promise<void> {
  await Promise.all([
    import('../../../src/components/Probe/ProjectFilesTab'),
    import('../../../src/components/Probe/AgentChangesTab'),
    import('../../../src/components/Probe/ProjectSettingsTab'),
    import('../../../src/components/Probe/RepositoryTab'),
    import('../../../src/components/Probe/CircuitsProbeTab'),
    import('../../../src/components/Probe/GitIssuesTab'),
    import('../../../src/components/Probe/GitPullRequestsTab'),
    import('../../../src/components/Probe/AgentHistoryTab'),
    import('../../../src/components/Probe/ScratchpadTab'),
    import('../../../src/components/Probe/UsageTab'),
  ]);
}
