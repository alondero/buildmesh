use crate::agent::provider::provider_conf::read_providers_conf;
use crate::agent::provider::{
    claude_direct_recipe, AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell,
};

pub struct MinimaxAdapter;
pub static MINIMAX: MinimaxAdapter = MinimaxAdapter;

impl AgentProvider for MinimaxAdapter {
    fn id(&self) -> &'static str {
        "minimax"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "MiniMax".into(),
            color: "#6366f1".into(),
            icon: "M".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform) -> SpawnRecipe {
        SpawnRecipe {
            binary: "cwrap",
            base_args: vec!["--minimax".into()],
            windows_shell: WindowsShell::PowerShell,
        }
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
        &[Platform::Windows, Platform::Linux]
    }

    /// Windows AppContainer sandbox: spawn claude.exe directly (cwrap → bash
    /// can't init in the container).
    fn sandbox_direct_recipe(&self, _platform: Platform) -> Option<SpawnRecipe> {
        Some(claude_direct_recipe())
    }

    /// The MiniMax backend env cwrap's `minimax` arm would export, rebuilt
    /// in-process from `~/.claude/providers.conf` (mirrors `~/.local/bin/cwrap`).
    fn sandbox_provider_env(&self) -> Vec<(String, String)> {
        let conf = read_providers_conf();
        let base_url = conf
            .get("MINIMAX_BASE_URL")
            .filter(|v| !v.is_empty())
            .cloned()
            .unwrap_or_else(|| "https://api.minimax.io/anthropic".to_string());
        let mut env = vec![
            ("ANTHROPIC_BASE_URL".to_string(), base_url),
            ("API_TIMEOUT_MS".to_string(), "3000000".to_string()),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(), "1".to_string()),
            ("CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(), "512000".to_string()),
            ("ANTHROPIC_MODEL".to_string(), "MiniMax-M3[1m]".to_string()),
            ("ANTHROPIC_SMALL_FAST_MODEL".to_string(), "MiniMax-M2.7".to_string()),
            ("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), "MiniMax-M3[1m]".to_string()),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), "MiniMax-M3[1m]".to_string()),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), "MiniMax-M2.7".to_string()),
        ];
        match conf.get("MINIMAX_API_KEY").filter(|v| !v.is_empty()) {
            Some(key) => env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), key.clone())),
            None => tracing::error!(
                "sandbox MiniMax spawn: MINIMAX_API_KEY missing from ~/.claude/providers.conf — claude will fail to authenticate"
            ),
        }
        env
    }
}
