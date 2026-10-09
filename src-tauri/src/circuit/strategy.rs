//! Harness-owned Circuit observation strategies (issue #2128).
//!
//! Each harness adapter declares, once, what it can contribute to a Circuit's
//! observation of one agent: the push hooks it parses, the pull source it
//! reconciles against, the identity and owned-work coverage those carry, where
//! its final report comes from, and its reconciliation budget. The Circuit
//! worker selects behaviour from that declaration and renders its operator
//! diagnostics from the same declaration, so what is advertised cannot drift
//! from what is executed.
//!
//! An adapter only *normalizes* vendor facts into the existing observation
//! vocabulary ([`super::observation`]). Freshness, requests, known-work blockers
//! and authorization stay with the shared Circuit policy; nothing here can
//! authorize progression, and there is deliberately no "supports autopilot"
//! flag. The `notes` are display-only provenance (inspected versions, platform
//! limits); no decision reads them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A hook payload reduced to the facts a Circuit may use: lifecycle, ownership
/// and the final report. The attention route owns transport and session
/// validation; an adapter's parser owns only the vendor payload shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeHook {
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub event: String,
    pub child_id: Option<String>,
    pub active_work: Option<BTreeSet<String>>,
    pub final_report: Option<String>,
    /// Antigravity `Stop` with `fullyIdle: false`: the turn yielded while
    /// harness-owned background work is still in flight (issue #1901).
    /// Never set by the Claude/Codex parsers.
    #[serde(default)]
    pub background_busy: bool,
    /// Antigravity `executionNum`: an opaque 0-based step counter, not a
    /// turn identity token. Retained so consecutive settled turns from one
    /// session hash to distinct receipt source ids instead of colliding
    /// into the history deduplicator (issue #1901 review). Never set by
    /// the Claude/Codex parsers.
    #[serde(default)]
    pub execution_num: Option<i64>,
    /// Antigravity `terminationReason` (e.g. `model_stop`,
    /// `NO_TOOL_CALL`): opaque triage telemetry, never a decision input.
    /// Never set by the Claude/Codex parsers.
    #[serde(default)]
    pub termination_reason: Option<String>,
    #[serde(default)]
    pub human_fact: Option<super::observation::ObservedWorkFact>,
    /// The adapter id that parsed this hook. Receipts persist it so a replay
    /// resolves the same strategy that produced the hook.
    #[serde(default)]
    pub provider: Option<String>,
    /// SHA-256 of the verbatim `prompt` Claude Code echoes on
    /// `UserPromptSubmit` (issue #1898). This is the only native field that
    /// can identify *which* Buildmesh submission a turn acknowledges: the
    /// text Buildmesh wrote is the text the harness reports receiving.
    /// Retained only alongside a turn token — the content proof and the turn
    /// name are only useful together. Absent for every other event, provider
    /// and payload shape, and an absent digest means "cannot prove
    /// submission", never "assume it matched". The prompt text itself is
    /// never retained.
    #[serde(default)]
    pub prompt_digest: Option<String>,
}

/// SHA-256 of the exact prompt text Buildmesh wrote into the PTY. The
/// submission record and the `UserPromptSubmit` echo must both be hashed
/// through this helper so an equal digest is a real byte-for-byte match
/// rather than two independently defined fingerprints.
pub(crate) fn submission_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Vendor payload → [`NativeHook`]. `None` means the payload is malformed or
/// names an event the harness's contract does not validate.
pub type HookParser = fn(&serde_json::Value) -> Option<NativeHook>;

/// The hook strategy a harness delivers through the attention route.
#[derive(Clone, Copy)]
pub struct HookPush {
    pub parse: HookParser,
    /// Observation source recorded for lifecycle facts from this harness.
    pub source: &'static str,
    /// Observation source recorded for question/permission facts.
    pub request_source: &'static str,
    /// `Some(reason)`: the harness has no child/background registry, so a
    /// settled turn that reports none is an explicit ownership gap surfaced to
    /// the operator. `None`: the harness may report a registry, and a turn
    /// without one leaves ownership merely unknown.
    pub ownership_gap: Option<&'static str>,
}

