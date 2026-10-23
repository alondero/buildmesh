// Issue #1542 — enforced ESLint + React Hooks gate.
//
// This is a flat config (ESLint 9+). Two principles shape the rule set:
//
//   1. **High-signal correctness only.** Rules that catch real defects
//      (`rules-of-hooks`, `eqeqeq`, `ban-ts-comment`) are errors.
//      Cosmetic / stylistic floods are intentionally omitted — they
//      drown the signal and breed disable-noise.
//
//   2. **`rules-of-hooks` is the reason this exists.** Closed #1242 was
//      a real conditional-hook crash. `react-hooks/rules-of-hooks` runs
//      as error from day one.
//
// `react-hooks/exhaustive-deps` is `warn` (not error) on purpose. The
// codebase has several narrow disable directives, each with a justification;
// we audit them first and tighten the rule after the audit closes. The
// fixtures under `tests/lint-fixtures/` are the regression proof —
// `npm run lint:fixtures` must FAIL on a clean checkout (rules are live).
//
// Global ignores:
//   - `src/types/generated/**`     — ts-rs output, regenerated from
//                                    `cargo test` (issue #359).
//   - `dist/**`, `dist-mobile/**`  — build output.
//   - `tests/lint-fixtures/**`     — intentional violations verified
//                                    by `npm run lint:fixtures`.
//   - `.claude/**`, `docs/**`      — agent / docs (markdown, JSON, etc).
//   - `scripts/version-data.mjs`   — generated.
//
// Globals:
//   - `src/`, `mobile/`, `tests/**/*.{ts,tsx}` — browser globals
//     (Tauri 2 webview + remote-access SPA).
//   - `tests/**` + `*.spec.ts`     — also node globals (vitest + Playwright).
//   - `tests/**/*.mjs`             — node globals only (Node test runner
//                                    scripts; no TS rules, since they're
//                                    plain JavaScript the TS parser is
//                                    permissive on but TS rules would
//                                    flag Node-style code).
//   - `scripts/**`, `*.config.{js,mjs,ts}` — node globals, console allowed.

import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';

