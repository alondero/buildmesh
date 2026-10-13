import { openUrl } from '@tauri-apps/plugin-opener';
import type { ILinkHandler, Terminal } from '@xterm/xterm';
import type { ILinkProviderOptions } from '@xterm/addon-web-links';

/**
 * Link-click routing for one agent terminal.
 *
 * A press on a link has two independent owners inside xterm:
 *
 *  - the `Linkifier` activates the link — an OSC 8 hyperlink through the
 *    `linkHandler` option (what Codex emits), a plain printed URL through
 *    `WebLinksAddon`;
 *  - xterm *also* forwards the press to the PTY as an SGR mouse report
 *    whenever the agent's TUI has turned mouse tracking on (Codex does:
 *    `CSI ? 1003 h` with `CSI ? 1006 h`), and Codex answers a press on its own
 *    OSC 8 hyperlink by opening that URL itself
 *    (`codex-rs/tui/src/tui/link_pointer.rs` → `open_url_in_browser`).
 *
 * Two owners, one URL: a single click in a Codex session spawned two browser
 * tabs, while a harness that prints plain URLs only ever reached ours.
 *
 * The press cannot simply be left to the agent, either. xterm records the link
 * under the pointer on `mousedown` and activates on `mouseup` only when that
 * record survived, so stopping the press before it is forwarded also cancels
 * our own activation. While the PTY app owns mouse input, this router
 * therefore owns a left press that lands on a link: it stops the press before
 * xterm forwards it and opens the hovered URI itself. A press anywhere else —
 * and every non-primary button — is left alone and still reaches the agent.
 */
export class TerminalLinkRouter {
  /**
   * The link under the pointer, or null. Tracked from both providers'
   * `hover`/`leave` callbacks — xterm only re-resolves the link under the
   * cursor on mousemove, so this is what a press can be matched against
   * without reaching into xterm's private linkifier.
   */
  private hoveredUri: string | null = null;
  private term: Terminal | null = null;
  private element: HTMLElement | null = null;

  /**
   * xterm's `linkHandler` option — the OSC 8 path. `allowNonHttpProtocols`
   * stays unset so xterm keeps dropping non-HTTP(S) links; terminal output is
   * untrusted agent text.
   */
  readonly linkHandler: ILinkHandler = {
    activate: (_event, uri) => this.open(uri),
    hover: (_event, uri) => {
      this.hoveredUri = uri;
    },
    leave: () => {
      this.hoveredUri = null;
    },
  };

  /** `WebLinksAddon`'s click handler — the plain-printed-URL path. */
  readonly openWebLink = (_event: MouseEvent, uri: string): void => {
    this.open(uri);
  };

  /** `WebLinksAddon` options that feed the same hovered-link tracking. */
  readonly webLinksOptions: ILinkProviderOptions = {
    hover: (_event, uri) => {
      this.hoveredUri = uri;
    },
    leave: () => {
      this.hoveredUri = null;
    },
  };

  /**
   * Bind to a freshly opened terminal. Call after `term.open()` — the press
   * guard lives on `term.element`, which does not exist before that. Safe to
   * call again for the same terminal (a re-attach re-parents the element and
   * must not stack listeners).
   */
  attach(term: Terminal): void {
    this.term = term;
    const element = term.element;
    if (!element || element === this.element) return;
    this.unbindElement();
    this.element = element;
    element.addEventListener('mousedown', this.onPress, true);
  }

  /**
   * Unbind the press guard. This does not touch the terminal itself — the
   * registry only reaches here when the node is deleted, and the terminal's
   * own teardown stays on `TerminalRegistry`'s single dispose path.
   */
  dispose(): void {
    this.unbindElement();
    this.term = null;
    this.hoveredUri = null;
  }

  private unbindElement(): void {
    this.element?.removeEventListener('mousedown', this.onPress, true);
    this.element = null;
  }

  private readonly onPress = (event: MouseEvent): void => {
    const term = this.term;
    const uri = this.hoveredUri;
    if (!term || uri === null) return;
    // Only the primary button is a link click. Codex binds right-click to
    // paste, so every other button keeps reaching the agent even when the
    // pointer happens to be over a link.
    if (event.button !== 0) return;
    // Only a mouse-tracking TUI would also read this press as a click on the
    // link it is drawing. Without tracking xterm never reports it, so the
    // press has to keep reaching its normal activate-on-release path.
    if (term.modes.mouseTrackingMode === 'none') return;

    // Mirror the two things xterm's own mousedown handler does before it
    // forwards the press, then take the press away from the PTY. The mouseup
    // is deliberately left alone: xterm will not activate (it has no
    // mousedown record), so this press opens the link exactly once.
    event.preventDefault();
    event.stopPropagation();
    this.hoveredUri = null;
    term.focus();
    this.open(uri);
  };

  private open(uri: string): void {
    openUrl(uri).catch(console.error);
  }
}