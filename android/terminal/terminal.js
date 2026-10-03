import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
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
terminal.onResize(({ cols, rows }) => window.BuildmeshTerminal.resize(cols, rows));
function layout() {
  // A WebView can load before Compose measures it; percentage heights retain that zero-sized root.
  document.documentElement.style.height = `${window.innerHeight}px`;
  document.body.style.height = `${window.innerHeight}px`;
  host.style.height = `${window.innerHeight}px`;
  fit.fit();
}
new ResizeObserver(layout).observe(host);
window.addEventListener('resize', layout);
window.buildmeshOutput = base64 => terminal.write(Uint8Array.from(atob(base64), c => c.charCodeAt(0)));
window.buildmeshReset = () => terminal.reset();
window.buildmeshLayout = layout;
layout();
window.BuildmeshTerminal.ready();
