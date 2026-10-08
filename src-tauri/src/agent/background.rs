//! Adapter-owned one-shot inference, shared by naming and circuit classifiers.

use std::path::{Path, PathBuf};

use super::capabilities::{
    BackgroundInferenceCapability, BackgroundPromptInput, BackgroundResultOutput,
};
use super::provider::{Platform, SpawnRecipe};
use crate::models::{EnvType, Provider};
use crate::preferences::{launch_configurations::ResolvedLaunchPlan, AppPreferences};

/// A recipe advertises its capability together with the invocation that fulfills it.
pub struct BackgroundRecipe {
    pub spawn: SpawnRecipe,
    pub capability: BackgroundInferenceCapability,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
}

impl BackgroundRecipe {
    pub fn new(
        spawn: SpawnRecipe,
        prompt_input: BackgroundPromptInput,
        result_output: BackgroundResultOutput,
    ) -> Self {
        let max_prompt_bytes =
            matches!(prompt_input, BackgroundPromptInput::Argument { .. }).then_some(16_000);
        Self {
            spawn,
            capability: BackgroundInferenceCapability {
                prompt_input,
                result_output,
                supports_provider_routing: false,
                max_prompt_bytes,
            },
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }
}

pub(crate) struct BackgroundLaunch {
    pub env: Vec<(String, String)>,
    pub args: Vec<String>,
    executable: Option<PathBuf>,
    recipe: BackgroundRecipe,
}

/// Own descendants as well as the CLI across timeout, cancellation, and exit.
pub(crate) struct BackgroundProcessGuard {
    pid: u32,
    job: Option<crate::process_util::JobHandle>,
}

impl BackgroundProcessGuard {
    pub(crate) fn new(pid: u32) -> Self {
        Self {
            pid,
            job: crate::process_util::JobHandle::contain(pid),
        }
    }

    pub(crate) fn terminate(&self) {
        if let Some(job) = &self.job {
            job.terminate();
        }
        #[cfg(windows)]
        if self.job.is_none() {
            crate::process_util::kill_process_tree(self.pid);
        }
        #[cfg(unix)]
        {
            let group = format!("-{}", self.pid);
            let _ = crate::process_util::command_no_window("kill")
                .args(["-KILL", "--", &group])
                .status();
        }
    }
}

impl Drop for BackgroundProcessGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub(crate) fn resolve_plan(
    plan: ResolvedLaunchPlan,
    prefs: &AppPreferences,
) -> Result<BackgroundLaunch, String> {
    if matches!(
        plan.harness.runtime,
        Some(EnvType::Wsl | EnvType::WindowsInterop)
    ) {
        return Err("Background inference requires a host-native Launch Configuration".into());
    }
    let adapter = Provider::try_from_db_str(&plan.harness.harness)
        .ok_or_else(|| {
            format!(
                "Unknown background inference harness: {:?}",
                plan.harness.harness
            )
        })?
        .adapter();
    let recipe = adapter.background_recipe(Platform::current()).ok_or_else(|| {
        format!("{} does not support one-shot background inference (prompt input, final answer, and non-interactive exit)", plan.harness.name)
    })?;
    if plan
        .extra_args
        .as_deref()
        .is_some_and(|args| !args.trim().is_empty())
    {
        return Err("Background inference uses the saved model and effort settings; remove extra CLI arguments".into());
    }
    let env = if let Some(route) = &plan.route {
        if !recipe.capability.supports_provider_routing {
            return Err(format!("{} background inference requires native authentication; provider routing is unavailable", plan.harness.name));
        }
        let account = prefs
            .provider_accounts
            .iter()
            .find(|account| account.id == route.provider_id)
            .ok_or("Provider account is missing")?;
        crate::preferences::compatibility::surface_env(
            route.surface,
            route.base_url.as_deref(),
            account.api_key.as_deref(),
            &route.model_tiers,
        )
    } else {
        Vec::new()
    };
    let mut args = Vec::new();
    if let Some(model) = &plan.model {
        args.extend(adapter.model_args(model));
    }
    if let Some(effort) = &plan.effort {
        args.extend(adapter.effort_args(effort));
    }
    Ok(BackgroundLaunch {
        env,
        args,
        executable: plan.harness.executable,
        recipe,
    })
}

impl BackgroundLaunch {
    pub(crate) fn command(
        &self,
        directory: &Path,
        result: &Path,
        prompt: &str,
    ) -> Result<std::process::Command, String> {
        self.command_in(
            directory,
            result,
            prompt,
            &crate::session_naming::ClaudeSearch::from_process_env(),
        )
    }