/// A validated native pull the worker re-reads while a step is unsettled.
#[derive(Clone, Copy)]
pub struct NativePull {
    /// Observation source for the completion read.
    pub source: &'static str,
    /// Observation source for resolving a foreground conflict.
    pub recheck_source: &'static str,
    /// Operator-facing harness name used in blocker text.
    pub label: &'static str,
    /// Why this pull cannot establish owned-work coverage.
    pub ownership_limit: &'static str,
}

#[derive(Clone, Copy)]
pub enum PushSource {
    None,
    Hooks(HookPush),
}

#[derive(Clone, Copy)]
pub enum PullSource {
    None,
    NativeTurnCompletion(NativePull),
}

/// How a native turn can be tied to the Buildmesh submission it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "ObservationTurnIdentity.ts")]
#[serde(rename_all = "snake_case")]
pub enum TurnIdentity {
    /// No turn token: evidence is session-fenced only and never authoritative.
    None,
    /// The harness names the turn; Buildmesh fences on that name.
    NativeToken,
    /// A native token plus a verbatim prompt echo bound to a recorded submission.
    SubmissionEcho,
}

/// Whether the harness reports which child and background work it owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnedWorkCoverage {
    /// No inventory: a settled foreground turn cannot verify owned work, and
    /// the carried reason is surfaced to the operator.
    Unavailable { reason: &'static str },
    /// Hooks may carry child events and task/cron registries. A settled turn
    /// without a registry still leaves ownership unknown.
    HookRegistry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "ObservationReportSource.ts")]
#[serde(rename_all = "snake_case")]
pub enum FinalReportSource {
    /// Transcript or terminal text may inform interpretation only.
    TranscriptOrTerminal,
    /// The scrubbed message a lifecycle hook carries.
    HookMessage,
    /// The scrubbed message a validated native pull returns.
    PullMessage,
}

/// A backend-owned transcript watcher that supplies this harness's turn
/// lifecycle when no native hook exists, and that must be re-attached after a
/// restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "ObservationPassiveWatcher.ts")]
#[serde(rename_all = "snake_case")]
pub enum PassiveWatcher {
    CommandCode,
    Muse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconciliationPolicy {
    /// How long a yielded step may wait for a fresh report before the
    /// watchdog treats the wait as expired.
    pub yielded_budget_ms: u32,
}

/// Display-only provenance for the diagnostics. Never read by a decision.
#[derive(Debug, Clone, Copy)]
pub struct StrategyNotes {
    pub foreground: &'static str,
    pub owned_work: &'static str,
    pub final_report: &'static str,
    pub reconciliation: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "ObservationPushKind.ts")]
#[serde(rename_all = "snake_case")]
pub enum PushKind {
    None,
    Hooks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "ObservationPullKind.ts")]
#[serde(rename_all = "snake_case")]
pub enum PullKind {
    None,
    NativeTurnCompletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "ObservationOwnedWorkKind.ts")]
#[serde(rename_all = "snake_case")]
pub enum OwnedWorkKind {
    Unavailable,
    HookRegistry,
}

/// The typed, wire-visible summary of a declaration. Diagnostics and tests read
/// this instead of parsing the prose notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "ObservationCoverage.ts")]
pub struct ObservationCoverage {
    pub push: PushKind,
    pub pull: PullKind,
    pub turn_identity: TurnIdentity,
    pub owned_work: OwnedWorkKind,
    pub final_report: FinalReportSource,
    pub passive_watcher: Option<PassiveWatcher>,
}

