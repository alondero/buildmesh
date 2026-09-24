import { useCallback, useEffect, useRef, useState } from "react";
import { AgentNode, isAuthError, listNodes, sendNodeKeys } from "../api";
import { getStatusConfig } from "../../lib/status";
import { AppBar } from "../ui";
import { useVisibilityPolling } from "../useVisibilityPolling";
import { useWsEvents } from "../useWsEvents";

export default function NodeOverview({
  node: initial,
  prompt,
  draft: savedDraft,
  replySending: savedReplySending,
  replyNotice: savedReplyNotice,
  onPromptChange,
  onDraftChange,
  onReplySendingChange,
  onReplyNoticeChange,
  onBack,
  onTerminal,
  onChanges,
  onAuthFailed,
}: {
  node: AgentNode;
  onBack: () => void;
  onTerminal: () => void;
  onChanges: () => void;
  onAuthFailed: () => void;
  prompt?: string;
  draft?: string;
  replySending?: boolean;
  replyNotice?: string;
  onPromptChange?: (prompt: string | undefined, nodeId: number) => void;
  onDraftChange?: (draft: string, nodeId: number) => void;
  onReplySendingChange?: (sending: boolean, nodeId: number) => void;
  onReplyNoticeChange?: (notice: string, nodeId: number) => void;
}) {
  const [node, setNode] = useState(initial);
  const [localDraft, setLocalDraft] = useState("");
  const draft = savedDraft ?? localDraft;
  const [localBusy, setLocalBusy] = useState(false);
  const busy = savedReplySending ?? localBusy;
  const [localNotice, setLocalNotice] = useState("");
  const notice = savedReplyNotice ?? localNotice;
  const [error, setError] = useState("");
  const [missing, setMissing] = useState(false);
  const [context, setContext] = useState(prompt);
  const updateDraft = (next: string) => {
    if (onDraftChange) onDraftChange(next, initial.id);
    else if (active.current) setLocalDraft(next);
  };
  const updateReplySending = (next: boolean) => {
    if (onReplySendingChange) onReplySendingChange(next, initial.id);
    else if (active.current) setLocalBusy(next);
  };
  const updateNotice = (next: string) => {
    if (onReplyNoticeChange) onReplyNoticeChange(next, initial.id);
    else if (active.current) setLocalNotice(next);
  };
  const updateContext = useCallback(
    (next: string | undefined) => {
      setContext(next);
      onPromptChange?.(next, initial.id);
    },
    [initial.id, onPromptChange],
  );
  const active = useRef(true);
  const sending = useRef(false);
  const eventVersion = useRef(0);
  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
    };
  }, []);
  const refresh = useCallback(
    async (isLatest: () => boolean) => {
      const version = eventVersion.current;
      try {
        const nodes = await listNodes();
        if (!active.current || !isLatest() || version !== eventVersion.current)
          return;
        const current = nodes.find((n) => n.id === initial.id);
        setMissing(!current);
        if (current) setNode(current);
        if (!current || current.status !== "awaiting_input")
          updateContext(undefined);
        setError("");
      } catch (e) {
        if (!active.current || !isLatest()) return;
        if (isAuthError(e)) onAuthFailed();
        else
          setError("Could not refresh status. The last known state is shown.");
      }
    },
    [initial.id, onAuthFailed, updateContext],
  );
  useVisibilityPolling(refresh, 5000);
  useWsEvents((msg) => {
    if (msg.type === "agent-lifecycle" && msg.session_id === initial.id) {
      eventVersion.current += 1;
      setNode((current) => ({
        ...current,
        status: msg.status,
        signal_health: msg.signal_health,
      }));
      updateContext(
        msg.status === "awaiting_input"
          ? (msg.semantic_turn?.description ?? msg.message ?? undefined)
          : undefined,
      );
    }
    if (msg.type === "attention-cleared" && msg.session_id === initial.id) {
      eventVersion.current += 1;
      updateContext(undefined);
    }
  }, onAuthFailed);
  const status = getStatusConfig(node.status);
  // The HTTP input endpoint caps the entire encoded JSON body at 1 KiB.
  const seq = `${draft.trim()}\r`;
  const fits = new TextEncoder().encode(JSON.stringify({ seq })).length <= 1024;
  const canReply =
    !missing && (node.status === "idle" || node.status === "awaiting_input");
  async function send() {
    if (!canReply || !draft.trim() || !fits || sending.current || busy) return;
    sending.current = true;
    updateReplySending(true);
    updateNotice("");
    try {
      await sendNodeKeys(node.id, seq);
      updateDraft("");
      updateNotice("Reply delivered to the terminal.");
    } catch (e) {
      if (isAuthError(e)) onAuthFailed();
      else updateNotice(`Reply not confirmed: ${(e as Error).message}`);
    } finally {
      sending.current = false;
      updateReplySending(false);
    }
  }
  return (
    <div className="screen">
      <AppBar title="Work details" subtitle={node.name} onBack={onBack} />
      <main className="mobile-page list-scroll">
        <span className="status-label" style={{ color: status.hex }}>
          {status.dot} {status.label}
        </span>
        <h1>{node.name}</h1>
        <p className="page-intro">
          {node.status === "error"
            ? "This agent hit a problem. Open the terminal to inspect the failure and decide what to do next."
            : node.status === "awaiting_input"
              ? context
                ? "Review the request below, then reply to your agent."
                : "Your agent is waiting. Open the terminal to inspect its request, then reply here or use terminal controls."
              : node.status === "running"
                ? "The agent is working. You can review its changes or follow progress in the terminal."
                : "Review the work, send the next instruction, or pick up the terminal."}
        </p>
        {error && (
          <p role="alert" className="inline-error">
            {error}
          </p>
        )}
        {missing && (
          <p role="alert" className="inline-error">
            This agent is no longer available. Return to the overview.
          </p>
        )}
        {node.status === "awaiting_input" && (
          <section className="agent-request">
            <h2>Agent request</h2>
            <p>
              {context ||
                "No request text received yet. Open the terminal to inspect what the agent needs."}
            </p>
          </section>
        )}
        {node.signal_health && node.signal_health !== "ok" && (
          <p className="health-note">
            Status reporting is {node.signal_health}. Check the terminal for
            current activity.
          </p>
        )}
        <dl className="work-context">
          <dt>Agent</dt>
          <dd>{node.launch_configuration?.name ?? node.provider}</dd>
          <dt>Base reference</dt>
          <dd>{node.branch || "No reference reported"}</dd>
        </dl>
        <div className="action-grid">
          <button
            className="btn-primary"
            onClick={onChanges}
            disabled={missing}
          >
            Review changes
          </button>
          <button className="btn-ghost" onClick={onTerminal} disabled={missing}>
            Open terminal
          </button>
        </div>
        <form
          className="reply-panel"
          onSubmit={(e) => {
            e.preventDefault();
            void send();
          }}
        >
          <label htmlFor="agent-reply">Reply or give direction</label>
          <p className="field-hint">
            {canReply
              ? "Sends text followed by Enter to this agent's terminal."
              : "Replies become available when the agent is idle or waiting for input."}
          </p>
          <textarea
            id="agent-reply"
            className="field"
            rows={4}
            value={draft}
            disabled={busy || !canReply}
            onChange={(e) => updateDraft(e.target.value)}
            placeholder="Tell the agent what to do next…"
          />
          {!fits && (
            <p role="alert" className="inline-error">
              This reply is too long. Shorten it or use the terminal.
            </p>
          )}
          <button
            className="btn-primary"
            disabled={busy || !canReply || !fits || !draft.trim()}
          >
            {busy ? "Sending…" : "Send reply"}
          </button>
          {notice && (
            <p role="status" className="field-hint">
              {notice}
            </p>
          )}
        </form>
      </main>
    </div>
  );
}
