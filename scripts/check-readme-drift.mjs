#!/usr/bin/env node
// Issue #1545 — README drift gate.
//
// The README is the user-facing landing page; a stale provider count or a
// missing SmartScreen warning becomes a real support burden. This module
// exposes a pure `checkReadmeDrift` function so tests can run it in-memory
// without spawning child processes, and the CLI wrapper below turns the
// same function into a script.
//
// Source of truth: `src/types/generated/Provider.ts` (the ts-rs-generated
// binding for the `Provider` enum — committed and regen-gated per the
// shared-types rule in CLAUDE.md) and `HARNESS_LABEL` exported from
// `src/components/Circuits/harnessCapabilities.ts` (the canonical harness
// label used by the Inspector dropdown and Spawn Menu — `UiMeta::label`
// in Rust mirrors this same table).
//
// Run:
//   node scripts/check-readme-drift.mjs          # CI / local
//   npm run check:readme                         # npm wrapper
//
// Test in-memory:
//   import { checkReadmeDrift } from './scripts/check-readme-drift.mjs';

import { readFileSync, existsSync, realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve, dirname } from 'node:path';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
export const repoRoot = resolve(__dirname, '..');
export const readmePath = resolve(repoRoot, 'README.md');
export const providerTypesPath = resolve(repoRoot, 'src/types/generated/Provider.ts');
export const harnessLabelPath = resolve(repoRoot, 'src/components/Circuits/harnessCapabilities.ts');

/** Number → English word for small integers. Used to verify the README's
 *  count claim ("Twelve harnesses") against the actual Provider enum
 *  size. Capped at 24 because reading the README as "twenty-five" reads
 *  worse than rewriting the count sentence; the bail below catches
 *  out-of-range counts so a future enum growth surfaces a clear error. */
const NUMBER_WORDS = [
  'zero', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight', 'nine',
  'ten', 'eleven', 'twelve', 'thirteen', 'fourteen', 'fifteen', 'sixteen',
  'seventeen', 'eighteen', 'nineteen', 'twenty', 'twenty-one', 'twenty-two',
  'twenty-three', 'twenty-four',
];
export function numberToWord(n) {
  if (!Number.isInteger(n) || n < 0 || n >= NUMBER_WORDS.length) return null;
  return NUMBER_WORDS[n];
}

/** Parse the `Provider` union out of the generated ts-rs binding.
 *  Reads ONLY the union body of `export type Provider = ...;` — not the
 *  whole document, so docstring content (ts-rs converts `///` Rust docs
 *  into JSDoc) cannot leak quoted tokens into the variant list. */
export function parseProviderVariants(providerTypes) {
  const m = providerTypes.match(/export\s+type\s+Provider\s*=\s*([^;]+);/);
  if (!m) return [];
  return [...new Set([...m[1].matchAll(/"([a-z][a-z0-9_-]*)"/g)].map((x) => x[1]))];
}

/** Parse the `HARNESS_LABEL` table out of harnessCapabilities.ts as text.
 *  Avoids pulling in ts-node / a TypeScript build step. The shape is
 *  statically known (`export const HARNESS_LABEL: Record<...> = { key: 'label', ... };`)
 *  and the regex is forgiving enough to survive the project's 2-space
 *  indent + trailing-comma convention. If the shape ever changes, this
 *  helper returns `null` and the gate trips with a clear error. */
