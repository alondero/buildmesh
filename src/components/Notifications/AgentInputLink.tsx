import { useState, type ReactNode } from 'react';
import { focusAgentNode } from '../../lib/focusAgentNode';
import { isAgentInput, type AgentInputNotificationAction } from '../../lib/notifications';
import { useToastStore } from '../../stores/toastStore';

/** Navigation only: replies stay with the agent's existing request controls. */
export function AgentInputLink({ action, children, className, onOpen }: {
  action: AgentInputNotificationAction;
  children?: ReactNode;
  className?: string;
  onOpen?: () => void;
}) {
  const [unavailable, setUnavailable] = useState(false);
  return <>
    <button type="button" className={className}
      aria-label={`Open agent ${action.agentName}${action.meshName ? ` in ${action.meshName}` : ''}`}
      onClick={() => {
        if (!focusAgentNode(action.nodeId)) {
          setUnavailable(true);
          return;
        }
        setUnavailable(false);
        const store = useToastStore.getState();
        for (const notice of store.notifications) {
          if (isAgentInput(notice.action) && notice.action.nodeId === action.nodeId) store.markNotificationRead(notice.id);
        }
        onOpen?.();
      }}>
      {children ?? 'Open agent'}
    </button>
    {unavailable && <span role="alert" className="block text-xs text-text-secondary mt-1">
      This agent is no longer available. The input request may have ended.
    </span>}
  </>;
}
