import { useRef } from "react";
import { eventsWsUrl, isAuthError, isRateLimited, type EventMsg } from "./api";
import { useAsyncEffect } from "../hooks/useAsyncEffect";

const RECONNECT_DELAYS_MS = [1000, 2000, 4000, 8000];
const TICKET_TIMEOUT_MS = 15000;

type EventsConnection =
  | { kind: 'idle' }
  | { kind: 'minting'; controller: AbortController; timer: number }
  | { kind: 'socket'; socket: WebSocket }
  | { kind: 'backoff'; timer: number }
  | { kind: 'rate-limited' }
  | { kind: 'unauthorized' }
  | { kind: 'disposed' };

/**
 * One events connection per mounted screen, including the ticket mint before
 * a socket exists. Failure takes one backoff slot (1/2/4/8s, capped); foreground
 * or network return can bypass that wait. A 429 pauses until the next resume,
 * a rejected session ends the connection, and disposal retires every callback.
 * The screen's list poll remains the fallback while events are unavailable.
 */
export function useWsEvents(onEvent: (msg: EventMsg) => void, onAuthError: () => void): void {
  const onEventRef = useRef(onEvent);
  onEventRef.current = onEvent;
  const onAuthErrorRef = useRef(onAuthError);
  onAuthErrorRef.current = onAuthError;

  useAsyncEffect((signal) => {
    let connection: EventsConnection = { kind: 'idle' };
    let attempt = 0;

    const closeSocket = (socket: WebSocket) => {
      socket.onopen = null;
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
      socket.close();
    };

    const scheduleReconnect = () => {
      if (signal.aborted || connection.kind === 'backoff') return;
      const previous = connection;
      const delay = RECONNECT_DELAYS_MS[Math.min(attempt, RECONNECT_DELAYS_MS.length - 1)];
      attempt++;
      const waiting = {
        kind: 'backoff',
        timer: window.setTimeout(() => {
          if (signal.aborted || connection !== waiting) return;
          connection = { kind: 'idle' };
          void connect();
        }, delay),
      } as const;
      connection = waiting;
      // Retire before closing: error and close can describe one failure, and
      // queued frames from that socket must not reach the screen afterwards.
      if (previous.kind === 'socket') closeSocket(previous.socket);
    };

    const connect = async () => {
      if (signal.aborted || connection.kind !== 'idle') return;
      const controller = new AbortController();
      let timer!: number;
      let cancelled!: () => void;
      const deadline = new Promise<never>((_, reject) => {
        cancelled = () => reject(controller.signal.reason);
        controller.signal.addEventListener('abort', cancelled, { once: true });
        timer = window.setTimeout(() => {
          controller.abort(new Error('Event connection ticket timed out'));
        }, TICKET_TIMEOUT_MS);
      });
      const mint = { kind: 'minting', controller, timer } as const;
      connection = mint;
      let url: string;
      try {
        // A suspended phone can leave fetch pending indefinitely. Bound the
        // whole attempt (including shared cookie recovery), not just fetch.
        url = await Promise.race([eventsWsUrl(controller.signal), deadline]);
      } catch (error) {
        if (signal.aborted || connection !== mint) return;
        if (isAuthError(error)) {
          connection = { kind: 'unauthorized' };
          onAuthErrorRef.current();
        } else if (isRateLimited(error)) {
          // The mint helper already tried its bounded Retry-After wait.
          connection = { kind: 'rate-limited' };
        } else {
          scheduleReconnect();
        }
        return;
      } finally {
        window.clearTimeout(timer);
        controller.signal.removeEventListener('abort', cancelled);
      }
      if (signal.aborted || connection !== mint) return;

      let socket: WebSocket;
      try {
        socket = new WebSocket(url);
      } catch {
        scheduleReconnect();
        return;
      }
      connection = { kind: 'socket', socket };
      const ownsSocket = () => !signal.aborted && connection.kind === 'socket' && connection.socket === socket;
      socket.onopen = () => {
        if (ownsSocket()) attempt = 0;
      };
      socket.onmessage = (event) => {
        if (!ownsSocket()) return;
        try {
          const msg = JSON.parse(typeof event.data === "string" ? event.data : "") as EventMsg;
          if (msg && (msg.type === "attention-cleared" || msg.type === "agent-lifecycle" || msg.type === "circuit-run-updated")) {
            onEventRef.current(msg);
          }
        } catch {
          // Ignore non-JSON frames silently.
        }
      };
      const failed = () => {
        if (ownsSocket()) scheduleReconnect();
      };
      socket.onclose = failed;
      socket.onerror = failed;
    };

    const resume = () => {
      if (signal.aborted) return;
      switch (connection.kind) {
        case 'minting':
        case 'unauthorized':
        case 'disposed':
          return;
        case 'socket':
          // CLOSING still owns its handshake; onclose owns its retry.
          if (connection.socket.readyState !== WebSocket.CLOSED) return;
          closeSocket(connection.socket);
          break;
        case 'backoff':
          window.clearTimeout(connection.timer);
          break;
        case 'idle':
        case 'rate-limited':
          break;
      }
      connection = { kind: 'idle' };
      attempt = 0;
      void connect();
    };
    const onForeground = () => {
      if (document.visibilityState === "visible") resume();
    };
    document.addEventListener("visibilitychange", onForeground);
    window.addEventListener("online", resume);

    void connect();
    return () => {
      const previous = connection;
      connection = { kind: 'disposed' };
      document.removeEventListener("visibilitychange", onForeground);
      window.removeEventListener("online", resume);
      if (previous.kind === 'backoff') window.clearTimeout(previous.timer);
      if (previous.kind === 'minting') {
        window.clearTimeout(previous.timer);
        previous.controller.abort();
      }
      if (previous.kind === 'socket') closeSocket(previous.socket);
    };
  }, []);
}
