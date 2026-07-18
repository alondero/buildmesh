use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;
use std::path::Path;

pub struct CodexAdapter;
pub static CODEX: CodexAdapter = CodexAdapter;

fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::PowerShell,
    }
}

fn base_flags() -> Vec<String> {
    vec![
        "--ask-for-approval".into(),
        "never".into(),
        "--sandbox".into(),
        "danger-full-access".into(),
        // Run the project-local `.codex/hooks.json` hooks without Codex's
        // interactive workspace-trust review (issue #884) — a headless spawn
        // must never block on a trust prompt, and Buildmesh never edits the
        // user's global ~/.codex/config.toml to trust the path instead.
        "--dangerously-bypass-hook-trust".into(),
    ]
}

/// The callback command Codex hooks run. Codex pipes the hook's stdin JSON
/// (`{hook_event_name, session_id, transcript_path, …}` — issue #884) into the
/// command; `--data-binary @-` forwards it as the POST body so
/// `http/routes/attention.rs` can classify the event. The port/session env
/// vars are set per-agent by `spawn_environment` and inherited by the hook
/// process; Codex executes the command string itself (no implicit login
/// shell), so each platform wraps in the shell that expands its own env-var
/// syntax.
fn hook_command(platform: Platform) -> String {
    match platform {
        Platform::Windows => {
            "cmd.exe /c \"curl -sf -X POST --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID%\""
                .to_string()
        }
        _ => {
            "sh -c \"curl -sf -X POST --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true\""
                .to_string()
        }
    }
}

/// Translate `backend_env` `OPENAI_BASE_URL` / `OPENAI_MODEL` into the
/// codex profile name a **Proxied Provider** spawn should load (issue #599).
///
/// Live probing of `codex-cli 0.144.0` established the canonical contract:
/// - `OPENAI_API_KEY` is honoured as an env var (regression-pinned via
///   `auth.credentials` reporting it under `auth env vars present`).
/// - `OPENAI_BASE_URL` / `OPENAI_MODEL` are **silently ignored** as env vars
///   even when exported — `codex doctor --json` reports
///   `endpoint: wss://api.openai.com/v1/<redacted>` regardless of what was
///   in `OPENAI_BASE_URL`.
/// - Custom (non-`openai`) `model_providers.<name>` entries **must** live in
///   `~/.codex/config.toml` — `-c key=value` overrides only validate against
///   known providers; `-p <name>` (profile layering) is the supported way
///   to inject a custom provider at runtime, reading
///   `$CODEX_HOME/<name>.config.toml` on top of the user's main config.
///
/// The fix therefore:
/// 1. Returns `Some(profile_name, base_url, model)` from this helper when
///    `backend_env` carries a non-empty `OPENAI_BASE_URL` (the marker for a
///    Codex Proxied Provider pairing).
/// 2. Spawn path writes the matching `<profile_name>.config.toml`
///    idempotently via [`ensure_proxy_profile`] (idempotent: the hash names
///    only one file per `(base_url, model)` pair; a pair change gets a new
///    name, so we never overwrite unrelated pairing configs).
/// 3. Spawn path adds `-p <profile_name>` to the codex CLI args.
///
/// Returns `None` for **native Codex** (`backend_env` carries no
/// `OPENAI_BASE_URL`, which is the marker that the spawn is *not* a Proxied
/// Provider pairing) — a bare `codex` harness spawn emits no extra flag,
/// byte-identical to the pre-#599 path. A partially-filled pairing (no
/// `base_url`) is also `None`, matching `openai_surface_env`'s half-fill
/// behaviour: emitting `-p` with an empty `model_providers.<...>.base_url=""
/// would route the spawn at an empty endpoint, which is exactly the
/// silent-leak bug this helper exists to close.
///
/// The return struct carries `base_url` + `model` alongside the profile name
/// so the spawn path doesn't have to re-scan `backend_env` — that re-scan
/// was previously a duplication risk: a different scan could in principle
/// find an empty or absent value and write `base_url = ""` to the profile
/// config, exactly the silent-leak class this helper exists to close.
pub(crate) fn proxy_pair(backend_env: &[(String, String)]) -> Option<ProxyPair<'_>> {
    let find = |key: &str| -> Option<&str> {
        backend_env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let base_url = find("OPENAI_BASE_URL")?;
    let model = find("OPENAI_MODEL");

    // Stable, build-local profile name. `DefaultHasher` (SipHasher13) is
    // deterministic within a binary — the profile name is only used to
    // namespace TOML keys within a single codex invocation, so cross-build
    // stability is unnecessary. 16 hex chars (~64 bits) makes collisions
    // vanishingly unlikely across the user's likely pairing set. The
    // base_url is the primary input; we hash the model too (when set) so a
    // future Kimi/MiniMax model-flag story (two pairings to the same
    // endpoint with different `OPENAI_MODEL` values) gets isolated profiles.
    let profile_name = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        base_url.hash(&mut hasher);
        if let Some(m) = model {
            '\0'.hash(&mut hasher);
            m.hash(&mut hasher);
        }
        format!("bm{:x}", hasher.finish())
    };
    Some(ProxyPair {
        profile_name,
        base_url,
        model,
    })
}

