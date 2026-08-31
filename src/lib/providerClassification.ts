/**
 * Self-auth / keyed-first-class classification for built-in Model Provider
 * ids (issue #571 / ADR-0025). The single source of truth on the Rust side
 * is `BUILTIN_PROVIDER_ACCOUNTS` in `src-tauri/src/preferences.rs`; this
 * module is the single TS-side mirror of that table so the classification
 * isn't hand-copied in multiple places (issue #1045). Keep this in sync
 * with the Rust table when a built-in is added or its `self_auth` flag
 * changes.
 */

// Self-auth first-class rows always appear and cannot be removed (ADR-0025).
// Kept in sync with `preferences::default_provider_accounts` /
// `BUILTIN_PROVIDER_ACCOUNTS` (self_auth: true rows).
export const SELF_AUTH_PROVIDER_IDS = ['anthropic', 'codex', 'agy', 'grok', 'opencode', 'commandcode', 'freebuff'];

// Keyed first-class catalog ids — removable once added. Sync with
// `preferences::keyed_first_class_catalog` / `BUILTIN_PROVIDER_ACCOUNTS`
// (self_auth: false rows).
export const KEYED_FIRST_CLASS_IDS = ['minimax', 'kimi', 'openrouter'];

export const isSelfAuthId = (id: string): boolean => SELF_AUTH_PROVIDER_IDS.includes(id);

export const isFirstClassId = (id: string): boolean =>
  SELF_AUTH_PROVIDER_IDS.includes(id) || KEYED_FIRST_CLASS_IDS.includes(id);

/**
 * Whether `id` names a Claude-compatible keyed provider — mirrors
 * `preferences::is_claude_compatible_id` exactly, including the
 * unknown-id case: an id not present in the built-in table is
 * Claude-compatible (`true`), same as an unknown/custom id on the Rust
 * side.
 */
export const isClaudeCompatibleId = (id: string): boolean => !isSelfAuthId(id);
