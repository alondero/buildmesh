//! Shared review prompt contract used by interactive and circuit spawns.
//!
//! Keeping the policy in this neutral module prevents the lower-level spawn
//! intent layer from depending on the Autopilot graph model, while giving
//! migrations one stable home for the historical prompt shapes they upgrade.

pub(crate) const REVIEW_POLICY: &str = "Review the implementation as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture. Inspect the reviewed commit or revision under review and the complete change, including relevant project instructions, tests, and previous findings. Report actionable correctness, specification, and verification findings with file locations and impact. Treat optional style suggestions as non-blocking. State the reviewed commit or revision under review and an explicit final verdict: approve only if no actionable findings remain; otherwise request changes or explain what blocks review. Review completion alone is not approval. Re-check previous findings against the current code.";

pub(crate) const PR_REVIEW_DELIVERY: &str =
    "Review PR {{pr.number}} and post the findings as a PR comment.";

pub(crate) const REVIEW_FEEDBACK_POLICY: &str = "Reviewer report:\n{{node.reviewer.output}}\nAddress every valid finding, run the relevant tests, and report what you changed. Explain any finding you disagree with. Do not start another review loop yourself.";

pub(crate) const LEGACY_LOCAL_REVIEW_PROMPT: &str = "Review the work of agent {{source.agent_id}} in {{source.path}}. Read that directory directly: review committed changes from the merge-base with {{source.base_ref}} and all uncommitted/untracked changes. The source task is {{source.name}}. Its latest report is: {{source.output}}\nInspect the code and relevant project instructions. Do not modify files, commit, push, post comments, or open a PR. Report actionable findings with file locations in your final response. If the work is satisfactory, explicitly state that you approve and have no remaining findings. Otherwise explicitly state that changes are requested. If you cannot assess the work, explain the blocker. This is review round {{retry.attempt}} of {{retry.max_retries}}.";

pub(crate) const LEGACY_FEEDBACK_PROMPT: &str = "An independent reviewer requested changes to your work. Review report:\n{{node.reviewer.output}}\nAddress every valid finding, run relevant checks, and report your changes. Explain any finding you disagree with. Another independent review will follow. Do not start another review loop yourself.";

pub(crate) const LEGACY_PR_REVIEW_PROMPT: &str = "review PR {{pr.number}} as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture. Add the review comments to the PR as a comment. The pull request URL is {{pr.url}}.";

pub(crate) const LEGACY_PR_REVIEW_WITH_VERDICT: &str = "review PR {{pr.number}} as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture. Add the review comments to the PR as a comment. The pull request URL is {{pr.url}}. State the reviewed commit and an explicit final verdict: approve only if there are no remaining actionable findings; otherwise request changes or explain what blocks review. Review completion alone is not approval. Re-check previous findings against the current code. Separate blocking correctness, specification and verification findings from optional style suggestions; do not turn optional preferences or unrelated redesigns into blockers.";