/// The resolved per-pairing data a Codex Proxied Provider spawn needs.
/// `model` is `None` when the pairing didn't pin a model — the
/// `[model_providers.<name>]` block writes without a `model = "…"` line so
/// the user's own `~/.codex/config.toml` model default stays in force.
pub(crate) struct ProxyPair<'a> {
    pub profile_name: String,
    pub base_url: &'a str,
    pub model: Option<&'a str>,
}

/// `-p <profile_name>` flag pair, ready to `.extend(recipe.base_args)` from
/// the spawn path. Pure: profile-name validity is [`proxy_profile_name`]'s
/// job; this helper is a one-line adapter that ensures the flag pair is
/// emitted in the documented order (`-p` first, then the name).
pub(crate) fn proxy_p_flag(profile_name: &str) -> Vec<String> {
    vec!["-p".into(), profile_name.into()]
}

/// Resolve the directory Codex reads profiles from (`-p <name>` looks up
/// `<coxehome>/<name>.config.toml`). Order matches Codex's own lookup:
///   1. `$CODEX_HOME` (explicit override)
///   2. `%USERPROFILE%\.codex` on Windows, `$HOME/.codex` elsewhere
///
/// Exposed as a separate helper so tests can drive `ensure_proxy_profile`
/// against a temp dir without touching the real env (cross-platform
/// `std::env::set_var`/`remove_var` is partially supported and racy on
/// Windows, so a parameterised helper is the deterministic unit-test seam).
fn resolve_coxehome() -> Option<std::path::PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let env_var = if cfg!(target_os = "windows") {
                "USERPROFILE"
            } else {
                "HOME"
            };
            std::env::var_os(env_var).map(|p| std::path::PathBuf::from(p).join(".codex"))
        })
}

