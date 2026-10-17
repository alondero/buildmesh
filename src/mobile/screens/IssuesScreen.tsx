import { useEffect, useRef, useState, type RefObject } from "react";
import {
  AgentNode,
  GitHubIssue,
  Mesh,
  listIssues,
  spawnFromIssue,
  isAuthError,
} from "../api";
import { AppBar, CenterNote, PulseDots } from "../ui";
import { useAsyncEffect } from "../../hooks/useAsyncEffect";
import type { LaunchVisit } from "../launchVisit";
import type { GitHubIssueFeed } from "../../types/generated/GitHubIssueFeed";
import { FeedCompletenessNote } from "../../components/Probe/FeedCompletenessNote";
import { IssueTags } from "./IssueTags";
import { IssueLaunchChoice } from "./IssueLaunchChoice";

type Props = {
  mesh: Mesh;
  /// Identifies this visit to the screen. Minted per visit by the App, so a
  /// return to the same Mesh's Issues screen is a new visit (issue #2327).
  visitId: number;
  onBack: () => void;
  /// Reports the created node together with the visit that started the launch,
  /// so the App can decide whether this completion still owns navigation.
  onSpawned: (node: AgentNode, visit: LaunchVisit) => void;
  onAuthFailed?: () => void;
  backHandlerRef?: RefObject<(() => boolean) | null>;
};

const BODY_PREVIEW_LIMIT = 600;

