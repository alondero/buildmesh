use crate::agent::provider::provider_conf::read_providers_conf;
use crate::agent::provider::{
    claude_direct_recipe, AgentProvider, Platform, SpawnRecipe, UiMeta,
};

pub struct KimiAdapter;
pub static KIMI: KimiAdapter = KimiAdapter;

impl AgentProvider for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Kimi".into(),
            color: "#00c4c4".into(),
            icon: "K".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform) -> SpawnRecipe {
        claude_direct_recipe(platform)
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

    /// The Kimi backend env cwrap's `kimi` arm would export, rebuilt in-process
    /// from `~/.claude/providers.conf` (mirrors `~/.local/bin/cwrap`).
    fn provider_env(&self) -> Vec<(String, String)> {
        let conf = read_providers_conf();
        let mut env = vec![
            ("ANTHROPIC_BASE_URL".to_string(), "https://api.moonshot.ai/anthropic".to_string()),
            ("API_TIMEOUT_MS".to_string(), "3000000".to_string()),
            ("ANTHROPIC_MODEL".to_string(), "kimi-k2.6".to_string()),
            ("ANTHROPIC_SMALL_FAST_MODEL".to_string(), "kimi-k2.5".to_string()),
            ("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), "kimi-k2.5".to_string()),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), "kimi-k2.6".to_string()),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), "kimi-k2.5".to_string()),
        ];
        match conf.get("MOONSHOT_API_KEY").filter(|v| !v.is_empty()) {
            Some(key) => env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), key.clone())),
            None => tracing::error!(
                "Kimi spawn: MOONSHOT_API_KEY missing from ~/.claude/providers.conf — claude will fail to authenticate"
            ),
        }
        env
    }
}
