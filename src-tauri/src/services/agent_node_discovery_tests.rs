//! Tests for `services::agent_node_discovery`.
//!
//! Split out of `src-tauri/src/services/agent_node_discovery.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path = "..."]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test services::agent_node_discovery::tests

use super::*;

#[test]
fn encode_path_replaces_separators() {
    assert_eq!(
        encode_path("/Users/adam/src/buildmesh"),
        "-Users-adam-src-buildmesh"
    );
}

#[test]
fn encode_path_replaces_windows_drive_colon() {
    // Matches Claude Code's on-disk form: ~/.claude/projects/X--src-buildmesh
    // The drive colon and every backslash both collapse to `-`.
    assert_eq!(encode_path("X:\\src\\buildmesh"), "X--src-buildmesh");
    assert_eq!(
        encode_path("C:\\Users\\adam\\src\\buildmesh"),
        "C--Users-adam-src-buildmesh"
    );
}

#[test]
fn encode_path_replaces_dot_for_worktrees() {
    // `.claude\worktrees\foo` → `--claude-worktrees-foo` on Windows.
    assert_eq!(
        encode_path("X:\\src\\buildmesh\\.claude\\worktrees\\foo"),
        "X--src-buildmesh--claude-worktrees-foo"
    );
}

#[test]
fn built_in_worktree_associations_survive_directory_changes() {
    for effective in ["/Users/adam/src/buildmesh/custom-wt", "/tmp/custom-wt"] {
        assert_eq!(
            extract_worktree_name(
                "-Users-adam-src-buildmesh--worktrees-new-node",
                "-Users-adam-src-buildmesh",
                &encode_path(effective),
            ),
            Some("new-node".to_string())
        );
        assert_eq!(
            extract_cursor_worktree_name(
                "Users-adam-src-buildmesh--worktrees-new-node",
                "Users-adam-src-buildmesh",
                &cursor_workspace_slug(effective),
            ),
            Some("new-node".to_string())
        );
        assert!(is_commandcode_project_dir_for_mesh(
            "users-adam-src-buildmesh-worktrees-new-node",
            "users-adam-src-buildmesh",
            &commandcode_project_slug(effective),
        ));
        assert_eq!(
            extract_worktree_name_from_path_with_effective(
                "/Users/adam/src/buildmesh/.worktrees/new-node/src",
                "/Users/adam/src/buildmesh",
                Some(effective),
            ),
            Some("new-node".to_string())
        );
        assert_eq!(
            extract_worktree_name_from_path_with_effective(
                "/Users/adam/src/buildmesh-other/.worktrees/new-node",
                "/Users/adam/src/buildmesh",
                Some(effective),
            ),
            None
        );
    }
}

#[test]
fn cursor_scanner_keeps_default_worktrees_after_override() {
    let root =
        std::env::temp_dir().join(format!("buildmesh_cursor_builtin_{}", std::process::id()));
    let session =
        root.join("Users-adam-src-buildmesh--worktrees-new-node/agent-transcripts/cursor-default");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("cursor-default.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":"Default worktree"}}"#,
    )
    .unwrap();
    let found = discover_cursor_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        Some("/tmp/custom-wt"),
    );
    fs::remove_dir_all(&root).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].worktree_name.as_deref(), Some("new-node"));
}

#[test]
fn extract_worktree_name_works() {
    let base = "-Users-adam-src-buildmesh";
    let legacy_eff = "-Users-adam-src-buildmesh--claude-worktrees";
    assert_eq!(
        extract_worktree_name(
            "-Users-adam-src-buildmesh--claude-worktrees-fancy-name",
            base,
            legacy_eff
        ),
        Some("fancy-name".to_string())
    );
    assert_eq!(
        extract_worktree_name("-Users-adam-src-buildmesh", base, legacy_eff),
        None
    );
    assert_eq!(
        extract_worktree_name("-Users-adam-src-buildmesh-src-tauri", base, legacy_eff),
        None
    );
}

#[test]
fn extract_worktree_name_covers_custom_effective_container() {
    // Issue #1519: relative custom dirs, absolute locations outside the
    // root, and legacy sessions surviving a directory change.
    let base = "-Users-adam-src-buildmesh";
    let custom_eff = "-Users-adam-src-buildmesh--custom-wt";
    assert_eq!(
        extract_worktree_name(
            "-Users-adam-src-buildmesh--custom-wt-olive-fox",
            base,
            custom_eff
        ),
        Some("olive-fox".to_string())
    );
    assert_eq!(
        extract_worktree_name("-tmp-wt-copper-bear", base, "-tmp-wt"),
        Some("copper-bear".to_string())
    );
    // Sibling-prefix trap: `--custom-wt2-` must not match `--custom-wt-`.
    assert_eq!(
        extract_worktree_name(
            "-Users-adam-src-buildmesh--custom-wt2-olive-fox",
            base,
            custom_eff
        ),
        None
    );
    // Pre-change legacy sessions stay visible under a custom effective.
    assert_eq!(
        extract_worktree_name(
            "-Users-adam-src-buildmesh--claude-worktrees-fancy-name",
            base,
            custom_eff
        ),
        Some("fancy-name".to_string())
    );
}

#[test]
fn extract_worktree_name_works_on_windows_encoded_base() {
    let base = "X--src-buildmesh";
    let legacy_eff = "X--src-buildmesh--claude-worktrees";
    assert_eq!(
        extract_worktree_name(
            "X--src-buildmesh--claude-worktrees-bold-live-plume",
            base,
            legacy_eff
        ),
        Some("bold-live-plume".to_string())
    );
    assert_eq!(
        extract_worktree_name("X--src-buildmesh", base, legacy_eff),
        None
    );
}

#[test]
fn strip_tags_removes_xml() {
    assert_eq!(
        strip_tags("<command-message>grill-me</command-message>"),
        "grill-me"
    );
    assert_eq!(strip_tags("hello world"), "hello world");
    assert_eq!(
        strip_tags("<b>bold</b> and <i>italic</i>"),
        "bold and italic"
    );
}

#[test]
fn truncate_respects_boundary() {
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("hello world this is long", 11), "hello world…");
}