    /// `command` with the Claude lookup inputs supplied, so a test can model a
    /// stale `PATH` without rewriting the process-wide one (issue #2109).
    pub(crate) fn command_in(
        &self,
        directory: &Path,
        result: &Path,
        prompt: &str,
        claude: &crate::session_naming::ClaudeSearch,
    ) -> Result<std::process::Command, String> {
        if self
            .recipe
            .capability
            .max_prompt_bytes
            .is_some_and(|limit| prompt.len() > limit as usize)
        {
            return Err("Background prompt exceeds the argument transport limit of 16000 bytes; select a stdin or file-backed harness".into());
        }
        let mut recipe = self.recipe.spawn.clone();
        recipe.base_args.extend(self.args.clone());
        if self.recipe.capability.result_output == BackgroundResultOutput::LastMessageFile {
            recipe.base_args.extend([
                "--output-last-message".into(),
                result.to_string_lossy().into_owned(),
            ]);
        }
        match &self.recipe.capability.prompt_input {
            BackgroundPromptInput::Stdin => {}
            BackgroundPromptInput::Argument { flag } => {
                recipe.trailing_args.extend([flag.clone(), prompt.into()]);
            }
            BackgroundPromptInput::File { flag } => {
                let input = directory.join("prompt.txt");
                std::fs::write(&input, prompt)
                    .map_err(|e| format!("failed to write background prompt: {e}"))?;
                recipe
                    .trailing_args
                    .extend([flag.clone(), input.to_string_lossy().into_owned()]);
            }
        }
        let executable = match &self.executable {
            Some(path) => path.clone(),
            None if matches!(recipe.binary, "claude" | "claude.exe") => crate::session_naming::resolve_claude_binary_in(claude)?,
            None => crate::agent::detection::resolve_spawn_binary(recipe.binary)
                .or_else(|| which::which(recipe.binary).ok())
                .ok_or_else(|| format!("{} binary not found; install the selected harness or set its executable in Settings", recipe.binary))?,
        };
        let mut cmd = super::spawn_environment::background_command(&recipe, Some(&executable));
        cmd.current_dir(directory);
        // Background inference must not signal turns on the calling agent node.
        for key in [
            "BUILDMESH_SESSION_ID",
            "BUILDMESH_PORT",
            "BUILDMESH_HOOK_TOKEN",
        ] {
            cmd.env_remove(key);
        }
        for key in &self.recipe.env_remove {
            cmd.env_remove(key);
        }
        for (key, value) in self.recipe.env.iter().chain(&self.env) {
            cmd.env(key, value);
        }
        Ok(cmd)
    }

    pub(crate) fn stdin_prompt<'a>(&self, prompt: &'a str) -> &'a str {
        match self.recipe.capability.prompt_input {
            BackgroundPromptInput::Stdin => prompt,
            _ => "",
        }
    }

    pub(crate) fn final_output(&self, stdout: String, result: &Path) -> Result<String, String> {
        use std::io::Read;
        let output = match self.recipe.capability.result_output {
            BackgroundResultOutput::Stdout => stdout,
            BackgroundResultOutput::LastMessageFile => {
                let file = std::fs::File::open(result)
                    .map_err(|e| format!("Background final message is unavailable: {e}"))?;
                let mut output = String::new();
                file.take(4097)
                    .read_to_string(&mut output)
                    .map_err(|e| e.to_string())?;
                output
            }
            format => decode_json_lines(&stdout, format)?,
        };
        if output.len() > 4096 {
            return Err("Background final message exceeded 4096 bytes".into());
        }
        if output.trim().is_empty() {
            return Err("Background inference returned no final answer".into());
        }
        Ok(output)
    }
}

