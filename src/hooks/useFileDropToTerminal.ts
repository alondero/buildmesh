import { getCurrentWebview } from '@tauri-apps/api/webview';
import { terminalManager } from '../components/Terminal/Terminal';
import { useUIStore } from '../stores/uiStore';
import { nodeIdFromPoint, pasteDropPaths } from '../lib/fileDropPaste';
import { useAsyncEffect } from './useAsyncEffect';

/**
 * Register a single window-level listener for OS file drops (Explorer / Finder).
 *
 * Tauri's native webview drag-drop handler intercepts the OS drop before the DOM,
 * so HTML5 `onDrop` never fires with usable files. We listen to the Tauri event
 * instead, hit-test the drop position to the agent node under the cursor, and
 * paste the file's absolute path into that node's terminal.
 *
 * Mount exactly once (from `App`).
 */
export function useFileDropToTerminal(): void {
  useAsyncEffect((signal) => {
    let unlisten: (() => void) | null = null;
    const setDragTargetNodeId = useUIStore.getState().setDragTargetNodeId;

    // Tauri reports the drop position in physical pixels; elementFromPoint wants
    // CSS pixels, so scale by devicePixelRatio (matters on HiDPI / 150% displays).
    const toNodeId = (pos: { x: number; y: number }): number | null => {
      const ratio = window.devicePixelRatio || 1;
      return nodeIdFromPoint(pos.x / ratio, pos.y / ratio);
    };

    getCurrentWebview()
      .onDragDropEvent((event) => {
        const payload = event.payload;

        if (payload.type === 'leave') {
          setDragTargetNodeId(null);
          return;
        }

        if (payload.type === 'enter' || payload.type === 'over') {
          setDragTargetNodeId(toNodeId(payload.position));
          return;
        }

        // payload.type === 'drop'
        setDragTargetNodeId(null);
        const nodeId = toNodeId(payload.position);
        if (nodeId === null || payload.paths.length === 0) return;
        const inst = terminalManager.getInstance(nodeId);
        if (!inst) return;
        pasteDropPaths(inst.term, payload.paths).catch((err) =>
          console.error('[useFileDropToTerminal] paste failed:', err),
        );
      })
      .then((fn) => {
        if (signal.aborted) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[useFileDropToTerminal] listener setup failed:', err));

    return () => {
      unlisten?.();
      setDragTargetNodeId(null);
    };
  }, []);
}