#[test]
fn truncate_respects_utf8_char_boundary() {
    // "héllo" — the é is a 2-byte UTF-8 char, so "h" sits at byte 0, "é"
    // starts at byte 1 (and ends at byte 3), "l" at byte 3, and the final
    // bytes total 6. Asking for max=2 would land inside the é and is the
    // classic naive `&s[..max]` panic case. `truncate` must step back to
    // the next char boundary (byte 1) and append the ellipsis.
    assert_eq!(truncate("héllo", 2), "h…");
    // Asking for max=4 lands mid-é too (bytes 0..4 cross both halves),
    // so the safe end is byte 3 and the result is "hél" + ellipsis.
    assert_eq!(truncate("héllo", 4), "hél…");
    // String already short enough for max → passes through untouched.
    assert_eq!(truncate("héllo", 10), "héllo");
}

#[test]
fn is_synthetic_detects_local_command_caveat() {
    assert!(is_synthetic_message(
            "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>"
        ));
    assert!(!is_synthetic_message("Fix the login bug"));
    assert!(!is_synthetic_message("  <p>some html</p>"));
    assert!(!is_synthetic_message(""));
}

/// Write a one-off JSONL session file in the temp dir, suffixed by pid +
/// name so parallel tests don't trample each other.
fn write_session(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "buildmesh_discovery_{name}_{}.jsonl",
        std::process::id()
    ));
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn parse_session_file_uses_first_text_block_for_title() {
    // A multi-text-block user message must yield a single-line title from the
    // FIRST block, not all blocks joined with an interior newline (issue
    // #335). Joining would have produced "Fix the login bug\nand the logout
    // bug" — a newline strip_tags doesn't collapse — before the 80-char cut.
    let body = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fix the login bug"},{"type":"text","text":"and the logout bug"}]},"cwd":"/x","gitBranch":"main","timestamp":"2026-06-14T10:00:00Z","uuid":"u1"}
"#;
    let path = write_session("multiblock", body);
    let parsed = parse_session_file(&path);
    std::fs::remove_file(&path).ok();
    let (title, branch, cwd, ts) = parsed.expect("multi-block user message should parse");
    assert_eq!(
        title, "Fix the login bug",
        "title is the first text block only"
    );
    assert!(!title.contains('\n'), "title stays single-line");
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(cwd.as_deref(), Some("/x"));
    assert_eq!(ts.as_deref(), Some("2026-06-14T10:00:00Z"));
}

#[test]
fn parse_session_file_single_block_is_unchanged() {
    // The common single-block case is identical to the old concat behaviour.
    let body = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Just one block"}]},"uuid":"u1"}
"#;
    let path = write_session("singleblock", body);
    let parsed = parse_session_file(&path);
    std::fs::remove_file(&path).ok();
    let (title, _b, _c, _t) = parsed.expect("single-block message should parse");
    assert_eq!(title, "Just one block");
}

#[test]
fn parse_session_file_skips_synthetic_then_takes_real_prompt() {
    let body = r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>noise</local-command-caveat>"},"uuid":"u0"}
{"type":"user","message":{"role":"user","content":"Real first prompt"},"cwd":"/x","uuid":"u1"}
"#;
    let path = write_session("synthetic", body);
    let parsed = parse_session_file(&path);
    std::fs::remove_file(&path).ok();
    let (title, _b, _c, _t) = parsed.expect("should skip caveat and find the real prompt");
    assert_eq!(title, "Real first prompt");
}

#[test]
fn cursor_discovery_finds_workspace_scoped_sessions_and_worktrees() {
    let root =
        std::env::temp_dir().join(format!("buildmesh_cursor_discovery_{}", std::process::id()));
    let base = root.join("Users-adam-src-buildmesh");
    let worktree = root.join("Users-adam-src-buildmesh--claude-worktrees-fancy-name");
    let session = base.join("agent-transcripts").join("cursor-1");
    let worktree_session = worktree.join("agent-transcripts").join("cursor-2");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::create_dir_all(&worktree_session).unwrap();
    std::fs::write(
        session.join("cursor-1.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":"Inspect the Cursor workspace"}}
"#,
    )
    .unwrap();
    std::fs::write(
        worktree_session.join("cursor-2.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":"Fix the worktree"}}
"#,
    )
    .unwrap();

    let discovered = discover_cursor_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(discovered.len(), 2);
    let base_session = discovered
        .iter()
        .find(|session| session.session_id == "cursor-1")
        .expect("base workspace session should be discovered");
    assert_eq!(base_session.first_message, "Inspect the Cursor workspace");
    assert_eq!(base_session.worktree_name, None);
    let worktree_session = discovered
        .iter()
        .find(|session| session.session_id == "cursor-2")
        .expect("worktree session should be discovered");
    assert_eq!(
        worktree_session.worktree_name.as_deref(),
        Some("fancy-name")
    );
}

#[test]
fn cursor_discovery_ignores_subagents_and_tracked_sessions() {
    let root = std::env::temp_dir().join(format!(
        "buildmesh_cursor_discovery_filter_{}",
        std::process::id()
    ));
    let session = root
        .join("Users-adam-src-buildmesh")
        .join("agent-transcripts")
        .join("cursor-1");
    std::fs::create_dir_all(session.join("subagents")).unwrap();
    std::fs::write(
        session.join("cursor-1.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":"Keep this out"}}
"#,
    )
    .unwrap();
    std::fs::write(
        session.join("subagents").join("child.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":"Never show this"}}
"#,
    )
    .unwrap();
    let tracked = ["cursor-1".to_string()].into_iter().collect();

    let discovered =
        discover_cursor_sessions_in(&root, "/Users/adam/src/buildmesh", &tracked, None);
    std::fs::remove_dir_all(&root).ok();
    assert!(discovered.is_empty());
}

#[test]
fn cursor_discovery_finds_custom_container_worktrees() {
    // Issue #1519: Cursor sessions under a custom effective container
    // (relative and absolute-outside) resolve to the node name.
    let root = std::env::temp_dir().join(format!(
        "buildmesh_cursor_discovery_custom_{}",
        std::process::id()
    ));
    let custom = root.join("Users-adam-src-buildmesh-custom-wt-olive-fox");
    let session = custom.join("agent-transcripts").join("cursor-9");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        session.join("cursor-9.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":"Custom worktree"}}
"#,
    )
    .unwrap();

    let discovered = discover_cursor_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        Some("/Users/adam/src/buildmesh/custom-wt"),
    );
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].worktree_name.as_deref(), Some("olive-fox"));
}

