//! Shared review prompt contract used by interactive and circuit spawns.
//!
//! Keeping the policy in this neutral module prevents the lower-level spawn
//! intent layer from depending on the Autopilot graph model, while giving
//! migrations one stable home for the historical prompt shapes they upgrade.

pub(crate) const REVIEW_POLICY: &str = "Review the implementation as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture. Inspect the reviewed commit or revision under review and the complete change, including relevant project instructions, tests, and previous findings. Report actionable correctness, specification, and verification findings with file locations and impact. Treat optional style suggestions as non-blocking. State the reviewed commit or revision under review and an explicit final verdict: approve only if no actionable findings remain; otherwise request changes or explain what blocks review. Review completion alone is not approval. Re-check previous findings against the current code.";

pub(crate) const PR_REVIEW_DELIVERY: &str =
    "Review PR {{pr.number}} and post the findings as a PR comment.";

pub(crate) const REVIEW_FEEDBACK_POLICY: &str = "Reviewer report:\n{{node.reviewer.output}}\nAddress every valid finding, run the relevant tests, and report what you changed. Explain any finding you disagree with. Do not start another review loop yourself.";

/// Sent to a borrowed source before its reviewer is spawned, so the review
/// and the eventual merge both act on a published branch.
pub(crate) const PUBLISH_FOR_REVIEW: &str = "Before an independent review starts, publish your work for review: commit all of your changes with a clear conventional commit message, push the branch (`git push -u origin HEAD`), and open a pull request for it if this branch does not already have one (`gh pr create --fill`). Do not merge the pull request. Report the pull request URL.";

/// Later rounds reuse the reviewer that produced the earlier findings, so it
/// is reminded of the policy rather than given a fresh scope.
pub(crate) const RE_REVIEW_DELIVERY: &str = "The author has addressed your previous review and updated the work. Review it again with the same scope and delivery instructions as your previous review: re-check every earlier finding against the current code and inspect the new changes. This is review round {{retry.attempt}} of {{retry.max_retries}}.";

/// The approval hand-off. `pr_argument` is appended to each `gh pr` command;
/// an empty argument lets `gh` resolve the pull request for the current branch.
///
/// A branch that fell behind its base while it was reviewed is the usual reason
/// a merge is refused, so the agent first brings it up to date (GitHub's own
/// update when it can, a local rebase when it cannot) and only then waits for
/// the checks that restarts. The final `gh pr view` gives the agent, and the
/// reader of its report, a merge result that can be checked.
pub(crate) fn merge_approved_pr(subject: &str, pr_argument: &str) -> String {
    format!(
        "The independent reviewer approved {subject}. Squash and merge it now: if it is still a draft, mark it ready for review (`gh pr ready{pr_argument}`). \
        Then check whether the PR branch is behind the base branch (`gh pr view{pr_argument} --json mergeStateStatus,mergeable`). \
        Only if mergeStateStatus reports BEHIND (behind the base branch) or the PR is otherwise reported out-of-date, bring it up to date with `gh pr update-branch{pr_argument}` — prefer update-branch over rebasing, because rebasing rewrites the approved commits and can dismiss the approving review. \
        Do not start new work or change what the PR does; updating the branch is expected, and report any merge-conflict resolution you had to perform. \
        Then wait for the required checks on the pushed commit: resolve the workflow run for that commit (`gh run list --commit <sha> --json databaseId`) and poll that run's jobs (`gh run view <run-id> --json status,conclusion,jobs`) until every job completes — do not use `gh pr checks --watch`, which aggregates superseded runs and can report stale pending entries. \
        Then squash-merge it (`gh pr merge{pr_argument} --squash`). \
        If a required check genuinely fails on the code, or the merge stays blocked for any other reason, stop and report why instead of merging. \
        Report the merge result. The Circuit has finished and will not send further prompts to this session."
    )
}

/// The hand-off text shipped before the branch-update and merge-check steps.
/// Startup upgrades replace only this exact stock text.
pub(crate) fn legacy_merge_approved_pr(subject: &str, pr_argument: &str) -> String {
    format!(
        "The independent reviewer approved {subject}. Squash and merge it now: if it is still a draft, mark it ready for review (`gh pr ready{pr_argument}`); wait for its required checks to pass (`gh pr checks{pr_argument} --watch`); then squash-merge it (`gh pr merge{pr_argument} --squash`). Do not make further changes. If a required check fails or the merge is blocked, stop and report why instead of merging. Report the merge result. The Circuit has finished and will not send further prompts to this session."
    )
}

/// The pre-branch-sync merge hand-off, kept verbatim so the stock-text
/// startup upgrade can recognize stored graphs that still carry it.
/// New graphs use [`merge_approved_pr`]; user-edited prompts never equal
/// this text and are left intact.
pub(crate) const LEGACY_MERGE_TEMPLATE: &str = "The independent reviewer approved {subject}. Squash and merge it now: if it is still a draft, mark it ready for review (`gh pr ready{pr_argument}`); wait for its required checks to pass (`gh pr checks{pr_argument} --watch`); then squash-merge it (`gh pr merge{pr_argument} --squash`). Do not make further changes. If a required check fails or the merge is blocked, stop and report why instead of merging. Report the merge result. The Circuit has finished and will not send further prompts to this session.";

