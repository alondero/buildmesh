import { useRef } from "react";
import { eventsWsUrl, isAuthError, isRateLimited, type EventMsg } from "./api";
import { useAsyncEffect } from "../hooks/useAsyncEffect";

const RECONNECT_DELAYS_MS = [1000, 2000, 4000, 8000];

/// Open a /ws/events WebSocket and call `onEvent` for every relevant message.
///
/// Auto-reconnects with simple backoff (1/2/4/8s, capped — losing the events
/// stream falls back to the 5-second list poll in `NodeList`, so an outage
/// is invisible beyond a brief lag). Resumes on foreground/network return
/// the same way `TerminalScreen` does for the terminal WS (issue #806):
/// when the page becomes visible or the browser signals `online` again, we
/// reset the attempt counter and reconnect immediately instead of waiting
/// out the next backoff slot — mobile backgrounding is the normal case and
/// the OS will silently exhaust the ladder on a long absence.
export function useWsEvents(onEvent: () => void, onAuthError: () => void): void {
  const onEventRef = useRef(onEvent);
  onEventRef.current = onEvent;
  const onAuthErrorRef = useRef(onAuthError);
  onAuthErrorRef.current = onAuthError;

  useAsyncEffect((signal) => {
    let ws: WebSocket | null = null;
    let reconnectTimer: number | null = null;
    let attempt = 0;

    const clearReconnectTimer = () => {
      if (reconnectTimer) {
        window.clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
    };

    const connect = async () => {
      if (signal.aborted) return;
      // Mint a single-use WS ticket (issue #500) before opening the socket.
      let url: string;
      try {
        url = await eventsWsUrl();
      } catch (e) {
        // A 401/403 from the mint means the cookie is gone — re-minting would
        // loop forever, so surface it for re-auth instead of backing off.
        if (isAuthError(e)) {
          onAuthErrorRef.current();
          return;
        }
        // Issue #552: a 429 from the mint is a server pacing signal, NOT a
        // connectivity failure. We deliberately do NOT fall through to the
        // 1/2/4/8s reconnect ladder here — the helper's own bounded wait
        // already gave the rate-limit window its best shot at draining, so
        // a second 429 is genuine oversaturation. The 5s poll that backs
        // this screen still surfaces new state, so a brief stall is
        // invisible to the user; stop reconnecting silently.
        if (isRateLimited(e)) return;
        scheduleReconnect();
        return;
      }
      if (signal.aborted) return;
      try {
        ws = new WebSocket(url);
      } catch {
        scheduleReconnect();
        return;
      }

      // A single socket's failure must advance `attempt` exactly once:
      // browsers fire BOTH `error` and `close` for one failure, in that
      // order. Without this flag the backoff ticks twice as fast as the
      // user expects and the `attempt` counter loses its meaning.
      let reconnectScheduled = false;

      ws.onopen = () => {
        attempt = 0;
      };
      ws.onmessage = (e) => {
        try {
          const msg = JSON.parse(typeof e.data === "string" ? e.data : "") as EventMsg;
          if (msg && (msg.type === "attention-needed" || msg.type === "attention-cleared")) {
            onEventRef.current();
          }
        } catch {
          // Ignore non-JSON frames silently.
        }
      };
      ws.onclose = () => {
        if (signal.aborted || reconnectScheduled) return;
        reconnectScheduled = true;
        scheduleReconnect();
      };
      ws.onerror = () => {
        if (signal.aborted || reconnectScheduled) return;
        reconnectScheduled = true;
        scheduleReconnect();
      };
    };

    const scheduleReconnect = () => {
      const delay = RECONNECT_DELAYS_MS[Math.min(attempt, RECONNECT_DELAYS_MS.length - 1)];
      attempt++;
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = null;
        connect();
      }, delay);
    };

    // Mobile backgrounding is the normal case: the OS suspends timers and
    // drops the WebSocket, so the connection can silently sit in a backoff
    // slot while the tab is hidden. Resume the moment the document is
    // foregrounded or the network returns, instead of waiting out the
    // next 8-second backoff tick. Issue #806.
    const resume = () => {
      if (signal.aborted) return;
      if (
        ws &&
        (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)
      ) {
        return;
      }
      clearReconnectTimer();
      attempt = 0;
      void connect();
    };
    const onForeground = () => {
      // `online` fires resume unconditionally (network return can race
      // ahead of foreground); visibilitychange only when we're actually
      // back in front of the user.
      if (document.visibilityState === "visible") resume();
    };
    document.addEventListener("visibilitychange", onForeground);
    window.addEventListener("online", resume);

    void connect();
    return () => {
      document.removeEventListener("visibilitychange", onForeground);
      window.removeEventListener("online", resume);
      clearReconnectTimer();
      if (ws) {
        ws.onclose = null;
        ws.onerror = null;
        ws.close();
      }
    };
  }, []);
}
