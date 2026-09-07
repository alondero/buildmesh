//! `/finish` wrap-up prompt (issue #484, ADR-0011).
//!
//! The wrap-up instructions live in a user-editable system configuration
//! file — `<app-data>/autopilot/finish.md` — mirroring the user's own
//! `finish.md` slash-command recipe. On first use the built-in default
//! template is written there so the user has a concrete file to customize
//! (ADR-0011: "customized per project/mesh or overridden by the user
//! without changing the compiled Rust application").
//!
//! Templating is deliberately tiny — two placeholders:
//! - `{{ISSUE_REF}}`   → `Closes #<n>` (or empty for manual finishes with no
//!   originating issue), so the PR auto-links and auto-closes the ticket.
//! - `{{PR_STEP}}`     → the PR-creation instruction derived from the mesh's
//!   `autopilot_action_on_success` policy (`draft_pr` / `pr` / `none`).

use std::path::PathBuf;

/// Built-in wrap-up template, used verbatim when the user hasn't customized
/// `finish.md` (and seeded to disk so they can).
pub const DEFAULT_FINISH_TEMPLATE: &str = "\
The implementation work for this task is considered complete. Wrap up the session now:

1. Run the project's build, lint, and full test suites. Fix any failures — the bar is green, even for pre-existing breakage you can reasonably fix.
2. Critically review your own diff (correctness, clarity, tests) and tidy what you find.
3. Stage and commit all your changes with a clear, conventional commit message.
4. Push the branch to origin: `git push -u origin HEAD`.
{{PR_STEP}}

Report exactly what passed and what you did. Do not stop while verification is red unless you are blocked on something only a human can resolve.
";

/// Path of the user-customizable wrap-up template.
fn finish_md_path() -> Option<PathBuf> {
    crate::preferences::app_data_dir().map(|d| d.join("autopilot").join("finish.md"))
}

/// Load the wrap-up template: the user's `finish.md` if present, else the
/// built-in default (which is also seeded to disk, best-effort, so the user
/// discovers the customization point).
pub fn load_template() -> String {
    let Some(path) = finish_md_path() else {
        return DEFAULT_FINISH_TEMPLATE.to_string();
    };
    match std::fs::read_to_string(&path) {
        Ok(content) if !content.trim().is_empty() => content,
        _ => {
            if !path.exists() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&path, DEFAULT_FINISH_TEMPLATE) {
                    tracing::debug!("autopilot: could not seed finish.md: {}", e);
                } else {
                    tracing::info!("autopilot: seeded default finish.md at {:?}", path);
                }
            }
            DEFAULT_FINISH_TEMPLATE.to_string()
        }
    }
}

/// Render a template into the concrete prompt for one node. Pure — the
/// impure `load_template` is separate so tests pin the substitution rules
/// without touching disk.
pub(crate) fn render(template: &str, issue_number: Option<i64>, action_on_success: &str) -> String {
    let issue_ref = match issue_number {
        Some(n) => format!("Closes #{}", n),
        None => String::new(),
    };
    let pr_step = match action_on_success {
        "none" => {
            "5. Do NOT open a pull request — pushing the branch is the final step.".to_string()
        }
        action => {
            let draft_flag = if action == "pr" { "" } else { " --draft" };
            let link = if issue_ref.is_empty() {
                String::new()
            } else {
                format!(
                    " Include \"{}\" in the PR body so the ticket auto-links.",
                    issue_ref
                )
            };
            format!(
                "5. Open a pull request: `gh pr create --fill{}`.{}",
                draft_flag, link
            )
        }
    };
    template
        .replace("{{PR_STEP}}", &pr_step)
        .replace("{{ISSUE_REF}}", &issue_ref)
}

/// The full wrap-up prompt for a node: user template + policy substitution.
pub fn finish_prompt(issue_number: Option<i64>, action_on_success: Option<&str>) -> String {
    render(
        &load_template(),
        issue_number,
        action_on_success.unwrap_or("draft_pr"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_renders_draft_pr_with_issue_link() {
        let p = render(DEFAULT_FINISH_TEMPLATE, Some(123), "draft_pr");
        assert!(p.contains("gh pr create --fill --draft"));
        assert!(p.contains("Closes #123"));
        assert!(!p.contains("{{"), "no unexpanded placeholders survive");
    }

    #[test]
    fn pr_action_drops_the_draft_flag() {
        let p = render(DEFAULT_FINISH_TEMPLATE, Some(7), "pr");
        assert!(p.contains("gh pr create --fill`"));
        assert!(!p.contains("--draft"));
    }

    #[test]
    fn none_action_forbids_the_pr() {
        let p = render(DEFAULT_FINISH_TEMPLATE, Some(7), "none");
        assert!(p.contains("Do NOT open a pull request"));
        assert!(!p.contains("gh pr create"));
    }

    #[test]
    fn manual_finish_without_issue_omits_the_closes_line() {
        let p = render(DEFAULT_FINISH_TEMPLATE, None, "draft_pr");
        assert!(!p.contains("Closes #"));
        assert!(p.contains("gh pr create --fill --draft"));
    }

    #[test]
    fn custom_template_placeholders_are_substituted() {
        let p = render("Ship it. {{PR_STEP}} ({{ISSUE_REF}})", Some(9), "draft_pr");
        assert!(p.starts_with("Ship it. 5. Open a pull request"));
        assert!(p.ends_with("(Closes #9)"));
    }
}