/// Render [`LEGACY_MERGE_TEMPLATE`] for one of the two stock merge nodes:
/// the local-review loop (`"your pull request for this work"`, `""`) or
/// the issue-review blueprint (`"PR #{{pr.number}} ({{pr.url}})"`,
/// `" {{pr.number}}"`).
pub(crate) fn legacy_merge_prompt(subject: &str, pr_argument: &str) -> String {
    LEGACY_MERGE_TEMPLATE
        .replace("{subject}", subject)
        .replace("{pr_argument}", pr_argument)
}

/// The reviewer stays open between rounds and re-reviews the published
/// branch, so every fix round must reach the remote.
pub(crate) const LOCAL_FEEDBACK_SCOPE: &str = "An independent reviewer requested changes to your work. Commit and push your fixes to update your pull request.";
pub(crate) const PR_FEEDBACK_SCOPE: &str = "Follow the feedback comments on PR #{{pr.number}} ({{pr.url}}). Commit and push your fixes to update the PR.";

/// Feedback scopes shipped before review rounds required the fixes to be
/// pushed. Startup upgrades replace only these exact stock texts.
pub(crate) const LEGACY_LOCAL_FEEDBACK_SCOPE: &str =
    "An independent reviewer requested changes to your work.";
pub(crate) const LEGACY_PR_FEEDBACK_SCOPE: &str =
    "Follow the feedback comments on PR #{{pr.number}} ({{pr.url}}) and update the PR.";

pub(crate) const LEGACY_LOCAL_REVIEW_PROMPT: &str = "Review the work of agent {{source.agent_id}} in {{source.path}}. Read that directory directly: review committed changes from the merge-base with {{source.base_ref}} and all uncommitted/untracked changes. The source task is {{source.name}}. Its latest report is: {{source.output}}\nInspect the code and relevant project instructions. Do not modify files, commit, push, post comments, or open a PR. Report actionable findings with file locations in your final response. If the work is satisfactory, explicitly state that you approve and have no remaining findings. Otherwise explicitly state that changes are requested. If you cannot assess the work, explain the blocker. This is review round {{retry.attempt}} of {{retry.max_retries}}.";

pub(crate) const LEGACY_FEEDBACK_PROMPT: &str = "An independent reviewer requested changes to your work. Review report:\n{{node.reviewer.output}}\nAddress every valid finding, run relevant checks, and report your changes. Explain any finding you disagree with. Another independent review will follow. Do not start another review loop yourself.";

pub(crate) const LEGACY_PR_REVIEW_PROMPT: &str = "review PR {{pr.number}} as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture. Add the review comments to the PR as a comment. The pull request URL is {{pr.url}}.";

pub(crate) const LEGACY_PR_REVIEW_WITH_VERDICT: &str = "review PR {{pr.number}} as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture. Add the review comments to the PR as a comment. The pull request URL is {{pr.url}}. State the reviewed commit and an explicit final verdict: approve only if there are no remaining actionable findings; otherwise request changes or explain what blocks review. Review completion alone is not approval. Re-check previous findings against the current code. Separate blocking correctness, specification and verification findings from optional style suggestions; do not turn optional preferences or unrelated redesigns into blockers.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_prompt_authorizes_branch_sync_instead_of_stopping_when_outdated() {
        let prompt = merge_approved_pr("PR #1 (http://example/pr/1)", " 1");
        assert!(
            prompt.contains("behind"),
            "merge prompt must name the behind-base case, was: {prompt:?}"
        );
        assert!(
            prompt.contains("update-branch"),
            "merge prompt must authorize bringing the branch up to date, was: {prompt:?}"
        );
        assert!(
            prompt.contains("mergeStateStatus"),
            "merge prompt must gate the sync on the machine-readable merge state, was: {prompt:?}"
        );
        assert!(
            prompt.contains("prefer update-branch over rebasing"),
            "merge prompt must steer away from history rewrites that dismiss approval, was: {prompt:?}"
        );
        assert!(
            prompt.contains("gh run list --commit"),
            "merge prompt must wait on the pushed commit's own workflow run, was: {prompt:?}"
        );
        assert!(
            prompt.contains("genuinely fails on the code"),
            "merge prompt must keep the stop condition for real check failures, was: {prompt:?}"
        );
        assert!(
            prompt.contains("report any merge-conflict resolution"),
            "merge prompt must require reporting manual conflict resolution, was: {prompt:?}"
        );
        assert!(
            !prompt.contains("Do not make further changes"),
            "blanket no-changes ban stops agents from updating an outdated branch, was: {prompt:?}"
        );
    }

    #[test]
    fn legacy_merge_template_renders_both_stock_merge_nodes() {
        assert_eq!(
            legacy_merge_prompt("your pull request for this work", ""),
            "The independent reviewer approved your pull request for this work. Squash and merge it now: if it is still a draft, mark it ready for review (`gh pr ready`); wait for its required checks to pass (`gh pr checks --watch`); then squash-merge it (`gh pr merge --squash`). Do not make further changes. If a required check fails or the merge is blocked, stop and report why instead of merging. Report the merge result. The Circuit has finished and will not send further prompts to this session."
        );
        let issue_prompt = legacy_merge_prompt("PR #{{pr.number}} ({{pr.url}})", " {{pr.number}}");
        assert!(!issue_prompt.contains("gh pr update-branch"));
        assert!(issue_prompt.contains("Do not make further changes"));
        assert!(issue_prompt.contains("gh pr merge {{pr.number}} --squash"));
    }
}
