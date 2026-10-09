//! Harness-owned observation strategies. Dispatch uses the trusted node provider;
//! unknown harnesses never fall back to another harness's payload mapping.
mod agy;
mod claude;
mod cline;
mod codex;
mod cursor;
mod grok;
mod kimi;
mod mcode;
mod opencode;

use super::common::*;
use serde_json::Value;
use std::path::Path;

type ClassifyHook = fn(
    &HookPayload,
    crate::agent::session_lifecycle::HookSignalDetail,
    &mut dyn FnMut(&Path) -> Option<usize>,
) -> Classified;

struct Normalizer {
    parse: fn(&Value) -> Option<HookPayload>,
    classify: ClassifyHook,
}

fn dispatch(provider: &str) -> Option<Normalizer> {
    Some(match provider {
        "claude" | "claude_code" | "anthropic" => Normalizer {
            parse: claude::parse,
            classify: claude::classify,
        },
        "codex" => Normalizer {
            parse: codex::parse,
            classify: codex::classify,
        },
        "cline" => Normalizer {
            parse: cline::parse,
            classify: cline::classify,
        },
        "cursor" => Normalizer {
            parse: cursor::parse,
            classify: cursor::classify,
        },
        "grok" => Normalizer {
            parse: grok::parse,
            classify: grok::classify,
        },
        "kimi" => Normalizer {
            parse: kimi::parse,
            classify: kimi::classify,
        },
        "mcode" => Normalizer {
            parse: mcode::parse,
            classify: mcode::classify,
        },
        "agy" => Normalizer {
            parse: agy::parse,
            classify: agy::classify,
        },
        "opencode" => Normalizer {
            parse: opencode::parse,
            classify: opencode::classify,
        },
        _ => return None,
    })
}

#[cfg(test)]
pub(super) fn parse(value: &Value, provider: &str) -> Option<HookPayload> {
    (dispatch(provider)?.parse)(value)
}

pub(super) struct Normalized {
    pub payload: Option<HookPayload>,
    pub classified: Classified,
    pub semantic: Option<SemanticTurn>,
    pub session_id: Option<String>,
    pub native_hook: Option<crate::services::circuit_worker::native_hooks::NativeHook>,
}

impl Default for Normalized {
    fn default() -> Self {
        Self {
            payload: None,
            classified: unavailable(Default::default()),
            semantic: None,
            session_id: None,
            native_hook: None,
        }
    }
}

pub(super) fn normalize(
    raw: Option<&Value>,
    provider: &str,
    count_pending: impl FnOnce(&Path) -> Option<usize>,
) -> Normalized {
    let Some(normalizer) = dispatch(provider) else {
        return Normalized::default();
    };
    let mut payload = raw.and_then(normalizer.parse);
    let mut count_pending = Some(count_pending);
    let classified = match payload.as_ref().filter(|p| **p != HookPayload::default()) {
        Some(payload) => (normalizer.classify)(payload, detail(payload, provider), &mut |path| {
            count_pending.take().and_then(|count| count(path))
        }),
        None => unavailable(Default::default()),
    };
    if !classified.validated {
        // An unrecognized observation may request review, but cannot alter turn,
        // question or child correlation state using another harness's vocabulary.
        payload = None;
    }
    let semantic = payload.as_ref().and_then(semantic_turn);
    let session_id = payload
        .as_ref()
        .and_then(|p| hook_session_id_from_payload(p, provider));
    // Native receipts have their own strict ownership contract; malformed ordinary
    // callbacks cannot create a receipt or mutate Circuit state.
    let native_hook = payload.as_ref().and(raw).and_then(|value| {
        crate::circuit::strategy::for_recorded_adapter(Some(provider)).parse_hook(value)
    });
    Normalized {
        payload,
        classified,
        semantic,
        session_id,
        native_hook,
    }
}

pub(super) fn resolve_node(
    addressed: Option<crate::models::AgentNode>,
    raw: Option<&Value>,
    generic_mcode: bool,
    resolve_mcode: impl FnOnce(&str, &str) -> Option<crate::models::AgentNode>,
) -> Option<crate::models::AgentNode> {
    let payload = raw.and_then(mcode::parse);
    mcode::resolve_attention_node(addressed, payload.as_ref(), generic_mcode, resolve_mcode)
}

#[cfg(test)]
pub(super) use mcode::resolve_attention_node;

#[cfg(test)]
fn replay(provider: &str, corpus: &str) {
    let cases: Value = serde_json::from_str(corpus).unwrap();
    for case in cases.as_array().unwrap() {
        let body = case
            .get("body")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| case["payload"].to_string());
        let value: Option<Value> = serde_json::from_str(&body).ok();
        let result = normalize(value.as_ref(), provider, |_| {
            case.get("pending")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
        })
        .classified;
        assert_eq!(
            format!("{:?}", result.decision),
            case["decision"].as_str().unwrap(),
            "{provider}: {}",
            case["name"]
        );
        assert_eq!(
            serde_json::to_value(result.detail.kind).unwrap(),
            case["kind"],
            "{provider}: {}",
            case["name"]
        );
        assert_eq!(
            serde_json::to_value(result.detail.signal_health).unwrap(),
            case["health"],
            "{provider}: {}",
            case["name"]
        );
    }
}

/// Resolve profiles and composite spawn options without the database executor's
/// legacy Anthropic fallback. Unknown hook strategies must stay unknown.
pub(super) fn provider_for(stored: &str) -> String {
    let selected = crate::preferences::launch_configurations::selection_option(stored)
        .unwrap_or_else(|_| stored.to_owned());
    provider_for_selection(&selected, |harness| {
        crate::preferences::harness_profiles()
            .into_iter()
            .find(|profile| profile.id == harness)
            .map(|profile| profile.harness)
    })
}

fn provider_for_selection(
    selected: &str,
    profile_harness: impl FnOnce(&str) -> Option<String>,
) -> String {
    let option = crate::agent::provider::SpawnOptionId::from(selected);
    let harness = option.harness_id();
    let executor = profile_harness(harness).unwrap_or_else(|| harness.to_owned());
    crate::models::Provider::try_from_db_str(&executor)
        .map(|provider| provider.adapter().id().to_owned())
        .unwrap_or(executor)
}

#[cfg(test)]
mod resolution_tests {
    use super::*;
    #[test]
    fn unknown_node_provider_reaches_the_unavailable_strategy() {
        for selected in ["unknown-harness", "unknown-harness:model-account", ""] {
            let provider = provider_for_selection(selected, |_| None);
            let result = normalize(
                Some(&serde_json::json!({"hook_event_name":"Stop"})),
                &provider,
                |_| panic!("unknown provider must not reconcile"),
            );
            assert_eq!(
                result.classified.detail.kind,
                Some(crate::agent::session_lifecycle::LifecycleKind::SignalUnavailable)
            );
        }
    }

    #[test]
    fn known_profile_and_composite_options_use_their_actual_harness() {
        assert_eq!(
            provider_for_selection("claude:minimax", |_| None),
            "anthropic"
        );
        assert_eq!(
            provider_for_selection("codex:model-account", |_| None),
            "codex"
        );
        assert_eq!(
            provider_for_selection("custom", |id| {
                assert_eq!(id, "custom");
                Some("grok".into())
            }),
            "grok"
        );
        assert_eq!(
            provider_for_selection("custom", |_| Some("unknown-executor".into())),
            "unknown-executor"
        );
    }
}