export function parseHarnessLabel(ts) {
  const m = ts.match(/export\s+const\s+HARNESS_LABEL[^{]*\{([\s\S]*?)\n\}\s*;?/);
  if (!m) return null;
  const out = {};
  for (const e of m[1].matchAll(/^\s*([a-z][a-z0-9_-]*)\s*:\s*['"]([^'"]+)['"]/gm)) {
    out[e[1]] = e[2];
  }
  return out;
}

/** Extract the body of the "Multi-agent orchestration" subsection
 *  under "## Features". All harness-label + count-claim checks run
 *  against this scoped slice — never against the whole document — so
 *  that bare words like "Terminal" inside unrelated prose (e.g.
 *  "persistent xterm.js terminal") cannot false-pass.
 *
 *  The shape is the standard `### Heading` immediately followed by a
 *  bullet list whose lines start with `- `. Returns `null` when the
 *  heading is missing, so the gate trips with a clear anchor rather
 *  than silently failing every downstream check. */
export function featuresMultiAgentSection(readme) {
  const m = readme.match(/###\s+Multi-agent orchestration\s*\n((?:- [^\n]*\n?)+)/);
  if (!m) return null;
  return m[1];
}

/** Format a failure as a single human-readable line. Centralised so
 *  every anchor speaks in the same voice and the test file can assert
 *  on substrings rather than exact wording. */
function failure(anchor, message) {
  return `[${anchor}] ${message}`;
}

/** Pure check. Returns `{failures, variants, harnessLabel}` so callers
 *  (tests, the CLI wrapper, future CI scripts) can introspect the
 *  result without re-running file I/O. */
export function checkReadmeDrift({ readme, providerTypes, harnessLabel }) {
  const failures = [];
  const variants = parseProviderVariants(providerTypes);

  if (variants.length === 0) {
    failures.push(failure(
      'provider-binding',
      'Could not parse any variants from the Provider union in src/types/generated/Provider.ts. ' +
        'The generated ts-rs binding shape may have changed.'
    ));
    return { failures, variants, harnessLabel: harnessLabel ?? {} };
  }

  const labels = harnessLabel ?? {};
  if (Object.keys(labels).length === 0) {
    failures.push(failure(
      'harness-label-source',
      'Could not parse the HARNESS_LABEL table from src/components/Circuits/harnessCapabilities.ts. ' +
        'The export shape may have changed.'
    ));
  }

  // 1. Every Provider variant must have a HARNESS_LABEL entry, and that
  //    label must appear in the README's "Multi-agent orchestration"
  //    bullet (scoped — never the whole document). This is the *real*
  //    drift check: adding a 13th provider to the enum without
  //    updating HARNESS_LABEL and the README trips the gate. Scoping
  //    to the bullet prevents bare labels like "Terminal" from
  //    false-passing via unrelated prose ("persistent xterm.js
  //    terminal", etc.).
  const featuresSection = featuresMultiAgentSection(readme);
  if (featuresSection === null) {
    failures.push(failure(
      'features-multi-agent-section',
      'README.md is missing the "### Multi-agent orchestration" bullet list under "## Features". ' +
        'The drift gate needs that bullet to scope harness-label + count claims.'
    ));
  }
  const checkScope = featuresSection ?? '';

  for (const variant of variants) {
    if (!(variant in labels)) {
      failures.push(failure(
        'harness-label-coverage',
        `Provider variant "${variant}" has no entry in HARNESS_LABEL. ` +
          'Add it to src/components/Circuits/harnessCapabilities.ts and mention it in README.md.'
      ));
      continue;
    }
    const label = labels[variant];
    if (!checkScope.includes(label)) {
      failures.push(failure(
        'harness-label-mentioned',
        `Provider variant "${variant}" (label "${label}") is not in the Features → Multi-agent orchestration bullet. ` +
          'Add it there so users see the same name in docs and in the Spawn Menu.'
      ));
    }
  }

  // 2. The README's count claim must match the variant count, scoped
  //    to the Features bullet. Accepts both English ("Twelve") and
  //    Arabic ("12") so the prose isn't forced into one style.
  const countWord = numberToWord(variants.length);
  if (!countWord) {
    failures.push(failure(
      'count-word-range',
      `${variants.length} variants is outside the number-to-word helper's range. ` +
        'Extend NUMBER_WORDS in scripts/check-readme-drift.mjs or rewrite the count sentence.'
    ));
  } else if (featuresSection !== null) {
    const wordRe = new RegExp(`\\b${countWord}\\b`, 'i');
    const numeralRe = new RegExp(`\\b${variants.length}\\b`);
    if (!wordRe.test(featuresSection) && !numeralRe.test(featuresSection)) {
      failures.push(failure(
        'count-claim',
        `The Features → Multi-agent orchestration bullet does not mention "${countWord}" or "${variants.length}" harnesses. ` +
          'Add the count so the README stays in lock-step with the Provider enum.'
      ));
    }
  }

  // 3. The README must not regress to the old "Six providers" copy.
  if (readme.includes('Six providers')) {
    failures.push(failure(
      'no-stale-six',
      'Stale "Six providers" claim reappeared in README.md. ' +
        `The current catalog has ${variants.length} harnesses; list them by display name instead.`
    ));
  }

  // 4. Quit-lifecycle overstatement must NOT reappear.
  if (readme.includes('never interrupts')) {
    failures.push(failure(
      'no-stale-never-interrupts',
      'README.md claims quitting "never interrupts" an agent. ' +
        'The lifecycle manager prompts to confirm before killing non-resumable sessions.'
    ));
  }

  // 5. Supported-platforms claim must mention Windows 10/11 as the
  //    stable channel (release.yml runs on windows-latest).
  if (!readme.includes('Windows 10/11')) {
    failures.push(failure(
      'windows-stable-channel',
      'README.md is missing the Windows 10/11 supported-platforms claim. ' +
        '.github/workflows/release.yml publishes only Windows installers; the README must reflect that.'
    ));
  }

  // 6. SmartScreen / unsigned-installer warning must remain.
  if (!readme.includes('SmartScreen')) {
    failures.push(failure(
      'smartscreen-warning',
      'README.md is missing the SmartScreen unsigned-installer warning. ' +
        'First launch on Windows shows "unknown publisher" until OS code-signing lands.'
    ));
  }

  // 7. Data-directory path must match what the Tauri app uses.
  const dataDir = '%APPDATA%\\com.alond.buildmesh';
  if (!readme.includes(dataDir)) {
    failures.push(failure(
      'data-directory',
      `README.md does not document the data directory at ${dataDir}. ` +
        'src-tauri/src/lib.rs derives this from app.path().app_data_dir() under the bundle identifier.'
    ));
  }

  // 8. A "Get help" heading must be present at any level (line-anchored
  //    regex, NOT `readme.includes('## Get help')` which accidentally
  //    matches inside `'### Get help'`).
  if (!/^#{1,6}\s+Get help\s*$/m.test(readme)) {
    failures.push(failure(
      'get-help-heading',
      'README.md is missing a "Get help" heading at any level. ' +
        'The end-user support quickstart requires it as a discoverable section.'
    ));
  }

  // 9. CLAUDE.md forbids internal issue numbers in user-facing docs. The
  //    regex set targets every shape contributors paste from internal docs:
  //      `[#NNNN](url)`           — GitHub issue link
  //      `(issue #NNNN)`          — prose in headings
  //      `(wayfinder #NNNN ...)`  — title attribute
  //      `(see #NNNN)`            — parenthetical cross-ref
  //      `fixed in #NNNN`         — prose
  //      `## Foo (issue #NNNN)`   — heading with parenthetical
  //      `#NNNN's`                — possessive prose
  //      `(#NNNN)`                — parenthetical cross-ref
  //      `ADR-NNNN`               — internal ADR cross-ref
  //      bare `#NNNN` (e.g. `Seatbelt, #497`) — prose
  //
  //    GitHub issue numbers are 1-5+ digits (the repo crossed #10000
  //    years ago), so `\d+` is the right width. The bare-`#NNNN`
  //    pattern's negative lookbehind/lookahead prevent it from
  //    matching `[#NNNN]` (link), `## NNNN` (heading), or ` #NNNN5`
  //    (alphanumeric continuation).
  //
  //    Each match is normalised before dedup so e.g. `(issue #1234)`
  //    and the bare `#1234` inside it collapse to a single entry —
  //    counting the same issue twice makes the failure message noisy.
  const issueRefPatterns = [
    /\[#\d+\]\([^)]+\)/g,
    /\(issue\s+#\d+\)/gi,
    /\(wayfinder\s+#\d+\b[^)]*\)/gi,
    /\(see\s+#\d+\)/gi,
    /\bfixed\s+in\s+#\d+\b/gi,
    /##\s+.*\(issue\s+#\d+\)/gi,
    /#\d+'s\b/g,
    /\(#\d+\)/g,
    /\bADR-\d+\b/g,
    /(?<!\[)(?<!#)#\d+\b(?![\w-])/g,
  ];
  // Normalise a match to just its issue number so the same reference
  // caught by multiple patterns collapses to one entry.
  const normalise = (s) => {
    const n = s.match(/#\d+/);
    return n ? n[0] : s;
  };
  const matchedRefs = new Set();
  for (const re of issueRefPatterns) {
    const m = readme.match(re);
    if (m) for (const hit of m) matchedRefs.add(normalise(hit));
  }
  if (matchedRefs.size > 0) {
    const unique = [...matchedRefs];
    failures.push(failure(
      'no-internal-issue-refs',
      `README.md contains ${unique.length} internal issue reference(s): ${unique.slice(0, 5).join(', ')}${unique.length > 5 ? ', ...' : ''}. ` +
        'CLAUDE.md forbids internal issue numbers and ADR cross-refs in user-facing docs — replace with descriptive prose.'
    ));
  }

  return { failures, variants, harnessLabel: labels };
}

// ---------------------------------------------------------------------------
// CLI wrapper. The pure function above is what tests exercise; the wrapper
// just reads the source-of-truth files, calls the function, and translates
// the result into the CI exit-code contract.
// ---------------------------------------------------------------------------

function bail(message) {
  console.error(`::error::${message}`);
  process.exit(1);
}

function runCli() {
  if (!existsSync(readmePath)) bail(`README.md not found at ${readmePath}`);
  if (!existsSync(providerTypesPath)) {
    bail(
      `Provider enum binding not found at ${providerTypesPath}. ` +
        'The README drift gate needs the generated TS binding; regenerate by running `cargo test` in src-tauri/.'
    );
  }
  if (!existsSync(harnessLabelPath)) {
    bail(
      `HARNESS_LABEL source not found at ${harnessLabelPath}. ` +
        'src/components/Circuits/harnessCapabilities.ts is the canonical label table; the gate cannot proceed without it.'
    );
  }

  const readme = readFileSync(readmePath, 'utf8');
  const providerTypes = readFileSync(providerTypesPath, 'utf8');
  const harnessLabelTs = readFileSync(harnessLabelPath, 'utf8');
  const harnessLabel = parseHarnessLabel(harnessLabelTs);

  const { failures, variants } = checkReadmeDrift({ readme, providerTypes, harnessLabel });

  if (failures.length > 0) {
    console.error(`::error::README drift check failed (${failures.length} issue${failures.length === 1 ? '' : 's'}):`);
    for (const f of failures) console.error(`  - ${f}`);
    console.error('\nFix the README claim(s) above, or update this script if the underlying reality changed.');
    process.exit(1);
  }

  console.log(`README drift check passed (${variants.length} variants from Provider.ts).`);
}

// Only run the CLI wrapper when invoked directly, not when imported as a
// module by tests. `process.argv[1]` may be undefined under dynamic
// import, so guard with realpathSync (which also normalises Windows
// path separators for the comparison).
const isMain = (() => {
  if (!process.argv[1]) return false;
  try {
    return realpathSync(fileURLToPath(import.meta.url)) === realpathSync(process.argv[1]);
  } catch {
    return false;
  }
})();
if (isMain) runCli();