#[test]
fn commandcode_discovery_finds_mesh_sessions_and_filters_foreign_or_tracked() {
    let root = std::env::temp_dir().join(format!(
        "buildmesh_commandcode_discovery_{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();

    let write_session = |id: &str, cwd: &str, prompt: &str, timestamp: &str| {
        std::fs::write(
            root.join(format!("{id}.jsonl")),
            format!(
                r#"{{"type":"session","id":"{id}","cwd":"{cwd}","timestamp":"{timestamp}"}}
{{"type":"model_change","model":"commandcode-default"}}
{{"type":"message","id":"user-1","message":{{"role":"user","content":"{prompt}"}}}}
"#
            ),
        )
        .unwrap();
    };
    write_session(
        "sess_base",
        "/Users/adam/src/buildmesh",
        "Inspect the base workspace",
        "2026-08-31T10:00:00Z",
    );
    write_session(
        "sess_worktree",
        "/Users/adam/src/buildmesh/.claude/worktrees/fancy-name",
        "Inspect the worktree",
        "2026-08-31T11:00:00Z",
    );
    write_session(
        "sess_foreign",
        "/Users/other/project",
        "Do not include this",
        "2026-08-31T12:00:00Z",
    );
    write_session(
        "sess_tracked",
        "/Users/adam/src/buildmesh",
        "Already tracked",
        "2026-08-31T13:00:00Z",
    );

    let tracked = ["sess_tracked".to_string()].into_iter().collect();
    let discovered = discover_commandcode_sessions_in(
        Some(root.as_path()),
        "/Users/adam/src/buildmesh",
        &tracked,
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(discovered.len(), 2);
    let base = discovered
        .iter()
        .find(|session| session.session_id == "sess_base")
        .expect("mesh-root Command Code session should be discovered");
    assert_eq!(base.first_message, "Inspect the base workspace");
    assert_eq!(base.cwd.as_deref(), Some("/Users/adam/src/buildmesh"));
    assert!(base.worktree_name.is_none());

    let worktree = discovered
        .iter()
        .find(|session| session.session_id == "sess_worktree")
        .expect("worktree Command Code session should be discovered");
    assert_eq!(worktree.first_message, "Inspect the worktree");
    assert_eq!(worktree.worktree_name.as_deref(), Some("fancy-name"));
}

#[test]
fn commandcode_project_dir_match_is_anchored_not_prefix_greedy() {
    // Issue #1500 review: `api` must not match `api-gateway` or `api-v2`;
    // only the exact root slug and `{slug}-claude-worktrees-...` match.
    let prefix = "home-user-api";
    // Empty effective slug degrades to legacy-only matching.
    let no_eff = "";
    assert!(is_commandcode_project_dir_for_mesh(
        "home-user-api",
        prefix,
        no_eff
    ));
    assert!(is_commandcode_project_dir_for_mesh(
        "home-user-api-claude-worktrees-fancy-name",
        prefix,
        no_eff
    ));
    assert!(!is_commandcode_project_dir_for_mesh(
        "home-user-api-gateway",
        prefix,
        no_eff
    ));
    assert!(!is_commandcode_project_dir_for_mesh(
        "home-user-api-v2",
        prefix,
        no_eff
    ));
    assert!(!is_commandcode_project_dir_for_mesh(
        "home-user-api-gateway-claude-worktrees-foo",
        prefix,
        no_eff
    ));
    assert!(!is_commandcode_project_dir_for_mesh(
        "home-user-other",
        prefix,
        no_eff
    ));
    assert!(!is_commandcode_project_dir_for_mesh(
        "home-user-api",
        "",
        ""
    ));
    assert!(!is_commandcode_project_dir_for_mesh("anything", "", ""));
    // Case-insensitive for Windows filesystems.
    assert!(is_commandcode_project_dir_for_mesh(
        "HOME-USER-API-CLAUDE-WORKTREES-FOO",
        prefix,
        no_eff
    ));
}

#[test]
fn commandcode_project_dir_match_covers_custom_effective_container() {
    // Issue #1519: relative custom dirs and absolute locations outside
    // the root match via the effective slug; sibling-prefix discipline
    // holds for the new prefix too.
    let mesh = "home-user-repo";
    let eff = "home-user-repo-custom-wt";
    assert!(is_commandcode_project_dir_for_mesh(
        "home-user-repo-custom-wt-olive-fox",
        mesh,
        eff
    ));
    assert!(!is_commandcode_project_dir_for_mesh(
        "home-user-repo-custom-wt2-olive-fox",
        mesh,
        eff
    ));
    let abs_eff = "tmp-wt";
    assert!(is_commandcode_project_dir_for_mesh(
        "tmp-wt-copper-bear",
        mesh,
        abs_eff
    ));
    assert!(!is_commandcode_project_dir_for_mesh(
        "tmp-wt2-copper-bear",
        mesh,
        abs_eff
    ));
}

#[test]
fn commandcode_discovery_finds_uuid_sessions_and_skips_sidecars() {
    // Issue #1500: v1.43.0 session IDs are UUIDs in
    // `projects/<slug>/<uuid>.jsonl` with sidecars beside them.
    let root = std::env::temp_dir().join(format!(
        "buildmesh_commandcode_discovery_uuid_{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();

    let id = "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9";
    std::fs::write(
            root.join(format!("{id}.jsonl")),
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-09-02T19:53:20.151Z\",\"cwd\":\"/Users/adam/src/buildmesh\"}}\n\
                 {{\"type\":\"message\",\"id\":\"user-1\",\"message\":{{\"role\":\"user\",\"content\":\"Inspect the UUID workspace\"}}}}\n"
            ),
        )
        .unwrap();
    // Sidecars must not surface as sessions.
    std::fs::write(
            root.join(format!("{id}.checkpoints.jsonl")),
            r#"{"id":"4f729f43-46ed-4721-a885-3f16f26bff5f","turnNumber":1,"createdAt":"2026-09-02T19:53:40.567Z"}"#,
        )
        .unwrap();

    let discovered = discover_commandcode_sessions_in(
        Some(root.as_path()),
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].session_id, id);
    assert_eq!(discovered[0].first_message, "Inspect the UUID workspace");
}

#[test]
fn commandcode_discovery_associates_custom_container_worktrees() {
    // Issue #1519: a header cwd under the effective container (relative
    // and absolute-outside) maps to the node name.
    let root = std::env::temp_dir().join(format!(
        "buildmesh_commandcode_discovery_custom_{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    let write_session = |id: &str, cwd: &str| {
        std::fs::write(
            root.join(format!("{id}.jsonl")),
            format!(
                r#"{{"type":"session","id":"{id}","cwd":"{cwd}","timestamp":"2026-08-31T10:00:00Z"}}
{{"type":"message","id":"user-1","message":{{"role":"user","content":"Hi"}}}}
"#
            ),
        )
        .unwrap();
    };
    write_session(
        "sess_custom_rel",
        "/Users/adam/src/buildmesh/custom-wt/olive-fox",
    );
    write_session("sess_custom_abs", "/tmp/wt/copper-bear");

    let discovered = discover_commandcode_sessions_in(
        Some(root.as_path()),
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        Some("/Users/adam/src/buildmesh/custom-wt"),
    );
    let abs_discovered = discover_commandcode_sessions_in(
        Some(root.as_path()),
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        Some("/tmp/wt"),
    );
    std::fs::remove_dir_all(&root).ok();

    let rel = discovered
        .iter()
        .find(|s| s.session_id == "sess_custom_rel")
        .expect("relative custom session present");
    assert_eq!(rel.worktree_name.as_deref(), Some("olive-fox"));
    let abs_row = abs_discovered
        .iter()
        .find(|s| s.session_id == "sess_custom_abs")
        .expect("absolute custom session present");
    assert_eq!(abs_row.worktree_name.as_deref(), Some("copper-bear"));
}

// --- AGY discovery (issue #1284) -----------------------------------

/// Build `<brain>/<conv>/.system_generated/logs/transcript.jsonl` with the
/// provided JSONL body. Returns the conv dir so callers can attach extra
/// files or directories if they need to.
fn write_agy_conv(root: &std::path::Path, conv_id: &str, body: &str) -> PathBuf {
    let logs = root.join(conv_id).join(".system_generated").join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("transcript.jsonl"), body).unwrap();
    root.join(conv_id)
}

#[test]
fn agy_discovery_finds_present_sessions_with_first_prompt_and_timestamp() {
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_present_{}",
        std::process::id()
    ));
    // Conversation 1: workspace matches the mesh → no worktree.
    // Conversation 2: workspace is a worktree under the mesh → worktree name.
    write_agy_conv(
        &root,
        "conv-aaaa-1111",
        r#"{"step_type":"USER_INPUT","text":"Open the AGY session","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-07-10T09:00:00Z"}
{"step_type":"MODEL_RESPONSE","text":"Sure thing"}
"#,
    );
    write_agy_conv(
        &root,
        "conv-bbbb-2222",
        r#"{"step_type":"USER_INPUT","text":"Fix the worktree","workspace_path":"/Users/adam/src/buildmesh/.claude/worktrees/fancy-name","created_at":"2026-07-12T11:30:00Z"}
"#,
    );

    let discovered = discover_agy_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(
        discovered.len(),
        2,
        "both conversations should be discovered"
    );
    let base = discovered
        .iter()
        .find(|s| s.session_id == "conv-aaaa-1111")
        .expect("mesh-root session present");
    assert_eq!(base.first_message, "Open the AGY session");
    assert_eq!(
        base.worktree_name, None,
        "mesh-root workspace → no worktree"
    );
    assert_eq!(base.cwd.as_deref(), Some("/Users/adam/src/buildmesh"));
    assert_eq!(base.timestamp.as_deref(), Some("2026-07-10T09:00:00Z"));
    assert!(
        base.branch.is_none(),
        "AGY transcripts don't carry a branch"
    );
    let worktree = discovered
        .iter()
        .find(|s| s.session_id == "conv-bbbb-2222")
        .expect("worktree session present");
    assert_eq!(worktree.first_message, "Fix the worktree");
    assert_eq!(worktree.worktree_name.as_deref(), Some("fancy-name"));
    assert_eq!(worktree.timestamp.as_deref(), Some("2026-07-12T11:30:00Z"));
}

#[test]
fn agy_discovery_skips_absent_sessions() {
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_absent_{}",
        std::process::id()
    ));
    // Brain dir exists but contains no conversation directories → empty
    // result instead of an error. Matches the Cursor/Claude scanners'
    // missing-dir behavior.
    std::fs::create_dir_all(&root).unwrap();

    let discovered = discover_agy_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();
    assert!(discovered.is_empty());
}

