-- Captured sample rows from `~/.cline/data/db/sessions.db` on Cline
-- 3.0.x. Two root sessions plus a subagent and a closed session,
-- covering every shape the capture / recovery code paths have to
-- distinguish. See fixtures/README.md for the regeneration protocol.

INSERT INTO sessions
    (session_id, source, pid, started_at, ended_at, status, status_lock,
     interactive, provider, model, cwd, workspace_root, enable_tools,
     enable_spawn, enable_teams, transcript_path, hook_path,
     messages_path, updated_at)
VALUES
    -- Current-shape root, idle, interactive.
    ('session_1789901791099_yrvad', 'cli', 1234,
     '2026-09-20T10:56:31.401Z', NULL, 'idle', 0, 1,
     'cline', 'claude-sonnet-4-5', 'F:\src\buildmesh',
     'F:\src\buildmesh', 1, 1, 1,
     '', '', NULL, '2026-09-20T10:56:31.401Z'),

    -- Legacy-shape root, completed, non-interactive (one-shot prompt).
    ('1789757012702_7of3e', 'cli', 1235,
     '2026-09-18T18:43:32.833Z', '2026-09-18T18:43:35.087Z',
     'failed', 0, 0,
     'cline', 'claude-sonnet-4-5',
     'F:\src\buildmesh\.claude\worktrees\stringy-uncaring-hippo',
     'F:\src\buildmesh\.claude\worktrees\stringy-uncaring-hippo',
     1, 1, 1, '', '', NULL,
     '2026-09-18T18:43:35.087Z'),

    -- Subagent continuation of the legacy root above. Must be rejected
    -- by `is_cline_session_id` and by the `NOT LIKE '%__agent_%'`
    -- SQL filter so a `--id` resume picks the parent root, not the
    -- subagent conversation.
    ('session_1789767699203_rfzyx__agent_1789771309502_ctlfb6',
     'cli', 1236,
     '2026-09-18T22:41:49.508Z', '2026-09-18T22:42:31.040Z',
     'completed', 0, 0,
     'cline', 'claude-sonnet-4-5', 'F:\src\buildmesh',
     'F:\src\buildmesh', 1, 1, 1, '', '', NULL,
     '2026-09-18T22:42:31.040Z');