/// Idempotently write `<coxehome>/<profile_name>.config.toml` carrying the
/// `[model_providers.<profile_name>]` entry that routes codex to the proxied
/// endpoint. CodeX's `-p <name>` flag layers the file on top of the user's
/// `~/.codex/config.toml` at startup, so we never have to round-trip the
/// user's existing config — the profile is purely additive.
///
/// `coxehome_for_test` lets tests point at a tmp dir (production callers pass
/// `None`, so the function uses the env-derived directory via
/// [`resolve_coxehome`]).
///
/// Returns `Ok(())` early when the file already exists: a re-spawn with the
/// same `(base_url, model)` pair hashes to the same name, and the file's
/// content only depends on those inputs (no timestamps, no random data).
/// The user can also hand-edit the file between spawns; we don't fight that.
///
/// Returns `Err` when `$CODEX_HOME`/`~/.codex` can't be located — the spawn
/// then proceeds without `-p` (logged), which restores the pre-#599 fall-back
/// of "codex ignores our OPENAI_BASE_URL and targets OpenAI's real endpoint".
/// Better than a hard spawn failure: at least the user's spawn runs, with a
/// warning that the pairing isn't routed correctly.
pub(crate) fn ensure_proxy_profile(
    profile_name: &str,
    base_url: &str,
    model: Option<&str>,
    coxehome_for_test: Option<&std::path::Path>,
) -> Result<(), String> {
    let coxehome = match coxehome_for_test {
        Some(p) => p.to_path_buf(),
        None => resolve_coxehome().ok_or_else(|| {
            "could not locate CODEX_HOME or user home for proxy profile write".to_string()
        })?,
    };
    std::fs::create_dir_all(&coxehome)
        .map_err(|e| format!("failed to create codex home dir: {e}"))?;
    let path = coxehome.join(format!("{profile_name}.config.toml"));
    if path.exists() {
        // Idempotent re-spawn: pairing signature unchanged → identical hash →
        // identical filename → skip the write. A user who edited the file by
        // hand has signalled intent; respect it.
        return Ok(());
    }
    let model_line = model
        .map(|m| format!("model = \"{m}\"\n"))
        .unwrap_or_default();
    let content = format!(
        r#"{model_line}model_provider = "{profile_name}"

[model_providers.{profile_name}]
name = "Buildmesh proxy {profile_name}"
base_url = "{base_url}"
env_key = "OPENAI_API_KEY"
requires_openai_auth = true
"#
    );
    std::fs::write(&path, content)
        .map_err(|e| format!("failed to write proxy profile {}: {e}", path.display()))?;
    tracing::info!("spawn_agent: wrote codex proxy profile {:?}", path);
    Ok(())
}

/// Ensure `<project>/.codex/config.toml` enables the hooks feature
/// (`[features] hooks = true` — `codex_hooks` is the legacy alias, issue
/// #884). Text-level merge (no toml dep): a file that already enables the
/// flag no-ops; an existing `[features]` section gets the flag inserted under
/// it; anything else gets the section appended, preserving existing content.
fn ensure_hooks_feature(path: &Path) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    // Substring check covers both `hooks = true` and `codex_hooks = true`.
    if existing.contains("hooks = true") {
        return Ok(());
    }
    let updated = if let Some(pos) = existing.find("[features]") {
        match existing[pos..].find('\n') {
            Some(nl) => {
                let insert_at = pos + nl + 1;
                format!(
                    "{}hooks = true\n{}",
                    &existing[..insert_at],
                    &existing[insert_at..]
                )
            }
            // `[features]` is the last line, without a trailing newline.
            None => format!("{existing}\nhooks = true\n"),
        }
    } else if existing.trim().is_empty() {
        "[features]\nhooks = true\n".to_string()
    } else {
        format!("{}\n\n[features]\nhooks = true\n", existing.trim_end())
    };
    std::fs::write(path, updated).map_err(|e| format!("failed to write config.toml: {e}"))
}

/// Ensure `<project>/.codex/hooks.json` carries the Stop + PermissionRequest
/// attention webhooks. Codex's matcher/event schema nests hook entries one
/// level deeper than Claude Code's (each event maps to matcher groups, each
/// carrying a `hooks` array — issue #884). Idempotent, and preserves any
/// unrelated top-level keys the user added.
fn ensure_hooks_json(path: &Path, command: &str) -> Result<(), String> {
    let mut settings: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !settings.is_object() {
        settings = serde_json::json!({});
    }

    let hook = serde_json::json!({ "type": "command", "command": command });
    // Stop fires the instant a turn ends; PermissionRequest fires when a tool
    // needs approval. Both mean "the user may be needed" — the backend's
    // `decide` sorts genuine yields from background-task waits.
    let expected = serde_json::json!({
        "Stop": [{ "hooks": [hook.clone()] }],
        "PermissionRequest": [{ "hooks": [hook] }],
    });
    if settings.get("hooks") == Some(&expected) {
        return Ok(());
    }
    settings["hooks"] = expected;
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize hooks.json failed: {e}"))?;
    std::fs::write(path, content).map_err(|e| format!("failed to write hooks.json: {e}"))?;
    tracing::info!("codex inject_attention_hook: wrote {:?}", path);
    Ok(())
}

