//! Background classifiers use a saved launch selection independently of agent defaults.

use crate::models::{EnvType, Provider};

pub(crate) enum ClassifierLaunch {
    Claude(crate::session_naming::NamingLaunch),
    Codex(Box<crate::preferences::launch_configurations::ResolvedLaunchPlan>),
}

pub(crate) fn resolve(selection: &str) -> Result<ClassifierLaunch, String> {
    let prefs = crate::preferences::load()?;
    let plan = crate::preferences::launch_configurations::resolve(&prefs, selection, &Default::default())?;
    if matches!(plan.harness.runtime, Some(EnvType::Wsl | EnvType::WindowsInterop)) {
        return Err("Circuit classification requires a host-native Launch Configuration".into());
    }
    match Provider::from_db_str(&plan.harness.harness) {
        Provider::Anthropic => crate::session_naming::naming_backend_env(selection).map(ClassifierLaunch::Claude),
        Provider::Codex if plan.route.is_none() => {
            if plan.extra_args.as_deref().is_some_and(|args| !args.trim().is_empty()) {
                return Err("Codex classifiers support model and effort settings; remove extra CLI arguments".into());
            }
            Ok(ClassifierLaunch::Codex(Box::new(plan)))
        }
        _ => Err("Circuit classification requires Claude Code or native Codex".into()),
    }
}

impl ClassifierLaunch {
    pub(crate) fn command(&self, directory: &std::path::Path, result: &std::path::Path) -> Result<std::process::Command, String> {
        let mut cmd = match self {
            Self::Claude(launch) => {
                let executable = launch.executable.clone().map(Ok).unwrap_or_else(crate::session_naming::resolve_claude_binary)?;
                let mut cmd = crate::process_util::command_no_window(executable);
                cmd.arg("--print").args(&launch.args);
                for key in crate::agent::provider::CLAUDE_BACKEND_ENV_VARS { cmd.env_remove(key); }
                for (key, value) in &launch.env { cmd.env(key, value); }
                cmd
            }
            Self::Codex(plan) => {
                let adapter = Provider::Codex.adapter();
                let mut recipe = adapter.spawn_recipe(crate::agent::provider::Platform::current(), crate::env::current_env().into());
                // Keep the adapter's platform shell, but replace interactive TUI flags.
                recipe.base_args = vec!["--ask-for-approval".into(), "never".into(), "exec".into(),
                    "--ignore-user-config".into(), "--ephemeral".into(), "--skip-git-repo-check".into(),
                    "--sandbox".into(), "read-only".into(), "--color".into(), "never".into(),
                    "-c".into(), "features.shell_tool=false".into(), "-c".into(), "features.multi_agent=false".into()];
                if let Some(model) = &plan.model { recipe.base_args.extend(adapter.model_args(model)); }
                if let Some(effort) = &plan.effort { recipe.base_args.extend(adapter.effort_args(effort)); }
                recipe.base_args.extend(["--output-last-message".into(), result.to_string_lossy().into_owned()]);
                recipe.trailing_args = vec!["-".into()];
                let mut cmd = crate::agent::spawn_environment::background_command(&recipe, plan.harness.executable.as_deref());
                // Native Codex uses its existing login, never an inherited proxy credential.
                cmd.env_remove("OPENAI_API_KEY").env_remove("OPENAI_BASE_URL");
                cmd
            }
        };
        cmd.current_dir(directory);
        Ok(cmd)
    }

    pub(crate) fn final_output(&self, stdout: String, result: &std::path::Path) -> Result<String, String> {
        match self {
            Self::Claude(_) => Ok(stdout),
            Self::Codex(_) => {
                use std::io::Read;
                let file = std::fs::File::open(result).map_err(|e| format!("Codex final message is unavailable: {e}"))?;
                let mut output = String::new();
                file.take(4097).read_to_string(&mut output).map_err(|e| e.to_string())?;
                if output.len() > 4096 { return Err("Codex final message exceeded 4096 bytes".into()); }
                Ok(output)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let launch = ClassifierLaunch::Codex(Box::new(plan));
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
