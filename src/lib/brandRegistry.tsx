import minimaxLogo from '../assets/providers/minimax.svg';
import antigravityLogo from '../assets/providers/antigravity.png';
import grokLogo from '../assets/providers/grok.svg';

export type InlineIconProps = { className?: string; title?: string };

type BrandIcon =
  | {
      kind: 'inline';
      component: (props: InlineIconProps) => React.JSX.Element;
    }
  | {
      kind: 'image';
      src: string;
    };

export type Brand = {
  readonly id: string;
  readonly icon: BrandIcon;
  readonly chipHex: string;
  readonly chipClass: string;
};

function ClaudeCodeIcon({ className, title }: InlineIconProps) {
  return (
    <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path fill="currentColor" d="M21 10.5h3v3h-3v3h-1.5v3H18v-3h-1.5v3H15v-3H9v3H7.5v-3H6v3H4.5v-3H3v-3H0v-3h3v-6h18Zm-15 0h1.5v-3H6Zm10.5 0H18v-3h-1.5z" />
    </svg>
  );
}

function OpenAIIcon({ className, title }: InlineIconProps) {
  return (
    <svg viewBox="0 0 306 320" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path fill="currentColor" d="M123.2,118.3V85c0-2.2,0.6-3.8,2.9-5.1L187.9,44c8.3-4.8,18.9-7,29.2-7c39.1,0,63.8,30.1,63.8,62.5c0,2.6,0,6.1-0.6,9l-64.7-37.8c-3.2-1.9-6.7-2.2-10.6,0L123.2,118.3z M266.1,236.6v-74c0-4.2-1.6-7-5.4-9.3l-82-47.7l28.8-16.7c1.6-1,4.2-1,5.8,0l62.2,35.9c17.6,10.3,29.8,32.7,29.8,54.1C305.2,204.2,289.8,227.6,266.1,236.6z M106.2,172.8l-28.5-17c-2.2-1.3-2.9-2.9-2.9-5.1V79.3c0-34.9,26.6-61.2,62.8-61.2c14.1,0,27.6,4.8,38.4,13.5L111.7,69c-3.8,2.2-5.4,5.1-5.4,9.3V172.8z M162,204.9l-38.8-21.8v-46.1l38.8-21.8l38.4,21.8v46.1L162,204.9z M186,301.9c-14.1,0-27.6-4.8-38.4-13.5L212,251c3.8-2.2,5.4-5.1,5.4-9.3v-94.5l28.8,17c2.2,1.3,2.9,2.9,2.9,5.1v71.5C249.1,275.7,222.2,301.9,186,301.9z M110.4,231.1l-62.2-35.9c-17.6-10.3-29.8-32.7-29.8-54.1c0-25.6,15.7-48.7,39.4-57.7v74.3c0,4.2,1.6,7,5.4,9.3l81.7,47.4l-28.8,16.7C114.6,232.1,112,232.1,110.4,231.1z M106.5,283c-36.8,0-63.8-27.6-63.8-61.8c0-3.2,0.3-6.4,0.6-9.3l64.4,37.2c3.8,2.2,7,2.2,10.9,0l81.7-47.4V235c0,2.2-0.6,3.8-2.9,5.1L135.7,276C127.4,280.8,116.8,283,106.5,283z M186,319.2c38.4,0,70.5-27.6,77.5-64.1c35.9-9,59-42.3,59-76.3c0-22.4-9.6-43.9-27.2-59.6c1.6-6.7,2.9-13.8,2.9-20.5c0-45.2-36.8-79.1-79.1-79.1c-8.7,0-17.3,1.6-25.6,4.5C179,9.7,159.4,0.8,137.6,0.8c-38.4,0-70.5,27.6-77.5,64.1c-35.9,9-59,42.3-59,76.3c0,22.4,9.6,43.9,27.2,59.6c-1.6,6.7-2.9,13.8-2.9,20.5c0,45.2,36.8,79.1,79.1,79.1c8.7,0,17.3,1.6,25.6,4.5C144.7,310.3,164.2,319.2,186,319.2z" />
    </svg>
  );
}

