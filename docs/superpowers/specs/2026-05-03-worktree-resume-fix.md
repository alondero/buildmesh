# Worktree Resume Fix — Specification

## Problem Statement

When resuming a session with `cwrap -w --resume <session-id>`, the agent CLI cannot find the session conversation. This happens because:

1. When `-w` is passed without an explicit worktree name, cwrap creates a **new worktree** with an auto-generated name on each spawn
2. Session data is stored at a path derived from the worktree name: `<user>/.claude/projects/<mangled-path--claude-worktrees-<worktree-name>/<session-id>.jsonl`
3. On resume, cwrap creates a new worktree with a different name, so it looks for session data at the wrong path

Furthermore, passing `-w <worktree-name>` on resume is also problematic because `claude` (and `cwrap`) ALWAYS execute `git worktree add` when the `-w` flag is present, resulting in a fatal git error: `worktree already checked out`.

## Solution

**Fresh Spawn**: Pass the worktree name explicitly and spawn in the Git Root: `cwrap -w <worktree-name> --session-id <session-id>`. This ensures `claude` executes `git worktree add` in the correct context.

**Resume**: Omit the `-w` flag entirely and spawn the process directly inside the worktree directory. Because `cwrap` is already executing within the worktree directory, Claude natively recognizes that directory as its current project root, finds the correct session data associated with it, and successfully resumes without ever trying to run `git worktree add` again.

## Session Name as Worktree Name

The session is already assigned a three-word hyphenated name (e.g., `async-plotting-riddle`) at creation time via `naming::generate_random_name()`. This name will be used as the worktree name.

### Why This Works

- The session name is stable and deterministic — same name is used throughout the session lifecycle
- cwrap with `-w <name>` creates/finds worktree at `<project>/.claude/worktrees/<name>/`
- Session data is stored at `<user>/.claude/projects/<mangled-project-path--claude-worktrees-<name>/<session-id>.jsonl`

## Database Schema Changes

Add `worktree_name TEXT` column to `sessions` table:

```sql
ALTER TABLE sessions ADD COLUMN worktree_name TEXT;
```

- Nullable — empty for non-cwrap providers, non-git projects, or sessions created before this fix

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
      → if Assign (fresh spawn):
          → set spawn CWD to main project git root
          → args.push("-w") and args.push(worktree_name)
      → if Resume:
          → set spawn CWD to inside the worktree directory
          → OMIT "-w" entirely to prevent git "already checked out" error
          → args.push("--resume")
  → build command with correct CWD and args
```

### Resume Semantics

| Scenario | spawn CWD | Command |
|----------|-----------|---------|
| New session, cwrap provider | Git Root | `-w name --session-id <id>` |
| Resume session, cwrap provider | Worktree Root | `--resume <id>` (no `-w`) |
| New session, non-cwrap provider | Worktree Root | no `-w` flag |
| Resume session, non-cwrap provider | Worktree Root | no `-w` flag |

## Test Cases

### Unit Tests (db/tests.rs or new test module)

1. `create_session_with_worktree_name`: Verify worktree_name is stored and retrieved
2. `get_session_includes_worktree_name`: Round-trip through DB
3. `list_suspended_sessions_includes_worktree_name`: For auto-resume flow

### Integration Tests (agent_tests.rs)

1. `spawn_with_explicit_worktree_name`: Verify Assign command args contain `-w <name>`
2. `resume_mode_omits_w_flag`: Verify Resume command omits `-w` flag
3. `non_cwrap_provider_no_worktree_flag`: Verify gemini/opencode don't get `-w`
4. `null_worktree_name_falls_back_to_w_without_name`: For backward compat

## Files to Modify

- `src-tauri/src/models/mod.rs` — add `worktree_name: Option<String>` to Session
- `src-tauri/src/db/mod.rs` — add column, update functions
- `src-tauri/src/commands/session.rs` — pass worktree_name at creation
- `src-tauri/src/commands/agent.rs` — logic for conditionally passing `-w` and setting spawn CWD