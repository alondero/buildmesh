//! Circuit-owned wrap-up template, captured when a run is created.
//! Authored prompts choose publication behavior; issue-review defaults to a draft PR.

use std::path::PathBuf;

/// Built-in wrap-up template, used verbatim when the user hasn't customized
/// `finish.md` (and seeded to disk so they can).
pub const DEFAULT_FINISH_TEMPLATE: &str = "\
The implementation work for this task is considered complete. Wrap up the session now:

1. Run the verification command your project's agent instructions name (AGENTS.md or CLAUDE.md, for example `npm run verify`); only when none is named, run the project's build, lint, and tests. Fix every failure your change causes, rerunning just the failing test or gate while you fix it and the full command once at the end. A failure your change did not cause is outside this task: confirm it by rerunning that test on its own (a flaky test passes) or reproducing it at the base commit, then report it with that evidence instead of fixing it.
2. Critically review your own diff (correctness, clarity, tests) and tidy what you find.
3. Stage and commit all your changes with a clear, conventional commit message.
4. Push the branch to origin: `git push -u origin HEAD`.
{{PR_STEP}}

Report exactly what passed and what you did. Do not stop while a failure your change caused is red unless you are blocked on something only a human can resolve.
";

/// Earlier built-in defaults. `load_template` seeds the default to disk, so an
/// untouched seeded copy is indistinguishable from a deliberate customization
/// unless it is recognised here; a recognised copy follows the current default.
const SUPERSEDED_DEFAULT_TEMPLATES: &[&str] = &["\
The implementation work for this task is considered complete. Wrap up the session now:

1. Run the project's build, lint, and full test suites. Fix any failures — the bar is green, even for pre-existing breakage you can reasonably fix.
2. Critically review your own diff (correctness, clarity, tests) and tidy what you find.
3. Stage and commit all your changes with a clear, conventional commit message.
4. Push the branch to origin: `git push -u origin HEAD`.
{{PR_STEP}}

Report exactly what passed and what you did. Do not stop while verification is red unless you are blocked on something only a human can resolve.
"];

/// Path of the user-customizable wrap-up template.
fn finish_md_path() -> Option<PathBuf> {
    let dir = crate::preferences::app_data_dir()?;
    let path = dir.join("circuits").join("finish.md");
    if !path.exists() {
        let legacy = dir.join("autopilot").join("finish.md");
        if legacy.is_file() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let migration = (|| -> std::io::Result<()> {
                use std::io::Write;
                let content = std::fs::read(&legacy)?;
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                output.write_all(&content)
            })();
            if let Err(error) = migration {
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    tracing::warn!(%error, "could not migrate Circuit finish template");
                }
            }
        }
    }
    Some(path)
}

/// Load the wrap-up template: the user's `finish.md` if present, else the
/// built-in default (which is also seeded to disk, best-effort, so the user
/// discovers the customization point).
pub fn load_template() -> String {
    let Some(path) = finish_md_path() else {
        return DEFAULT_FINISH_TEMPLATE.to_string();
    };
    match std::fs::read_to_string(&path) {
        Ok(content) if is_superseded_default(&content) => {
            if let Err(e) = std::fs::write(&path, DEFAULT_FINISH_TEMPLATE) {
                tracing::debug!("circuits: could not refresh seeded finish.md: {}", e);
            }
            DEFAULT_FINISH_TEMPLATE.to_string()
        }
        Ok(content) if !content.trim().is_empty() => content,
        _ => {
            if !path.exists() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&path, DEFAULT_FINISH_TEMPLATE) {
                    tracing::debug!("circuits: could not seed finish.md: {}", e);
                } else {
                    tracing::info!("circuits: seeded default finish.md at {:?}", path);
                }
            }
            DEFAULT_FINISH_TEMPLATE.to_string()
        }
    }
}

/// Line endings are ignored: an editor may have re-saved the seeded copy as CRLF
/// without changing a word of it.
fn is_superseded_default(content: &str) -> bool {
    let content = content.replace("\r\n", "\n");
    SUPERSEDED_DEFAULT_TEMPLATES
        .iter()
        .any(|old| content.trim() == old.trim())
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
    fn custom_legacy_template_migrates_once_without_becoming_a_live_dependency() {
        let directory = tempfile::tempdir().unwrap();
        crate::preferences::storage::init_for_tests(directory.path().to_path_buf());
        let legacy = directory.path().join("autopilot");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join("finish.md"),
            "Custom Circuit wrap-up {{PR_STEP}}",
        )
        .unwrap();
        assert_eq!(load_template(), "Custom Circuit wrap-up {{PR_STEP}}");
        let current = directory.path().join("circuits/finish.md");
        assert_eq!(
            std::fs::read_to_string(&current).unwrap(),
            "Custom Circuit wrap-up {{PR_STEP}}"
        );
        std::fs::write(legacy.join("finish.md"), "Old setting edited later").unwrap();
        assert_eq!(load_template(), "Custom Circuit wrap-up {{PR_STEP}}");
        std::fs::write(&current, "New Circuit template").unwrap();
        assert_eq!(load_template(), "New Circuit template");
    }

    #[test]
    fn default_template_scopes_verification_to_the_projects_named_command() {
        // The 2026-10-07 retro: "full test suites ... even for pre-existing
        // breakage" sent implementers into hours of flake-chasing on top of
        // the project's own scope-selected verification.
        assert!(DEFAULT_FINISH_TEMPLATE
            .contains("verification command your project's agent instructions name"));
        assert!(
            DEFAULT_FINISH_TEMPLATE.contains("report it with that evidence instead of fixing it")
        );
        assert!(!DEFAULT_FINISH_TEMPLATE.contains("full test suites"));
        assert!(!DEFAULT_FINISH_TEMPLATE.contains("pre-existing breakage"));
    }

    #[test]
    fn an_unmodified_seeded_copy_of_a_superseded_default_follows_the_new_default() {
        let directory = tempfile::tempdir().unwrap();
        crate::preferences::storage::init_for_tests(directory.path().to_path_buf());
        let current = directory.path().join("circuits/finish.md");
        std::fs::create_dir_all(current.parent().unwrap()).unwrap();
        let seeded_with_crlf = SUPERSEDED_DEFAULT_TEMPLATES[0].replace('\n', "\r\n");
        std::fs::write(&current, seeded_with_crlf).unwrap();

        assert_eq!(load_template(), DEFAULT_FINISH_TEMPLATE);
        assert_eq!(
            std::fs::read_to_string(&current).unwrap(),
            DEFAULT_FINISH_TEMPLATE
        );
    }

    #[test]
    fn an_edited_copy_of_a_superseded_default_is_kept() {
        let directory = tempfile::tempdir().unwrap();
        crate::preferences::storage::init_for_tests(directory.path().to_path_buf());
        let current = directory.path().join("circuits/finish.md");
        std::fs::create_dir_all(current.parent().unwrap()).unwrap();
        let edited = format!(
            "{}\nAlso run the e2e suite.\n",
            SUPERSEDED_DEFAULT_TEMPLATES[0]
        );
        std::fs::write(&current, &edited).unwrap();

        assert_eq!(load_template(), edited);
        assert_eq!(std::fs::read_to_string(&current).unwrap(), edited);
    }

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