#[derive(Clone, Copy)]
pub struct ObservationStrategy {
    pub push: PushSource,
    pub pull: PullSource,
    pub turn_identity: TurnIdentity,
    pub owned_work: OwnedWorkCoverage,
    pub final_report: FinalReportSource,
    pub reconciliation: ReconciliationPolicy,
    pub passive_watcher: Option<PassiveWatcher>,
    pub notes: StrategyNotes,
}

impl ObservationStrategy {
    /// A harness with no authoritative Circuit lifecycle adapter. Execution
    /// support never implies lifecycle or ownership authority, so this is the
    /// default and stays visibly Unverified in the diagnostics.
    pub const UNWIRED: ObservationStrategy = ObservationStrategy {
        push: PushSource::None,
        pull: PullSource::None,
        turn_identity: TurnIdentity::None,
        owned_work: OwnedWorkCoverage::Unavailable {
            reason: "no authoritative Circuit ownership adapter is wired",
        },
        final_report: FinalReportSource::TranscriptOrTerminal,
        reconciliation: ReconciliationPolicy {
            yielded_budget_ms: 30_000,
        },
        passive_watcher: None,
        notes: StrategyNotes {
            foreground: "no authoritative Circuit lifecycle adapter is wired",
            owned_work: "no authoritative Circuit ownership adapter is wired",
            final_report:
                "Transcript or PTY text may inform interpretation; complete native report unavailable",
            reconciliation: "Status and report discovery only; unsupported lifecycle remains unverified",
        },
    };

    pub fn hooks(&self) -> Option<&HookPush> {
        match &self.push {
            PushSource::Hooks(hooks) => Some(hooks),
            PushSource::None => None,
        }
    }

    pub fn native_pull(&self) -> Option<&NativePull> {
        match &self.pull {
            PullSource::NativeTurnCompletion(pull) => Some(pull),
            PullSource::None => None,
        }
    }

    /// Whether any Circuit-native foreground evidence is wired.
    pub fn has_foreground_source(&self) -> bool {
        self.hooks().is_some() || self.native_pull().is_some()
    }

    pub fn coverage(&self) -> ObservationCoverage {
        ObservationCoverage {
            push: match self.push {
                PushSource::None => PushKind::None,
                PushSource::Hooks(_) => PushKind::Hooks,
            },
            pull: match self.pull {
                PullSource::None => PullKind::None,
                PullSource::NativeTurnCompletion(_) => PullKind::NativeTurnCompletion,
            },
            turn_identity: self.turn_identity,
            owned_work: match self.owned_work {
                OwnedWorkCoverage::Unavailable { .. } => OwnedWorkKind::Unavailable,
                OwnedWorkCoverage::HookRegistry => OwnedWorkKind::HookRegistry,
            },
            final_report: self.final_report,
            passive_watcher: self.passive_watcher,
        }
    }

    /// Parse a hook with this harness's own contract. A harness without hooks
    /// never produces a receipt, whatever the payload looks like.
    pub fn parse_hook(&self, value: &serde_json::Value) -> Option<NativeHook> {
        (self.hooks()?.parse)(value)
    }
}

/// The harness that owns a stored node provider string. Profiles and proxied
/// provider accounts resolve to their executing harness; unknown strings stay
/// unknown rather than falling back to the legacy Anthropic executor, because
/// a strategy grants evidence authority.
pub fn provider_for_stored(stored: &str) -> Option<crate::models::Provider> {
    let selected = crate::preferences::launch_configurations::selection_option(stored)
        .unwrap_or_else(|_| stored.to_owned());
    provider_for_selection(&selected, |harness| {
        crate::preferences::harness_profiles()
            .into_iter()
            .find(|profile| profile.id == harness)
            .map(|profile| profile.harness)
    })
}

pub(crate) fn provider_for_selection(
    selected: &str,
    profile_harness: impl FnOnce(&str) -> Option<String>,
) -> Option<crate::models::Provider> {
    let option = crate::agent::provider::SpawnOptionId::from(selected);
    let harness = option.harness_id();
    let executor = profile_harness(harness).unwrap_or_else(|| harness.to_owned());
    provider_for_id(&executor)
}

