import { useCallback, useEffect, useRef, useState } from 'react';
import { gitBranch, isAuthError, listPulls, prMergeability, spawnFromPull, squashMerge } from '../api';
import type { Mesh } from '../api';
import type { GitHubPullRequest } from '../../types/generated/GitHubPullRequest';
import type { GitHubPullRequestFeed } from '../../types/generated/GitHubPullRequestFeed';
import { useVisibilityPolling } from '../useVisibilityPolling';
import { AppBar, Sheet } from '../ui';
import { SpawnSplitButton } from '../components/SpawnSplitButton';
import { SpawnMenuSheet } from '../components/SpawnMenuSheet';
import { useSpawnMenu } from '../useSpawnMenu';
import { meshDefaultLabel, meshDefaultProvider, type LaunchChoice } from '../spawnMenu';

type StateFilter = 'open' | 'closed';

function canMerge(pr: GitHubPullRequest) {
  return pr.state === 'open' && !pr.draft && pr.mergeable === true &&
    ['clean', 'has_hooks', 'unstable'].includes(pr.mergeable_state) &&
    !['failure', 'error', 'pending', 'expected'].includes(pr.checks_state ?? '') &&
    !['changes_requested', 'review_required'].includes(pr.review_decision ?? '');
}

function mergeLabel(pr: GitHubPullRequest) {
  if (pr.draft) return 'Draft';
  if (pr.mergeable === null) return 'Checking mergeability';
  if (pr.mergeable === false || pr.mergeable_state === 'dirty') return 'Conflicts';
  return ({ clean: 'Mergeable', blocked: 'Blocked by required checks or reviews', behind: 'Branch is behind base', unstable: 'Checks need attention', has_hooks: 'Mergeable' } as Record<string, string>)[pr.mergeable_state] ?? 'Mergeability unknown';
}

export function NodePullRequest({ meshId, nodeId, onAuthFailed }: { meshId: number; nodeId: number; onAuthFailed: () => void }) {
  // A node's own pull request list is read-only: it has no Spawn Menu, so no
  // launch is ever in flight on it.
  return <PullRequestList key={`${meshId}:${nodeId}`} meshId={meshId} nodeId={nodeId} onAuthFailed={onAuthFailed} spawningPr={null} onSpawningPrChange={() => {}} />;
}

export default function PullRequestsScreen({ mesh, onBack, onAuthFailed, backHandlerRef }: { mesh: Mesh; onBack: () => void; onAuthFailed: () => void; backHandlerRef?: { current: (() => boolean) | null } }) {
  // The Spawn Menu sheet belongs to the screen, not to the list, so the app
  // bar's back button and the App's back gesture can both dismiss it before
  // the screen pops (issue #2385).
  const [menuPr, setMenuPr] = useState<GitHubPullRequest | null>(null);
  /**
   * The pull request whose launch is in flight. It lives on the screen
   * because the sheet's dismissal is the screen's decision: a launch in
   * flight owns the sheet, so the back gesture is consumed without closing
   * it — the same rule the issue screen follows.
   */
  const [spawningPr, setSpawningPr] = useState<number | null>(null);
  const dismissMenu = useCallback(() => {
    if (!menuPr) return false;
    if (spawningPr === null) setMenuPr(null);
    return true;
  }, [menuPr, spawningPr]);
  useEffect(() => {
    if (!backHandlerRef) return;
    backHandlerRef.current = dismissMenu;
    return () => { backHandlerRef.current = null; };
  }, [backHandlerRef, dismissMenu]);
  return <div className="screen">
    <AppBar title="Pull requests" subtitle={mesh.name} onBack={() => { if (!dismissMenu()) onBack(); }} />
    <main className="mobile-page list-scroll">
      <PullRequestList key={mesh.id} meshId={mesh.id} mesh={mesh} onAuthFailed={onAuthFailed} menuPr={menuPr} onMenuPrChange={setMenuPr} spawningPr={spawningPr} onSpawningPrChange={setSpawningPr} />
    </main>
  </div>;
}