#[test]
fn agy_discovery_skips_missing_brain_dir() {
    // A brain dir that doesn't exist must not crash — same contract as the
    // Claude/Cursor scanners (the user may never have run `agy`, so the
    // path is likely absent on first launch).
    let missing = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_missing_{}",
        std::process::id()
    ));
    // Intentionally NOT creating the directory.
    let discovered = discover_agy_sessions_in(
        &missing,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    assert!(discovered.is_empty());
}

#[test]
fn agy_discovery_filters_already_tracked_session_ids() {
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_tracked_{}",
        std::process::id()
    ));
    write_agy_conv(
        &root,
        "conv-tracked",
        r#"{"step_type":"USER_INPUT","text":"Tracked session","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-07-10T09:00:00Z"}
"#,
    );
    write_agy_conv(
        &root,
        "conv-orphan",
        r#"{"step_type":"USER_INPUT","text":"Orphan session","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-07-12T11:30:00Z"}
"#,
    );
    // Mark the first conversation as already tracked by an existing
    // Buildmesh node (so it's expected to be filtered out).
    let tracked: std::collections::HashSet<String> =
        ["conv-tracked".to_string()].into_iter().collect();

    let discovered = discover_agy_sessions_in(&root, "/Users/adam/src/buildmesh", &tracked, None);
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].session_id, "conv-orphan");
    assert_eq!(discovered[0].first_message, "Orphan session");
}

#[test]
fn agy_discovery_drops_conversation_without_transcript() {
    // A conversation directory with no `.system_generated/logs/transcript.jsonl`
    // is treated as absent — same as a missing JSONL in the Claude scanner.
    // Probably a half-written session mid-flight; not safe to surface.
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_no_transcript_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("conv-empty/.system_generated/logs")).unwrap();
    // No transcript.jsonl written.

    let discovered = discover_agy_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();
    assert!(discovered.is_empty());
}

