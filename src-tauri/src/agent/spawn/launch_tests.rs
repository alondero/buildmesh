/// Only a prompt that reaches the command line can start a turn at launch;
/// it decides whether the post-spawn promotion lands Running or Ready.
#[test]
fn launch_carries_prompt_only_for_a_supported_non_empty_prefill() {
    use super::launch::launch_carries_prompt;
    assert!(launch_carries_prompt(true, Some("fix the bug")));
    assert!(!launch_carries_prompt(true, None));
    assert!(!launch_carries_prompt(true, Some("")));
    assert!(!launch_carries_prompt(false, Some("fix the bug")));
}
