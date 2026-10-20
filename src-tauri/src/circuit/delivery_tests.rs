//! Tests for `circuit::delivery`.
//!
//! Split out of `src-tauri/src/circuit/delivery.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test circuit::delivery::tests

use super::*;
#[test]
fn injection_payload_never_carries_the_enter_keystroke() {
    assert!(!injection_payload("line one\nline two").contains('\r'));
    assert!(!injection_payload("single line prompt").contains('\r'));
}

#[test]
fn multiline_payload_is_bracketed_paste_wrapped() {
    let p = injection_payload("a\nb");
    assert!(p.starts_with("\x1b[200~"));
    assert!(p.ends_with("\x1b[201~"));
    assert!(p.contains("a\nb"));
}

#[test]
fn single_line_payload_is_written_verbatim() {
    assert_eq!(injection_payload("do the thing"), "do the thing");
}

#[test]
fn cr_separated_payload_is_still_bracketed() {
    // A CR-only body is multi-line too: unbracketed, its first CR would
    // submit the prompt.
    assert_eq!(injection_payload("a\rb"), "\x1b[200~a\rb\x1b[201~");
}

#[test]
fn grok_paste_text_uses_cr_for_lf_crlf_and_mixed_endings() {
    use crate::agent::provider::adapters::GROK;
    // Windows xterm selections arrive as CRLF; programmatic prompts as LF.
    assert_eq!(
        paste_text_for(&GROK, "one\r\ntwo\r\nthree"),
        "one\rtwo\rthree"
    );
    assert_eq!(paste_text_for(&GROK, "one\ntwo\nthree"), "one\rtwo\rthree");
    assert_eq!(
        paste_text_for(&GROK, "one\r\ntwo\nthree\rfour"),
        "one\rtwo\rthree\rfour"
    );
    assert_eq!(paste_text_for(&GROK, "no newline"), "no newline");
}

#[test]
fn grok_handover_payload_is_one_bracketed_paste_with_no_lf() {
    use crate::agent::provider::adapters::GROK;
    let payload = injection_payload(&paste_text_for(&GROK, "one\r\ntwo\r\nthree"));
    assert_eq!(payload, "\x1b[200~one\rtwo\rthree\x1b[201~");
    assert!(!payload.contains('\n'));
}

#[test]
fn harnesses_that_did_not_opt_in_keep_their_line_endings() {
    use crate::agent::provider::adapters::ANTHROPIC;
    for text in ["a\r\nb", "a\nb"] {
        assert_eq!(paste_text_for(&ANTHROPIC, text), text);
    }
}

/// #2199: Codex needs CR inside its bracketed paste for the same ConPTY
/// reason Grok does, even though its own paste count is newline-normalized.
#[test]
fn codex_paste_text_uses_cr_like_grok() {
    use crate::agent::provider::adapters::{CODEX, GROK};
    assert_eq!(
        paste_text_for(&CODEX, "one\r\ntwo\r\nthree"),
        "one\rtwo\rthree"
    );
    assert_eq!(paste_text_for(&CODEX, "one\ntwo\nthree"), "one\rtwo\rthree");
    assert_eq!(
        paste_text_for(&CODEX, "one\r\ntwo\nthree\rfour"),
        "one\rtwo\rthree\rfour"
    );
    assert_eq!(paste_text_for(&CODEX, "no newline"), "no newline");
    // One policy, one behavior: the two CR-requiring harnesses agree.
    for text in ["a\r\nb", "a\nb", "a\rb", "a\r\nb\nc\rd"] {
        assert_eq!(
            paste_text_for(&CODEX, text),
            paste_text_for(&GROK, text),
            "{text:?}"
        );
    }
}

#[test]
fn grok_staged_prompt_write_normalizes_newlines_and_brackets_payload() {
    let _db = crate::db::test_support::isolated();
    let path = std::env::temp_dir().join(format!("grok-stage-test-{}", std::process::id()));
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh("grok stage mesh", &path).unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "worker",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "grok",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let id = node.id;
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);

    let prompt = "one\ntwo\r\nthree";
    let prepared = PreparedPrompt::prepare(id, prompt).unwrap();
    let expected = registry.input_stamp(id).unwrap();
    let (guard, _) = stage_prompt_write(&registry, &prepared, Some(&expected))
        .unwrap()
        .unwrap();
    assert!(guard.is_some());

    let written = writes.recv_timeout(Duration::from_secs(1)).unwrap();
    let expected_payload = b"\x1b[200~one\rtwo\rthree\x1b[201~".to_vec();
    assert_eq!(written, expected_payload);
    assert!(!written.contains(&b'\n'));

    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn piloted_node_without_db_row_fails_preparation_loudly() {
    let _db = crate::db::test_support::isolated();
    let id = 999_999;
    let (registry, _) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);

    let err = PreparedPrompt::prepare(id, "prompt").unwrap_err();
    assert!(
        err.contains("could not identify prompt target 999999"),
        "expected target identification error, got: {err}"
    );

    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn grok_staged_prompt_write_unguarded_normalizes_newlines() {
    let _db = crate::db::test_support::isolated();
    let path =
        std::env::temp_dir().join(format!("grok-stage-unguarded-test-{}", std::process::id()));
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh("grok stage unguarded mesh", &path).unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "worker",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "grok",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let id = node.id;
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);

    let prompt = "line1\nline2\r\nline3";
    let prepared = PreparedPrompt::prepare(id, prompt).unwrap();
    let (guard, _) = stage_prompt_write(&registry, &prepared, None)
        .unwrap()
        .unwrap();
    assert!(guard.is_none());

    let written = writes.recv_timeout(Duration::from_secs(1)).unwrap();
    let expected_payload = b"\x1b[200~line1\rline2\rline3\x1b[201~".to_vec();
    assert_eq!(written, expected_payload);
    assert!(!written.contains(&b'\n'));

    registry.kill_session(id);
}

#[test]
fn grok_opted_in_adapter_recorded_digest_equals_submission_digest_of_written_text() {
    let _db = crate::db::test_support::isolated();
    let path = std::env::temp_dir().join(format!("grok-digest-test-{}", std::process::id()));
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh("grok digest mesh", &path).unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "worker",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "grok",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let id = node.id;
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);

    let raw_prompt = "one\ntwo\r\nthree";
    let prepared = PreparedPrompt::prepare(id, raw_prompt).unwrap();

    let circuit = crate::db::create_autopilot_circuit(
        mesh.id,
        "test-circuit",
        "",
        &crate::circuit::model::CircuitGraph::walking_skeleton("task")
            .to_json()
            .unwrap(),
    )
    .unwrap();
    let run_id =
        crate::db::create_circuit_run(circuit.id, mesh.id, "manual:digest-test", "{}").unwrap();
    crate::db::circuit::evidence::record_prompt_submission(run_id, "step-1", 1, &prepared).unwrap();

    let expected = registry.input_stamp(id).unwrap();
    let (guard, _) = stage_prompt_write(&registry, &prepared, Some(&expected))
        .unwrap()
        .unwrap();
    assert!(guard.is_some());

    let written = writes.recv_timeout(Duration::from_secs(1)).unwrap();
    let written_str = String::from_utf8(written).unwrap();
    let inner_text = written_str
        .strip_prefix("\x1b[200~")
        .and_then(|s| s.strip_suffix("\x1b[201~"))
        .unwrap();

    let conn = crate::db::read_conn().unwrap();
    let detail: String = conn
        .query_row(
            "SELECT detail FROM circuit_run_history WHERE kind='prompt_submitted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let submission: crate::db::circuit::evidence::PromptSubmission =
        serde_json::from_str(&detail).unwrap();

    assert_eq!(
        submission.prompt_digest,
        crate::circuit::strategy::submission_digest(inner_text)
    );

    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn grok_prompt_preparation_converts_newlines_on_continuation_and_nudge_shaped_text() {
    let _db = crate::db::test_support::isolated();
    let path = std::env::temp_dir().join(format!("grok-cont-test-{}", std::process::id()));
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh("grok cont mesh", &path).unwrap();

    // Continuation prompt generated through prepare_continuation
    let cont_node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "worker-cont",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "grok",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let cont_id = cont_node.id;
    let (registry_cont, writes_cont) = crate::agent::process::testing::capturing_registry(cont_id);
    evaluator::register(cont_id);

    let run_id = 42;
    crate::circuit::handoff::set_agent_turn(
        run_id,
        cont_id,
        Some(&crate::circuit::handoff::HandoffTurn {
            node_id: "step-1".to_string(),
            attempt: 1,
        }),
    );

    let raw_continuation = "first line\nsecond line\r\nthird line";
    let cont_prompt = crate::circuit::handoff::prepare_continuation(
        run_id,
        cont_id,
        cont_node.env,
        raw_continuation,
    );
    let prepared_cont = PreparedPrompt::prepare(cont_id, &cont_prompt).unwrap();
    assert!(prepared_cont
        .text()
        .starts_with("first line\rsecond line\rthird line\r\r"));
    assert!(prepared_cont
        .text()
        .contains("When, and only when, you have completely finished this task"));
    assert!(!prepared_cont.text().contains('\n'));

    let expected_cont = registry_cont.input_stamp(cont_id).unwrap();
    let (guard, _) = stage_prompt_write(&registry_cont, &prepared_cont, Some(&expected_cont))
        .unwrap()
        .unwrap();
    assert!(guard.is_some());
    let written_cont = writes_cont.recv_timeout(Duration::from_secs(1)).unwrap();
    let expected_payload = injection_payload(prepared_cont.text()).into_bytes();
    assert_eq!(written_cont, expected_payload);

    crate::circuit::handoff::set_agent_turn(run_id, cont_id, None);
    evaluator::unregister(cont_id);
    registry_cont.kill_session(cont_id);

    // Wake-up nudge prompt
    let nudge_node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "worker-nudge",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "grok",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let nudge_id = nudge_node.id;
    let (registry_nudge, writes_nudge) =
        crate::agent::process::testing::capturing_registry(nudge_id);
    evaluator::register(nudge_id);

    let nudge = "wake up line 1\nwake up line 2";
    let prepared_nudge = PreparedPrompt::prepare(nudge_id, nudge).unwrap();
    assert_eq!(prepared_nudge.text(), "wake up line 1\rwake up line 2");
    assert!(!prepared_nudge.text().contains('\n'));

    let expected_nudge = registry_nudge.input_stamp(nudge_id).unwrap();
    let (guard, _) = stage_prompt_write(&registry_nudge, &prepared_nudge, Some(&expected_nudge))
        .unwrap()
        .unwrap();
    assert!(guard.is_some());
    let written_nudge = writes_nudge.recv_timeout(Duration::from_secs(1)).unwrap();
    let expected_nudge_payload = injection_payload(prepared_nudge.text()).into_bytes();
    assert_eq!(written_nudge, expected_nudge_payload);

    evaluator::unregister(nudge_id);
    registry_nudge.kill_session(nudge_id);
}

#[test]
fn codex_cr_only_prompt_is_bracketed_and_enters_rendered_paste_gate() {
    let _db = crate::db::test_support::isolated();
    let path = std::env::temp_dir().join(format!("codex-cr-test-{}", std::process::id()));
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh("codex cr mesh", &path).unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "source",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "codex",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let id = node.id;
    evaluator::register(id);
    let prompt = "step 1\rstep 2\rstep 3";
    let prepared = PreparedPrompt::prepare(id, prompt).unwrap();
    assert_eq!(prepared.text(), prompt);
    let payload = injection_payload(prepared.text());
    assert_eq!(payload, format!("\x1b[200~{prompt}\x1b[201~"));

    let readiness = paste_readiness(id, prepared.text()).unwrap();
    assert!(
        matches!(readiness.paste, PasteReadiness::RenderedMultiline { .. }),
        "a CR-separated multiline prompt to Codex must enter the rendered paste gate"
    );
    evaluator::unregister(id);
}

/// #2199: a programmatic multiline prompt to a circuit-piloted Codex node
/// reached ConPTY as bare LF inside its bracketed paste, and Windows ConPTY
/// drops LF — the composer glued every line together. ConPTY evidence for
/// LF / CRLF / CR is in `docs/learning/grok-terminal-paste.md`.
///
/// This runs the production path (`PreparedPrompt::prepare`, then
/// `stage_prompt_write`) and asserts the literal staged bytes. It does not
/// re-derive the defect: the glued composer outcome is a runtime CLI
/// property the probe establishes, while this pins the byte stream we send.
#[test]
fn codex_circuit_multiline_delivery_stages_cr_and_never_bare_lf() {
    let _db = crate::db::test_support::isolated();
    for (label, prompt, staged) in [
        ("lf", "line 1\nline 2", "\x1b[200~line 1\rline 2\x1b[201~"),
        (
            "crlf",
            "line 1\r\nline 2",
            "\x1b[200~line 1\rline 2\x1b[201~",
        ),
        ("cr", "line 1\rline 2", "\x1b[200~line 1\rline 2\x1b[201~"),
    ] {
        let id = codex_worker_node(&format!("codex-lf-repro-{label}"));
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);

        let prepared = PreparedPrompt::prepare(id, prompt).unwrap();
        let expected = registry.input_stamp(id).unwrap();
        let _ = stage_prompt_write(&registry, &prepared, Some(&expected)).unwrap();

        let written = writes.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(
            String::from_utf8(written.clone()).unwrap(),
            staged,
            "{label}: Codex must stage CR line endings inside the bracketed paste"
        );
        let has_bare_lf = written
            .iter()
            .enumerate()
            .any(|(i, &b)| b == b'\n' && (i == 0 || written[i - 1] != b'\r'));
        assert!(
            !has_bare_lf,
            "{label}: staged PTY payload for Codex must not contain bare LF, \
                 which ConPTY drops in a bracketed paste"
        );

        evaluator::unregister(id);
        registry.kill_session(id);
    }
}