#[test]
fn agy_discovery_carries_timestamps_for_outer_sort() {
    // The scanner itself doesn't sort — `discover()` applies a single
    // unified sort across Claude/Cursor/AGY results. This test only
    // pins that the AGY scanner populates the timestamp field for every
    // discovered session so the outer sort has something to order by.
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_sort_{}",
        std::process::id()
    ));
    write_agy_conv(
        &root,
        "conv-old",
        r#"{"step_type":"USER_INPUT","text":"Old","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-01-01T00:00:00Z"}
"#,
    );
    write_agy_conv(
        &root,
        "conv-new",
        r#"{"step_type":"USER_INPUT","text":"New","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-08-01T00:00:00Z"}
"#,
    );
    write_agy_conv(
        &root,
        "conv-mid",
        r#"{"step_type":"USER_INPUT","text":"Mid","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-05-01T00:00:00Z"}
"#,
    );

    let discovered = discover_agy_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(discovered.len(), 3);
    // Every session must carry a timestamp so the outer `discover()`
    // sort has something to order by. We don't assert ordering here —
    // read_dir order is filesystem-dependent and the per-scanner
    // contract is just "populate the field"; the sort lives one level
    // up.
    assert!(
        discovered.iter().all(|s| s.timestamp.is_some()),
        "every session must carry a timestamp for the outer discover() sort"
    );
    // Spot-check that each row preserved the source timestamp verbatim.
    for session in &discovered {
        let id = &session.session_id;
        let ts = session.timestamp.as_deref().unwrap();
        match id.as_str() {
            "conv-old" => assert_eq!(ts, "2026-01-01T00:00:00Z"),
            "conv-new" => assert_eq!(ts, "2026-08-01T00:00:00Z"),
            "conv-mid" => assert_eq!(ts, "2026-05-01T00:00:00Z"),
            _ => panic!("unexpected session id {id}"),
        }
    }
}

#[test]
fn agy_discovery_sanitizes_first_prompt_via_strip_tags_and_truncates() {
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_sanitize_{}",
        std::process::id()
    ));
    // First USER_INPUT carries a tag-wrapped prompt and a long body so
    // strip_tags + truncate both get exercised (mirrors the Claude scanner's
    // shape — issue #335).
    write_agy_conv(
            &root,
            "conv-tags",
            &format!(
                "{{\"step_type\":\"USER_INPUT\",\"text\":\"<system>{}</system>\",\"workspace_path\":\"/Users/adam/src/buildmesh\",\"created_at\":\"2026-07-10T09:00:00Z\"}}\n",
                "a".repeat(200)
            ),
        );

    let discovered = discover_agy_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(discovered.len(), 1);
    let first_message = &discovered[0].first_message;
    assert!(
        !first_message.contains('<'),
        "tags must be stripped, got: {first_message:?}"
    );
    // `truncate(s, 80)` returns `&s[..80] + "…"` — 80 chars of content
    // plus the ellipsis. The 81-char ceiling is the canonical contract
    // shared with the Claude/Cursor scanners (see `truncate_respects_boundary`
    // in transcript_reader for the matching shape).
    assert!(
        first_message.chars().count() <= 81,
        "first message must be truncated to 80 chars + ellipsis, got: {first_message:?} ({} chars)",
        first_message.chars().count()
    );
    assert!(
        first_message.ends_with('…'),
        "truncated first message must end with the ellipsis, got: {first_message:?}"
    );
}

#[test]
fn agy_discovery_worktree_name_only_when_inside_this_mesh() {
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_foreign_{}",
        std::process::id()
    ));
    // Workspace is under a *different* mesh's `.claude/worktrees/` — must
    // NOT be associated with our mesh's worktree (would mislead the user
    // into thinking a foreign conversation was a Buildmesh worktree).
    write_agy_conv(
        &root,
        "conv-foreign",
        r#"{"step_type":"USER_INPUT","text":"Foreign worktree","workspace_path":"/Users/adam/src/other-repo/.claude/worktrees/brave-fox","created_at":"2026-07-10T09:00:00Z"}
"#,
    );
    write_agy_conv(
        &root,
        "conv-mine",
        r#"{"step_type":"USER_INPUT","text":"My worktree","workspace_path":"/Users/adam/src/buildmesh/.claude/worktrees/gentle-fox","created_at":"2026-07-12T11:30:00Z"}
"#,
    );

    let discovered = discover_agy_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    let foreign = discovered
        .iter()
        .find(|s| s.session_id == "conv-foreign")
        .expect("foreign conversation present");
    assert_eq!(
        foreign.worktree_name, None,
        "foreign-mesh worktree must not attach to ours"
    );
    let mine = discovered
        .iter()
        .find(|s| s.session_id == "conv-mine")
        .expect("our-mesh conversation present");
    assert_eq!(mine.worktree_name.as_deref(), Some("gentle-fox"));
}

#[test]
fn agy_discovery_handles_windows_backslash_workspace_paths() {
    // AGY on Windows may store workspace_path with backslashes; the
    // worktree-association rule must normalize them to forward slashes
    // before matching against `mesh_path` (Windows often mixes both).
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_winpath_{}",
        std::process::id()
    ));
    write_agy_conv(
        &root,
        "conv-win",
        r#"{"step_type":"USER_INPUT","text":"Windows worktree","workspace_path":"C:\\Users\\adam\\src\\buildmesh\\.claude\\worktrees\\bold-fox","created_at":"2026-07-10T09:00:00Z"}
"#,
    );

    let discovered = discover_agy_sessions_in(
        &root,
        "C:\\Users\\adam\\src\\buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].worktree_name.as_deref(), Some("bold-fox"));
}

#[test]
fn agy_discovery_accepts_alternative_workspace_field_names() {
    // AGY may evolve the field name (`working_directory`, `cwd`, etc.);
    // the scanner must keep discovering sessions across the rename.
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_altkeys_{}",
        std::process::id()
    ));
    write_agy_conv(
        &root,
        "conv-cwd",
        r#"{"step_type":"USER_INPUT","text":"cwd field","cwd":"/Users/adam/src/buildmesh/.claude/worktrees/red-bear","created_at":"2026-07-10T09:00:00Z"}
"#,
    );
    write_agy_conv(
        &root,
        "conv-meta",
        r#"{"step_type":"USER_INPUT","text":"metadata.workspace_path","metadata":{"workspace_path":"/Users/adam/src/buildmesh/.claude/worktrees/blue-owl"},"created_at":"2026-07-12T11:30:00Z"}
"#,
    );

    let discovered = discover_agy_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    let cwd_row = discovered
        .iter()
        .find(|s| s.session_id == "conv-cwd")
        .expect("`cwd` field should be discovered");
    assert_eq!(cwd_row.worktree_name.as_deref(), Some("red-bear"));
    let meta_row = discovered
        .iter()
        .find(|s| s.session_id == "conv-meta")
        .expect("`metadata.workspace_path` field should be discovered");
    assert_eq!(meta_row.worktree_name.as_deref(), Some("blue-owl"));
}

