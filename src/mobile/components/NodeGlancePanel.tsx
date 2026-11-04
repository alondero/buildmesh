import { useCallback, useEffect, useState } from 'react';
import { getNodeGlance, isAuthError } from '../api';
import { useVisibilityPolling } from '../useVisibilityPolling';
import { getStatusConfig } from '../../lib/status';
import {
  activityHeadline, activityLine, glanceTimings, timingLine, toolCountLine, unavailableCopy,
} from '../../lib/nodeGlance';
import type { NodeGlance } from '../../types/generated/NodeGlance';

/**
 * "Right now" on the phone's node screen: what the agent is doing, how long it
 * has been at it, what it last said and what it was asked. Read only while the
 * screen is visible, on the same cadence as the screen's status refresh.
 */
export function NodeGlancePanel({ nodeId, onAuthFailed }: { nodeId: number; onAuthFailed: () => void }) {
  const [glance, setGlance] = useState<{ value: NodeGlance; readAt: number } | null>(null);
  const [failed, setFailed] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const refresh = useCallback(async (isLatest: () => boolean, signal: AbortSignal) => {
    try {
      const value = await getNodeGlance(nodeId, signal);
      if (!isLatest()) return;
      setGlance({ value, readAt: Date.now() });
      setFailed(false);
    } catch (error) {
      if (!isLatest()) return;
      if (isAuthError(error)) onAuthFailed();
      else setFailed(true);
    }
  }, [nodeId, onAuthFailed]);
  useVisibilityPolling(refresh, 5000);
  useEffect(() => {
    if (!glance) return;
    const clock = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(clock);
  }, [glance]);

  if (!glance) {
    return failed
      ? <section className="node-glance" aria-label="Right now"><p className="field-hint">Could not read what this agent is doing. Your desktop may need updating.</p></section>
      : null;
  }
  const value = glance.value;
  const transcript = value.transcript;
  const headline = activityHeadline(value);
  const earlier = transcript?.activity.slice(0, -1) ?? [];
  const tools = toolCountLine(value);
  return (
    <section className="node-glance" aria-label="Right now">
      <h2>Right now</h2>
      <p className="node-glance-timing">{timingLine(getStatusConfig(value.status).label, glanceTimings(value, now - glance.readAt))}</p>
      {headline && <p className="node-glance-activity">{headline}</p>}
      {earlier.map((activity, index) => <p key={index} className="node-glance-earlier">{activityLine(activity)}</p>)}
      {tools && <p className="node-glance-timing">{tools}</p>}
      {transcript?.last_message && <>
        <h3>{transcript.message_after_prompt ? 'Last message' : 'Last message (earlier task)'}</h3>
        <p className="node-glance-text">{transcript.last_message}</p>
      </>}
      {transcript?.last_prompt && <>
        <h3>Task</h3>
        <p className="node-glance-text">{transcript.last_prompt}</p>
      </>}
      {value.transcript_unavailable && <p className="field-hint">{unavailableCopy(value.transcript_unavailable)}</p>}
      {failed && <p className="field-hint">Refresh failed; showing the last reading.</p>}
    </section>
  );
}