// Closed migration list: remove entries as legacy callers adopt domain facets.
// New callers must name their IPC domain in the import (issue #2454).
const legacyTauriImporters = [
  "src/App.tsx",
  "src/components/AgentNodeView/AgentNodeView.tsx",
  "src/components/AgentNodeView/AgentReviewButton.tsx",
  "src/components/AgentNodeView/CenterDiffOverlay.tsx",
  "src/components/AgentNodeView/DiffFileNavDrawer.tsx",
  "src/components/AgentNodeView/GridNodeHeader.tsx",
  "src/components/AgentNodeView/PrDiffView.tsx",
  "src/components/AgentNodeView/PrPill.tsx",
  "src/components/AgentNodeView/PrReviewerSpawnDialog.tsx",
  "src/components/AgentNodeView/SemanticTurnBanner.tsx",
  "src/components/AppSettings/AccountCard.tsx",
  "src/components/AppSettings/AccountsPane.tsx",
  "src/components/AppSettings/AddProviderForm.tsx",
  "src/components/AppSettings/AutopilotPane.tsx",
  "src/components/AppSettings/DataRecoverySection.tsx",
  "src/components/AppSettings/GeneralPane.tsx",
  "src/components/AppSettings/HarnessCard.tsx",
  "src/components/AppSettings/HarnessConfigList.tsx",
  "src/components/AppSettings/HarnessDefaultsSection.tsx",
  "src/components/AppSettings/HarnessesPane.tsx",
  "src/components/AppSettings/harnessOrder.ts",
  "src/components/AppSettings/HarnessOrderList.tsx",
  "src/components/AppSettings/harnessVocabulary.ts",
  "src/components/AppSettings/OpenCodeAccountCard.tsx",
  "src/components/AppSettings/OpenCodeAccountViews.tsx",
  "src/components/AppSettings/OpenCodeAwaitingActivationView.tsx",
  "src/components/AppSettings/PreferencesCorruptionPanel.tsx",
  "src/components/AppSettings/PromptsPane.tsx",
  "src/components/AppSettings/ProvidersPane.tsx",
  "src/components/AppSettings/ProxiedChildRow.tsx",
  "src/components/AppSettings/RemoteAccessPane.tsx",
  "src/components/AppSettings/SettingsDataContext.tsx",
  "src/components/AppSettings/settingsUtils.ts",
  "src/components/AppSettings/UsageRender.tsx",
  "src/components/AppSettings/useSettingsResources.ts",
  "src/components/BootErrorPanel/BootErrorPanel.tsx",
  "src/components/Circuits/CircuitEditorOverlay.tsx",
  "src/components/Circuits/CircuitFlowEditor.tsx",
  "src/components/Circuits/CircuitRunOverview.tsx",
  "src/components/Circuits/RunHistoryDrawer.tsx",
  "src/components/CommandOmnibar/omnibarActions.ts",
  "src/components/Diff/Diff.tsx",
  "src/components/Diff/diffFormat.ts",
  "src/components/FileTree/ChangedFileRow.tsx",
  "src/components/FileTree/ChangedFilesSection.tsx",
  "src/components/FileTree/FileTree.tsx",
  "src/components/Mesh/MeshCreateModal.tsx",
  "src/components/Probe/AgentChangedFilesList.tsx",
  "src/components/Probe/AiContextSection.tsx",
  "src/components/Probe/ArchivedNodesTab.tsx",
  "src/components/Probe/CircuitRunCapacity.tsx",
  "src/components/Probe/CircuitRunCard.tsx",
  "src/components/Probe/CircuitsProbeTab.tsx",
  "src/components/Probe/GitIssuesTab.tsx",
  "src/components/Probe/GitPullRequestsTab.tsx",
  "src/components/Probe/IssueLabels.tsx",
  "src/components/Probe/ProjectFilesTab.tsx",
  "src/components/Probe/ProjectSettingsTab.tsx",
  "src/components/Probe/RepositoryTab.tsx",
  "src/components/Probe/UsageTab.tsx",
  "src/components/RemoteAccess/RemoteAccessModal.tsx",
  "src/components/shared/PathHeader.tsx",
  "src/components/Sidebar/MeshItem.tsx",
  "src/components/Terminal/BuildRunTerminalRegistry.ts",
  "src/components/Terminal/Terminal.tsx",
  "src/components/Terminal/TerminalRegistry.ts",
  "src/hooks/useAgentChangedFiles.ts",
  "src/hooks/useChangedFiles.ts",
  "src/hooks/useGitBranchStatus.ts",
  "src/hooks/useGitSummary.ts",
  "src/hooks/useMeshGitHubUrl.ts",
  "src/hooks/useMeshGitStatus.ts",
  "src/hooks/useMeshHealth.ts",
  "src/hooks/useMeshRecovery.ts",
  "src/hooks/useMuseSessionTelemetry.ts",
  "src/hooks/useOpenPr.ts",
  "src/hooks/useProviderList.ts",
  "src/hooks/useWindowControlOverlay.ts",
  "src/hooks/useWorktreeEffectiveDir.ts",
  "src/lib/fileDropPaste.ts",
  "src/lib/meshSyncFeedback.ts",
  "src/lib/status.ts",
  "src/lib/updater.ts",
  "src/lib/usagePace.ts",
  "src/lib/worktreeNotifications.ts",
  "src/stores/agentNodeStore.ts",
  "src/stores/blockedCleanupStore.ts",
  "src/stores/exitPromptStore.ts",
  "src/stores/meshSettingsStore.ts",
  "src/stores/meshStore.ts"
];

function ipcImportRestrictions({ legacy = false, transport = false, raw = false } = {}) {
  return ['error', {
    paths: raw ? [] : [{ name: '@tauri-apps/api/core', importNames: ['invoke'], message: 'Raw invoke belongs in the IPC chokepoint.' }],
    patterns: [
      ...(!legacy ? [{ regex: '(^|/)(?:lib/)?tauri(?:\\.ts)?$', message: 'Import the owning domain from lib/tauri/<facet>; the compatibility barrel is deprecated.' }] : []),
      ...(!transport ? [{ regex: '(^|/)tauri/_invoke(?:\\.ts)?$', message: 'Import the owning Tauri domain facet; _invoke is private to src/lib/tauri/.' }] : []),
    ],
  }];
}