#[test]
fn agy_discovery_falls_back_to_mtime_when_no_step_timestamp() {
    // An AGY transcript that omits created_at/timestamp on every step
    // must still surface a sortable timestamp (the file mtime), so the
    // row lands in the Archive list instead of being dropped silently.
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_mtime_{}",
        std::process::id()
    ));
    write_agy_conv(
        &root,
        "conv-no-ts",
        r#"{"step_type":"USER_INPUT","text":"No timestamps here","workspace_path":"/Users/adam/src/buildmesh"}
"#,
    );

    let discovered = discover_agy_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        None,
    );
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(discovered.len(), 1);
    assert!(
        discovered[0].timestamp.is_some(),
        "must fall back to file mtime when no step carries a timestamp"
    );
}

#[test]
fn agy_discovery_associates_custom_effective_dir_worktrees() {
    // Issue #1519: a workspace under the mesh's configured container dir
    // (relative custom dir, and absolute outside the root) must map to
    // its worktree name when the caller supplies the effective dir.
    let root = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_custom_{}",
        std::process::id()
    ));
    write_agy_conv(
        &root,
        "conv-custom-rel",
        r#"{"step_type":"USER_INPUT","text":"Custom rel","workspace_path":"/Users/adam/src/buildmesh/custom-wt/olive-fox","created_at":"2026-07-12T11:30:00Z"}
"#,
    );
    write_agy_conv(
        &root,
        "conv-custom-abs",
        r#"{"step_type":"USER_INPUT","text":"Custom abs","workspace_path":"/tmp/wt/copper-bear","created_at":"2026-07-12T11:30:00Z"}
"#,
    );

    let discovered = discover_agy_sessions_in(
        &root,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        Some("/Users/adam/src/buildmesh/custom-wt"),
    );
    std::fs::remove_dir_all(&root).ok();

    let rel = discovered
        .iter()
        .find(|s| s.session_id == "conv-custom-rel")
        .expect("custom-rel conversation present");
    assert_eq!(rel.worktree_name.as_deref(), Some("olive-fox"));

    // Absolute containers outside the root need their own effective dir.
    let root2 = std::env::temp_dir().join(format!(
        "buildmesh_agy_discovery_custom2_{}",
        std::process::id()
    ));
    write_agy_conv(
        &root2,
        "conv-custom-abs",
        r#"{"step_type":"USER_INPUT","text":"Custom abs","workspace_path":"/tmp/wt/copper-bear","created_at":"2026-07-12T11:30:00Z"}
"#,
    );
    let discovered2 = discover_agy_sessions_in(
        &root2,
        "/Users/adam/src/buildmesh",
        &std::collections::HashSet::new(),
        Some("/tmp/wt"),
    );
    std::fs::remove_dir_all(&root2).ok();
    let abs_row = discovered2
        .iter()
        .find(|s| s.session_id == "conv-custom-abs")
        .expect("custom-abs conversation present");
    assert_eq!(abs_row.worktree_name.as_deref(), Some("copper-bear"));
}

// --- Durable archived rows in the Archive list (issue #1065) ----------

/// A mesh plus one archived agent node, in the private database the
/// calling test installed. `cli_session_id` is left unset when `None` —
/// the shape of an archived Terminal node, which never gets one.
fn seed_archived_node(
    name: &str,
    cli_session_id: Option<&str>,
) -> (crate::models::Mesh, crate::models::AgentNode) {
    let mesh = db::create_mesh("demo", "/repos/demo").unwrap();
    let node = db::create_agent_node(
        mesh.id,
        name,
        "/repos/demo/.claude/worktrees/brave-fox",
        "feat/1065",
        EnvType::Windows,
        "terminal",
        Some("brave-fox"),
        None,
        None,
        None,
        true,
        None,
        None,
        None,
    )
    .unwrap();
    if let Some(session_id) = cli_session_id {
        db::update_cli_session_id(node.id, session_id).unwrap();
    }
    db::archive_agent_node(node.id).unwrap();
    let node = db::get_agent_node_by_id(node.id).unwrap();
    (mesh, node)
}

#[test]
fn archived_node_without_cli_session_id_is_still_listed() {
    // Issue #1065: an archived Terminal node has no CLI session id, so the
    // pre-#1065 disk-only archive list dropped it entirely even though its
    // row, worktree and branch all still exist.
    let _db = db::test_support::isolated();
    let (mesh, node) = seed_archived_node("brave-fox", None);
    assert!(node.cli_session_id.is_none());

    let listed = durable_archived_entries(&mut [], &db::list_agent_nodes_by_mesh(mesh.id).unwrap());

    assert_eq!(listed.len(), 1, "the durable row must still be listed");
    let entry = &listed[0];
    assert_eq!(
        entry.session_id,
        format!("node-{}", node.id),
        "a row with no session id is keyed by its node id"
    );
    assert_eq!(entry.first_message, "brave-fox");
    assert_eq!(entry.branch.as_deref(), Some("feat/1065"));
    assert_eq!(
        entry.cwd.as_deref(),
        Some("/repos/demo/.claude/worktrees/brave-fox")
    );
    assert_eq!(entry.worktree_name.as_deref(), Some("brave-fox"));
    assert!(
        entry.timestamp.is_some(),
        "a durable row must still sort by time"
    );
    assert!(
        !entry.resumable,
        "no transcript exists, so Resume must be unavailable"
    );
}

#[test]
fn archived_node_whose_transcript_was_removed_is_still_listed() {
    // Same bug, other cause: the id survives on the row but the session
    // directory on disk is gone, so the disk scan finds nothing to show.
    let _db = db::test_support::isolated();
    let (mesh, node) = seed_archived_node(
        "lost-transcript",
        Some("c0ffee00-1111-2222-3333-444455556666"),
    );
    assert_eq!(
        node.cli_session_id.as_deref(),
        Some("c0ffee00-1111-2222-3333-444455556666"),
        "the row keeps the id whose transcript is gone"
    );

    let listed = durable_archived_entries(&mut [], &db::list_agent_nodes_by_mesh(mesh.id).unwrap());

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, "c0ffee00-1111-2222-3333-444455556666");
    assert_eq!(listed[0].first_message, "lost-transcript");
    assert!(
        !listed[0].resumable,
        "a deleted transcript cannot be resumed"
    );
}