function OpenCodeIcon({ className, title }: InlineIconProps) {
  return (
    <svg viewBox="0 0 240 300" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path d="M180 240H60V120H180V240Z" fill="currentColor" fillOpacity="0.4" />
      <path d="M180 60H60V240H180V60ZM240 300H0V0H240V300Z" fill="currentColor" />
    </svg>
  );
}

function KimiIcon({ className, title }: InlineIconProps) {
  return (
    <svg viewBox="0 0 24 25" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path fill="currentColor" d="M21.72 0.94C22.95 0.94 23.95 1.94 23.95 3.17C23.95 4.4 22.95 5.4 21.72 5.4H19.75C19.6 5.4 19.49 5.28 19.49 5.14V3.17C19.49 1.94 20.49 0.94 21.72 0.94Z" />
      <path fill="currentColor" d="M9.39 13.95L17.82 5.59C17.98 5.43 17.89 5.12 17.68 5.12H13.14C13.14 5.12 13.04 5.14 13 5.18L3.92 14.19C3.78 14.33 3.57 14.21 3.57 13.98V5.39C3.57 5.24 3.47 5.12 3.35 5.12H0.22C0.1 5.12 0 5.24 0 5.39V23.92C0 24.07 0.1 24.19 0.22 24.19H3.35C3.47 24.19 3.57 24.07 3.57 23.92V20.14C3.57 20.06 3.6 19.98 3.65 19.93L6.47 17.14C6.54 17.07 6.63 17.06 6.71 17.11L14.24 22.65C15.47 23.48 16.85 23.99 18.25 24.14C18.37 24.15 18.48 24.03 18.48 23.87V20.31C18.48 20.17 18.4 20.06 18.29 20.05C17.47 19.92 16.66 19.6 15.94 19.11L9.42 14.39C9.28 14.3 9.27 14.07 9.39 13.95Z" />
    </svg>
  );
}

function CursorIcon({ className, title }: InlineIconProps) {
  return (
    <svg viewBox="400 395 168 191" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path
        fill="currentColor"
        d="M563.463 439.971L487.344 396.057C484.899 394.646 481.883 394.646 479.439 396.057L403.323 439.971C401.269 441.156 400 443.349 400 445.723V534.276C400 536.647 401.269 538.843 403.323 540.029L479.443 583.943C481.887 585.353 484.903 585.353 487.347 583.943L563.466 540.029C565.521 538.843 566.79 536.651 566.79 534.276V445.723C566.79 443.352 565.521 441.156 563.466 439.971H563.463ZM558.681 449.273L485.199 576.451C484.703 577.308 483.391 576.958 483.391 575.966V492.691C483.391 491.027 482.501 489.488 481.058 488.652L408.887 447.016C408.03 446.52 408.38 445.209 409.373 445.209H556.337C558.424 445.209 559.728 447.47 558.685 449.276H558.681V449.273Z"
      />
    </svg>
  );
}

function TerminalIcon({ className, title }: InlineIconProps) {
  return (
    <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path fill="currentColor" d="M3 5l9 7-9 7V5z" />
      <path fill="currentColor" d="M14 17h7v3h-7z" />
    </svg>
  );
}

function OpenRouterIcon({ className, title }: InlineIconProps) {
  return (
    <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path fillRule="evenodd" fill="currentColor" d="M12 2 L21 7 L21 17 L12 22 L3 17 L3 7 Z M12 6 L19 10 L19 14 L12 18 L5 14 L5 10 Z" />
    </svg>
  );
}

function DeepSeekIcon({ className, title }: InlineIconProps) {
  return (
    <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <path
        fill="currentColor"
        d="M20.25 10.5c-.4-3.5-3.2-6.25-6.75-6.5-4.25-.3-7.85 2.75-8.25 7-.25 2.6 1 5 3 6.4v2.6c0 .55.45 1 1 1h5.5c.55 0 1-.45 1-1v-2.2c2.75-1.45 4.65-4.25 4.5-7.3zM9 13a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3zm6 0a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3z"
      />
    </svg>
  );
}