/// A harness id or legacy alias (`claude_code`, `antigravity`, ...) → its
/// adapter. Unknown ids stay unknown.
pub(crate) fn provider_for_id(id: &str) -> Option<crate::models::Provider> {
    crate::models::Provider::try_from_db_str(id).or_else(|| {
        super::compatibility::resolve_harness_adapter_id(id)
            .and_then(crate::models::Provider::try_from_db_str)
    })
}

/// The strategy for a stored provider string; unknown harnesses are unwired.
pub fn for_stored(stored: &str) -> ObservationStrategy {
    provider_for_stored(stored).map_or(ObservationStrategy::UNWIRED, |provider| {
        provider.adapter().circuit_observation()
    })
}

/// How to find the harness that runs one agent. A launch snapshot freezes the
/// executor at launch, so it never consults today's preferences: remapping a
/// profile afterwards cannot change which harness an existing node is read as.
/// A node without a snapshot resolves its stored provider through the current
/// profiles, which reads preferences and so must not run while a database
/// connection is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessSelector {
    /// The executor id frozen in the node's resolved launch plan.
    Frozen(String),
    /// The stored provider string: a harness id, profile id or `harness:account`.
    Stored(String),
}

impl HarnessSelector {
    pub fn provider(&self) -> Option<crate::models::Provider> {
        match self {
            HarnessSelector::Frozen(executor) => provider_for_id(executor),
            HarnessSelector::Stored(stored) => provider_for_stored(stored),
        }
    }

    pub fn strategy(&self) -> ObservationStrategy {
        self.provider()
            .map_or(ObservationStrategy::UNWIRED, |provider| {
                provider.adapter().circuit_observation()
            })
    }
}

/// Pure over the agent row: safe to call while a database connection is held.
pub fn selector_for_agent(agent: &crate::models::AgentNode) -> HarnessSelector {
    match agent
        .launch_configuration
        .as_ref()
        .and_then(|configuration| configuration.resolved.as_ref())
    {
        Some(plan) => HarnessSelector::Frozen(plan.harness.harness.clone()),
        None => HarnessSelector::Stored(agent.provider.clone()),
    }
}

/// The harness that runs this agent; `None` when it is unknown.
pub fn provider_for_agent(agent: &crate::models::AgentNode) -> Option<crate::models::Provider> {
    selector_for_agent(agent).provider()
}

/// The strategy for one agent node.
pub fn for_agent(agent: &crate::models::AgentNode) -> ObservationStrategy {
    selector_for_agent(agent).strategy()
}

/// The strategy that parsed a persisted hook, for replay. Receipts store the
/// adapter id, so resolution never consults mutable profile preferences.
pub fn for_recorded_adapter(adapter_id: Option<&str>) -> ObservationStrategy {
    adapter_id
        .and_then(provider_for_id)
        .map_or(ObservationStrategy::UNWIRED, |provider| {
            provider.adapter().circuit_observation()
        })
}