#[test]
fn archived_node_with_a_readable_transcript_keeps_the_resumable_entry() {
    // The behaviour that must not regress: when the transcript IS on disk
    // the session is discovered like any other, so it keeps the resumable
    // entry and must not be duplicated by a durable row.
    let _db = db::test_support::isolated();
    let (mesh, node) = seed_archived_node(
        "still-resumable",
        Some("a1b2c3d4-1111-2222-3333-444455556666"),
    );
    let mut discovered = vec![ArchivedAgentNode {
        session_id: "a1b2c3d4-1111-2222-3333-444455556666".to_string(),
        first_message: "Fix the archive list".to_string(),
        branch: Some("feat/1065".to_string()),
        cwd: Some("/repos/demo".to_string()),
        timestamp: Some("2026-10-01T09:00:00Z".to_string()),
        worktree_name: Some("brave-fox".to_string()),
        resumable: true,
        provider: Some(Provider::Anthropic.to_string()),
        node_id: None,
    }];

    let durable = durable_archived_entries(
        &mut discovered,
        &db::list_agent_nodes_by_mesh(mesh.id).unwrap(),
    );

    assert!(
        durable.is_empty(),
        "an archived row whose transcript was found must not add a second entry"
    );
    assert!(
        discovered[0].resumable,
        "the discovered entry stays resumable"
    );
    assert_eq!(node.status, crate::models::SessionStatus::Archived);
}

// --- Harness provenance (issue #2332) ---------------------------------
//
// The archive entry must say which harness owns the conversation, from
// the scanner that read it and from the durable row that recorded it.

/// Each scanner knows its own harness statically, so the provenance is
/// known at the moment the entry is built — never guessed from the session
/// id or the path shape later. This is the discovery-side half of "a
/// Cursor/AGY/Command Code entry defaults to its own executor", and it runs
/// the real scanners against fixture directories.
#[test]
fn every_scanner_stamps_its_own_harness_on_its_entries() {
    let mesh_path = "/Users/adam/src/buildmesh";
    let stamp = format!("buildmesh_provenance_{}", std::process::id());
    let root = std::env::temp_dir().join(&stamp);

    // Claude Code: `<projects>/<encoded mesh path>/<session>.jsonl`.
    let claude_dir = root.join("claude_projects").join(encode_path(mesh_path));
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("11111111-1111-1111-1111-111111111111.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":"Claude work"},"gitBranch":"feat/2332"}
"#,
    )
    .unwrap();
    let claude = discover_claude_sessions_in(
        &root.join("claude_projects"),
        mesh_path,
        mesh_path,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // Cursor: `<projects>/<workspace slug>/agent-transcripts/<id>/<id>.jsonl`.
    let cursor_id = "22222222-2222-2222-2222-222222222222";
    let cursor_transcripts = root
        .join("cursor_projects")
        .join(cursor_workspace_slug(mesh_path))
        .join("agent-transcripts")
        .join(cursor_id);
    std::fs::create_dir_all(&cursor_transcripts).unwrap();
    std::fs::write(
        cursor_transcripts.join(format!("{cursor_id}.jsonl")),
        r#"{"type":"user","message":{"role":"user","content":"Cursor work"}}
"#,
    )
    .unwrap();
    let cursor = discover_cursor_sessions_in(
        &root.join("cursor_projects"),
        mesh_path,
        &std::collections::HashSet::new(),
        None,
    );

    // Command Code: `<projects>/<slug>/<id>.jsonl`, one file per session.
    let commandcode_id = "33333333-3333-3333-3333-333333333333";
    let commandcode_dir = root
        .join("commandcode_projects")
        .join(commandcode_project_slug(mesh_path));
    std::fs::create_dir_all(&commandcode_dir).unwrap();
    std::fs::write(
            commandcode_dir.join(format!("{commandcode_id}.jsonl")),
            format!(
                r#"{{"type":"session","id":"{commandcode_id}","cwd":"{mesh_path}","timestamp":"2026-07-10T09:00:00Z"}}
{{"type":"message","id":"user-1","message":{{"role":"user","content":"Command Code work"}}}}
"#
            ),
        )
        .unwrap();
    let commandcode = discover_commandcode_sessions_in(
        Some(
            &root
                .join("commandcode_projects")
                .join(commandcode_project_slug(mesh_path)),
        ),
        mesh_path,
        &std::collections::HashSet::new(),
        None,
    );

    // Antigravity: `<brain>/<conversation>/.system_generated/logs/transcript.jsonl`.
    write_agy_conv(
        &root.join("agy_brain"),
        "44444444-4444-4444-4444-444444444444",
        r#"{"step_type":"USER_INPUT","text":"AGY work","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-07-10T09:00:00Z"}
"#,
    );
    let agy = discover_agy_sessions_in(
        &root.join("agy_brain"),
        mesh_path,
        &std::collections::HashSet::new(),
        None,
    );

    std::fs::remove_dir_all(&root).ok();

    for (scanner, entries, session_id) in [
        ("claude", &claude, "11111111-1111-1111-1111-111111111111"),
        ("cursor", &cursor, "22222222-2222-2222-2222-222222222222"),
        (
            "commandcode",
            &commandcode,
            "33333333-3333-3333-3333-333333333333",
        ),
        ("agy", &agy, "44444444-4444-4444-4444-444444444444"),
    ] {
        let entry = entries
            .iter()
            .find(|entry| entry.session_id == session_id)
            .unwrap_or_else(|| panic!("{scanner} fixture produced no entry for {session_id}"));
        // The expected id is the enum's own rendering, so an adapter-id
        // rename is caught here rather than at resume time.
        let expected = match scanner {
            "claude" => Provider::Anthropic.to_string(),
            "cursor" => Provider::Cursor.to_string(),
            "commandcode" => Provider::CommandCode.to_string(),
            "agy" => Provider::Agy.to_string(),
            other => unreachable!("no provenance case for {other}"),
        };
        assert_eq!(
            entry.provider.as_deref(),
            Some(expected.as_str()),
            "the {scanner} scanner must stamp its own harness id"
        );
        assert!(
            entry.resumable,
            "a transcript-backed {scanner} entry is resumable"
        );
        assert_eq!(entry.node_id, None, "{scanner} has no durable row yet");
    }
}

