# Worktree Resume Fix — Specification

## Problem Statement

When resuming a session with `cwrap -w --resume <session-id>`, the agent CLI cannot find the session conversation. This happens because:

1. When `-w` is passed without an explicit worktree name, cwrap creates a **new worktree** with an auto-generated name on each spawn
2. Session data is stored at a path derived from the worktree name: `<user>/.claude/projects/<mangled-path--claude-worktrees-<worktree-name>/<session-id>.jsonl`
3. On resume, cwrap creates a new worktree with a different name, so it looks for session data at the wrong path

## Solution

**Pass the worktree name explicitly**: `cwrap -w <worktree-name> --resume <session-id>`

This ensures cwrap looks for session data in the same location where it was originally stored.

## Session Name as Worktree Name

The session is already assigned a three-word hyphenated name (e.g., `async-plotting-riddle`) at creation time via `naming::generate_random_name()`. This name will be used as the worktree name.

### Why This Works

- The session name is stable and deterministic — same name is used throughout the session lifecycle
- cwrap with `-w <name>` creates/finds worktree at `<project>/.claude/worktrees/<name>/`
- Session data is stored at `<user>/.claude/projects/<mangled-project-path--claude-worktrees-<name>/<session-id>.jsonl`
- Resume with `-w <name>` looks in the correct location

## Database Schema Changes

Add `worktree_name TEXT` column to `sessions` table:

```sql
ALTER TABLE sessions ADD COLUMN worktree_name TEXT;
```

- Nullable — empty for non-cwrap providers, non-git projects, or sessions created before this fix
- When present, passed to cwrap as `-w <worktree_name>`

## Model Changes

### Session struct (models/mod.rs)

Add `worktree_name: Option<String>` field.

### Database Functions (db/mod.rs)

1. `get_session_by_id_inner`: Read `worktree_name` column
2. `create_session`: Accept `worktree_name` parameter (nullable)
3. `list_suspended_sessions`: Include `worktree_name` in returned sessions

## Command Flow

### Session Creation

```
create_session(project_id, name, path, branch, provider)
  → db::create_session(..., worktree_name: Some(name))
  → session.path = path (main repo, not worktree path yet)
  → session.worktree_name = Some(name)
```

### Agent Spawning (spawn_agent_inner)

```
spawn_agent(session_id, provider, resume, rows, cols)
  → get session from DB (includes worktree_name)
  → for cwrap providers (Anthropic, Minimax):
      → worktree_name = session.worktree_name
      → if worktree_name.is_some():
          → args.push("-w".to_string())
          → args.push(worktree_name.clone())
      → else: args.push("-w") (no explicit name)
  → build command with worktree args
```

### Resume Semantics

| Scenario | worktree_name | Command |
|----------|---------------|---------|
| New session, cwrap provider | Some("name") | `-w name --session-id <id>` |
| Resume session, cwrap provider | Some("name") | `-w name --resume <id>` |
| New session, non-cwrap provider | None | no `-w` flag |
| Resume session, non-cwrap provider | None | no `-w` flag |

## Backward Compatibility

- Sessions created before this fix have `worktree_name = NULL`
- On resume, if `worktree_name` is NULL, fall back to `-w` without explicit name
- This may still fail for old sessions (expected — those sessions were stored with auto-generated names anyway)

## Test Cases

### Unit Tests (db/tests.rs or new test module)

1. `create_session_with_worktree_name`: Verify worktree_name is stored and retrieved
2. `get_session_includes_worktree_name`: Round-trip through DB
3. `list_suspended_sessions_includes_worktree_name`: For auto-resume flow

### Integration Tests (agent_tests.rs)

1. `spawn_with_explicit_worktree_name`: Verify command args contain `-w <name>`
2. `resume_with_explicit_worktree_name`: Verify resume uses same worktree name
3. `non_cwrap_provider_no_worktree_flag`: Verify gemini/opencode don't get `-w`
4. `null_worktree_name_falls_back_to_w_without_name`: For backward compat

## Files to Modify

- `src-tauri/src/models/mod.rs` — add `worktree_name: Option<String>` to Session
- `src-tauri/src/db/mod.rs` — add column, update functions
- `src-tauri/src/commands/session.rs` — pass worktree_name at creation
- `src-tauri/src/commands/agent.rs` — pass worktree_name to build_spawn_command