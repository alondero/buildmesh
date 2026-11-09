import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
// The shared mobile touch-scroll contract. xterm's own scroller only answers
// the mouse wheel, so on a phone the scrollback was unreachable until this
// converts a vertical drag into a scroll through xterm's public API.
import { attachTouchPan } from '../../src/components/Terminal/attachTouchPan';
import { attachMobileTerminalLinks } from '../../src/components/Terminal/attachMobileTerminalLinks';
import '@xterm/xterm/css/xterm.css';
import './terminal.css';

const terminal = new Terminal({
  cursorBlink: true, fontSize: 13, scrollback: 10000, allowProposedApi: false, disableStdin: true,
  theme: { background: '#0a0a0e', foreground: '#e2e8f0', cursor: '#00d4ff', selectionBackground: '#1a2a3a' },
});
const fit = new FitAddon();
terminal.loadAddon(fit);
const host = document.getElementById('terminal');
terminal.open(host);
attachTouchPan(host, terminal);
attachMobileTerminalLinks(terminal, uri => window.BuildmeshTerminal.openUrl(uri));
terminal.onResize(({ cols, rows }) => window.BuildmeshTerminal.resize(cols, rows));
// `viewportY >= baseY` means the viewport shows the newest line — the same
// test the browser mobile client uses for its "jump to latest" affordance. The
// native pane shows that affordance whenever the user has panned back.
const atLiveTail = () => {
  const buffer = terminal.buffer.active;
  return buffer.viewportY >= buffer.baseY;
};
// xterm fires its scroll event per line feed (`BufferService.scroll`), so a
// streaming agent would cross the JS→Kotlin bridge once per output line for
// an answer that rarely changes. Report only when the answer flips; `null`
// lets the first report through, so the pane starts from the real position.
let reportedAtLiveTail = null;
const reportScrollState = () => {
  const atTail = atLiveTail();
  if (atTail === reportedAtLiveTail) return;
  reportedAtLiveTail = atTail;
  window.BuildmeshTerminal.scrollState(atTail);
};
terminal.onScroll(reportScrollState);
// Typing is native (issue #2747): Kotlin's TerminalSurface owns the keyboard
// and sends each key to the agent, while this xterm stays display-only so it
// never answers the terminal queries the desktop already answers. Its hidden
// textarea must not raise a keyboard of its own, which would type nowhere.
terminal.textarea?.setAttribute('inputmode', 'none');
if (terminal.textarea) terminal.textarea.readOnly = true;
// The native side needs the modes the agent switched on, so arrows and pastes
// it sends match what this terminal would send. Report only when one flips:
// output arrives far more often than a mode changes.
let reportedModes = '';
const reportModes = () => {
  const { applicationCursorKeysMode, bracketedPasteMode } = terminal.modes;
  const modes = `${applicationCursorKeysMode}/${bracketedPasteMode}`;
  if (modes === reportedModes) return;
  reportedModes = modes;
  window.BuildmeshTerminal.modes(applicationCursorKeysMode, bracketedPasteMode);
};
// A plain tap types into the agent's own input box. A link tap is claimed
// first by attachMobileTerminalLinks (it cancels the touchend), and a drag is a
// scroll, so only a short, still, single-finger touch opens the keyboard.
const TAP_SLOP_PX = 8;
const TAP_TIMEOUT_MS = 500;
let tap = null;
host.addEventListener('touchstart', event => {
  const touch = event.touches[0];
  tap = event.touches.length === 1 ? { x: touch.clientX, y: touch.clientY, time: event.timeStamp } : null;
}, { passive: true });
host.addEventListener('touchend', event => {
  const start = tap;
  tap = null;
  const touch = event.changedTouches[0];
  if (!start || !touch || event.defaultPrevented || event.touches.length !== 0) return;
  if (event.timeStamp - start.time > TAP_TIMEOUT_MS) return;
  if (Math.hypot(touch.clientX - start.x, touch.clientY - start.y) > TAP_SLOP_PX) return;
  window.BuildmeshTerminal.focusInput();
}, { passive: true });
host.addEventListener('touchcancel', () => { tap = null; }, { passive: true });
function layout() {
  // A WebView can load before Compose measures it; percentage heights retain that zero-sized root.
  document.documentElement.style.height = `${window.innerHeight}px`;
  document.body.style.height = `${window.innerHeight}px`;
  host.style.height = `${window.innerHeight}px`;
  fit.fit();
  reportScrollState();
}
new ResizeObserver(layout).observe(host);
window.addEventListener('resize', layout);
window.buildmeshOutput = base64 => terminal.write(Uint8Array.from(atob(base64), c => c.charCodeAt(0)), reportModes);
window.buildmeshReset = () => { terminal.reset(); reportScrollState(); reportModes(); };
window.buildmeshLayout = layout;
// Escape hatch for the native pane's "jump to latest" button: without it a
// phone that panned back into the scrollback has no way to reach the live tail
// again except dragging the whole history back.
window.buildmeshScrollToBottom = () => terminal.scrollToBottom();
layout();
reportModes();
window.BuildmeshTerminal.ready();
