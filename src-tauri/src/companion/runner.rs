//! Native subscription CLI inference, with tools executed only by Buildmesh.
use super::{tools, CompanionState, CompanionTask};
use crate::{
    agent::background::{BackgroundLaunch, BackgroundProcessGuard},
    blocking::run_blocking,
};
use serde::Deserialize;
use serde_json::json;
use std::{process::Stdio, time::Duration};
use tauri::AppHandle;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::watch,
};

#[derive(Deserialize)]
#[serde(untagged)]
enum Reply {
    Tool(tools::Tool),
    Final(FinalReply),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalReply {
    response: String,
}

fn isolate(command: &mut std::process::Command) {
    for key in crate::agent::provider::CLAUDE_BACKEND_ENV_VARS {
        command.env_remove(key);
    }
    for key in [
        "CLAUDECODE",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    ] {
        command.env_remove(key);
    }
    command.env("CLAUDE_CODE_SAFE_MODE", "1");
}

fn prepare() -> Result<(BackgroundLaunch, tempfile::TempDir, u32), String> {
    let prefs = crate::preferences::load()?;
    let selection = prefs
        .companion
        .launch_configuration
        .as_deref()
        .or(prefs.default_provider.as_deref())
        .unwrap_or("claude");
    let plan =
        crate::preferences::launch_configurations::resolve(&prefs, selection, &Default::default())?;
    if plan.harness.harness != "anthropic" || plan.route.is_some() {
        return Err("Companion currently requires a native Claude subscription Launch Configuration; API provider routes are not supported".into());
    }
    let launch = crate::agent::background::resolve_plan(plan, &prefs)?;
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    Ok((launch, directory, prefs.companion.spawn_limit.clamp(1, 10)))
}

async fn bounded_read(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(262_145)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() > 262_144 {
        return Err("Companion CLI output exceeded 256 KiB".into());
    }
    Ok(bytes)
}

async fn inference(
    command: std::process::Command,
    input: &str,
    cancel: &mut watch::Receiver<bool>,
) -> Result<String, String> {
    if *cancel.borrow() {
        return Err("Task cancelled".into());
    }
    let mut command: tokio::process::Command = command.into();
    command.kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start Companion CLI: {e}"))?;
    let guard = child.id().map(BackgroundProcessGuard::new);
    let mut stdin = child.stdin.take().ok_or("CLI stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("CLI stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("CLI stderr unavailable")?;
    let operation = async {
        let write = async {
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            stdin.shutdown().await.map_err(|e| e.to_string())?;
            drop(stdin);
            Ok::<(), String>(())
        };
        let (_, stdout, _stderr, status) =
            tokio::try_join!(write, bounded_read(stdout), bounded_read(stderr), async {
                child.wait().await.map_err(|e| e.to_string())
            })?;
        if !status.success() {
            return Err(format!(
                "Companion CLI exited with {status}; check your subscription login and CLI version"
            ));
        }
        String::from_utf8(stdout).map_err(|_| "CLI output was not UTF-8".into())
    };
    let result = tokio::select! {
        _ = cancel.changed() => Err("Task cancelled".into()),
        result = tokio::time::timeout(Duration::from_secs(180), operation) => result.map_err(|_| "Companion CLI timed out after 180 seconds")?,
    };
    drop(guard);
    result
}

pub async fn run(
    app: &AppHandle,
    state: &CompanionState,
    task: &CompanionTask,
    mut cancel: watch::Receiver<bool>,
) -> Result<String, String> {
    let (launch, directory, limit) = run_blocking("companion_prepare", prepare).await?;
    let launch = std::sync::Arc::new(launch);
    let directory = std::sync::Arc::new(directory);
    let make_command = |prompt: String| {
        let launch = launch.clone();
        let directory = directory.clone();
        async move {
            run_blocking("companion_command", move || {
                let mut command = launch.command(
                    directory.path(),
                    &directory.path().join("result.txt"),
                    &prompt,
                )?;
                isolate(&mut command);
                command.args([
                    "--safe-mode",
                    "--setting-sources",
                    "",
                    "--strict-mcp-config",
                    "--mcp-config",
                    "{\"mcpServers\":{}}",
                ]);
                Ok(command)
            })
            .await
        }
    };
    let sample = make_command(String::new()).await?;
    let mut auth = crate::process_util::command_no_window(sample.get_program());
    auth.current_dir(directory.path());
    isolate(&mut auth);
    auth.args([
        "--safe-mode",
        "--setting-sources",
        "",
        "auth",
        "status",
        "--json",
    ]);
    let auth = inference(auth, "", &mut cancel).await?;
    let auth: serde_json::Value =
        serde_json::from_str(&auth).map_err(|_| "Cannot verify subscription authentication")?;
    if auth["loggedIn"] != true
        || auth["authMethod"] != "claude.ai"
        || auth["apiProvider"] != "firstParty"
        || !matches!(
            auth["subscriptionType"].as_str(),
            Some("pro" | "max" | "team" | "enterprise")
        )
    {
        return Err("Companion requires a verified Claude subscription login; run claude auth login with your subscription account".into());
    }
    state.update(app, &task.id, "companion://step", |t| {
        t.steps.push("Verified subscription login".into())
    });
    let mut history = vec![json!({"role":"user", "content":task.prompt})];
    let mut used = 0;
    for index in 0..24 {
        if *cancel.borrow() {
            return Err("Task cancelled".into());
        }
        let prompt = format!("You are Buildmesh Companion, a single-intent fleet coordinator. Spawn budget for this task: {limit}.\n{}\nConversation JSON:\n{}", tools::CATALOG, serde_json::to_string(&history).map_err(|e| e.to_string())?);
        if prompt.len() > 512_000 {
            return Err("Task context limit reached; narrow the request".into());
        }
        let raw = inference(make_command(prompt.clone()).await?, &prompt, &mut cancel).await?;
        let reply: Reply = serde_json::from_str(raw.trim())
            .map_err(|_| "Companion returned an invalid tool response; please retry")?;
        match reply {
            Reply::Final(reply) => {
                if reply.response.trim().is_empty() {
                    return Err("Companion returned an empty answer".into());
                }
                return Ok(reply.response);
            }
            Reply::Tool(tool) => {
                let name = serde_json::from_str::<serde_json::Value>(&raw)
                    .ok()
                    .and_then(|v| v["tool"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                state.update(app, &task.id, "companion://step", |t| {
                    t.steps.push(format!("Running {}", name.replace('_', " ")))
                });
                let result = tools::execute(
                    tools::ToolContext {
                        app,
                        state,
                        id: &task.id,
                        limit,
                        cancel: &cancel,
                        index,
                    },
                    tool,
                    &mut used,
                )
                .await;
                state.update(app, &task.id, "companion://step", |t| {
                    t.steps.push(format!(
                        "{} {}",
                        if result.is_ok() {
                            "Completed"
                        } else {
                            "Failed"
                        },
                        name.replace('_', " ")
                    ))
                });
                history.push(json!({"role":"assistant", "content":raw}));
                history.push(json!({"role":"tool", "content": match result { Ok(value) => value, Err(error) => json!({"error":error}) }}));
            }
        }
    }
    Err("Companion reached its 24-step limit; completed actions are shown above".into())
}
