/**
 * Self-auth / keyed-first-class classification for built-in Model Provider
 * ids and harness aliases (issue #571 / ADR-0025). Account rows follow
 * `BUILTIN_PROVIDER_ACCOUNTS` in `src-tauri/src/preferences.rs`; harnesses
 * sharing an account are listed here too. Keep account classification in
 * sync with the Rust table when a built-in or its `self_auth` flag changes.
 */

// Self-auth first-class rows always appear and cannot be removed (ADR-0025).
// Kept in sync with `preferences::default_provider_accounts` /
// `BUILTIN_PROVIDER_ACCOUNTS` (self_auth: true rows).
// Also includes opencode2, the V2 harness sharing the opencode account.
export const SELF_AUTH_PROVIDER_IDS = ['anthropic', 'codex', 'agy', 'grok', 'opencode', 'opencode2', 'commandcode', 'muse-code', 'perch'];

// Keyed first-class catalog ids — removable once added. Sync with
// `preferences::keyed_first_class_catalog` / `BUILTIN_PROVIDER_ACCOUNTS`
// (self_auth: false rows).
export const KEYED_FIRST_CLASS_IDS = ['minimax', 'kimi', 'openrouter'];

export const isSelfAuthId = (id: string): boolean => SELF_AUTH_PROVIDER_IDS.includes(id);

export const isFirstClassId = (id: string): boolean =>
  SELF_AUTH_PROVIDER_IDS.includes(id) || KEYED_FIRST_CLASS_IDS.includes(id);

/**
 * Whether `id` names a Claude-compatible keyed provider. Account ids follow
 * `preferences::is_claude_compatible_id`; self-auth harness aliases are also
 * excluded here. Unknown/custom ids are Claude-compatible (`true`).
 */
export const isClaudeCompatibleId = (id: string): boolean => !isSelfAuthId(id);