/// A durable archived row carries its own recorded executor, including one
/// that cannot resume — that is honest about *why* the entry has no
/// transcript to offer.
#[test]
fn durable_entries_carry_the_rows_own_provider_and_node_id() {
    let _db = db::test_support::isolated();
    let (mesh, node) = seed_archived_node(
        "resumable-harness-no-transcript",
        Some("beef0000-1111-2222-3333-444455556666"),
    );

    let listed = durable_archived_entries(&mut [], &db::list_agent_nodes_by_mesh(mesh.id).unwrap());

    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].provider.as_deref(),
        Some("terminal"),
        "the row's recorded executor must survive into the archive entry"
    );
    assert_eq!(
            listed[0].node_id,
            Some(node.id),
            "the durable row reference must survive so a caller can tell reopening known work from importing external work"
        );
}

/// The merge case from the issue: a readable disk transcript used to
/// suppress its archived row and with it every trace of the row's
/// identity. The entry keeps its scanner provenance and gains the durable
/// reference.
#[test]
fn a_merged_disk_entry_keeps_its_harness_and_gains_the_node_reference() {
    let _db = db::test_support::isolated();
    let (mesh, node) = seed_archived_node("reopened", Some("beef0001-1111-2222-3333-444455556666"));
    let mut discovered = vec![ArchivedAgentNode {
        session_id: "beef0001-1111-2222-3333-444455556666".to_string(),
        first_message: "Reopened work".to_string(),
        branch: Some("feat/1065".to_string()),
        cwd: Some("/repos/demo".to_string()),
        timestamp: Some("2026-10-02T09:00:00Z".to_string()),
        worktree_name: Some("brave-fox".to_string()),
        resumable: true,
        // A Cursor scanner found this transcript, so Cursor owns it even
        // though the archived row was created through Claude Code.
        provider: Some(Provider::Cursor.to_string()),
        node_id: None,
    }];

    let durable = durable_archived_entries(
        &mut discovered,
        &db::list_agent_nodes_by_mesh(mesh.id).unwrap(),
    );

    assert!(durable.is_empty(), "the disk entry is the resumable one");
    assert_eq!(
        discovered[0].provider.as_deref(),
        Some("cursor"),
        "the scanner that read the transcript owns its provenance"
    );
    assert_eq!(
        discovered[0].node_id,
        Some(node.id),
        "the archived row's reference must not be lost in the merge"
    );
}

/// A row with no recorded provider is genuinely unknown, and says so with
/// `None` rather than borrowing Claude Code's id.
#[test]
fn a_durable_row_without_a_provider_reports_unknown_provenance() {
    let _db = db::test_support::isolated();
    let mesh = db::create_mesh("demo", "/repos/demo").unwrap();
    let node = db::create_agent_node(
        mesh.id,
        "no-provider",
        "/repos/demo",
        "main",
        EnvType::Windows,
        "",
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .unwrap();
    db::archive_agent_node(node.id).unwrap();

    let listed = durable_archived_entries(&mut [], &db::list_agent_nodes_by_mesh(mesh.id).unwrap());

    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].provider, None,
        "an empty provider column is unknown, not Anthropic"
    );
    assert_eq!(listed[0].node_id, Some(node.id));
}

#[test]
fn live_nodes_never_reach_the_durable_archive_list() {
    // The durable list is the archive's alone; an active/idle node stays
    // excluded from the Archive view exactly as before.
    let _db = db::test_support::isolated();
    let mesh = db::create_mesh("demo", "/repos/demo").unwrap();
    db::create_agent_node(
        mesh.id,
        "live",
        "/repos/demo",
        "main",
        EnvType::Windows,
        "terminal",
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .unwrap();

    let listed = durable_archived_entries(&mut [], &db::list_agent_nodes_by_mesh(mesh.id).unwrap());

    assert!(listed.is_empty());
}

// --- Unavailable ownership must not read as "nothing to import" (#2330) ---

/// Issue #2330, headline acceptance check: a failed ownership read returns
/// no importable result at all.
///
/// The pre-fix code was `db::list_agent_nodes_by_mesh(mesh_id)
/// .unwrap_or_default()`, so a failed read produced an empty tracked-node
/// set. Two real consequences followed, and both are silent:
///
/// 1. every session already owned by a live node became *eligible* for
///    import, so the user could import a duplicate of work that is running;
/// 2. every durable archived row vanished from the Archive list.
///
/// Disk scanning cannot substitute for ownership persistence, so the read
/// must be authoritative. This drives the real `discover` against a private
/// database whose `agent_nodes` table has been dropped.
#[test]
fn an_unreadable_ownership_row_returns_no_importable_result() {
    let _db = db::test_support::isolated();
    let mesh = db::create_mesh("demo", "/repos/demo").unwrap();
    // A real row exists, so a pass cannot be explained by an empty mesh.
    seed_archived_node("brave-fox", Some("11111111-1111-1111-1111-111111111111"));
    // Now break the query `discover` depends on.
    db::write_conn()
        .execute_batch("DROP TABLE agent_nodes;")
        .unwrap();

    let outcome = super::discover(mesh.id, "/repos/demo");

    match outcome {
        Err(failure) => {
            // The fault, not "there is nothing here".
            assert!(
                matches!(failure, crate::read_outcome::Unavailable::Read(_)),
                "a failed ownership read must be unavailability, got: {failure:?}"
            );
        }
        Ok(nodes) => panic!(
            "an unreadable ownership row must not return importable entries; got {}",
            nodes.len()
        ),
    }
}

/// The companion: a *successful* read of an empty mesh is still a success,
/// and an empty Archive list is a legitimate answer. This is what makes
/// the failing case above a real distinction rather than "discovery always
/// errors".
#[test]
fn a_readable_mesh_with_no_rows_still_returns_an_empty_list() {
    let _db = db::test_support::isolated();
    let mesh = db::create_mesh("empty", "/repos/empty").unwrap();

    let outcome = super::discover(mesh.id, "/repos/empty");

    assert!(
        outcome.is_ok(),
        "an empty mesh is a successful read, not a fault: {:?}",
        outcome.err()
    );
}