fn decode_json_lines(stdout: &str, format: BackgroundResultOutput) -> Result<String, String> {
    let mut answer = None;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let event: serde_json::Value = serde_json::from_str(line)
            .map_err(|_| "Background inference returned malformed JSON output")?;
        if event.get("type").and_then(|v| v.as_str()) == Some("error")
            || event.get("is_error").and_then(|v| v.as_bool()) == Some(true)
        {
            return Err("Background inference reported an error".into());
        }
        let text = match format {
            BackgroundResultOutput::OpenCodeJsonLines if event["type"] == "text" => {
                event["part"]["text"].as_str().map(str::to_owned)
            }
            BackgroundResultOutput::ResultJsonLines if event["type"] == "result" => {
                event["result"].as_str().map(str::to_owned)
            }
            BackgroundResultOutput::AssistantJsonLines
                if event["role"] == "assistant"
                    && event
                        .get("tool_calls")
                        .is_none_or(|v| v.is_null() || v.as_array().is_some_and(Vec::is_empty)) =>
            {
                match &event["content"] {
                    serde_json::Value::String(text) => Some(text.clone()),
                    serde_json::Value::Array(parts) => Some(
                        parts
                            .iter()
                            .filter(|part| part["type"] == "text")
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(text) = text {
            answer = Some(text);
        }
    }
    answer.ok_or_else(|| "Background inference returned no final answer".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::launch_configurations::{capture, LaunchOverrides};

    fn plan(harness: &str) -> ResolvedLaunchPlan {
        capture(
            &AppPreferences::default(),
            harness,
            &LaunchOverrides::default(),
        )
        .unwrap()
    }

    /// A stale-`PATH` Claude lookup (empty `PATH`, home and AppData under a temp
    /// directory) passed explicitly. Rewriting the process-wide `PATH` instead
    /// would make every concurrent test that spawns `git`/`node`/`powershell.exe`
    /// fail with "program not found" (issue #2109).
    #[cfg(windows)]
    fn with_stale_path(test: impl FnOnce(&Path, &Path, &crate::session_naming::ClaudeSearch)) {
        let directory = tempfile::Builder::new()
            .prefix("background inference ")
            .tempdir()
            .unwrap();
        let empty_path = directory.path().join("empty PATH");
        let appdata = directory.path().join("AppData");
        std::fs::create_dir_all(&empty_path).unwrap();
        std::fs::create_dir_all(&appdata).unwrap();
        let search = crate::session_naming::ClaudeSearch {
            path: Some(empty_path.into_os_string()),
            userprofile: Some(directory.path().to_string_lossy().into_owned()),
            appdata: Some(appdata.to_string_lossy().into_owned()),
        };
        test(directory.path(), &appdata, &search);
    }

    #[cfg(windows)]
    #[test]
    fn default_claude_launch_resolves_windows_native_install_with_stale_path() {
        with_stale_path(|directory, _, search| {
            let install = directory.join(".local/bin/claude.exe");
            std::fs::create_dir_all(install.parent().unwrap()).unwrap();
            std::fs::write(&install, b"resolution fixture").unwrap();
            let plan = plan("claude");
            assert!(plan.harness.executable.is_none());
            let launch = resolve_plan(plan, &AppPreferences::default()).unwrap();
            let command = launch
                .command_in(directory, &directory.join("result.txt"), "prompt", search)
                .unwrap();
            assert_eq!(Path::new(command.get_program()), install);
            assert!(command.get_args().any(|arg| arg == "--print"));
        });
    }

    #[cfg(windows)]
    #[test]
    fn default_claude_launch_executes_windows_npm_shim_with_stale_path() {
        with_stale_path(|directory, appdata, search| {
            let install = appdata.join("npm/claude.cmd");
            std::fs::create_dir_all(install.parent().unwrap()).unwrap();
            std::fs::write(&install, "@echo off\r\necho fix-background-naming\r\n").unwrap();
            let plan = plan("claude");
            assert!(plan.harness.executable.is_none());
            let launch = resolve_plan(plan, &AppPreferences::default()).unwrap();
            let mut command = launch
                .command_in(directory, &directory.join("result.txt"), "prompt", search)
                .unwrap();
            assert_eq!(Path::new(command.get_program()), install);
            let output = command.output().unwrap();
            assert!(output.status.success(), "{:?}", output);
            assert_eq!(
                String::from_utf8(output.stdout).unwrap().trim(),
                "fix-background-naming"
            );
        });
    }

    #[cfg(windows)]
    #[test]
    fn explicit_claude_executable_takes_precedence_over_windows_install_fallback() {
        with_stale_path(|directory, appdata, search| {
            let install = appdata.join("npm/claude.cmd");
            std::fs::create_dir_all(install.parent().unwrap()).unwrap();
            std::fs::write(&install, "@echo off\r\necho wrong-fallback\r\n").unwrap();
            let executable = directory.join("custom claude.cmd");
            std::fs::write(&executable, "@echo off\r\necho explicit-executable\r\n").unwrap();
            let mut plan = plan("claude");
            plan.harness.executable = Some(executable.clone());
            let launch = resolve_plan(plan, &AppPreferences::default()).unwrap();
            let mut command = launch
                .command_in(directory, &directory.join("result.txt"), "prompt", search)
                .unwrap();
            assert_eq!(command.get_program(), executable.as_os_str());
            let output = command.output().unwrap();
            assert!(output.status.success(), "{:?}", output);
            assert_eq!(
                String::from_utf8(output.stdout).unwrap().trim(),
                "explicit-executable"
            );
        });
    }

    #[test]
    fn unknown_background_harness_is_rejected_instead_of_using_claude() {
        for harness in ["codxe", "", "unknown-harness"] {
            let mut plan = plan("codex");
            plan.harness.harness = harness.into();
            let error = resolve_plan(plan, &AppPreferences::default())
                .err()
                .expect("unknown harness must fail resolution");
            assert!(
                error.contains("Unknown background inference harness"),
                "{error}"
            );
        }
    }

    #[test]
    fn background_resolution_preserves_known_harness_aliases() {
        for harness in [
            " Anthropic ",
            "Claude",
            "miniMax-code",
            "command-code",
            "cmdc",
            "cmd",
        ] {
            let mut plan = plan("codex");
            plan.harness.harness = harness.into();
            assert!(
                resolve_plan(plan, &AppPreferences::default()).is_ok(),
                "{harness}"
            );
        }
    }

    #[test]
    fn background_capability_and_recipe_are_one_contract_on_all_platforms() {
        for provider in Provider::all() {
            let adapter = provider.adapter();
            for platform in [Platform::Windows, Platform::Macos, Platform::Linux] {
                let recipe = adapter.background_recipe(platform);
                assert_eq!(
                    adapter.capabilities().background_inference,
                    recipe.as_ref().map(|r| r.capability.clone()),
                    "{provider:?} {platform:?}"
                );
            }
        }
        for harness in [
            "claude",
            "codex",
            "opencode",
            "kimi",
            "grok",
            "agy",
            "commandcode",
            "mcode",
        ] {
            assert!(
                resolve_plan(plan(harness), &AppPreferences::default()).is_ok(),
                "{harness}"
            );
        }
    }

    #[test]
    fn missing_capability_and_non_native_runtime_report_the_requirement() {
        for harness in ["terminal", "freebuff", "cline", "cursor", "muse", "dsh"] {
            let error = resolve_plan(plan(harness), &AppPreferences::default())
                .err()
                .unwrap();
            assert!(
                error.contains("one-shot background inference"),
                "{harness}: {error}"
            );
        }
        for runtime in [EnvType::Wsl, EnvType::WindowsInterop] {
            let mut plan = plan("codex");
            plan.harness.runtime = Some(runtime);
            let error = resolve_plan(plan, &AppPreferences::default())
                .err()
                .unwrap();
            assert!(error.contains("host-native"));
        }
    }

    #[test]
    fn dropping_background_ownership_closes_descendant_pipes() {
        use std::io::{BufRead, Read};
        // Sibling tests swap PATH for an empty directory under this lock.
        // `powershell.exe` is resolved from the process environment, so a
        // parallel spawn otherwise fails with "program not found" or exits
        // before the child line is written.
        let _env = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let directory = tempfile::tempdir().unwrap();
        let mut command = if cfg!(windows) {
            let parent_script = directory.path().join("parent.ps1");
            let child_script = directory.path().join("child.ps1");
            std::fs::write(&parent_script, "param($ChildScript)\n[Console]::Out.WriteLine('ready')\n& powershell.exe -NoProfile -File $ChildScript\n").unwrap();
            std::fs::write(
                &child_script,
                "[Console]::Out.WriteLine('child-ready')\nStart-Sleep -Seconds 60\n",
            )
            .unwrap();
            let mut command = crate::process_util::command_no_window("powershell.exe");
            // The helper inherits stdout. Killing only the parent leaves the
            // pipe open, so EOF is evidence that descendants were terminated.
            command
                .args(["-NoProfile", "-File"])
                .arg(parent_script)
                .arg(child_script);
            command
        } else {
            let mut command = crate::process_util::command_no_window("sh");
            command.args([
                "-c",
                "echo ready; sh -c 'echo child-ready; sleep 60' & wait",
            ]);
            command
        };
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command.stdout(std::process::Stdio::piped());
        let mut child = command.spawn().unwrap();
        let guard = BackgroundProcessGuard::new(child.id());
        let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ready");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "child-ready");
        let (tx, rx) = std::sync::mpsc::channel();
        let drain = std::thread::spawn(move || {
            let mut remaining = String::new();
            tx.send(reader.read_to_string(&mut remaining)).unwrap();
        });
        drop(guard);
        let closed = rx.recv_timeout(std::time::Duration::from_secs(5));
        if closed.is_err() {
            crate::process_util::kill_process_tree(child.id());
            let _ = child.kill();
        }
        child.wait().unwrap();
        closed
            .expect("descendants must not retain the output pipe after cancellation")
            .unwrap();
        drain.join().unwrap();
    }

    #[test]
    fn background_launch_rejects_routing_it_cannot_honor_and_extra_arguments() {
        let mut routed = plan("codex");
        routed.route = Some(crate::preferences::ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "custom".into(),
            surface: crate::preferences::ApiSurface::OpenAI,
            base_url: Some("https://example.test/v1".into()),
            model_tiers: Default::default(),
        });
        let error = resolve_plan(routed, &AppPreferences::default())
            .err()
            .unwrap();
        assert!(error.contains("native authentication"));
        let mut configured = plan("claude");
        configured.extra_args = Some("--output-format stream-json".into());
        let error = resolve_plan(configured, &AppPreferences::default())
            .err()
            .unwrap();
        assert!(error.contains("remove extra CLI arguments"));
    }

    #[test]
    fn json_output_extracts_final_answers_without_progress_reasoning_or_tool_results() {
        assert_eq!(decode_json_lines(
            "{\"type\":\"step_start\"}\n{\"type\":\"text\",\"part\":{\"text\":\"working\"}}\n{\"type\":\"reasoning\",\"part\":{\"text\":\"ignore this\"}}\n{\"type\":\"text\",\"part\":{\"text\":\"fix-background-naming\"}}\n{\"type\":\"step_finish\"}",
            BackgroundResultOutput::OpenCodeJsonLines).unwrap(), "fix-background-naming");
        assert_eq!(decode_json_lines(
            "{\"role\":\"assistant\",\"content\":\"working\",\"tool_calls\":[{}]}\n{\"role\":\"tool\",\"content\":\"wrong-answer\"}\n{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"ignore\"},{\"type\":\"text\",\"text\":\"fix-background-naming\"}]}",
            BackgroundResultOutput::AssistantJsonLines).unwrap(), "fix-background-naming");
        assert_eq!(decode_json_lines(
            "{\"type\":\"assistant\",\"message\":\"working\"}\n{\"type\":\"result\",\"result\":\"fix-background-naming\"}",
            BackgroundResultOutput::ResultJsonLines).unwrap(), "fix-background-naming");
    }

    #[test]
    fn json_output_rejects_malformed_errors_and_missing_answers() {
        for format in [
            BackgroundResultOutput::OpenCodeJsonLines,
            BackgroundResultOutput::AssistantJsonLines,
            BackgroundResultOutput::ResultJsonLines,
        ] {
            for stdout in [
                "not json",
                "{\"type\":\"error\"}",
                "{\"type\":\"result\",\"is_error\":true,\"result\":\"wrong-answer\"}",
                "{\"type\":\"step_start\"}",
            ] {
                assert!(
                    decode_json_lines(stdout, format).is_err(),
                    "{format:?}: {stdout}"
                );
            }
        }
    }

    #[test]
    fn argument_transports_reject_large_reports_before_spawning() {
        let directory = tempfile::tempdir().unwrap();
        let result = directory.path().join("result.txt");
        for harness in ["kimi", "agy"] {
            let launch = resolve_plan(plan(harness), &AppPreferences::default()).unwrap();
            let error = launch
                .command(directory.path(), &result, &"x".repeat(16_001))
                .unwrap_err();
            assert!(
                error.contains("argument transport limit of 16000 bytes"),
                "{harness}: {error}"
            );
            assert_eq!(launch.recipe.capability.max_prompt_bytes, Some(16_000));
        }
    }

    #[test]
    fn prompt_file_transport_preserves_long_multiline_reports() {
        let directory = tempfile::tempdir().unwrap();
        let result = directory.path().join("result.txt");
        let mut plan = plan("grok");
        plan.harness.executable = Some(std::env::current_exe().unwrap());
        let launch = resolve_plan(plan, &AppPreferences::default()).unwrap();
        let prompt = "quotes ' \" $HOME `literal`\n".repeat(1000);
        let command = launch.command(directory.path(), &result, &prompt).unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            std::fs::read_to_string(directory.path().join("prompt.txt")).unwrap(),
            prompt
        );
        assert!(args.contains(&"--prompt-file".into()));
        assert!(!args.contains(&prompt));
        assert_eq!(launch.stdin_prompt(&prompt), "");
    }

    #[test]
    fn final_message_file_never_falls_back_to_stdout_and_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let result = directory.path().join("result.txt");
        let launch = resolve_plan(plan("codex"), &AppPreferences::default()).unwrap();
        assert!(launch.final_output("wrong-answer".into(), &result).is_err());
        std::fs::write(&result, "fix-background-naming").unwrap();
        assert_eq!(
            launch.final_output("wrong-answer".into(), &result).unwrap(),
            "fix-background-naming"
        );
        std::fs::write(&result, "x".repeat(4097)).unwrap();
        assert!(launch
            .final_output("wrong-answer".into(), &result)
            .unwrap_err()
            .contains("4096"));
        std::fs::write(&result, " \n").unwrap();
        assert!(launch.final_output("wrong-answer".into(), &result).is_err());
    }
}