export default [
  // ---------- global ignores (intentional exclusions) ------------------------
  {
    ignores: [
      'dist/**',
      'dist-mobile/**',
      'android/**/build/**',
      'android/.gradle/**',
      'node_modules/**',
      'coverage/**',
      'playwright-report/**',
      'test-results/**',
      '.claude/**',
      // Local smoke tests, logs, and screenshots; never product source.
      '.tmp/**',
      '.harness/**',
      '.opencode/**',
      // `.agents/` is a Claude-Code runtime dir (hooks, sessions, tasks,
      // memory) — every entry is gitignored at the per-file level
      // (`.agents/hooks.json`, `.agents/settings.local.json`, etc.) but
      // the directory itself can hold symlinks to `.claude/skills` we
      // don't want ESLint re-traversing.
      '.agents/**',
      '.vscode/**',
      'docs/**',
      // Tauri / Rust source tree — Rust has its own `cargo clippy` gate
      // (issue #1491) and a vendored Node plugin inside it should not
      // be linted by the frontend ESLint config. Tracking issue for the
      // intended Node pass lives in the tauri vendor plugin's repo.
      'src-tauri/**',
      // ts-rs output regenerated from `cargo test` (issue #359).
      'src/types/generated/**',
      // Intentional rule violations verified by `npm run lint:fixtures`.
      // Excluded from the main gate so a clean checkout passes; the
      // fixture check itself asserts they trip the right rules.
      'tests/lint-fixtures/**',
      // Generated by `npm run version:set`.
      'scripts/version-data.mjs',
    ],
  },

  // ---------- baseline: JS recommended + TS recommended ----------------------
  js.configs.recommended,
  // ESLint 10's `recommended` set adds `no-useless-assignment` (dead stores).
  // Deliberately not adopted, for the reason principle 1 above states. Its
  // findings here are initial values that a later assignment replaces before
  // any read — `let text = ''` in the updater's error sanitizer, `let raw: string
  // | null = null` in the grid-controls boot read, `let removeWorktree = false`
  // in the agent-node worktree-close path, and the same shape in three scripts.
  // Satisfying them means restructuring control flow (a defaulted flag, a
  // try/catch that assigns or returns) to buy a check the compiler already
  // makes. That is churn a correctness gate should not buy.
  //
  // The other rule ESLint 10 adds to `recommended` — `preserve-caught-error` —
  // IS adopted: a discarded `cause` is a real diagnostic loss, and the fix at
  // each site is additive (it attaches context to an error that was already
  // being thrown) rather than a control-flow change.
  { rules: { 'no-useless-assignment': 'off' } },
  { files: ['android/terminal/*.js'], languageOptions: { globals: globals.browser } },
  ...tseslint.configs.recommended,

  // ---------- all TS / TSX files (src/ + mobile/ + tests/**) -----------------
  {
    files: ['**/*.{ts,tsx}'],
    plugins: {
      'react-hooks': reactHooks,
    },
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      globals: {
        ...globals.browser,
        ...globals.es2021,
      },
    },
    // `useAsyncEffect` (src/hooks/useAsyncEffect.ts) is the repo's own
    // wrapper around useEffect that takes a runtime abort signal. The
    // React Hooks plugin only knows the built-in hooks by default;
    // without `additionalHooks`, the ~25 files that call useAsyncEffect
    // get no `exhaustive-deps` coverage at all. `useLayoutEffect` is
    // already part of the plugin's built-in hook list, so it doesn't
    // need to be added (listing it explicitly would be a no-op).
    settings: {
      reactHooks: {
        additionalHooks: 'useAsyncEffect',
      },
    },
    rules: {
      // ---- React Hooks (the why-does-this-exist) ----------------------------
      'react-hooks/rules-of-hooks': 'error',
      // `exhaustive-deps` stays at warn until the audit closes — see
      // the file-level comment for context. Tighten to error after.
      // Combined with `npm run lint -- --max-warnings 0` (see package.json),
      // every newly-introduced violation still fails the gate even though
      // the rule severity is "warn".
      'react-hooks/exhaustive-deps': 'warn',

      // ---- TypeScript safety ------------------------------------------------
      // TS already enforces `noUnusedLocals` + `noUnusedParameters`; the
      // ESLint variant adds the leading-underscore convention.
      '@typescript-eslint/no-unused-vars': [
        'warn',
        {
          argsIgnorePattern: '^_',
          varsIgnorePattern: '^_',
          caughtErrorsIgnorePattern: '^_',
        },
      ],
      // Permit `any` but warn. Two intentional uses remain (xterm.js
      // private API in TerminalRegistry.ts / BuildRunTerminalRegistry.ts);
      // a future tightening would extract a typed wrapper instead of
      // expanding the allow-list.
      '@typescript-eslint/no-explicit-any': 'warn',
      // `// @ts-expect-error` is allowed when followed by a description
      // (forces the contributor to record why the suppression is
      // needed). `// @ts-ignore` is forbidden outright — it has no
      // self-checking behavior and silently rots.
      '@typescript-eslint/ban-ts-comment': [
        'error',
        {
          'ts-ignore': true,
          'ts-expect-error': 'allow-with-description',
          'ts-nocheck': true,
          'ts-check': false,
        },
      ],

      // ---- General correctness ---------------------------------------------
      'eqeqeq': ['error', 'always', { null: 'ignore' }],
      'no-debugger': 'error',
      // `console.warn` / `console.error` / `console.info` are legitimate
      // diagnostic output (pathInvalidatedCache logs fetch failures,
      // frontendLog bridges them to Rust tracing, xterm_mount reports
      // spawn timing). Plain `console.log` is the only entry that's a
      // smell — it's an un-levelled message that's neither a warning
      // nor an info event.
      'no-console': ['warn', { allow: ['warn', 'error', 'info'] }],
      // Empty `case` labels stacking (`case 'a': case 'b': case 'c':`)
      // are idiomatic for mapping multiple ids to one handler — used
      // in `omnibarActions.ts`. The bug class the rule catches is a
      // non-empty case falling through silently; that's still an error.
      'no-fallthrough': ['error', { allowEmptyCase: true }],
      // Prefer `const` for bindings that are never reassigned.
      // `prefer-const` is high-signal and uncontroversial.
      'prefer-const': 'warn',
      // `++` / `--` style footgun in tests (and our code).
      'no-plusplus': 'off',

      // TypeScript already owns this; the legacy ESLint rule produces
      // false positives against TS-narrowed symbols.
      'no-undef': 'off',

      // ---- Zustand whole-store subscriptions (issue #1246) ------------------
      // `useSomeStore()` with NO selector subscribes the component to the
      // entire state object, so every write (an attention flip, a status
      // update, a spawn event) re-renders every subscriber. Per-action
      // selectors return references Zustand stores once at creation, so
      // `Object.is` short-circuits and the component stays quiet. App.tsx
      // pays this cost more than anyone: it sits above every node.
      //
      // `useSomeStore.getState()` is unaffected (the callee is a member
      // expression, not an identifier) — imperative reads inside handlers and
      // effects are the right tool and stay allowed.
      //
      // This rule replaced a test that read `src/App.tsx` and regexed it for
      // the anti-pattern spelling (issue #2240): one file's source text is a
      // weaker gate than a lint rule over every file. Real consumer render
      // isolation is covered by `tests/unit/sidebar-render-count.test.tsx`.
      'no-restricted-syntax': [
        'error',
        {
          selector:
            "CallExpression[callee.type='Identifier'][callee.name=/^use[A-Z][A-Za-z0-9]*Store$/]:not([arguments.0])",
          message:
            'Whole-store Zustand subscription: pass a per-action selector (issue #1246). `useStore.getState()` is the right call for imperative reads.',
        },
      ],
    },
  },

  {
    files: ['src/**/*.{ts,tsx}'],
    rules: { 'no-restricted-imports': ipcImportRestrictions() },
  },
  {
    files: legacyTauriImporters,
    rules: { 'no-restricted-imports': ipcImportRestrictions({ legacy: true }) },
  },
  {
    files: ['src/lib/tauri/*.ts'],
    rules: { 'no-restricted-imports': ipcImportRestrictions({ transport: true }) },
  },
  {
    files: ['src/lib/tauri/_invoke.ts', 'src/lib/frontendLog.ts'],
    rules: { 'no-restricted-imports': ipcImportRestrictions({ transport: true, raw: true }) },
  },
  {
    files: ['src/mobile/screens/NodeList.tsx'],
    rules: { 'max-lines': ['error', { max: 600 }] },
  },

  // ---------- test TS/TSX files: relaxed rules for vitest / Playwright ------
  // Tests import a lot of fixtures that they intentionally don't use
  // (mock store actions, helper imports for type-only assertions), log
  // freely during debugging, and use `any` in mock harnesses. The
  // production rules apply, but these three categories are downgraded
  // to a warning that's only an issue if a contributor runs with
  // `--max-warnings 0`. `react-hooks` rules still apply — a conditional
  // hook in a test render would still be caught.
  {
    files: [
      'tests/**/*.{ts,tsx}',
      'tests/**/*.spec.ts',
      'tests/**/*.steps.mjs',
      '**/*.test.{ts,tsx}',
    ],
    languageOptions: {
      globals: {
        ...globals.node,
        ...globals.browser,
      },
    },
    rules: {
      // Test files log freely.
      'no-console': 'off',
      // Mock harnesses use `any` for the Tauri command surface;
      // tightening here would shadow the prod rule without value.
      '@typescript-eslint/no-explicit-any': 'off',
      // Test imports are intentionally broad — many `import` lines
      // exist only to set up side-effecting mocks.
      '@typescript-eslint/no-unused-vars': 'off',
    },
  },

  // ---------- plain .mjs scripts in tests: node globals, no TS rules --------
  // The agent-infra tests and ui-shot step files are pure Node.js —
  // no React, no TSX, no Tailwind. Apply only the JS recommended
  // rules + node globals; TS rules either don't apply or produce
  // false positives against Node-style module imports.
  {
    files: ['tests/**/*.mjs', 'tests/**/*.steps.mjs'],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      globals: {
        ...globals.node,
      },
    },
    rules: {
      // These files intentionally use Node 22+ features; let `globals`
      // (set above) handle the symbol visibility instead of the legacy
      // `no-undef` rule.
      'no-undef': 'off',
      '@typescript-eslint/no-unused-vars': 'off',
      // `tests/integration/ui-shot-circuit-recovery.steps.mjs` etc log
      // freely as test fixtures.
      'no-console': 'off',
    },
  },

  // ---------- scripts + config files: node globals, console allowed ---------
  {
    files: [
      'scripts/**/*.{js,mjs,cjs}',
      '*.config.{js,mjs,ts}',
      'vite.config.ts',
      'vitest.config.ts',
      'playwright.config*.ts',
      'eslint.config.js',
    ],
    languageOptions: {
      globals: {
        ...globals.node,
      },
    },
    rules: {
      // CLI scripts log freely; the no-console warn is for app code.
      'no-console': 'off',
      // Scripts are plain JavaScript (Node 22+); the typescript-eslint
      // vars rule produces false positives against Node-style imports
      // (e.g. dynamic `import()` of `.mjs` hooks from `.mjs` callers).
      '@typescript-eslint/no-unused-vars': 'off',
      // `no-undef` is satisfied by the `globals.node` block above;
      // re-enabling it just duplicates the same lookup.
      'no-undef': 'off',
      // ts-rs in scripts/check-agent-diff.mjs uses dynamic import of
      // .mjs hooks; the dynamic-import restriction would block it.
      '@typescript-eslint/no-require-imports': 'off',
    },
  },
];