/// #2199 follow-up: the split-marker proof must not move when the staged
/// payload carries CR. Codex normalizes CRLF and bare CR to LF *before*
/// counting its `[Pasted Content N chars]` marker, so one prompt delivered
/// with any line ending produces the same count and the same match.
///
/// Each variant is built through the adapter's real paste policy, because
/// `RenderedPastePrompt::new` receives text `prepare` has already resolved.
#[test]
fn codex_split_marker_count_is_identical_for_cr_crlf_and_lf_payloads() {
    use crate::agent::provider::adapters::CODEX;
    let body = format!("Review this change\n{}", "x".repeat(1000));
    let mut counts = Vec::new();
    for (label, text) in [
        ("lf", body.clone()),
        ("crlf", body.replace('\n', "\r\n")),
        ("cr", body.replace('\n', "\r")),
    ] {
        let prepared = paste_text_for(&CODEX, &text);
        assert!(
            !prepared.contains('\n'),
            "{label}: prepare must leave no LF for ConPTY to drop"
        );
        let prompt = RenderedPastePrompt::new(&prepared);
        assert_eq!(
            prompt.payload, prepared,
            "{label}: payload keeps the prepared body"
        );
        counts.push(prompt.text.offsets.len() - 1);
        assert!(
            prompt.visible(
                "› Review this change [Pasted Content 1000 chars]",
                Instant::now() + RENDERED_PASTE_TOTAL_BUDGET
            ),
            "{label}: the same marker must confirm every line ending"
        );
    }
    assert_eq!(counts, [1019, 1019, 1019]);
}

#[test]
fn muse_cr_only_prompt_is_bracketed_and_enters_rendered_paste_gate() {
    let _db = crate::db::test_support::isolated();
    let id = muse_publisher_node("muse-cr-paste");
    evaluator::register(id);
    let prompt = "step 1\rstep 2\rstep 3";
    let prepared = PreparedPrompt::prepare(id, prompt).unwrap();
    assert_eq!(prepared.text(), prompt);
    let payload = injection_payload(prepared.text());
    assert_eq!(payload, format!("\x1b[200~{prompt}\x1b[201~"));

    let readiness = paste_readiness(id, prepared.text()).unwrap();
    assert!(
        matches!(readiness.paste, PasteReadiness::RenderedMultiline { .. }),
        "a CR-separated multiline prompt to Muse must enter the rendered paste gate"
    );
    evaluator::unregister(id);
}

#[test]
fn paste_text_for_is_idempotent() {
    use crate::agent::provider::adapters::GROK;
    let original = "line1\nline2\r\nline3\rline4";
    let once = paste_text_for(&GROK, original);
    let twice = paste_text_for(&GROK, &once);
    let thrice = paste_text_for(&GROK, &twice);
    assert_eq!(once, "line1\rline2\rline3\rline4");
    assert_eq!(once, twice);
    assert_eq!(twice, thrice);
}

#[test]
fn codex_multiline_waits_for_paste_render_before_enter() {
    let _db = crate::db::test_support::isolated();
    let path = std::env::temp_dir().join("codex-guarded-paste");
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh("guarded Codex paste", &path).unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "source",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "codex",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let id = node.id;
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    let prompt = format!("feedback\n{}", "x".repeat(9398));
    evaluator::on_output(id, "old [Pasted Content 9407 chars]");
    let expected = registry.input_stamp(id).unwrap();
    let (guard, readiness) = stage_prompt_write(
        &registry,
        &PreparedPrompt::from_raw(id, &prompt),
        Some(&expected),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        injection_payload(&prompt).into_bytes()
    );
    assert!(
        registry.input_stamp(id).is_none(),
        "report admission must still reject the staged draft"
    );
    assert!(
        registry.input_is_current(id, guard.as_deref().unwrap()),
        "the post-write stamp must own its nonempty draft"
    );
    let registry_for_submit = Arc::clone(&registry);
    let submit = std::thread::spawn(move || {
        submit_staged_prompt_result(&registry_for_submit, id, guard, &readiness)
    });

    evaluator::on_output(id, "Codex startup redraw; review this test");
    assert_eq!(
        writes.recv_timeout(Duration::from_millis(1500)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout),
        "startup output must not acknowledge a paste that Codex has not rendered"
    );
    evaluator::on_output(
        id,
        "\x1b[38;5;6m[Pasted Content     \x1b[m  \x1b[38;5;6m9407 chars]\x1b[K\x1b[m",
    );
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(3)).unwrap(),
        b"\r".to_vec(),
    );
    evaluator::on_output(id, "task started");
    assert_eq!(submit.join().unwrap().unwrap(), Some(1));
    assert_eq!(
        writes.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "exactly one separate Enter is allowed"
    );
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn codex_paste_readiness_accepts_short_visible_text() {
    assert!(rendered_paste_visible(
        "› review the diff",
        15,
        15,
        "reviewthediff"
    ));
    assert!(!rendered_paste_visible(
        "› review the",
        15,
        15,
        "reviewthediff"
    ));
    assert!(!rendered_paste_visible(
        "Codex startup redraw",
        3279,
        3279,
        "reviewthistest"
    ));
    assert!(rendered_paste_visible(
        "[Pasted Content 14 chars]",
        15,
        14,
        ""
    ));
    assert!(!rendered_paste_visible(
        "[Pasted Content 13 chars]",
        15,
        14,
        ""
    ));
}

#[test]
fn codex_paste_readiness_matches_wrapped_terminal_marker() {
    let id = -930_027;
    evaluator::register(id);
    evaluator::on_output(id, "old [Pasted Content 9407 chars]");
    let cursor = evaluator::output_cursor(id).unwrap();
    assert!(!rendered_paste_visible(
        &evaluator::cleaned_output_since(id, cursor),
        9407,
        9407,
        ""
    ));
    for output in [
        "\x1b[36m[Pasted Content \x1b[0m\r\n  9407 chars]",
        // Captured from Codex 0.160.0 in a 22-column Windows ConPTY.
        "\x1b[38;5;6m[Pasted Content     \x1b[m  \x1b[38;5;6m9407 chars]\x1b[K\x1b[m",
    ] {
        let cursor = evaluator::output_cursor(id).unwrap();
        evaluator::on_output(id, output);
        assert!(
            rendered_paste_visible(&evaluator::cleaned_output_since(id, cursor), 9407, 9407, ""),
            "terminal line wrapping must not hide a complete matching paste marker"
        );
    }
    assert!(!rendered_paste_visible(
        "[Pasted Content\r\n 940 chars]",
        9407,
        9407,
        ""
    ));
    assert!(!rendered_paste_visible(
        "[Pasted Content 9407 chars",
        9407,
        9407,
        ""
    ));
    evaluator::unregister(id);
}

