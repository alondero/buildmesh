use super::*;

#[test]
fn question_matcher_migration_preserves_user_handlers_in_mixed_group() {
    let legacy_owned = serde_json::json!({
        "type":"http",
        "url":"http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID"
    });
    let owned = serde_json::json!({"type":"command", "command":HOOK_COMMAND_UNIX});
    let user = serde_json::json!({"type":"command", "command":"user-policy"});
    let mut groups = vec![serde_json::json!({
        "matcher":"Bash",
        "hooks":[legacy_owned, user.clone()]
    })];
    let ours = |candidate: &serde_json::Value| GROK_OWNERSHIP.owns_handler(candidate);
    assert!(crate::agent::attention_hooks::merge_question_handler(
        &mut groups,
        &ours,
        &owned,
        "^ask_user_question$"
    ));
    assert_eq!(
        groups,
        vec![
            serde_json::json!({"matcher":"Bash", "hooks":[user]}),
            serde_json::json!({"matcher":"^ask_user_question$", "hooks":[owned.clone()]})
        ]
    );
    assert!(!crate::agent::attention_hooks::merge_question_handler(
        &mut groups,
        &ours,
        &owned,
        "^ask_user_question$"
    ));
}
