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
 * our own activation.
 *
 * So while the PTY app owns mouse input this router owns the whole gesture
 * around a link: it stops the press before xterm can forward it or record it,
 * waits for the release, and opens the URI itself when the release lands on
 * the link the press started on. It stops that release too, because xterm
 * keeps a `_mouseDownLink` from earlier gestures and never clears it after
 * activating (`Linkifier.ts:216-231`) — a release that reached xterm could
 * therefore activate a link this router has already opened.
 *
 * A press away from a link, and every non-primary button (Codex binds
 * right-click to paste), are left to the agent untouched.
 */
export class TerminalLinkRouter {
  /**
   * The link under the pointer, or null. Tracked from both providers'
   * `hover`/`leave` callbacks — xterm only re-resolves the link under the
   * cursor when the pointer moves to a different cell (`Linkifier.ts:83-85`),
   * so this stays valid across repeated clicks on one link and is what a press
   * is matched against without reaching into xterm's private linkifier.
   */
  private hoveredUri: string | null = null;
  /** The link a press we intercepted started on, pending its release. */
  private pressedUri: string | null = null;
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
    this.unbindListeners();
    this.element = element;
    // Capture on the element runs before xterm's own listeners on the screen
    // element beneath it, so the press never reaches the PTY or the linkifier.
    element.addEventListener('mousedown', this.onPress, true);
    // The release is resolved on the document so a release outside this
    // terminal still ends the gesture; only a release inside it is withheld
    // from xterm.
    document.addEventListener('mouseup', this.onRelease, true);
  }

  /**
   * Unbind the press guard. This does not touch the terminal itself — the
   * registry only reaches here when the node is deleted, and the terminal's
   * own teardown stays on `TerminalRegistry`'s single dispose path.
   */
  dispose(): void {
    this.unbindListeners();
    this.term = null;
    this.hoveredUri = null;
    this.pressedUri = null;
  }

  private unbindListeners(): void {
    this.element?.removeEventListener('mousedown', this.onPress, true);
    document.removeEventListener('mouseup', this.onRelease, true);
    this.element = null;
  }

  private readonly onPress = (event: MouseEvent): void => {
    const term = this.term;
    const uri = this.hoveredUri;
    // Any press ends the gesture we were tracking, whether or not this one is
    // ours.
    this.pressedUri = null;
    // Only the primary button is a link click.
    if (!term || uri === null || event.button !== 0) return;
    // Only a mouse-tracking TUI would also read this press as a click on the
    // link it is drawing. Without tracking xterm never reports it, so the
    // press has to keep reaching its normal activate-on-release path.
    if (term.modes.mouseTrackingMode === 'none') return;

    // Mirror what xterm's own mousedown handler does before it forwards the
    // press, then take the press away from both the PTY and the linkifier.
    event.preventDefault();
    event.stopPropagation();
    this.pressedUri = uri;
    term.focus();
  };

  private readonly onRelease = (event: MouseEvent): void => {
    const uri = this.pressedUri;
    if (uri === null) return;
    this.pressedUri = null;
    // Released outside this terminal: the gesture ends without an open, and
    // whatever the release landed on keeps handling it.
    if (!this.element?.contains(event.target as Node | null)) return;
    if (event.button !== 0) return;

    event.preventDefault();
    event.stopPropagation();
    // xterm activates only when the press and the release are on the same
    // link, so a drag off the link must not open it.
    if (this.hoveredUri === uri) this.open(uri);
  };

  private open(uri: string): void {
    openUrl(uri).catch(console.error);
  }
}