function PullRequestList({ meshId, mesh, nodeId, onAuthFailed, menuPr, onMenuPrChange, spawningPr, onSpawningPrChange }: {
  meshId: number;
  mesh?: Mesh;
  nodeId?: number;
  onAuthFailed: () => void;
  /** The pull request whose Spawn Menu sheet is open (the mesh-level list only). */
  menuPr?: GitHubPullRequest | null;
  /** Opens or closes the Spawn Menu sheet (the mesh-level list only). */
  onMenuPrChange?: (pr: GitHubPullRequest | null) => void;
  /** The pull request whose launch is in flight. */
  spawningPr: number | null;
  /** Records the launch in flight (the screen owns the dismissal decision). */
  onSpawningPrChange: (pr: number | null) => void;
}) {
  const [feed, setFeed] = useState<GitHubPullRequestFeed | null>(null);
  const [stateFilter, setStateFilter] = useState<StateFilter>('open');
  const [branch, setBranch] = useState('');
  const [error, setError] = useState('');
  const [spawnError, setSpawnError] = useState('');
  const [spawnNotice, setSpawnNotice] = useState('');
  const [notice, setNotice] = useState('');
  const [confirm, setConfirm] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const active = useRef(false);
  const version = useRef(0);
  // Polling reads the filter through a ref: the polling hook keeps the
  // latest `refresh` closure, so a filter change is picked up on the next
  // tick even before the state update lands.
  const filter = useRef<StateFilter>('open');
  const merging = useRef(false);
  // A launch is guarded by a ref, not by state: `setSpawningPr` is applied on
  // the next render, so two taps in the same tick would both read
  // `spawningPr === null` and start two agents for the same pull request.
  const spawning = useRef(false);
  // Only the mesh-level list offers the Spawn Menu; a node's own pull request
  // list is a read-only view of the branch this node already works on.
  const { choices, providers, error: menuError, retry } = useSpawnMenu(nodeId === undefined);
  useEffect(() => {
    active.current = true;
    version.current++;
    return () => { active.current = false; };
  }, []);

  async function refresh(isLatest: () => boolean = () => true, signal?: AbortSignal) {
    if (merging.current) return;
    const request = ++version.current;
    const owns = () => active.current && request === version.current && isLatest();
    try {
      // `signal` is the polling hook's dispose signal (issue #1576): an
      // unmounted screen cancels its own in-flight feed read.
      const [next, currentBranch] = await Promise.all([
        listPulls(meshId, filter.current, signal), nodeId === undefined ? Promise.resolve('') : gitBranch(nodeId, signal).then(b => b.branch),
      ]);
      if (!owns()) return;
      setConfirm(current => {
        if (current === null) return null;
        const previous = feed?.items.find(pr => pr.number === current);
        const updated = next.items.find(pr => pr.number === current);
        return currentBranch === branch && previous && updated && canMerge(updated) && previous.head_sha === updated.head_sha ? current : null;
      });
      setFeed(next); setBranch(currentBranch); setError('');
    } catch (e) {
      if (!owns()) return;
      setFeed(null); setConfirm(null);
      if (isAuthError(e)) onAuthFailed();
      else setError((e as Error).message);
    }
  }
  useVisibilityPolling(refresh, 10000);

  function changeFilter(next: StateFilter) {
    if (next === filter.current) return;
    filter.current = next;
    setStateFilter(next);
    // The old rows belong to the previous filter. Drop them so an open PR
    // cannot linger under the "Closed" heading while the new read is in
    // flight.
    setFeed(null);
    setConfirm(null);
    setError('');
    setNotice('');
    onMenuPrChange?.(null);
    void refresh();
  }

  async function merge(pr: GitHubPullRequest) {
    if (merging.current || !canMerge(pr) || confirm !== pr.number) return;
    merging.current = true; setBusy(true); setConfirm(null); setError(''); setNotice('');
    ++version.current;
    try {
      // Recheck GitHub immediately before the irreversible request; policy can change while confirming.
      const latest = await prMergeability(meshId, pr.number);
      if (!active.current) return;
      if (!canMerge({ ...pr, ...latest })) throw new Error('This pull request is no longer mergeable. Refresh its status.');
      await squashMerge(meshId, pr.number, pr.url);
      if (!active.current) return;
      setNotice(`PR #${pr.number} squash-merged. Branch deleted.`);
      setFeed(current => current && { ...current, items: current.items.filter(item => item.number !== pr.number) });
    } catch (e) {
      if (!active.current) return;
      if (isAuthError(e)) onAuthFailed();
      else setError(`Merge not confirmed: ${(e as Error).message}. Refresh before trying again.`);
      setFeed(null);
    } finally {
      merging.current = false;
      if (active.current) setBusy(false);
    }
  }
  // Start an agent on a pull request (issue #2385). `launch` is the Spawn Menu
  // row the user picked; no row means the mesh default, which the backend
  // resolves from its own default chain.
  async function spawn(pr: GitHubPullRequest, launch?: LaunchChoice | null) {
    // The ref guard is synchronous, so a second tap in the same tick cannot
    // start a second agent for the same pull request.
    if (spawning.current) return;
    spawning.current = true;
    onSpawningPrChange(pr.number);
    setSpawnError('');
    setSpawnNotice('');
    try {
      await spawnFromPull(meshId, pr, launch);
      onSpawningPrChange(null);
      onMenuPrChange?.(null);
      if (!active.current) return;
      setSpawnNotice(`Started an agent on PR #${pr.number}.`);
    } catch (e) {
      onSpawningPrChange(null);
      if (!active.current) return;
      if (isAuthError(e)) onAuthFailed();
      else setSpawnError((e as Error).message);
    } finally {
      spawning.current = false;
    }
  }
  const prs = feed?.items.filter(pr => nodeId === undefined || (branch && pr.head_ref === branch &&
    pr.head_repo_clone_url === `${pr.url.split('/pull/')[0]}.git`)) ?? [];
  // The main half of the split button starts the mesh's own default.
  const defaultLabel = meshDefaultLabel(mesh, providers);
  const defaultProvider = meshDefaultProvider(mesh, providers);
  return <section className="mobile-pulls" aria-label="Pull requests">
    <div className="pulls-heading"><h2>{nodeId === undefined ? (stateFilter === 'open' ? 'Open pull requests' : 'Closed pull requests') : "This node’s pull request"}</h2>
      <button className="chip-btn" disabled={busy} onClick={() => void refresh()}>Refresh PR status</button></div>
    {nodeId === undefined && <div className="filter-chips" aria-label="Pull request state">
      {(['open', 'closed'] as const).map(state => <button
        key={state}
        className="chip-btn"
        aria-pressed={stateFilter === state}
        disabled={busy}
        onClick={() => changeFilter(state)}
      >{state === 'open' ? 'Open' : 'Closed'}</button>)}
    </div>}
    {error && <p role="alert" className="inline-error">{error}</p>}
    {spawnError && !menuPr && <p role="alert" className="inline-error">{spawnError}</p>}
    {spawnNotice && <p role="status">{spawnNotice}</p>}
    {notice && <p role="status">{notice}</p>}
    {!feed && !error && !notice && <p>Loading pull requests…</p>}
    {feed && !feed.completeness.complete && <p className="health-note">Showing first {feed.completeness.returned}{feed.completeness.reported_total !== null ? ` of ${feed.completeness.reported_total}` : ''} pull requests. Results are incomplete ({feed.completeness.incomplete_reason}). {nodeId !== undefined && 'This node’s PR may be missing from this list.'}</p>}
    {feed && prs.length === 0 && <p>{nodeId === undefined ? `No ${stateFilter} pull requests in the returned results.` : !branch ? 'The node has no current branch.' : `No open pull request found for ${branch} in the returned results.`}</p>}
    {prs.map(pr => <article className="pull-request-card" key={pr.number}>
      <a href={pr.url} target="_blank" rel="noreferrer">PR #{pr.number}: {pr.title}</a>
      <p>{pr.head_ref} · {pr.state}{pr.state === 'open' ? ` · ${mergeLabel(pr)}` : ''}</p>
      <p>Checks: {({ success: 'passing', failure: 'failing', error: 'error', pending: 'pending', expected: 'expected' } as Record<string, string>)[pr.checks_state ?? ''] ?? 'not reported'}</p>
      <p>Review: {({ approved: 'approved', changes_requested: 'changes requested', review_required: 'required' } as Record<string, string>)[pr.review_decision ?? ''] ?? 'no decision reported'}</p>
      {nodeId === undefined && stateFilter === 'open' && <SpawnSplitButton
        label={defaultLabel}
        busyLabel="Starting…"
        provider={defaultProvider}
        testId={`pr-spawn-${pr.number}`}
        ariaLabel={`Start agent on pull request #${pr.number} with ${defaultLabel}`}
        busy={spawningPr === pr.number}
        disabled={spawningPr !== null}
        menuOpen={menuPr?.number === pr.number}
        onStart={() => void spawn(pr)}
        onToggleMenu={() => {
          setSpawnError('');
          onMenuPrChange?.(menuPr?.number === pr.number ? null : pr);
        }}
      />}
      {nodeId !== undefined && (confirm === pr.number ? <Sheet label="Confirm squash merge" onClose={() => setConfirm(null)}>
        <p>Squash-merge PR #{pr.number} and delete its branch on GitHub?</p>
        <div className="action-grid"><button className="btn-ghost" disabled={busy} onClick={() => setConfirm(null)}>Cancel</button>
          <button className="btn-primary" disabled={busy || !canMerge(pr)} onClick={() => void merge(pr)}>Confirm squash merge</button></div>
      </Sheet> : <button className="btn-primary" disabled={busy || !canMerge(pr)} onClick={() => setConfirm(pr.number)}>{busy ? 'Merging…' : 'Squash and merge'}</button>)}
    </article>)}
    {menuPr && <SpawnMenuSheet
      label="Choose a launch configuration"
      testId="pr-launch-menu"
      choices={choices}
      error={menuError}
      notice={spawnError}
      onRetry={retry}
      // A launch in flight owns the sheet: it cannot be dismissed out from
      // under the request, and its rows are locked so a second launch cannot
      // start.
      dismissible={spawningPr === null}
      onClose={() => onMenuPrChange?.(null)}
      onChoose={(choice) => void spawn(menuPr, choice)}
    />}
  </section>;
}