export default function IssuesScreen({
  mesh,
  visitId,
  onBack,
  onSpawned,
  onAuthFailed,
  backHandlerRef,
}: Props) {
  const [feed, setFeed] = useState<GitHubIssueFeed | null>(null);
  const issues = feed?.items ?? null;
  const [error, setError] = useState<string | null>(null);
  const [busyIssue, setBusyIssue] = useState<number | null>(null);
  const [filter, setFilter] = useState("");
  // Spawning creates a worktree and starts a real agent — first tap expands
  // the issue (body preview, GitHub link), the explicit button commits.
  const [selectedIssue, setSelectedIssue] = useState<number | null>(null);
  const [launchIssue, setLaunchIssue] = useState<GitHubIssue | null>(null);
  const lifetime = useRef<AbortSignal | null>(null);
  const pending = useRef(false);
  const dismissLaunch = () => {
    if (!launchIssue) return false;
    if (!pending.current) { setLaunchIssue(null); setError(null); }
    return true;
  };
  useEffect(() => {
    if (!backHandlerRef) return;
    backHandlerRef.current = () => {
      if (!launchIssue) return false;
      if (!pending.current) { setLaunchIssue(null); setError(null); }
      return true;
    };
    return () => { backHandlerRef.current = null; };
  }, [backHandlerRef, launchIssue]);

  useAsyncEffect((signal) => {
    lifetime.current = signal;
    setFeed(null);
    setError(null);
    setSelectedIssue(null);
    setLaunchIssue(null);
    setBusyIssue(null);
    setFilter("");
    // `pending` guards one launch per visit, so a visit that has moved on has
    // to release it: `spawn` early-returns while it is set, and `dismissLaunch`
    // and the Back gate both refuse while it is set too, so leaving it behind
    // would leave the sheet both undismissable and unlaunchable (issue #2327).
    pending.current = false;
    listIssues(mesh.id)
      .then((i) => {
        if (signal.aborted) return;
        setFeed(i);
      })
      .catch((e) => {
        if (signal.aborted) return;
        if (isAuthError(e)) {
          onAuthFailed?.();
          return;
        }
        setError((e as Error).message);
      });
    // `visitId` is a dep for the same reason `mesh.id` is: a new visit is a new
    // screen load, and the read effect's cleanup is what aborts the previous
    // visit's in-flight launch (issue #2327).
  }, [mesh.id, visitId, onAuthFailed]);

  const spawn = async (issue: GitHubIssue, provider?: string) => {
    if (pending.current) return;
    pending.current = true;
    const owner = lifetime.current;
    // Captured before the await, so the App can recognise this completion as
    // belonging to *this* visit rather than whichever one is on screen later.
    const ownerVisit: LaunchVisit = { kind: "issues", meshId: mesh.id, visitId };
    // `useAsyncEffect` aborts on cleanup, and `visitId` is a dep of the read
    // effect, so this covers unmount and a new visit alike. Whether it still
    // owns *navigation* is the App's call, not this screen's.
    const retired = () => owner?.aborted === true;
    setBusyIssue(issue.number);
    setError(null);
    try {
      const node = await spawnFromIssue(mesh.id, issue, provider);
      if (retired()) return;
      setBusyIssue(null);
      setLaunchIssue(null);
      onSpawned(node, ownerVisit);
    } catch (e) {
      if (retired()) return;
      setBusyIssue(null);
      if (isAuthError(e)) {
        onAuthFailed?.();
        return;
      }
      setError((e as Error).message);
    } finally {
      pending.current = false;
    }
  };

  const query = filter.trim().toLowerCase();
  const filtered = (issues ?? []).filter(i => !query || i.title.toLowerCase().includes(query) || String(i.number).includes(query.replace(/^#/, "")) || i.labels.some(label => label.toLowerCase().includes(query)));
  const openNumbers = new Set(issues?.map(issue => issue.number));

  return (
    <div data-testid="issues-screen" className="screen">
      <AppBar onBack={() => { if (!dismissLaunch()) onBack(); }} title="GitHub Issues" subtitle={mesh.name} />
      <div style={{ padding: 12, paddingBottom: 0 }}>
        <input
          placeholder="Filter issues…"
          aria-label="Filter issues"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          data-testid="issues-filter"
          className="field"
        />
      </div>
      <div style={{ flex: 1, overflowY: "auto", padding: 12 }}>
        {feed && <FeedCompletenessNote completeness={feed.completeness} label="issues" />}
        {error && !launchIssue && (
          <div style={{ color: "var(--red)", padding: 12, fontSize: 13 }}>
            {error}
          </div>
        )}
        {!error && issues === null && (
          <div style={{ padding: 24, textAlign: "center" }}>
            <PulseDots />
          </div>
        )}
        {issues !== null && filtered.length === 0 && (
          <CenterNote testId="issues-empty">
            {issues.length === 0
              ? "No open issues — or this mesh has no GitHub remote."
              : "No matches."}
          </CenterNote>
        )}
        {filtered.map((issue) => {
          const owner = lifetime.current;
          const open = selectedIssue === issue.number;
          const busy = busyIssue === issue.number;
          const body = issue.body.trim();
          return (
            <div
              key={issue.number}
              role="button"
              tabIndex={0}
              aria-expanded={open}
              // Issue #1259 — the card announces role="button" but ignored
              // Enter and Space, so keyboard / switch-access users could
              // focus but not expand (WCAG 2.1.1, 4.1.2). Mirror the native
              // <button> activation keys; preventDefault stops Space from
              // scrolling the page between repeated activations.
              onClick={() => setSelectedIssue(open ? null : issue.number)}
              onKeyDown={(e) => {
                // Only respond when the card itself is the focused element.
                // The nested "Start agent" button and "View ↗" link handle
                // their own keyboard activation; without this guard, a
                // bubbled keydown from them would collapse the card on top
                // of their own action.
                if (e.target !== e.currentTarget) return;
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setSelectedIssue(open ? null : issue.number);
                }
              }}
              data-testid={`issue-${issue.number}`}
              className="card"
              style={{
                display: "block",
                borderColor: open ? "var(--border-strong)" : "transparent",
              }}
            >
              <div
                style={{
                  display: "flex",
                  gap: 8,
                  alignItems: "baseline",
                  marginBottom: 4,
                }}
              >
                <span style={{ color: "var(--text-dim)", fontSize: 12, flexShrink: 0 }}>
                  #{issue.number}
                </span>
                <span
                  style={{
                    fontSize: 13,
                    color: "var(--text)",
                    fontWeight: 500,
                    overflow: "hidden",
                    flex: 1,
                    ...(open
                      ? { whiteSpace: "normal" as const }
                      : { textOverflow: "ellipsis", whiteSpace: "nowrap" as const }),
                  }}
                >
                  {issue.title}
                </span>
              </div>
              {issue.author && <div style={{ fontSize: 12, color: "var(--text-dim)" }}>@{issue.author}</div>}
              {issue.blocked_by.some(number => openNumbers.has(number)) && <div style={{ fontSize: 12, color: "var(--red)", overflowWrap: "anywhere" }}>Blocked by {issue.blocked_by.filter(number => openNumbers.has(number)).map(number => `#${number}`).join(", ")}</div>}
              <IssueTags key={`${mesh.id}-${issue.number}`} meshId={mesh.id} issue={issue} onAuthFailed={onAuthFailed} onError={message => { if (!owner?.aborted) setError(message); }} onChange={(label, present) => {
                if (owner?.aborted) return;
                setFeed(current => current && ({ ...current, items: current.items.map(row => row.number !== issue.number ? row : { ...row, labels: [...row.labels.filter(value => value.toLowerCase() !== label.toLowerCase()), ...(present ? [label] : [])] }) }));
              }} />
              {open && (
                <div onClick={(e) => e.stopPropagation()}>
                  {body && (
                    <div
                      data-testid={`issue-body-${issue.number}`}
                      style={{
                        marginTop: 10,
                        fontSize: 12,
                        lineHeight: 1.5,
                        color: "var(--text-dim)",
                        whiteSpace: "pre-wrap",
                        overflowWrap: "anywhere",
                        maxHeight: 180,
                        overflowY: "auto",
                        background: "var(--bg)",
                        borderRadius: 8,
                        padding: 10,
                      }}
                    >
                      {body.length > BODY_PREVIEW_LIMIT
                        ? body.slice(0, BODY_PREVIEW_LIMIT) + "…"
                        : body}
                    </div>
                  )}
                  <div
                    style={{
                      marginTop: 10,
                      display: "flex",
                      gap: 8,
                      alignItems: "center",
                    }}
                  >
                    <button
                      className="btn-primary"
                      style={{ flex: 1 }}
                      disabled={busyIssue !== null}
                      data-testid={`issue-spawn-${issue.number}`}
                      onClick={() => { setError(null); setLaunchIssue(issue); }}
                    >
                      {busy ? "Spawning agent…" : "Start agent on this issue"}
                    </button>
                    <a
                      href={issue.url}
                      target="_blank"
                      rel="noreferrer"
                      onClick={(e) => e.stopPropagation()}
                      className="btn-ghost"
                      data-testid={`issue-view-${issue.number}`}
                      style={{ textDecoration: "none", lineHeight: 1.2 }}
                    >
                      View ↗
                    </a>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
      {launchIssue && <IssueLaunchChoice busy={busyIssue !== null} error={error} onCancel={() => { setLaunchIssue(null); setError(null); }} onStart={provider => void spawn(launchIssue, provider)} onAuthFailed={onAuthFailed} />}
    </div>
  );
}