#[test]
fn codex_paste_readiness_rechecks_late_echo_without_repeating_input() {
    let id = -930_028;
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    let expected = registry.input_stamp(id).unwrap();
    let prompt = format!("feedback\n{}", "x".repeat(9398));
    let guard = registry
        .write_bytes_if_current(id, injection_payload(&prompt).as_bytes(), &expected)
        .unwrap()
        .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        injection_payload(&prompt).into_bytes()
    );
    let readiness = PasteReadiness::RenderedMultiline {
        chars: 9407,
        normalized_chars: 9407,
        content: String::new(),
        split_marker_prompt: None,
        output_cursor: evaluator::output_cursor(id).unwrap(),
    };
    let registry_for_wait = Arc::clone(&registry);
    let wait = std::thread::spawn(move || {
        settle_after_paste(
            &registry_for_wait,
            id,
            &readiness,
            Some(&guard),
            Duration::from_secs(5),
        )
    });
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(2)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout),
        "an idle draft without a complete echo must receive no input while readiness is rechecked"
    );
    evaluator::on_output(id, "[Pasted Content\r\n 9407 chars]");
    assert!(
        wait.join().unwrap().unwrap(),
        "a late complete echo within the total budget must be accepted"
    );
    assert_eq!(
        writes.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "readiness retries must not paste again or send Enter"
    );
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn codex_paste_readiness_stops_on_changed_input_or_exhaustion() {
    let id = -930_029;
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    let expected = registry.input_stamp(id).unwrap();
    let prompt = format!("feedback\n{}", "x".repeat(9398));
    let guard = registry
        .write_bytes_if_current(id, injection_payload(&prompt).as_bytes(), &expected)
        .unwrap()
        .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        injection_payload(&prompt).into_bytes()
    );
    let readiness = PasteReadiness::RenderedMultiline {
        chars: 9407,
        normalized_chars: 9407,
        content: String::new(),
        split_marker_prompt: None,
        output_cursor: evaluator::output_cursor(id).unwrap(),
    };
    let error = settle_after_paste(
        &registry,
        id,
        &readiness,
        Some(&guard),
        Duration::from_millis(1),
    )
    .unwrap_err();
    assert!(
        error.contains("after ") && error.contains("s (readiness budget 1ms)"),
        "{error}"
    );
    assert_eq!(
        writes.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "an idle agent without a complete echo must not receive blind input"
    );
    registry.write_bytes(id, b"human draft").unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        b"human draft"
    );
    assert!(!settle_after_paste(
        &registry,
        id,
        &readiness,
        Some(&guard),
        Duration::from_secs(1)
    )
    .unwrap());
    assert_eq!(writes.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn live_codex_node_selects_rendered_paste_gate() {
    let _db = crate::db::test_support::isolated();
    let path = std::env::temp_dir().join(format!("codex-paste-selection-{}", std::process::id()));
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh("codex paste selection", &path).unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "reviewer",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "codex",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let node_id = node.id;
    evaluator::register(node_id);

    assert!(matches!(
        paste_readiness(node_id, "review the change\nwith context")
            .unwrap()
            .paste,
        PasteReadiness::RenderedMultiline { .. }
    ));
    let proxied = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "proxied reviewer",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "codex:minimax",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let proxied_id = proxied.id;
    evaluator::register(proxied_id);
    assert!(matches!(
        paste_readiness(proxied_id, "review the change\nwith context")
            .unwrap()
            .paste,
        PasteReadiness::RenderedMultiline { .. }
    ));
    let (registry, writes) = crate::agent::process::testing::capturing_registry(proxied_id);
    let prompt = "review the change\r\nwith context";
    evaluator::on_output(proxied_id, "old [Pasted Content 30 chars]");
    // A proxied `codex:minimax` node resolves the same Codex adapter, so it
    // must stage CR endings too (#2199) — the gate still counts 30 chars.
    let prepared = PreparedPrompt::prepare(proxied_id, prompt).unwrap();
    assert_eq!(prepared.text(), "review the change\rwith context");
    let (_, readiness) = stage_prompt_write(&registry, &prepared, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        injection_payload("review the change\rwith context").into_bytes()
    );
    let PasteReadiness::RenderedMultiline {
        chars,
        normalized_chars,
        content,
        output_cursor,
        ..
    } = readiness.paste
    else {
        panic!("proxied Codex must use the rendered paste gate");
    };
    assert_eq!((chars, normalized_chars), (30, 30));
    assert!(!rendered_paste_visible(
        &evaluator::cleaned_output_since(proxied_id, output_cursor),
        chars,
        normalized_chars,
        &content
    ));
    evaluator::on_output(proxied_id, "[Pasted Content 30 chars]");
    assert!(rendered_paste_visible(
        &evaluator::cleaned_output_since(proxied_id, output_cursor),
        chars,
        normalized_chars,
        &content
    ));
    let long_prompt = format!("review this change\n{}", "x".repeat(7_000));
    let PasteReadiness::RenderedMultiline {
        content,
        chars,
        normalized_chars,
        ..
    } = paste_readiness(proxied_id, &long_prompt).unwrap().paste
    else {
        panic!("long Codex prompts must retain the paste gate");
    };
    assert_eq!(
        content,
        "x".repeat(TAIL_ANCHOR_CHARS),
        "a long Codex prompt is confirmed by its tail (issue #2061)"
    );
    assert!(!rendered_paste_visible(
        "reviewthischange",
        chars,
        normalized_chars,
        &content
    ));
    assert!(rendered_paste_visible(
        &format!("composerdrawn{content}"),
        chars,
        normalized_chars,
        &content
    ));
    assert!(rendered_paste_visible(
        &format!("[Pasted Content {chars} chars]"),
        chars,
        normalized_chars,
        &content
    ));
    registry.kill_session(proxied_id);
    evaluator::register(-930_099);
    assert!(
        paste_readiness(-930_099, "review the change\nwith context")
            .unwrap_err()
            .contains("could not identify prompt target"),
        "a failed provider lookup must not silently use the early-Enter path"
    );
    evaluator::unregister(-930_099);
    evaluator::unregister(node_id);
    evaluator::unregister(proxied_id);
}

#[test]
fn live_muse_node_waits_for_rendered_multiline_paste() {
    let _db = crate::db::test_support::isolated();
    let path = std::env::temp_dir().join("muse-paste-selection");
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh("muse paste selection", &path).unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "source",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "muse",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    let node_id = node.id;
    evaluator::register(node_id);
    assert!(matches!(
        paste_readiness(node_id, "Review feedback\nApply the changes")
            .unwrap()
            .paste,
        PasteReadiness::RenderedMultiline { .. }
    ));
    crate::db::update_cli_session_id(node_id, "nonexistent-muse-receipt-session").unwrap();
    assert!(
        paste_readiness(node_id, "Apply the review findings")
            .unwrap_err()
            .contains("session log is unavailable"),
        "single-line follow-ups also require the established session receipt"
    );
    evaluator::unregister(node_id);
    assert!(
        matches!(
            paste_readiness(node_id, "Manual\nfollow-up").unwrap().paste,
            PasteReadiness::Generic
        ),
        "ordinary unbuffered nodes retain their existing submission path"
    );
}

/// Stand-in for run 343's 831-character `publish` prompt (that run's own text
/// was not retained): a 600-character multiline draft in the same size band.
/// Muse Code 1.3.0 draws such a paste in full with no `[Pasted Content N
/// chars]` marker, and the gate used to demand the marker past the
/// visible-text limit, so the staged prompt sat unsent for the 90 s budget.
const MUSE_MIDSIZE_PROMPT: &str = concat!(
    "Publish the work from this session as a pull request.\n",
    "\n",
    "- Push the current branch to origin and open a PR against `main`.\n",
    "- Title it with a Conventional Commit summary of the change.\n",
    "- In the body, explain what changed and why, list the checks you ran, and\n",
    "  link the issue with a `Closes #1234` line if one exists.\n",
    "- Do not merge the PR; a reviewer will pick it up next.\n",
    "\n",
    "When the PR is open, reply with its URL and a one-paragraph summary of what\n",
    "you verified, so the reviewer knows what has already been covered. If any gate\n",
    "is red, say so plainly instead of opening the PR and explain the failure.",
);

/// The final composer redraw from a real Muse Code 1.3.0 capture of
/// `MUSE_MIDSIZE_PROMPT` in a 79x57 Windows ConPTY (box rules and status
/// line abbreviated): wrapped mid-sentence ("If any" / "gate"), indented,
/// positioned with cursor escapes, and carrying no paste marker.
const MUSE_MIDSIZE_COMPOSER_FRAME: &str = concat!(
        "\x1b[8;1H\x1b[J\x1b[8;1H\x1b[2m\x1b[38;2;103;108;116;49m\u{2500}\u{2500} \x1b[22m",
        "\x1b[38;2;138;144;152;49mVoice input (Alt+V to start)\x1b[2m\u{2500}\u{2500}\u{2500}\u{2500}",
        "\x1b[9;1H\x1b[22m\x1b[38;2;90;160;255;49m\u{276f} \x1b[38;2;204;211;219;49m",
        "Publish the work from this session as a pull request.",
        "\x1b[10;1H\x1b[38;2;103;108;116;49m  ",
        "\x1b[11;1H  \x1b[38;2;204;211;219;49m- Push the current branch to origin and open a PR against `main`.",
        "\x1b[12;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49m- Title it with a Conventional Commit summary of the change.",
        "\x1b[13;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49m- In the body, explain what changed and why, list the checks you ran, and",
        "\x1b[14;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49m  link the issue with a `Closes #1234` line if one exists.",
        "\x1b[15;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49m- Do not merge the PR; a reviewer will pick it up next.",
        "\x1b[16;1H\x1b[38;2;103;108;116;49m  ",
        "\x1b[17;1H  \x1b[38;2;204;211;219;49mWhen the PR is open, reply with its URL and a one-paragraph summary of what",
        "\x1b[18;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49myou verified, so the reviewer knows what has already been covered. If any ",
        "\x1b[19;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49mgate",
        "\x1b[20;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49mis red, say so plainly instead of opening the PR and explain the failure.",
        "\x1b[21;1H\x1b[2m\x1b[38;2;103;108;116;49m\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "\x1b[22;1H\x1b[22m  \x1b[38;2;90;160;255;49mmuse-spark-1.3-contributor\x1b[38;2;138;144;152;49m \u{b7} ",
        "\x1b[38;2;90;160;255;49mhigh\x1b[38;2;138;144;152;49m \u{b7} Auto-review\x1b[39m\x1b[49m\x1b[59m\x1b[0m",
    );

/// Create a muse node in this test's own database and return its id.
///
/// The caller installs the database with `db::test_support::isolated()` and
/// holds that guard for the rest of the test (issue #2048): a helper that
/// installed it and dropped the guard on return would leave the rest of the
/// test reading the process-global database.
fn muse_publisher_node(label: &str) -> i64 {
    let path = std::env::temp_dir().join(label);
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh(label, &path).unwrap();
    crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "publisher",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "muse",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap()
    .id
}

#[test]
fn only_tail_anchor_harnesses_confirm_a_long_draft_by_their_tail() {
    let long = "word".repeat(100);
    let tail = |policy| visible_paste_proof(policy, long.clone());
    assert_eq!(
        tail(PasteGatePolicy::RenderedWithTailAnchor),
        "word".repeat(TAIL_ANCHOR_CHARS / 4)
    );
    assert_eq!(
        tail(PasteGatePolicy::RenderedMarkerOnly),
        "",
        "a collapsed draft carries no visible tail: only its marker can confirm it"
    );
    assert_eq!(
        tail(PasteGatePolicy::Generic),
        "",
        "the generic path never takes a tail proof"
    );
    // Under the limit every policy still requires the complete text.
    assert_eq!(
        visible_paste_proof(
            PasteGatePolicy::RenderedWithTailAnchor,
            "short draft".into()
        ),
        "short draft"
    );
    assert_eq!(
        visible_paste_proof(PasteGatePolicy::RenderedMarkerOnly, "short draft".into()),
        "short draft"
    );
    assert_eq!(
        visible_paste_proof(PasteGatePolicy::Generic, "short draft".into()),
        "short draft"
    );
    // The cut is by character, so a multi-byte tail is never split mid-codepoint.
    let accented = "é".repeat(VISIBLE_PASTE_TEXT_LIMIT + 1);
    assert_eq!(
        visible_paste_proof(PasteGatePolicy::RenderedWithTailAnchor, accented),
        "é".repeat(TAIL_ANCHOR_CHARS)
    );
}

#[test]
fn the_full_text_limit_counts_characters_not_bytes() {
    // 200 characters but 400 bytes: under the limit, so every policy keeps the full text.
    let accented = "é".repeat(200);
    assert!(
        accented.len() > VISIBLE_PASTE_TEXT_LIMIT,
        "fixture precondition: over the limit in bytes only"
    );
    assert_eq!(
        visible_paste_proof(PasteGatePolicy::RenderedMarkerOnly, accented.clone()),
        accented
    );
    assert_eq!(
        visible_paste_proof(PasteGatePolicy::RenderedWithTailAnchor, accented.clone()),
        accented
    );
    // One character past the limit is the boundary for marker-only harnesses.
    let at_limit = "é".repeat(VISIBLE_PASTE_TEXT_LIMIT);
    assert_eq!(
        visible_paste_proof(PasteGatePolicy::RenderedMarkerOnly, at_limit.clone()),
        at_limit
    );
    assert_eq!(
        visible_paste_proof(
            PasteGatePolicy::RenderedMarkerOnly,
            "é".repeat(VISIBLE_PASTE_TEXT_LIMIT + 1)
        ),
        ""
    );
}

#[test]
fn muse_midsize_paste_rendered_in_full_is_confirmed_by_its_tail() {
    let _db = crate::db::test_support::isolated();
    let id = muse_publisher_node("muse-midsize-paste");
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    assert!(
        crate::circuit::launch::normalize_for_match(MUSE_MIDSIZE_PROMPT)
            .chars()
            .count()
            > VISIBLE_PASTE_TEXT_LIMIT,
        "fixture precondition: past the full-text limit, where only a marker used to be accepted"
    );
    let (_, readiness) = stage_prompt_write(
        &registry,
        &PreparedPrompt::from_raw(id, MUSE_MIDSIZE_PROMPT),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        injection_payload(MUSE_MIDSIZE_PROMPT).into_bytes()
    );

    let registry_for_wait = Arc::clone(&registry);
    let wait = std::thread::spawn(move || {
        settle_after_paste(
            &registry_for_wait,
            id,
            &readiness.paste,
            None,
            Duration::from_secs(5),
        )
    });
    evaluator::on_output(id, MUSE_MIDSIZE_COMPOSER_FRAME);
    assert!(
        wait.join()
            .unwrap()
            .expect("a prompt Muse rendered in full must be confirmed, not time out"),
        "the staged prompt is ready for Enter"
    );
    assert_eq!(
        writes.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "confirming the paste must not write anything"
    );
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn muse_midsize_paste_is_not_confirmed_until_its_tail_is_drawn() {
    let _db = crate::db::test_support::isolated();
    let id = muse_publisher_node("muse-midsize-partial-paste");
    evaluator::register(id);
    let PasteReadiness::RenderedMultiline {
        chars,
        normalized_chars,
        content,
        ..
    } = paste_readiness(id, MUSE_MIDSIZE_PROMPT).unwrap().paste
    else {
        panic!("a multiline Muse prompt must use the rendered paste gate");
    };
    // Read each frame back as production does: output since a cursor, escapes stripped.
    let confirmed_by = |frame: &str| {
        let cursor = evaluator::output_cursor(id).unwrap();
        evaluator::on_output(id, frame);
        rendered_paste_visible(
            &evaluator::cleaned_output_since(id, cursor),
            chars,
            normalized_chars,
            &content,
        )
    };
    assert!(confirmed_by(MUSE_MIDSIZE_COMPOSER_FRAME));
    // Everything but the last row: the head of the draft is drawn, the paste is not complete.
    let partial = MUSE_MIDSIZE_COMPOSER_FRAME
        .split("\x1b[20;1H")
        .next()
        .unwrap();
    assert!(
        !confirmed_by(partial),
        "a head-only redraw must not acknowledge a paste that is still arriving"
    );
    // A different draft with the same opening is not this paste either.
    assert!(!confirmed_by(
        &MUSE_MIDSIZE_COMPOSER_FRAME.replace("explain the failure", "do something else")
    ));
    evaluator::unregister(id);
}

/// #2144: a fixture's node must keep the id range its own database was
/// given, which is what makes the id safe to key a process-global
/// registry on.
///
/// `unique_node_id` renumbered a freshly created node to
/// `k * 1_000_000 + 1`, but `db::test_support::isolated` had already been
/// handing each database a disjoint 100_000-wide range since #2130, so the
/// two id spaces overlapped by construction: the eleventh database's first
/// id is `1_000_001`, exactly what the first renumbering handed out. Two
/// parallel delivery tests could therefore register one key in the
/// evaluator's process-global node map, and whichever finished first ran
/// `unregister`, dropping the whole entry — so the survivor's
/// `output_cursor` read returned `None`, or a composer frame drawn for one
/// draft confirmed (or failed to confirm) the other's. Only the concurrent
/// shard runs overlapped enough to hit it, which is why it read as an
/// intermittent Muse flake.
#[test]
fn delivery_fixture_ids_stay_inside_their_own_database_range() {
    // Both fixture shapes this module uses, so neither can drift back into
    // renumbering a node the database already gave a unique id.
    let fixture = |muse: bool, label: String| {
        std::thread::spawn(move || {
            let _db = crate::db::test_support::isolated();
            let range = crate::db::test_support::installed_node_id_range()
                .expect("the install owns an agent-node id range");
            let id = if muse {
                muse_publisher_node(&label)
            } else {
                codex_worker_node(&label)
            };
            (id, range)
        })
    };
    // Twelve databases: the eleventh is the first whose range reaches a
    // `k * 1_000_000 + 1` id, so an overlapping renumbering collides here.
    let mut ids: Vec<i64> = Vec::new();
    for n in 0..12 {
        let (id, (first, last)) = fixture(n % 2 == 0, format!("delivery-range-{n}"))
            .join()
            .unwrap();
        assert!(
            first <= id && id <= last,
            "fixture {n} holds node {id}, outside the {first}..={last} range its database owns"
        );
        ids.push(id);
    }
    let mut distinct = ids.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "two delivery fixtures claimed one node id: {ids:?}"
    );

    // What a shared key cost, asserted through the evaluator itself:
    // `unregister` drops the whole entry, so one fixture finishing emptied
    // every buffer that shared its id, and the survivor's paste gate read
    // `output_cursor` as `None` (issue #2144).
    for id in &ids {
        evaluator::register(*id);
        evaluator::on_output(*id, "a composer redraw");
    }
    evaluator::unregister(ids[0]);
    for id in &ids[1..] {
        assert!(
            evaluator::output_cursor(*id).is_some(),
            "node {id} lost its PTY output buffer to another fixture's unregister"
        );
    }
    for id in &ids[1..] {
        evaluator::unregister(*id);
    }
}

/// Stand-in for the #2061 Codex gap: a mid-size multiline draft in the
/// same size band as run 343's 831-character Muse prompt. A partial frame
/// from Codex 0.160.0 (79x57 Windows ConPTY) showed a ~600-character paste
/// drawn inline with no `[Pasted Content N chars]` marker, but a clean
/// capture was blocked by a hooks-review dialog — so
/// `CODEX_MIDSIZE_COMPOSER_FRAME` is synthetic (the full draft text inside
/// redraw chrome, no marker), pinning the rendering shape the tail rule
/// relies on rather than a byte-exact recording.
const CODEX_MIDSIZE_PROMPT: &str = concat!(
    "Review the changes on this branch and leave line-level feedback.\n",
    "\n",
    "- Compare against `main` and focus on behavior changes, not style.\n",
    "- For each finding, quote the exact lines and explain the failure mode.\n",
    "- Check the new tests exercise the changed paths: name the test and the\n",
    "  assertion that would fail without the fix.\n",
    "- If a check is red, paste the failing output instead of approving.\n",
    "\n",
    "When the review is complete, reply with APPROVE or REQUEST-CHANGES and\n",
    "the full finding list, so the author knows what remains before merge.\n",
    "Do not push any commits yourself.",
);

/// Synthetic Codex composer redraw of `CODEX_MIDSIZE_PROMPT`: cursor
/// addressing and line clears around the full draft text, `›` input
/// prefixes, and no paste marker anywhere.
const CODEX_MIDSIZE_COMPOSER_FRAME: &str = concat!(
        "\x1b[5;1H\x1b[J\x1b[5;1H› Review the changes on this branch and leave line-level feedback.\x1b[K",
        "\x1b[6;1H\x1b[K",
        "\x1b[7;1H› - Compare against `main` and focus on behavior changes, not style.\x1b[K",
        "\x1b[8;1H› - For each finding, quote the exact lines and explain the failure mode.\x1b[K",
        "\x1b[9;1H› - Check the new tests exercise the changed paths: name the test and the\x1b[K",
        "\x1b[10;1H›   assertion that would fail without the fix.\x1b[K",
        "\x1b[11;1H› - If a check is red, paste the failing output instead of approving.\x1b[K",
        "\x1b[12;1H\x1b[K",
        "\x1b[13;1H› When the review is complete, reply with APPROVE or REQUEST-CHANGES and\x1b[K",
        "\x1b[14;1H› the full finding list, so the author knows what remains before merge.\x1b[K",
        "\x1b[15;1H› Do not push any commits yourself.\x1b[K",
        "\x1b[16;1H\x1b[2mesc to cancel · enter to send\x1b[0m",
    );

/// Earlier prompt sharing exactly the ending of `CODEX_MIDSIZE_PROMPT`
/// (its last two sentences) and nothing else, for the stale-redraw
/// limit pin: a redraw of this draft after a newer write carries the
/// newer draft's whole tail proof.
const CODEX_EARLIER_SAME_ENDING_PROMPT: &str = concat!(
    "Summarize yesterday's incidents for the status page.\n",
    "\n",
    "- List each incident with its start time and duration.\n",
    "- Note which ones paged and which stayed silent.\n",
    "\n",
    "So the author knows what remains before merge. Do not push any commits yourself.",
);

/// That earlier prompt's composer redraw: cursor chrome around A's full
/// text, no paste marker. It carries B's tail and none of B's head.
const CODEX_EARLIER_REDRAW_FRAME: &str = concat!(
        "\x1b[5;1H\x1b[J› Summarize yesterday's incidents for the status page.\x1b[K",
        "\x1b[6;1H\x1b[K",
        "\x1b[7;1H› - List each incident with its start time and duration.\x1b[K",
        "\x1b[8;1H› - Note which ones paged and which stayed silent.\x1b[K",
        "\x1b[9;1H\x1b[K",
        "\x1b[10;1H› So the author knows what remains before merge. Do not push any commits yourself.\x1b[K",
        "\x1b[11;1H\x1b[2mesc to cancel · enter to send\x1b[0m",
    );

/// Create a Codex node in this test's own database and return its id.
///
/// The id is already unique across the test process: `isolated()` hands
/// each database its own `agent_nodes` id range (issue #2130), so a node
/// keeps the id its database gave it. Renumbering it into a private range
/// is what reintroduced a shared evaluator key (issue #2144).
fn codex_worker_node(label: &str) -> i64 {
    let path = std::env::temp_dir().join(format!("{}-{}", label, std::process::id()));
    let path = path.to_string_lossy();
    let mesh = crate::db::create_mesh(label, &path).unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "worker",
        path: &path,
        branch: "main",
        env: crate::models::EnvType::Windows,
        provider: "codex",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    node.id
}

#[test]
fn codex_split_marker_paste_submits_with_one_separate_enter() {
    let _db = crate::db::test_support::isolated();
    let id = codex_worker_node("codex-split-marker");
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    let prompt = format!(
        "Please review this example change.\n{}\nFinish with APPROVE or REQUEST_CHANGES.",
        "Explain each finding carefully using the provided context. ".repeat(90)
    );
    assert_eq!(prompt.chars().count(), 5385);
    let expected = registry.input_stamp(id).unwrap();
    let (guard, readiness) = stage_prompt_write(
        &registry,
        &PreparedPrompt::from_raw(id, &prompt),
        Some(&expected),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        injection_payload(&prompt).into_bytes()
    );
    // Codex 0.160.1, native Windows ConPTY at 79 columns: the first 119
    // characters are inline; only the remaining 5266 are collapsed.
    evaluator::on_output(id, concat!(
            "› Please review this example change.                                             Explain each finding carefully using the provided context. Explain each      \r\n",
            "  finding care[Pasted Content 5266 chars]                                      \r\n",
        ));
    let settled = settle_after_paste(
        &registry,
        id,
        &readiness.paste,
        guard.as_deref(),
        Duration::from_secs(3),
    );
    assert!(settled.expect("the complete mixed inline/collapsed Codex paste must become ready"));
    let submitting = Arc::clone(&registry);
    let submit = std::thread::spawn(move || {
        press_enter_until_output_guarded(&submitting, id, guard, Duration::from_secs(2), None)
    });
    assert_eq!(writes.recv_timeout(Duration::from_secs(1)).unwrap(), b"\r");
    evaluator::on_output(id, "task started");
    assert_eq!(submit.join().unwrap().unwrap(), Some(1));
    assert_eq!(writes.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn codex_two_burst_paste_submits_with_one_separate_enter() {
    let _db = crate::db::test_support::isolated();
    for (case, separator) in ["0123456789", "#42!", "#2!"].into_iter().enumerate() {
        let id = codex_worker_node(&format!("codex-two-burst-{case}"));
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        let prompt = format!(
            "Review this change\n{}{separator}{}",
            "x".repeat(1001),
            "y".repeat(1001)
        );
        let expected = registry.input_stamp(id).unwrap();
        let (guard, readiness) = stage_prompt_write(
            &registry,
            &PreparedPrompt::from_raw(id, &prompt),
            Some(&expected),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(1)).unwrap(),
            injection_payload(&prompt).into_bytes()
        );
        evaluator::on_output(id, &format!("› Review this change [Pasted Content 1001 chars]{separator}[Pasted Content 1001 chars] #2"));
        assert!(settle_after_paste(
            &registry,
            id,
            &readiness.paste,
            guard.as_deref(),
            Duration::from_secs(3)
        )
        .unwrap());
        let submitting = Arc::clone(&registry);
        let submit = std::thread::spawn(move || {
            press_enter_until_output_guarded(&submitting, id, guard, Duration::from_secs(2), None)
        });
        assert_eq!(writes.recv_timeout(Duration::from_secs(1)).unwrap(), b"\r");
        evaluator::on_output(id, "task started");
        assert_eq!(submit.join().unwrap().unwrap(), Some(1));
        assert_eq!(writes.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        evaluator::unregister(id);
        registry.kill_session(id);
    }
}

#[test]
fn codex_control_sequence_paste_submits_with_one_separate_enter() {
    let _db = crate::db::test_support::isolated();
    for (case, prompt, expected_prompt, raw_chars, composer_chars) in [
        (
            "middle",
            format!("Review this change\n{}\x1b[31mtail", "x".repeat(1000)),
            format!("Review this change\n{}tail", "x".repeat(1000)),
            1028,
            1023,
        ),
        (
            "trailing",
            format!("Review this change\n{}\x1b[31m", "x".repeat(1001)),
            format!("Review this change\n{}", "x".repeat(1001)),
            1025,
            1020,
        ),
        (
            "unfinished",
            format!(
                "Review this change\n{}\x1b[3{}",
                "x".repeat(1001),
                "界".repeat(1001)
            ),
            format!("Review this change\n{}", "x".repeat(1001)),
            2024,
            1020,
        ),
    ] {
        let id = codex_worker_node(&format!("codex-control-sequence-{case}"));
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        assert_eq!(prompt.chars().count(), raw_chars);
        let expected = registry.input_stamp(id).unwrap();
        let (guard, readiness) = stage_prompt_write(
            &registry,
            &PreparedPrompt::from_raw(id, &prompt),
            Some(&expected),
        )
        .unwrap()
        .unwrap();
        // Assert the literal canonical body, not only its counted proof.
        // No CSI fragment can now reach a later Codex paste burst.
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(1)).unwrap(),
            injection_payload(&expected_prompt).into_bytes()
        );
        let PasteReadiness::RenderedMultiline {
            chars,
            normalized_chars,
            content,
            split_marker_prompt,
            ..
        } = &readiness.paste
        else {
            panic!("Codex must use the rendered paste gate");
        };
        assert_eq!(
            (*chars, *normalized_chars),
            (composer_chars, composer_chars)
        );
        assert!(split_marker_prompt.as_ref().unwrap().payload.is_empty());
        assert!(!rendered_paste_visible(
            &format!("[Pasted Content {raw_chars} chars]"),
            *chars,
            *normalized_chars,
            content
        ));
        evaluator::on_output(id, &format!("[Pasted Content {composer_chars} chars]"));
        let settled = settle_after_paste(
            &registry,
            id,
            &readiness.paste,
            guard.as_deref(),
            Duration::from_secs(3),
        );
        assert!(settled.expect("a complete sanitized Codex paste must become ready"));
        let submitting = Arc::clone(&registry);
        let submit = std::thread::spawn(move || {
            press_enter_until_output_guarded(&submitting, id, guard, Duration::from_secs(2), None)
        });
        assert_eq!(writes.recv_timeout(Duration::from_secs(1)).unwrap(), b"\r");
        evaluator::on_output(id, "task started");
        assert_eq!(submit.join().unwrap().unwrap(), Some(1));
        assert_eq!(writes.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        evaluator::unregister(id);
        registry.kill_session(id);
    }
}

#[test]
fn codex_split_marker_requires_the_matching_complete_prefix_and_suffix_count() {
    let prompt = format!("Review this change\n{}", "x".repeat(1000));
    for output in [
        "› Review this change [Pasted Content 1000 chars]",
        "› Review this change [Pasted Content\r\n1000 chars] #2",
    ] {
        assert!(rendered_split_paste_visible(output, &prompt), "{output:?}");
        assert!(
            rendered_split_paste_visible(output, &prompt.replace('\n', "\r\n")),
            "CRLF input: {output:?}"
        );
    }
    for output in [
        "[Pasted Content 1000 chars]",
        "› Review a different change [Pasted Content 1000 chars]",
        "› Review this [Pasted Content 1000 chars]",
        "› Review this change [Pasted Content 999 chars]",
        "› Review this change [Pasted Content 1002 chars]",
        "› Review this change [Pasted Content 2000 chars]",
        "› Review this change [Pasted Content 0 chars]",
        "› Review this change [Pasted Content 1000 chars",
    ] {
        assert!(!rendered_split_paste_visible(output, &prompt), "{output:?}");
    }
    let unicode_prompt = format!("Résumé change\n{}", "é".repeat(1000));
    assert!(rendered_split_paste_visible(
        "› Résumé change [Pasted Content 1000 chars]",
        &unicode_prompt
    ));
    assert!(!rendered_split_paste_visible(
        "› Résumé change [Pasted Content 2000 chars]",
        &unicode_prompt
    ));
    let long_prefix = format!("Review this change\n{}\nFinish uniquely", "x".repeat(1000));
    assert!(
        !rendered_split_paste_visible(
            &format!(
                "› Review this change {}[Pasted Content 100 chars]",
                "x".repeat(64)
            ),
            &long_prefix
        ),
        "a repeated prefix fragment and smaller paste must not prove the whole draft"
    );
}

fn rendered_split_paste_visible(output: &str, prompt: &str) -> bool {
    RenderedPastePrompt::new(prompt).visible(output, Instant::now() + RENDERED_PASTE_TOTAL_BUDGET)
}

#[test]
fn codex_composer_sanitization_preserves_the_pinned_count_contract() {
    for (input, expected) in [
        ("\x1b[31mred\x1b[0m\0\x7f", "red"),
        ("a\tb\r\nc\rd\n", "a\tb\nc\nd\n"),
        ("a\u{009b}31mb\u{0085}c", "a31mbc"),
        ("left\x1b]0;title\x07right", "left]0;titleright"),
        ("left\x1bXright\x1b", "leftXright"),
        ("left\x1b[31", "left"),
        ("left\x1b[\r\n12", "left"),
        ("İé界\u{200e}", "İé界\u{200e}"),
    ] {
        assert_eq!(codex_composer_text(input), expected, "{input:?}");
    }
}

#[test]
fn codex_sanitized_segments_require_the_composer_count() {
    let prompt = format!(
        "\x1b[31mReview\0 this\x7f change\x1b[0m\r\n{}\x1b[32m\t{}\u{009f}\rtail",
        "x".repeat(1001),
        "é".repeat(1001)
    );
    assert_eq!(prompt.chars().count(), 2045);
    assert_eq!(codex_composer_text(&prompt).chars().count(), 2027);
    let complete =
        "› Review this change [Pasted Content 1001 chars]\t[Pasted Content 1001 chars] #2\ntail";
    assert!(rendered_split_paste_visible(complete, &prompt));
    for wrong in [
        complete.replacen("1001", "1000", 1),
        complete.replace("1001 chars] #2", "2002 chars] #2"),
        complete.replace("Review this", "Review31m this"),
    ] {
        assert!(!rendered_split_paste_visible(&wrong, &prompt), "{wrong:?}");
    }
    let color_prompt = format!("Review this change\n{}\x1b[31mtail", "x".repeat(1000));
    assert!(rendered_split_paste_visible(
        "[Pasted Content 1023 chars]",
        &color_prompt
    ));
    assert!(!rendered_split_paste_visible(
        "[Pasted Content 1028 chars]",
        &color_prompt
    ));
}

#[test]
fn codex_sanitized_inline_paste_uses_the_composer_text() {
    let _db = crate::db::test_support::isolated();
    let id = codex_worker_node("codex-sanitized-inline");
    let (registry, _) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    let prompt = "\x1b[31mReview this change\r\nwith context\x1b[0m\0\x7f";
    let PasteReadiness::RenderedMultiline {
        chars,
        normalized_chars,
        content,
        ..
    } = paste_readiness(id, prompt).unwrap().paste
    else {
        panic!("Codex must use the rendered paste gate");
    };
    assert_eq!((chars, normalized_chars), (31, 31));
    assert_eq!(content, "reviewthischangewithcontext");
    assert!(rendered_paste_visible(
        "› Review this change\nwith context",
        chars,
        normalized_chars,
        &content
    ));
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn codex_multi_burst_counts_require_all_intervening_text() {
    let prompt = format!(
        "Review this change\n{}0123456789{}",
        "x".repeat(500),
        "y".repeat(500)
    );
    let complete =
        "› Review this change [Pasted Content 500 chars]0123456789[Pasted Content 500 chars] #2";
    assert!(rendered_split_paste_visible(complete, &prompt));
    assert!(rendered_split_paste_visible(
        complete,
        &prompt.replace('\n', "\r\n")
    ));
    for output in [
        "› Review this change [Pasted Content 500 chars]0123456789",
        "› Review this change [Pasted Content 500 chars]0123456789[Pasted Content 499 chars]",
        "› Review this change [Pasted Content 500 chars]0123456789[Pasted Content 501 chars]",
        "› Review this change [Pasted Content 500 chars]0123456788[Pasted Content 500 chars] #2",
        "› Review this change [Pasted Content 500 chars][Pasted Content 500 chars] #2",
        "› Review this change 0123456789[Pasted Content 500 chars]",
        "› Review this [Pasted Content 500 chars]0123456789[Pasted Content 500 chars] #2",
        "› Review this change [Pasted Content 500 chars]0123456789[Pasted Content 500 chars",
    ] {
        assert!(!rendered_split_paste_visible(output, &prompt), "{output:?}");
    }
    let with_tail = format!("{prompt}\nFinish uniquely");
    assert!(rendered_split_paste_visible(
        &format!("{complete}\n  Finish uniquely\n  esc to cancel"),
        &with_tail
    ));
    assert!(!rendered_split_paste_visible(
        &format!("{complete}\n  Finish unique"),
        &with_tail
    ));
    let all_collapsed = format!("{}\n{}", "é".repeat(499), "界".repeat(500));
    assert!(rendered_split_paste_visible(
        "› [Pasted Content 500 chars][Pasted Content 500 chars] #2",
        &all_collapsed
    ));
    assert!(!rendered_split_paste_visible(
        "› [Pasted Content 1001 chars][Pasted Content 1001 chars] #2",
        &all_collapsed
    ));
    let three = format!("{prompt}{}", "z".repeat(500));
    assert!(rendered_split_paste_visible(
        &format!("{complete}[Pasted Content 500 chars] #3"),
        &three
    ));
}

#[test]
fn codex_marker_count_survives_an_omitted_inline_newline() {
    let prompt = format!(
        "Please review this example change.\n{}\nFinish with APPROVE or REQUEST_CHANGES.",
        "Explain each finding carefully using the provided context. ".repeat(90)
    );
    // Native 0.160.1 submission trace: LF at character 34 is absent from
    // the inline text, while the collapsed suffix still counts 5242.
    let inline = concat!(
        "Please review this example change.Explain each finding carefully using the ",
        "provided context. Explain each finding carefully using the provided"
    );
    assert_eq!(prompt.chars().count(), 5385);
    assert_eq!(inline.chars().count(), 142);
    assert_eq!(prompt.chars().skip(143).count(), 5242);
    assert_eq!(
        crate::circuit::launch::normalize_for_match(inline),
        crate::circuit::launch::normalize_for_match(&prompt.chars().take(143).collect::<String>())
    );
    assert!(rendered_split_paste_visible(
        &format!("› {inline}[Pasted\r\n  Content 5242 chars]"),
        &prompt
    ));
    // Moving the boundary across a visible letter must fail. A boundary
    // differing only by invisible whitespace is indistinguishable in a TUI.
    assert!(!rendered_split_paste_visible(
        &format!("› {inline}[Pasted Content 5240 chars]"),
        &prompt
    ));
}

#[test]
fn codex_multi_burst_counts_normalize_hidden_crlf_and_count_unicode_chars() {
    let prompt = format!(
        "Review this change\r\n{}\r\nİssue #42!{}\r\nFinish uniquely",
        "é".repeat(1000),
        "界".repeat(1001)
    );
    let frame = "› Review this change [Pasted Content 1001 chars]İssue #42![Pasted Content 1001 chars] #2\r\n  Finish uniquely\r\n  esc to cancel";
    assert!(rendered_split_paste_visible(frame, &prompt));
    assert!(!rendered_split_paste_visible(
        &frame.replace("1001 chars", "2002 chars"),
        &prompt
    ));
    assert!(!rendered_split_paste_visible(
        &frame.replace("İssue #42!", "İssue #43!"),
        &prompt
    ));
}

#[test]
fn codex_repeated_marker_redraws_do_not_count_as_new_bursts() {
    let prompt = format!("{}\n{}", "x".repeat(1000), "y".repeat(1001));
    for repeated in [
        "› [Pasted Content 1001 chars][Pasted Content 1001 chars]",
        "› [Pasted Content 1001 chars] #2[Pasted Content 1001 chars] #2",
        "› [Pasted Content 1001 chars][Pasted Content 1001 chars] #0",
    ] {
        assert!(
            !rendered_split_paste_visible(repeated, &prompt),
            "{repeated:?}"
        );
    }
    assert!(rendered_split_paste_visible(
        "› [Pasted Content 1001 chars][Pasted Content 1001 chars] #2",
        &prompt
    ));
    let ambiguous = format!(
        "Review this change\n{}#2{}",
        "x".repeat(1001),
        "y".repeat(999)
    );
    assert!(!rendered_split_paste_visible(
        "› Review this change [Pasted Content 1001 chars]#2[Pasted Content 1001 chars]",
        &ambiguous
    ));
}

#[test]
fn codex_inline_digits_after_an_ordinal_remain_prompt_text() {
    let prompt = format!(
        "Review this change\n{}{}42{}",
        "x".repeat(1001),
        "y".repeat(1001),
        "z".repeat(1001)
    );
    assert!(rendered_split_paste_visible("› Review this change [Pasted Content 1001 chars][Pasted Content 1001 chars] #242[Pasted Content 1001 chars] #3", &prompt));
}

#[test]
fn codex_rejected_bursts_with_long_ignored_segments_do_not_branch_repeatedly() {
    let prompt = format!("{}\n{}Unique ending", "h".repeat(6000), " ".repeat(6000));
    let output = "› Wrong heading [Pasted Content 1001 chars][Pasted Content 1001 chars] #2[Pasted Content 1001 chars] #3[Pasted Content 1001 chars] #4[Pasted Content 1001 chars] #5Unique ending";
    assert!(!rendered_split_paste_visible(output, &prompt));
}

#[test]
fn codex_rejected_distinct_bursts_discard_irrelevant_ordinal_histories() {
    let prompt = format!("{}\n", "2".repeat(50_000));
    let mut output = String::from("Wrong heading ");
    for count in 1001..=1030 {
        output.push_str(&format!("[Pasted Content {count} chars]#22"));
    }
    assert!(!rendered_split_paste_visible(&output, &prompt));
}

#[test]
fn codex_recurring_numeric_ambiguities_exhaust_a_bounded_search() {
    let prompt = format!("{}\n", "2".repeat(80_000));
    let mut output = String::from("Wrong heading ");
    for _ in 0..2 {
        for count in 1001..=1030 {
            output.push_str(&format!("[Pasted Content {count} chars]#22"));
        }
    }
    let mut budget = PasteMatchBudget::new(Instant::now() + RENDERED_PASTE_TOTAL_BUDGET);
    assert!(!RenderedPastePrompt::new(&prompt).visible_with_budget(&output, &mut budget));
    assert_eq!(
        budget.remaining_states, 0,
        "ambiguous paths must stop at the search limit"
    );
}

#[test]
fn codex_partial_burst_redraws_exhaust_readiness_without_enter() {
    let _db = crate::db::test_support::isolated();
    let id = codex_worker_node("codex-partial-burst-redraw");
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    let prompt = format!(
        "Review this change\n{}{}",
        "x".repeat(1001),
        "y".repeat(1001)
    );
    let expected = registry.input_stamp(id).unwrap();
    let (guard, readiness) = stage_prompt_write(
        &registry,
        &PreparedPrompt::from_raw(id, &prompt),
        Some(&expected),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        injection_payload(&prompt).into_bytes()
    );
    evaluator::on_output(id, "› Review this change [Pasted Content 1001 chars]");
    evaluator::on_output(id, "\x1b[8;3H[Pasted Content 1001 chars]");
    // Let the real quiet fence open: otherwise a faulty proof could still
    // time out and leave this negative regression green.
    std::thread::sleep(Duration::from_millis(PASTE_SETTLE_QUIET_MS as u64));
    assert!(settle_after_paste(
        &registry,
        id,
        &readiness.paste,
        guard.as_deref(),
        Duration::from_millis(1)
    )
    .is_err());
    assert_eq!(writes.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn codex_split_marker_rejects_stale_output_and_changed_input() {
    let _db = crate::db::test_support::isolated();
    let id = codex_worker_node("codex-split-marker-fences");
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    let prompt = format!("Review this change\n{}", "x".repeat(1000));
    let frame = "› Review this change [Pasted Content 1000 chars]";
    evaluator::on_output(id, frame);
    // The stale frame must already satisfy quiet readiness; otherwise
    // the timeout could pass even if delivery accidentally reused it.
    std::thread::sleep(Duration::from_millis(PASTE_SETTLE_QUIET_MS as u64));
    let expected = registry.input_stamp(id).unwrap();
    let (guard, readiness) = stage_prompt_write(
        &registry,
        &PreparedPrompt::from_raw(id, &prompt),
        Some(&expected),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        injection_payload(&prompt).into_bytes()
    );
    assert!(
        settle_after_paste(
            &registry,
            id,
            &readiness.paste,
            guard.as_deref(),
            Duration::from_millis(1)
        )
        .is_err(),
        "an earlier matching composer must not prove the new paste"
    );
    evaluator::on_output(id, frame);
    registry.write_bytes(id, b"user correction").unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        b"user correction"
    );
    assert!(!settle_after_paste(
        &registry,
        id,
        &readiness.paste,
        guard.as_deref(),
        Duration::from_secs(1)
    )
    .unwrap());
    assert_eq!(
        writes.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "lost ownership must not send Enter or repeat the paste"
    );
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn codex_midsize_paste_rendered_in_full_is_confirmed_by_its_tail() {
    let _db = crate::db::test_support::isolated();
    let id = codex_worker_node("codex-midsize-paste");
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    assert!(
        crate::circuit::launch::normalize_for_match(CODEX_MIDSIZE_PROMPT)
            .chars()
            .count()
            > VISIBLE_PASTE_TEXT_LIMIT,
        "fixture precondition: past the full-text limit, where only a marker used to be accepted"
    );
    let (_, readiness) = stage_prompt_write(
        &registry,
        &PreparedPrompt::from_raw(id, CODEX_MIDSIZE_PROMPT),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        injection_payload(CODEX_MIDSIZE_PROMPT).into_bytes()
    );

    let registry_for_wait = Arc::clone(&registry);
    let wait = std::thread::spawn(move || {
        settle_after_paste(
            &registry_for_wait,
            id,
            &readiness.paste,
            None,
            Duration::from_secs(2),
        )
    });
    evaluator::on_output(id, CODEX_MIDSIZE_COMPOSER_FRAME);
    assert!(
        wait.join()
            .unwrap()
            .expect("a prompt Codex rendered in full must be confirmed, not time out"),
        "the staged prompt is ready for Enter"
    );
    assert_eq!(
        writes.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "confirming the paste must not write anything"
    );
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn codex_midsize_paste_is_not_confirmed_until_its_tail_is_drawn() {
    let _db = crate::db::test_support::isolated();
    let id = codex_worker_node("codex-midsize-partial-paste");
    evaluator::register(id);
    let PasteReadiness::RenderedMultiline {
        chars,
        normalized_chars,
        content,
        ..
    } = paste_readiness(id, CODEX_MIDSIZE_PROMPT).unwrap().paste
    else {
        panic!("a multiline Codex prompt must use the rendered paste gate");
    };
    // Read each frame back as production does: output since a cursor, escapes stripped.
    let confirmed_by = |frame: &str| {
        let cursor = evaluator::output_cursor(id).unwrap();
        evaluator::on_output(id, frame);
        rendered_paste_visible(
            &evaluator::cleaned_output_since(id, cursor),
            chars,
            normalized_chars,
            &content,
        )
    };
    assert!(confirmed_by(CODEX_MIDSIZE_COMPOSER_FRAME));
    // Everything but the last row: the head of the draft is drawn, the paste is not complete.
    let partial = CODEX_MIDSIZE_COMPOSER_FRAME
        .split("\x1b[15;1H")
        .next()
        .unwrap();
    assert!(
        !confirmed_by(partial),
        "a head-only redraw must not acknowledge a paste that is still arriving"
    );
    // A different draft with the same opening is not this paste either.
    assert!(!confirmed_by(&CODEX_MIDSIZE_COMPOSER_FRAME.replace(
        "Do not push any commits yourself",
        "Do something else instead"
    )));
    evaluator::unregister(id);
}

#[test]
fn paste_confirmation_policy_is_declared_per_harness() {
    use crate::agent::provider::{adapters, AgentProvider, PasteGatePolicy};
    // Every adapter declares its paste-gate policy, so no per-harness
    // string comparison in delivery.rs can drift out of sync when a new
    // harness is added: extend this table alongside the new adapter.
    // Tail-anchor harnesses draw mid-size pastes inline (issues
    // #2060/#2061); every other harness keeps the default Generic gate.
    let policies: &[(&str, PasteGatePolicy)] = &[
        ("agy", adapters::AGY.paste_gate_policy()),
        ("anthropic", adapters::ANTHROPIC.paste_gate_policy()),
        ("cline", adapters::CLINE.paste_gate_policy()),
        ("codex", adapters::CODEX.paste_gate_policy()),
        ("commandcode", adapters::COMMANDCODE.paste_gate_policy()),
        ("cursor", adapters::CURSOR.paste_gate_policy()),
        ("dsh", adapters::DSH.paste_gate_policy()),
        ("freebuff", adapters::FREEBUFF.paste_gate_policy()),
        ("grok", adapters::GROK.paste_gate_policy()),
        ("kimi", adapters::KIMI.paste_gate_policy()),
        ("mcode", adapters::MCODE.paste_gate_policy()),
        ("muse", adapters::MUSE.paste_gate_policy()),
        ("opencode", adapters::OPENCODE.paste_gate_policy()),
        ("terminal", adapters::TERMINAL.paste_gate_policy()),
    ];
    assert_eq!(policies.len(), 14, "one row per harness adapter");
    for (name, policy) in policies {
        let expected = if *name == "codex" {
            PasteGatePolicy::RenderedWithSplitMarker
        } else if *name == "muse" {
            PasteGatePolicy::RenderedWithTailAnchor
        } else {
            PasteGatePolicy::Generic
        };
        assert_eq!(*policy, expected, "paste-gate policy for {name}");
    }
}

#[test]
fn a_stale_redraw_with_an_identical_ending_is_a_known_tail_match_limit() {
    // Issue #2061 item 3: the tail proof cannot tell a fresh redraw from
    // a stale one repainting an EARLIER prompt that shares only the
    // ending. A and B below share exactly their last 64 normalized
    // characters and differ everywhere else; staging B while A's redraw
    // lands after the write still satisfies the match. Output-cursor
    // scoping plus the 1 s quiet gate narrow this but do not remove it.
    // Pinned here so a future hardening (a prompt-specific token, or
    // checking the composer region only) visibly flips the final
    // assertion to `assert!(!...)`.
    let _db = crate::db::test_support::isolated();
    let id = codex_worker_node("codex-stale-redraw-limit");
    evaluator::register(id);
    let PasteReadiness::RenderedMultiline {
        chars,
        normalized_chars,
        content,
        ..
    } = paste_readiness(id, CODEX_MIDSIZE_PROMPT).unwrap().paste
    else {
        panic!("a multiline Codex prompt must use the rendered paste gate");
    };
    // The earlier prompt shares B's tail and nothing else: if its redraw
    // also carried B's head, this would re-test the happy path instead
    // of the stale-redraw limit.
    let b_normalized = crate::circuit::launch::normalize_for_match(CODEX_MIDSIZE_PROMPT);
    let b_head: String = b_normalized.chars().take(30).collect();
    let a_normalized =
        crate::circuit::launch::normalize_for_match(CODEX_EARLIER_SAME_ENDING_PROMPT);
    assert!(
        a_normalized.ends_with(&content),
        "fixture precondition: the earlier prompt shares B's tail"
    );
    assert!(
        !a_normalized.contains(&b_head),
        "fixture precondition: the earlier prompt shares ONLY the tail"
    );
    // A's redraw lands after B's write: the matcher sees B's tail in it.
    let cursor = evaluator::output_cursor(id).unwrap();
    evaluator::on_output(id, CODEX_EARLIER_REDRAW_FRAME);
    assert!(
        rendered_paste_visible(
            &evaluator::cleaned_output_since(id, cursor),
            chars,
            normalized_chars,
            &content
        ),
        "known limit: a stale redraw sharing only the ending satisfies the tail match"
    );
    evaluator::unregister(id);
}

#[test]
fn tail_anchor_survives_narrow_terminal_fragmentation() {
    // Issue #2061 item 5: a 22-column ConPTY capture is known to insert
    // padding inside `[Pasted Content N chars]` markers. The tail anchor
    // must survive the same fragmentation when a narrow composer wraps
    // mid-word — `normalize_for_match` drops whitespace, so wrap padding
    // and line breaks inside the tail region cannot hide it.
    let normalized = crate::circuit::launch::normalize_for_match(CODEX_MIDSIZE_PROMPT);
    let proof = visible_paste_proof(PasteGatePolicy::RenderedWithTailAnchor, normalized);
    assert_eq!(proof.chars().count(), TAIL_ANCHOR_CHARS);
    // The split lands inside the tail: the fragmented span is part of the proof.
    let tail_span = crate::circuit::launch::normalize_for_match("any commits yourself");
    assert!(
        proof.contains(&tail_span),
        "fixture precondition: the narrow wrap splits the tail itself"
    );
    let _db = crate::db::test_support::isolated();
    let id = codex_worker_node("codex-narrow-tail");
    evaluator::register(id);
    let PasteReadiness::RenderedMultiline {
        chars,
        normalized_chars,
        content,
        ..
    } = paste_readiness(id, CODEX_MIDSIZE_PROMPT).unwrap().paste
    else {
        panic!("a multiline Codex prompt must use the rendered paste gate");
    };
    assert_eq!(
        content, proof,
        "the gate anchors on the same tail with or without fragmentation"
    );
    let fragmented = CODEX_MIDSIZE_COMPOSER_FRAME
        .replace("any commits yourself.", "any\x1b[K\r\n   commits yourself.");
    let cursor = evaluator::output_cursor(id).unwrap();
    evaluator::on_output(id, &fragmented);
    assert!(
        rendered_paste_visible(
            &evaluator::cleaned_output_since(id, cursor),
            chars,
            normalized_chars,
            &content
        ),
        "ConPTY padding and line breaks inside the tail must not hide it"
    );
    evaluator::unregister(id);
}

#[test]
fn tail_rule_applies_at_any_size_without_a_collapse_cutoff() {
    // Issue #2061 item 4: the exact collapse point is unmeasured (Muse
    // drew 839 chars and collapsed 1,509; chars vs lines vs width is
    // unknown), so the gate does not encode one — marker OR tail confirms
    // at any size. A collapsed draft carries no visible tail, so only its
    // marker can confirm it.
    for size in [839, 1_509, 9_407] {
        let draft = format!("prompt\n{}", "z".repeat(size));
        let normalized = crate::circuit::launch::normalize_for_match(&draft);
        assert!(
            normalized.chars().count() > VISIBLE_PASTE_TEXT_LIMIT,
            "fixture precondition"
        );
        assert_eq!(
            visible_paste_proof(PasteGatePolicy::RenderedWithTailAnchor, normalized),
            "z".repeat(TAIL_ANCHOR_CHARS),
            "no collapse cutoff at size {size}",
        );
    }
    let draft = format!("prompt\n{}", "z".repeat(1_509));
    let normalized = crate::circuit::launch::normalize_for_match(&draft);
    assert_eq!(
        visible_paste_proof(PasteGatePolicy::RenderedMarkerOnly, normalized),
        "",
        "marker-only harnesses keep marker-only at every size",
    );
}

#[test]
fn muse_enter_ignores_redraw_until_the_matching_prompt_is_accepted() {
    let id = -930_026;
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let record = |prompt: &str| {
        format!(
            "{}\n",
            serde_json::json!({
                "payload_type":"runtime.session", "payload":{"kind":"run",
                    "event":{"kind":"started","prompt":prompt}}
            })
        )
    };
    std::fs::write(&path, record("feedback\nfix it")).unwrap();
    let receipt =
        crate::services::muse_watcher::PromptReceipt::from_log(path.clone(), "feedback\r\nfix it")
            .unwrap();
    assert!(
        !receipt.accepted(),
        "an earlier matching prompt is not this submission"
    );
    let registry_for_submit = Arc::clone(&registry);
    let submit = std::thread::spawn(move || {
        press_enter_until_output_guarded(
            &registry_for_submit,
            id,
            None,
            Duration::from_millis(300),
            Some(&receipt),
        )
    });
    assert_eq!(writes.recv_timeout(Duration::from_secs(2)).unwrap(), b"\r");
    evaluator::on_output(id, "[Pasted Content 15 chars] redraw");
    // The second Enter proves redraw alone did not satisfy the first one.
    assert_eq!(writes.recv_timeout(Duration::from_secs(2)).unwrap(), b"\r");
    use std::io::Write;
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    log.write_all(record("feedback\nfix it").as_bytes())
        .unwrap();
    log.flush().unwrap();
    assert_eq!(submit.join().unwrap().unwrap(), Some(2));
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn codex_paste_wait_stops_when_process_dies() {
    let id = -930_025;
    let (registry, _writes) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    let readiness = PromptReadiness {
        paste: PasteReadiness::RenderedMultiline {
            chars: 100,
            normalized_chars: 100,
            content: String::new(),
            split_marker_prompt: None,
            output_cursor: evaluator::output_cursor(id).unwrap(),
        },
        receipt: None,
    };
    registry.kill_session(id);
    let started = Instant::now();
    let error = settle_after_paste(
        &registry,
        id,
        &readiness.paste,
        None,
        RENDERED_PASTE_TOTAL_BUDGET,
    )
    .unwrap_err();
    assert!(error.contains("no live agent process"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(1));
    evaluator::unregister(id);
}

#[test]
fn output_seen_within_only_counts_output_newer_than_the_mark() {
    // Output 200ms ago, mark set 500ms ago → the output followed the mark.
    assert!(output_seen_within(Some(200), 500));
    // Output 800ms ago predates a 500ms-old mark → not an acknowledgement.
    assert!(!output_seen_within(Some(800), 500));
    // No output tracked at all → never an acknowledgement.
    assert!(!output_seen_within(None, 10_000));
}

// ── submit posture (#874): what a target that cannot be observed gets ───
//
// The staged-submit path verifies its Enter against the evaluator's output
// clock, which exists only for nodes the evaluator buffers. The terminal's
// "Handover to node" injects into a target a human spawned, which is never
// buffered — and that case previously ran the retry ladder anyway, typing
// two extra carriage returns into an agent already working on the prompt and
// then reporting "never submitted" (which marks the node for attention).

/// Everything the path under test wrote, drained until the channel goes
/// quiet. Asserted as a whole so a missing or extra Enter fails loudly
/// instead of hanging the suite on a blocking `recv`.
fn drained(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
        chunks.push(chunk);
    }
    chunks
}

#[test]
fn a_target_with_no_output_clock_submits_with_exactly_one_enter() {
    let id = -930_020;
    let (registry, rx) = crate::agent::process::testing::capturing_registry(id);
    assert!(
        !evaluator::is_circuit_piloted(id),
        "fixture precondition: this target is not buffered by the evaluator"
    );

    assert_eq!(
        press_enter_until_output_guarded(&registry, id, None, Duration::from_millis(50), None)
            .expect("an unobservable submit cannot fail: nothing can be compared"),
        Some(1),
    );
    assert_eq!(
        drained(&rx),
        vec![b"\r".to_vec()],
        "one Enter, and no retry ladder behind it"
    );
    registry.kill_session(id);
}

#[test]
fn existing_codex_handover_stages_one_file_pointer_and_one_enter() {
    let _db = crate::db::test_support::isolated();
    let root = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(root.path().to_path_buf());
    let id = codex_worker_node("codex-file-handover");
    let node = crate::db::get_agent_node_by_id(id).unwrap();
    let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
    let text = "REQUEST CHANGES.\r\nReview all findings — 🚀\r\n".repeat(40);
    let prompt = crate::agent::handover::prepare(
        &node,
        crate::models::Provider::Codex,
        &text,
        Some(root.path()),
    )
    .unwrap();
    let prepared = PreparedPrompt::prepare(id, prompt.prompt()).unwrap();
    let (_, readiness) = stage_prompt_write(&registry, &prepared, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        writes.recv_timeout(Duration::from_secs(1)).unwrap(),
        prompt.prompt().as_bytes()
    );
    assert!(!prompt.prompt().contains(['\r', '\n']));
    assert_eq!(
        submit_staged_prompt_result(&registry, id, None, &readiness).unwrap(),
        Some(1)
    );
    assert_eq!(drained(&writes), vec![b"\r".to_vec()]);
    let files: Vec<_> = std::fs::read_dir(root.path().join(format!("handovers/node-{id}")))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(std::fs::read(&files[0]).unwrap(), text.as_bytes());
    registry.kill_session(id);
    crate::preferences::reset_for_tests();
}

/// The ladder must survive for a buffered node — including one that has not
/// produced output yet, which is the state `node_launch` injects its prefill
/// into (it registers the evaluator before spawning). Keying the posture off
/// "has output yet" instead of "is buffered" would silently demote that
/// caller to a single blind Enter.
#[test]
fn a_buffered_but_silent_target_retries_then_fails_loudly() {
    let id = -930_021;
    let (registry, rx) = crate::agent::process::testing::capturing_registry(id);
    evaluator::register(id);
    assert!(
        evaluator::is_circuit_piloted(id) && evaluator::millis_since_last_output(id).is_none(),
        "fixture precondition: buffered, but no output to acknowledge against"
    );

    let err =
        press_enter_until_output_guarded(&registry, id, None, Duration::from_millis(50), None)
            .expect_err("nothing acknowledges the Enter ⇒ the path must not claim success");
    assert!(err.contains("no PTY output followed"), "{err}");
    assert_eq!(
        drained(&rx),
        vec![b"\r".to_vec(); MAX_ENTER_ATTEMPTS as usize],
        "a buffered target keeps its retry ladder"
    );
    evaluator::unregister(id);
    registry.kill_session(id);
}

#[test]
fn a_staged_prompt_needs_a_live_process_not_a_db_row() {
    let id = -930_022;
    let (registry, _rx) = crate::agent::process::testing::capturing_registry(id);
    assert!(ensure_prompt_target_alive(&registry, id).is_ok());

    let err = ensure_prompt_target_alive(&registry, -930_099)
        .expect_err("a node with no registry entry has no live process");
    assert!(err.contains("no live agent process"), "{err}");
    registry.kill_session(id);
}

// ── in-flight guard: queue, don't drop (#874 candidate 3) ──────────────
