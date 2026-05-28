import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SerializeAddon } from '@xterm/addon-serialize';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { openUrl } from '@tauri-apps/plugin-opener';
import { createTerminalOptions } from './terminalConfig';
import { resolveKeyAction } from './terminalKeyAction';
import { isMac } from '../../lib/platform';
import { TerminalWriter, type TerminalWriteData } from './TerminalWriter';
import { FontSizeManager } from './FontSizeManager';

export interface TerminalInstance {
  term: Terminal;
  fitAddon: FitAddon;
  serializeAddon: SerializeAddon;
  searchAddon: SearchAddon;
  unlisten: UnlistenFn;
  opened: boolean;
  resizeObserver: ResizeObserver | null;
  attachedContainer: HTMLElement | null;
  onFindRequest: (() => void) | null;
}

interface AgentOutputPayload {
  session_id: number;
  data?: string;
  line?: string;
}

function decodeBase64Bytes(data: string): Uint8Array {
  const binary = globalThis.atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function terminalDataFromPayload(payload: AgentOutputPayload): TerminalWriteData | null {
  if (payload.data !== undefined) return decodeBase64Bytes(payload.data);
  if (payload.line !== undefined) return payload.line;
  return null;
}

function measureAndFit(inst: TerminalInstance): void {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
  charSizeService?.measure();
  inst.fitAddon.fit();
}

export class TerminalRegistry {
  private instances = new Map<number, TerminalInstance>();
  private pending = new Map<number, Promise<TerminalInstance | null>>();
  private listeners = new Set<() => void>();
  private writer = new TerminalWriter();
  private fontSizeManager = new FontSizeManager();

  getInstance(nodeId: number): TerminalInstance | undefined {
    return this.instances.get(nodeId);
  }

  getTerminal(nodeId: number): Terminal | undefined {
    return this.instances.get(nodeId)?.term;
  }

  async getOrCreate(nodeId: number): Promise<TerminalInstance | null> {
    if (this.instances.has(nodeId)) {
      return this.instances.get(nodeId)!;
    }
    if (this.pending.has(nodeId)) {
      return this.pending.get(nodeId)!;
    }
    const promise = this.doCreate(nodeId);
    this.pending.set(nodeId, promise);
    try {
      return await promise;
    } finally {
      this.pending.delete(nodeId);
    }
  }

  async attach(nodeId: number, container: HTMLElement): Promise<TerminalInstance | null> {
    const inst = await this.getOrCreate(nodeId);
    if (!inst) return null;
    return this.attachToDOM(inst, container);
  }

  private attachToDOM(inst: TerminalInstance, container: HTMLElement): TerminalInstance {
    const wasFreshOpen = !inst.opened;
    if (!inst.opened) {
      inst.opened = true;
      inst.term.open(container);
    } else {
      const termEl = inst.term.element;
      if (termEl && termEl.parentElement !== container) {
        container.appendChild(termEl);
      }
    }

    inst.attachedContainer = container;

    if (inst.resizeObserver) {
      inst.resizeObserver.disconnect();
    }
    inst.resizeObserver = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        if (!inst.attachedContainer) return;
        measureAndFit(inst);
      });
    });
    inst.resizeObserver.observe(container);

    requestAnimationFrame(() => {
      measureAndFit(inst);
      // Only auto-scroll-to-tail on the first open. On re-attach the user
      // may have scrolled back to read history; forcing the tail here would
      // silently destroy that position (and flash the jump-to-latest pill).
      if (wasFreshOpen) inst.term.scrollToBottom();
      inst.term.refresh(0, inst.term.rows - 1);
    });

    return inst;
  }

  detach(nodeId: number): void {
    const inst = this.instances.get(nodeId);
    if (!inst) return;

    if (inst.resizeObserver) {
      inst.resizeObserver.disconnect();
      inst.resizeObserver = null;
    }
    inst.term.element?.remove();
    inst.attachedContainer = null;
  }

  fit(nodeId: number): void {
    const inst = this.instances.get(nodeId);
    if (!inst) return;
    measureAndFit(inst);
  }

  fitAll(): void {
    for (const [nodeId, inst] of this.instances) {
      if (inst.attachedContainer) {
        this.fit(nodeId);
      }
    }
  }

  dispose(nodeId: number): void {
    const instance = this.instances.get(nodeId);
    if (instance) {
      if (instance.resizeObserver) {
        instance.resizeObserver.disconnect();
      }
      instance.unlisten();
      instance.term.dispose();
      this.instances.delete(nodeId);
      this.writer.unregister(nodeId);
      this.fontSizeManager.unregister(nodeId);
      this.notify();
    }
  }

  subscribe(cb: () => void): () => void {
    this.listeners.add(cb);
    return () => { this.listeners.delete(cb); };
  }

  private notify(): void {
    this.listeners.forEach(cb => cb());
  }

  private async doCreate(nodeId: number): Promise<TerminalInstance | null> {
    try {
      const term = new Terminal(createTerminalOptions());

      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      const serializeAddon = new SerializeAddon();
      term.loadAddon(serializeAddon);
      const searchAddon = new SearchAddon();
      term.loadAddon(searchAddon);
      const webLinksAddon = new WebLinksAddon((_event, uri) => {
        openUrl(uri).catch(console.error);
      });
      term.loadAddon(webLinksAddon);

      const instance: TerminalInstance = {
        term,
        fitAddon,
        serializeAddon,
        searchAddon,
        unlisten: () => {},
        opened: false,
        resizeObserver: null,
        attachedContainer: null,
        onFindRequest: null,
      };

      this.writer.register(nodeId, (data) => term.write(data));
      this.fontSizeManager.register(nodeId, term, () => measureAndFit(instance));

      const unlisten = await listen<AgentOutputPayload>('agent-output', (event) => {
        if (event.payload.session_id === nodeId) {
          const data = terminalDataFromPayload(event.payload);
          if (data !== null) {
            this.writer.append(nodeId, data);
          }
        }
      });

      const unlistenSerialize = await listen<{ node_id: number; request_id: string }>('serialize-terminal-request', (event) => {
        if (event.payload.node_id === nodeId) {
          const snapshot = instance.serializeAddon.serialize({ scrollback: 200 });
          invoke('submit_terminal_snapshot', {
            requestId: event.payload.request_id,
            data: snapshot,
          }).catch(console.error);
        }
      });

      term.onData((data) => {
        invoke('write_to_agent', { sessionId: nodeId, data }).catch(console.error);
      });

      term.attachCustomKeyEventHandler((ev: KeyboardEvent) => {
        if (ev.type !== 'keydown') return true;

        const action = resolveKeyAction({
          key: ev.key,
          ctrlKey: ev.ctrlKey,
          shiftKey: ev.shiftKey,
          metaKey: ev.metaKey,
          isMac,
          hasSelection: term.hasSelection(),
        });

        switch (action) {
          case 'copy':
            navigator.clipboard.writeText(term.getSelection()).catch(err => {
              console.warn('[TerminalRegistry] Clipboard write failed:', err);
            });
            return false;
          case 'paste':
            ev.preventDefault();
            navigator.clipboard.readText().then(text => {
              if (text) term.paste(text);
            }).catch(err => {
              console.warn('[TerminalRegistry] Clipboard read failed:', err);
            });
            return false;
          case 'selectAll':
            term.selectAll();
            return false;
          case 'find':
            instance.onFindRequest?.();
            return false;
          case 'clear':
            term.clear();
            return false;
          case 'passthrough':
            return true;
        }
      });

      term.onResize(({ cols, rows }) => {
        invoke('resize_agent', { sessionId: nodeId, rows, cols }).catch(err => {
          if (err !== 'Agent not running') {
            console.error(`[TerminalRegistry] Resize failed for node ${nodeId}:`, err);
          }
        });
      });

      const combinedUnlisten: UnlistenFn = () => { unlisten(); unlistenSerialize(); };
      instance.unlisten = combinedUnlisten;

      this.instances.set(nodeId, instance);
      this.notify();
      return instance;
    } catch (e) {
      console.error(`[TerminalRegistry] Failed to create terminal for ${nodeId}`, e);
      return null;
    }
  }

  destroy(): void {
    for (const nodeId of this.instances.keys()) {
      this.dispose(nodeId);
    }
    this.fontSizeManager.destroy();
  }
}
