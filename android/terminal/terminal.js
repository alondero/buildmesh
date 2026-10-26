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
window.buildmeshOutput = base64 => terminal.write(Uint8Array.from(atob(base64), c => c.charCodeAt(0)));
window.buildmeshReset = () => { terminal.reset(); reportScrollState(); };
window.buildmeshLayout = layout;
// Escape hatch for the native pane's "jump to latest" button: without it a
// phone that panned back into the scrollback has no way to reach the live tail
// again except dragging the whole history back.
window.buildmeshScrollToBottom = () => terminal.scrollToBottom();
layout();
window.BuildmeshTerminal.ready();