impl AgentProvider for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "OpenAI Codex".into(),
            color: "#10a37f".into(),
            icon: "X".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "codex",
            base_args: base_flags(),
            windows_shell: shell_for(platform),
        }
    }

    fn spawn_recipe_for_resume(
        &self,
        platform: Platform,
        session_id: &str,
    ) -> Option<SpawnRecipe> {
        let mut args = vec!["resume".into(), session_id.into()];
        args.extend(base_flags());
        Some(SpawnRecipe {
            binary: "codex",
            base_args: args,
            windows_shell: shell_for(platform),
        })
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    fn requires_attention_hook(&self) -> bool {
        true
    }

    /// Codex hooks live in the project-local `.codex/` dir: `config.toml`
    /// enables the hooks feature, `hooks.json` declares the webhooks
    /// (issue #886). The spawn flags carry the trust bypass that lets them
    /// run headlessly.
    fn inject_attention_hook(&self, project_path: &Path) -> Result<(), String> {
        let codex_dir = project_path.join(".codex");
        std::fs::create_dir_all(&codex_dir)
            .map_err(|e| format!("failed to create .codex dir: {e}"))?;
        ensure_hooks_feature(&codex_dir.join("config.toml"))?;
        ensure_hooks_json(&codex_dir.join("hooks.json"), &hook_command(Platform::current()))
    }

    /// Codex writes rollout transcripts under `~/.codex/sessions/` that
    /// `services::transcript_reader` parses via `TranscriptFormat::Codex`
    /// (issue #887).
    fn produces_readable_transcript(&self) -> bool {
        true
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Macos, Platform::Windows, Platform::Linux]
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn resume_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn effort_args(&self, _effort: &str) -> Vec<String> {
        vec![]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Local hooks only run headlessly with the trust bypass (issue #884) —
    /// both the fresh and resume spawn paths must carry the flag, or Codex
    /// blocks on an interactive workspace-review prompt.
    #[test]
    fn spawn_recipes_carry_the_hook_trust_bypass() {
        let bypass = "--dangerously-bypass-hook-trust".to_string();
        let fresh = CODEX.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert!(fresh.base_args.contains(&bypass), "fresh: {:?}", fresh.base_args);
        let resume = CODEX
            .spawn_recipe_for_resume(Platform::Windows, "sid-123")
            .expect("codex has a resume recipe");
        assert!(resume.base_args.contains(&bypass), "resume: {:?}", resume.base_args);
    }

    #[test]
    fn codex_declares_attention_hook_and_readable_transcript() {
        assert!(CODEX.requires_attention_hook());
        assert!(CODEX.produces_readable_transcript());
    }

    fn read_hooks_json(project: &Path) -> serde_json::Value {
        let content = std::fs::read_to_string(project.join(".codex").join("hooks.json"))
            .expect("hooks.json not written");
        serde_json::from_str(&content).expect("hooks.json is not valid JSON")
    }

    /// Injection writes both files: the feature flag and the two webhooks in
    /// Codex's nested matcher/event schema, POSTing the hook's stdin to the
    /// attention endpoint.
    #[test]
    fn inject_writes_config_and_hooks() {
        let temp = TempDir::new().unwrap();
        CODEX.inject_attention_hook(temp.path()).unwrap();

        let config = std::fs::read_to_string(temp.path().join(".codex").join("config.toml"))
            .expect("config.toml not written");
        assert!(config.contains("[features]"), "config: {config}");
        assert!(config.contains("hooks = true"), "config: {config}");

        let hooks = read_hooks_json(temp.path());
        for event in ["Stop", "PermissionRequest"] {
            let command = hooks["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("{event} hook missing: {hooks:#}"));
            assert!(
                command.contains("/api/attention/"),
                "{event} must POST to the attention endpoint: {command}"
            );
            assert!(
                command.contains("--data-binary @-"),
                "{event} must forward the hook stdin as the POST body: {command}"
            );
        }
    }

    /// Re-running injection over an already-correct project is a no-op.
    #[test]
    fn inject_is_idempotent() {
        let temp = TempDir::new().unwrap();
        CODEX.inject_attention_hook(temp.path()).unwrap();
        let config_first =
            std::fs::read_to_string(temp.path().join(".codex").join("config.toml")).unwrap();
        let hooks_first = read_hooks_json(temp.path());

        CODEX.inject_attention_hook(temp.path()).unwrap();
        let config_second =
            std::fs::read_to_string(temp.path().join(".codex").join("config.toml")).unwrap();
        assert_eq!(config_first, config_second);
        assert_eq!(hooks_first, read_hooks_json(temp.path()));
    }

    /// A user's existing config.toml keys survive; the flag lands under an
    /// existing `[features]` section instead of duplicating it (a duplicate
    /// table is a TOML parse error that would break Codex's whole config).
    #[test]
    fn config_merge_preserves_content_and_existing_features_section() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            "model = \"gpt-5.2-codex\"\n\n[features]\nweb_search = true\n",
        )
        .unwrap();

        CODEX.inject_attention_hook(temp.path()).unwrap();

        let config = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(config.contains("model = \"gpt-5.2-codex\""), "config: {config}");
        assert!(config.contains("web_search = true"), "config: {config}");
        assert!(config.contains("hooks = true"), "config: {config}");
        assert_eq!(
            config.matches("[features]").count(),
            1,
            "must not duplicate the [features] table: {config}"
        );
    }

    /// A config without a `[features]` section gets one appended, keeping the
    /// user's content intact.
    #[test]
    fn config_merge_appends_features_section_when_missing() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.2-codex\"\n").unwrap();

        CODEX.inject_attention_hook(temp.path()).unwrap();

        let config = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(config.contains("model = \"gpt-5.2-codex\""));
        assert!(config.contains("[features]\nhooks = true"), "config: {config}");
    }

    /// Injection only owns the `hooks` key of hooks.json — unrelated keys the
    /// user added survive.
    #[test]
    fn hooks_json_merge_preserves_unrelated_keys() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("hooks.json"), r#"{"custom":"kept"}"#).unwrap();

        CODEX.inject_attention_hook(temp.path()).unwrap();

        let hooks = read_hooks_json(temp.path());
        assert_eq!(hooks["custom"], "kept");
        assert!(hooks["hooks"]["Stop"].is_array());
    }

    /// The Windows hook command must expand env vars with cmd syntax (`%VAR%`)
    /// under an explicit `cmd.exe /c`, and the Unix one with sh syntax —
    /// Codex executes the command string without a login shell of its own.
    #[test]
    fn hook_command_uses_platform_env_syntax() {
        let win = hook_command(Platform::Windows);
        assert!(win.starts_with("cmd.exe /c"), "win: {win}");
        assert!(win.contains("%BUILDMESH_PORT%"), "win: {win}");
        assert!(win.contains("%BUILDMESH_SESSION_ID%"), "win: {win}");
        for platform in [Platform::Macos, Platform::Linux] {
            let unix = hook_command(platform);
            assert!(unix.starts_with("sh -c"), "unix: {unix}");
            assert!(unix.contains("$BUILDMESH_PORT"), "unix: {unix}");
            assert!(unix.contains("$BUILDMESH_SESSION_ID"), "unix: {unix}");
        }
    }

    // ─── Issue #599: Codex proxy profile plumbing ────────────────────────────

    /// Native Codex has no `OPENAI_BASE_URL` in its `backend_env` — the
    /// translator must return `None`, byte-identical to the pre-#599 path
    /// (regression-pinned).
    #[test]
    fn proxy_pair_none_for_native_codex() {
        let env: Vec<(String, String)> = vec![];
        assert!(
            proxy_pair(&env).is_none(),
            "empty env → None (native Codex)"
        );

        // Even if other OPENAI_* leak into the env (e.g. an unrelated process
        // export), the marker key is `OPENAI_BASE_URL` — without it the
        // translator must not emit any flag.
        let env = vec![("OPENAI_API_KEY".into(), "sk-foo".into())];
        assert!(
            proxy_pair(&env).is_none(),
            "OPENAI_API_KEY alone is not a proxy marker"
        );
    }

    /// A partially-filled pairing (no `OPENAI_BASE_URL`) must NOT emit a
    /// profile name — codex would interpret the empty `base_url` as a real
    /// (broken) endpoint, exactly the silent-leak bug this helper exists to
    /// close. Matches `openai_surface_env`'s half-fill behaviour.
    #[test]
    fn proxy_pair_none_for_blank_base_url() {
        let env = vec![("OPENAI_BASE_URL".into(), String::new())];
        assert!(
            proxy_pair(&env).is_none(),
            "blank OPENAI_BASE_URL must not produce a profile name (would route to an empty endpoint)"
        );

        // `OPENAI_MODEL` alone (no `OPENAI_BASE_URL`) is also None — model
        // without an endpoint is meaningless for codex.
        let env = vec![("OPENAI_MODEL".into(), "some-model".into())];
        assert!(
            proxy_pair(&env).is_none(),
            "OPENAI_MODEL without OPENAI_BASE_URL must not produce a profile name"
        );
    }

    /// Full pairing → a stable `ProxyPair` with the resolved `base_url` and
    /// `model` carried through (so the spawn path doesn't re-scan
    /// `backend_env` and risk inconsistency). The `profile_name` is
    /// `bm` + 16 hex chars; two calls with the same inputs must yield the
    /// same name (idempotent — a re-spawn reuses the existing on-disk
    /// profile, no churn).
    #[test]
    fn proxy_pair_stable_for_full_pairing() {
        let env = vec![
            ("OPENAI_BASE_URL".into(), "https://api.minimax.io/v1".into()),
            ("OPENAI_API_KEY".into(), "sk-mm".into()),
            ("OPENAI_MODEL".into(), "MiniMax-M3[1m]".into()),
        ];
        let pair = proxy_pair(&env).expect("full pairing → Some");
        let name = &pair.profile_name;
        assert!(
            name.starts_with("bm") && name.len() == 18 && name[2..].chars().all(|c| c.is_ascii_hexdigit()),
            "profile_name must be `bm` + 16 hex chars (got {name:?})"
        );
        assert_eq!(pair.base_url, "https://api.minimax.io/v1");
        assert_eq!(pair.model, Some("MiniMax-M3[1m]"));
        let again = proxy_pair(&env).expect("full pairing is deterministic");
        assert_eq!(
            name, &again.profile_name,
            "proxy_pair must be deterministic for identical inputs"
        );
    }

    /// Different `base_url` or `model` must yield different profile names —
    /// codex namespaces the `[model_providers.<name>]` table by name, so a
    /// collision would let a Kimi spawn inherit a MiniMax URL (or vice-versa).
    /// 16 hex chars (≈ 64 bits of entropy) makes natural collisions
    /// vanishingly unlikely; this test pins that *intentional* inputs (same
    /// model, different endpoint) produce *distinct* profile names.
    #[test]
    fn proxy_pair_distinct_per_endpoint() {
        let env_a = vec![(
            "OPENAI_BASE_URL".into(),
            "https://api.minimax.io/v1".into(),
        )];
        let env_b = vec![(
            "OPENAI_BASE_URL".into(),
            "https://api.moonshot.ai/v1".into(),
        )];
        let name_a = proxy_pair(&env_a).unwrap().profile_name;
        let name_b = proxy_pair(&env_b).unwrap().profile_name;
        assert_ne!(
            name_a, name_b,
            "different base_urls must hash to different profile names (got {name_a:?} == {name_b:?})"
        );
    }

    /// The model variant of the same base_url must also yield a distinct
    /// profile — two pairings sharing an endpoint but different models
    /// (e.g. a future Kimi model-flag story) get isolated TOML tables.
    #[test]
    fn proxy_pair_distinct_per_model() {
        let env_a = vec![
            ("OPENAI_BASE_URL".into(), "https://api.minimax.io/v1".into()),
            ("OPENAI_MODEL".into(), "MiniMax-M3[1m]".into()),
        ];
        let env_b = vec![
            ("OPENAI_BASE_URL".into(), "https://api.minimax.io/v1".into()),
            ("OPENAI_MODEL".into(), "MiniMax-M2.7".into()),
        ];
        let name_a = proxy_pair(&env_a).unwrap().profile_name;
        let name_b = proxy_pair(&env_b).unwrap().profile_name;
        assert_ne!(
            name_a, name_b,
            "different models at the same endpoint must get isolated profile names"
        );
    }

    /// `-p <name>` flag pair — the spawn-time addition the codex runtime
    /// expects (regression-pinned so a future refactor doesn't drop the `-p`
    /// and silently re-introduce the silent-OpenAI-routing bug).
    #[test]
    fn proxy_p_flag_is_dash_p_name() {
        assert_eq!(proxy_p_flag("bm1234567890abcdef"), ["-p", "bm1234567890abcdef"]);
    }

    /// `ensure_proxy_profile` is the I/O side of the translation: idempotent
    /// on identical inputs (file already there → no re-write), and writes
    /// the canonical `[model_providers.<name>]` TOML block codex consumes.
    /// Pointed at a temp dir via the `coxehome_for_test` parameter so the
    /// host's `~/.codex/` stays clean — mutating `CODEX_HOME` / `HOME` via
    /// `std::env::set_var` is partially supported and racy on Windows.
    #[test]
    fn ensure_proxy_profile_is_idempotent_and_writes_canonical_toml() {
        let temp = TempDir::new().unwrap();
        ensure_proxy_profile(
            "bme2btest_minimax",
            "https://api.minimax.io/v1",
            Some("MiniMax-M3[1m]"),
            Some(temp.path()),
        )
        .expect("first call writes the file");

        let path = temp.path().join("bme2btest_minimax.config.toml");
        let content = std::fs::read_to_string(&path).expect("file written");
        assert!(content.contains(r#"model_provider = "bme2btest_minimax""#), "{content}");
        assert!(
            content.contains(r#"[model_providers.bme2btest_minimax]"#),
            "{content}"
        );
        assert!(
            content.contains(r#"base_url = "https://api.minimax.io/v1""#),
            "{content}"
        );
        assert!(content.contains(r#"env_key = "OPENAI_API_KEY""#), "{content}");
        assert!(content.contains("requires_openai_auth = true"), "{content}");
        assert!(content.contains(r#"name = "Buildmesh proxy bme2btest_minimax""#), "{content}");
        assert!(content.contains(r#"model = "MiniMax-M3[1m]""#), "{content}");

        // Idempotency: a second call must NOT rewrite (preserves any
        // hand-edits the user made between spawns).
        let first_content = content.clone();
        ensure_proxy_profile(
            "bme2btest_minimax",
            "https://api.minimax.io/v1",
            Some("MiniMax-M3[1m]"),
            Some(temp.path()),
        )
        .expect("second call is a no-op");
        let second_content = std::fs::read_to_string(&path).expect("file unchanged");
        assert_eq!(
            first_content, second_content,
            "ensure_proxy_profile must be idempotent (file content unchanged on re-call)"
        );
    }

    /// Missing model → no `model = "..."` line is written (so the user's
    /// own `~/.codex/config.toml` model default wins). The
    /// `[model_providers.<name>]` block still writes, since the base_url +
    /// env_key are the only required fields for routing.
    #[test]
    fn ensure_proxy_profile_omits_model_line_when_model_absent() {
        let temp = TempDir::new().unwrap();
        ensure_proxy_profile(
            "bme2btest_no_model",
            "https://example.com/v1",
            None,
            Some(temp.path()),
        )
        .expect("no-model call still writes");
        let content = std::fs::read_to_string(
            temp.path().join("bme2btest_no_model.config.toml"),
        )
        .expect("file written");
        assert!(
            !content.contains("model = "),
            "absent model must not write a model= line; got: {content}"
        );
        assert!(
            content.contains(r#"base_url = "https://example.com/v1""#),
            "{content}"
        );
    }
}