function CommandCodeIcon({ className, title }: InlineIconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" xmlns="http://www.w3.org/2000/svg" className={className} aria-hidden="true">
      <title>{title}</title>
      <polyline points="4 17 10 11 4 5" />
      <line x1="12" y1="19" x2="20" y2="19" />
    </svg>
  );
}

type BrandRegistration = Brand & { readonly aliases?: readonly string[] };

// Colour-baked assets stay as images; monochrome marks remain inline so
// currentColor can follow the surface where ProviderIcon is rendered.
const BRANDS: readonly BrandRegistration[] = [
  { id: 'anthropic', aliases: ['claude'], icon: { kind: 'inline', component: ClaudeCodeIcon }, chipHex: '#1d7cfc', chipClass: 'bg-blue-500' },
  { id: 'minimax', aliases: ['mcode'], icon: { kind: 'image', src: minimaxLogo }, chipHex: '#6366f1', chipClass: 'bg-indigo-500' },
  { id: 'kimi', icon: { kind: 'inline', component: KimiIcon }, chipHex: '#00c4c4', chipClass: 'bg-cyan-500' },
  // Cursor's current 2D mark is the warm-black/light cube treatment shown
  // in the official brand assets: https://cursor.com/en-US/brand
  { id: 'cursor', icon: { kind: 'inline', component: CursorIcon }, chipHex: '#1B1913', chipClass: 'bg-neutral-900' },
  { id: 'agy', icon: { kind: 'image', src: antigravityLogo }, chipHex: '#10b981', chipClass: 'bg-emerald-500' },
  { id: 'opencode', icon: { kind: 'inline', component: OpenCodeIcon }, chipHex: '#f59e0b', chipClass: 'bg-amber-500' },
  // DeepSeek brand identity (issue #1127). The harness (`dsh`) and the
  // First-class Model Provider (`deepseek`) share one brand record; the
  // primary id is `deepseek` so `brandFor('deepseek')` resolves to its own
  // entry rather than aliasing. `dsh` and `deepseek-harness` stay
  // resolvable as aliases for the harness-side lookups (`brandFor('dsh')`)
  // so the DeepSeek Harness adapter's brand chip keeps working.
  { id: 'deepseek', aliases: ['dsh', 'deepseek-harness'], icon: { kind: 'inline', component: DeepSeekIcon }, chipHex: '#1E88E5', chipClass: 'bg-blue-600' },
  { id: 'commandcode', aliases: ['cmdc', 'command-code'], icon: { kind: 'inline', component: CommandCodeIcon }, chipHex: '#8C4EDD', chipClass: 'bg-purple-600' },
  { id: 'terminal', icon: { kind: 'inline', component: TerminalIcon }, chipHex: '#9ca3af', chipClass: 'bg-gray-500' },
  { id: 'codex', icon: { kind: 'inline', component: OpenAIIcon }, chipHex: '#10a37f', chipClass: 'bg-gray-500' },
  { id: 'openrouter', icon: { kind: 'inline', component: OpenRouterIcon }, chipHex: '#615EFF', chipClass: 'bg-gray-500' },
  { id: 'grok', icon: { kind: 'image', src: grokLogo }, chipHex: '#0A0A0A', chipClass: 'bg-gray-500' },
];

const BRAND_REGISTRY = new Map<string, Brand>();

for (const { aliases = [], ...brand } of BRANDS) {
  BRAND_REGISTRY.set(brand.id, brand);
  for (const alias of aliases) BRAND_REGISTRY.set(alias, brand);
}

/** Resolve a native harness or Proxied Spawn Option id to brand identity. */
export function brandFor(id: string): Brand | undefined {
  const separator = id.indexOf(':');
  const brandId = separator === -1 ? id : id.slice(separator + 1);
  return BRAND_REGISTRY.get(brandId);
}