/// The pull a completion observation came from, identified by the source its
/// own harness declared. Used to re-issue a recheck only for the harness that
/// produced the completion.
pub fn native_pull_for_source(source: &str) -> Option<NativePull> {
    crate::models::Provider::all().iter().find_map(|provider| {
        provider
            .adapter()
            .circuit_observation()
            .native_pull()
            .filter(|pull| pull.source == source)
            .copied()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Provider;
    use serde_json::json;

    fn claude_style_stop() -> serde_json::Value {
        json!({"hook_event_name":"Stop","session_id":"session","prompt_id":"turn-1",
            "last_assistant_message":"Final report"})
    }

    fn agy_stop() -> serde_json::Value {
        json!({"hookEventName":"Stop","conversationId":"550e8400-e29b-41d4-a716-446655440000",
            "executionNum":3,"fullyIdle":false,"terminationReason":"model_stop"})
    }

    #[test]
    fn declarations_agree_with_the_capabilities_the_node_already_advertises() {
        // Node capabilities, the Circuit declaration and the executed strategy
        // used to be separate inventories. A hook or pull strategy needs the
        // matching node-level capability, or the harness would advertise
        // evidence it cannot produce.
        for provider in Provider::all() {
            let adapter = provider.adapter();
            let strategy = adapter.circuit_observation();
            let caps = adapter.capabilities();
            let id = adapter.id();
            if strategy.hooks().is_some() {
                assert!(
                    caps.requires_attention_hook,
                    "{id}: hooks need an attention hook"
                );
            }
            if strategy.native_pull().is_some() {
                assert!(
                    caps.produces_readable_transcript,
                    "{id}: a pull needs a readable record"
                );
            }
            assert_eq!(
                caps.supports_passive_turn_watcher,
                strategy.passive_watcher.is_some(),
                "{id}: the watcher capability is derived from the declaration"
            );
            if strategy.passive_watcher.is_some() {
                assert!(
                    !caps.requires_attention_hook,
                    "{id}: a watcher stands in for a hook"
                );
            }
            let budget = strategy.reconciliation.yielded_budget_ms;
            assert!(
                (1..=90_000).contains(&budget),
                "{id}: bounded reconciliation"
            );
            let coverage = strategy.coverage();
            assert_eq!(
                coverage.push == PushKind::Hooks,
                strategy.hooks().is_some(),
                "{id}"
            );
            assert_eq!(
                coverage.pull == PullKind::NativeTurnCompletion,
                strategy.native_pull().is_some(),
                "{id}"
            );
        }
    }

    #[test]
    fn only_claude_antigravity_and_codex_wire_a_circuit_native_source_today() {
        // Guards against silently widening authority: exactly these harnesses
        // declare a Circuit-native source. Missing evidence for the rest
        // (MiniMax Code, OpenCode, Grok, ...) stays explicit rather than
        // inherited from a neighbour.
        let wired: Vec<(&str, PushKind, PullKind)> = Provider::all()
            .iter()
            .map(|p| {
                (
                    p.adapter().id(),
                    p.adapter().circuit_observation().coverage(),
                )
            })
            .filter(|(_, c)| c.push != PushKind::None || c.pull != PullKind::None)
            .map(|(id, c)| (id, c.push, c.pull))
            .collect();
        assert_eq!(
            wired,
            vec![
                ("anthropic", PushKind::Hooks, PullKind::None),
                ("agy", PushKind::Hooks, PullKind::None),
                ("codex", PushKind::Hooks, PullKind::NativeTurnCompletion),
            ]
        );
        for id in ["opencode", "grok", "mcode", "cline"] {
            let strategy = for_stored(id);
            assert!(!strategy.has_foreground_source(), "{id}");
            assert_eq!(
                strategy.coverage().owned_work,
                OwnedWorkKind::Unavailable,
                "{id}"
            );
        }
    }

    #[test]
    fn hooks_are_parsed_only_by_the_harness_that_declares_them() {
        // The same payloads reach every harness. Those without a declared hook
        // never produce a receipt, whatever the payload looks like.
        for provider in Provider::all() {
            let adapter = provider.adapter();
            let strategy = adapter.circuit_observation();
            for payload in [claude_style_stop(), agy_stop()] {
                let hook = strategy.parse_hook(&payload);
                if strategy.hooks().is_none() {
                    assert!(
                        hook.is_none(),
                        "{} must not parse another harness's hook",
                        adapter.id()
                    );
                } else if let Some(hook) = hook {
                    assert_eq!(
                        hook.provider.as_deref(),
                        Some(adapter.id()),
                        "a parsed hook always names the harness that parsed it"
                    );
                }
            }
        }
    }

    #[test]
    fn a_foreign_payload_never_inherits_antigravity_yield_semantics() {
        // An Antigravity-shaped Stop delivered to a Claude or Codex node is
        // read with that node's contract: it cannot carry Antigravity's
        // background-busy yield or its execution counter.
        for id in ["anthropic", "codex"] {
            let hook = for_stored(id)
                .parse_hook(&agy_stop())
                .expect("the shared contract accepts a Stop");
            assert_eq!(hook.provider.as_deref(), Some(id));
            assert!(!hook.background_busy);
            assert_eq!(hook.execution_num, None);
            assert_eq!(hook.termination_reason, None);
        }
        // And a Claude-style Stop is malformed for Antigravity: no UUID
        // conversation id, so it is never turn evidence for the node.
        assert!(for_stored("agy").parse_hook(&claude_style_stop()).is_none());
    }

    #[test]
    fn profiles_and_proxied_accounts_resolve_to_the_harness_that_executes() {
        let profiles = |id: &str| match id {
            "deepseek-via-claude" => Some("claude".to_owned()),
            "my-codex" => Some("codex".to_owned()),
            "custom-opencode" => Some("opencode".to_owned()),
            "ghost" => Some("future-harness".to_owned()),
            _ => None,
        };
        let resolve =
            |selected: &str| provider_for_selection(selected, profiles).map(|p| p.adapter().id());
        assert_eq!(resolve("claude:minimax"), Some("anthropic"));
        assert_eq!(resolve("deepseek-via-claude"), Some("anthropic"));
        assert_eq!(resolve("deepseek-via-claude:account"), Some("anthropic"));
        assert_eq!(resolve("my-codex"), Some("codex"));
        assert_eq!(resolve("codex:openrouter"), Some("codex"));
        assert_eq!(resolve("custom-opencode"), Some("opencode"));
        assert_eq!(resolve("claude_code"), Some("anthropic"));
        // Unknown stays unknown: a strategy grants evidence authority, so the
        // legacy "unknown means Anthropic" fallback must not reach it.
        assert_eq!(resolve("ghost"), None);
        assert_eq!(resolve("future-harness:account"), None);
        assert_eq!(resolve(""), None);
    }

    #[test]
    fn a_proxied_claude_account_uses_claudes_contract_and_an_opencode_account_gets_none() {
        let hook = for_stored("claude:minimax")
            .parse_hook(&claude_style_stop())
            .expect("claude contract");
        assert_eq!(hook.provider.as_deref(), Some("anthropic"));
        assert_eq!(hook.final_report.as_deref(), Some("Final report"));
        assert!(for_stored("opencode:account")
            .parse_hook(&claude_style_stop())
            .is_none());
        assert!(for_stored("future-harness")
            .parse_hook(&claude_style_stop())
            .is_none());
    }

    #[test]
    fn a_recorded_hook_resolves_the_strategy_that_parsed_it() {
        assert!(for_recorded_adapter(Some("codex")).native_pull().is_some());
        assert!(for_recorded_adapter(Some("anthropic"))
            .native_pull()
            .is_none());
        // Receipts written before adapter ids were normalized used the alias.
        assert!(for_recorded_adapter(Some("claude")).hooks().is_some());
        assert!(for_recorded_adapter(Some("claude_code")).hooks().is_some());
        // Missing or unknown provenance is unwired, never Claude's contract.
        assert!(for_recorded_adapter(None).hooks().is_none());
        assert!(for_recorded_adapter(Some("future-harness"))
            .hooks()
            .is_none());
    }

    #[test]
    fn a_pull_is_recognised_only_by_the_source_its_own_harness_declared() {
        let codex = for_stored("codex");
        let pull = codex.native_pull().expect("codex pull");
        let found = native_pull_for_source(pull.source).expect("declared source");
        assert_eq!(found.label, "Codex");
        assert!(native_pull_for_source(pull.recheck_source).is_none());
        assert!(native_pull_for_source("claude_native_hook").is_none());
        assert!(native_pull_for_source("").is_none());
    }
}
