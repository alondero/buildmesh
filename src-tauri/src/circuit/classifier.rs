//! Background classifiers share the adapter-owned one-shot inference contract.

pub(crate) use crate::agent::background::BackgroundLaunch as ClassifierLaunch;

pub(crate) fn resolve(selection: &str) -> Result<ClassifierLaunch, String> {
    let prefs = crate::preferences::load()?;
    let plan = crate::preferences::launch_configurations::resolve(&prefs, selection, &Default::default())?;
    resolve_plan(plan, &prefs)
}

fn resolve_plan(
    plan: crate::preferences::launch_configurations::ResolvedLaunchPlan,
    prefs: &crate::preferences::AppPreferences,
) -> Result<ClassifierLaunch, String> {
    crate::agent::background::resolve_plan(plan, prefs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Provider;

    #[test]
    fn claude_classifier_uses_saved_model_and_effort_and_rejects_extra_args() {
        use crate::preferences::launch_configurations::{self, LaunchOverrides};

        let prefs = crate::preferences::AppPreferences::default();
        let model = "claude-sonnet-4-20250514";
        let effort = "low";
        let plan = launch_configurations::capture(
            &prefs,
            "claude",
            &LaunchOverrides {
                model: Some(model.into()),
                effort: Some(effort.into()),
                extra_args: None,
            },
        )
        .unwrap();
        let launch = resolve_plan(plan, &prefs).unwrap();
        let adapter = Provider::Anthropic.adapter();
        let mut expected = adapter.model_args(model);
        expected.extend(adapter.effort_args(effort));
        assert_eq!(launch.args, expected);

        let plan = launch_configurations::capture(
            &prefs,
            "claude",
            &LaunchOverrides {
                extra_args: Some("--dangerously-skip-permissions".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(matches!(
            resolve_plan(plan, &prefs),
            Err(error) if error.contains("remove extra CLI arguments")
        ));
    }

    #[test]
    fn circuit_classifier_codex_uses_saved_model_and_final_message() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join(if cfg!(windows) { "classifier.ps1" } else { "classifier.sh" });
        let args_file = directory.path().join("args.txt");
        if cfg!(windows) {
            let content = format!(
                "$inputText = [Console]::In.ReadToEnd()\n\
                 if ($inputText -ne 'classify this report') {{ exit 9 }}\n\
                 if ($env:OPENAI_API_KEY -or $env:OPENAI_BASE_URL) {{ exit 8 }}\n\
                 [IO.File]::WriteAllLines({}, [string[]]$args)\n\
                 $index = [Array]::IndexOf([object[]]$args, '--output-last-message')\n\
                 [IO.File]::WriteAllText($args[$index + 1], 'COMPLETED')\n\
                 [Console]::Out.WriteLine('BLOCKED')\nexit 0\n",
                crate::env::powershell_literal(&args_file.to_string_lossy()));
            std::fs::write(&script, content).unwrap();
        } else {
            let content = format!("#!/bin/sh\n[ \"$(cat)\" = 'classify this report' ] || exit 9\n[ -z \"$OPENAI_API_KEY$OPENAI_BASE_URL\" ] || exit 8\nprintf '%s\\n' \"$@\" > '{}'\nwhile [ \"$#\" -gt 0 ]; do\n if [ \"$1\" = '--output-last-message' ]; then shift; printf COMPLETED > \"$1\"; break; fi\n shift\ndone\nprintf BLOCKED\n", args_file.display());
            std::fs::write(&script, content).unwrap();
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let mut plan = crate::preferences::launch_configurations::capture(
            &crate::preferences::AppPreferences::default(), "codex",
            &crate::preferences::launch_configurations::LaunchOverrides {
                model: Some("gpt-6-luna".into()), effort: Some("low".into()), extra_args: None,
            }).unwrap();
        plan.harness.executable = Some(script);
        let launch = resolve_plan(plan, &crate::preferences::AppPreferences::default()).unwrap();
        let verdict = crate::circuit::evaluator::classify_with_prompt(1, &launch, "classify this report").unwrap();
        assert_eq!(verdict, crate::circuit::evaluator::Classification::Completed);
        let args = std::fs::read_to_string(args_file).unwrap();
        let args: Vec<_> = args.lines().collect();
        assert!(args.windows(2).any(|pair| pair == ["--model", "gpt-6-luna"]));
        assert!(args.contains(&"model_reasoning_effort=\"low\""));
        assert!(args.contains(&"--ignore-user-config"));
        assert!(args.windows(2).any(|pair| pair == ["--sandbox", "read-only"]));
        assert_eq!(args.last(), Some(&"-"));
    }
}
