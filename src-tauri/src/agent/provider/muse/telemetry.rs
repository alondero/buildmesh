//! Observed Muse session telemetry from MSP notifications (issue #1680).
//!
//! In-band `session/tokenUsage` and `session/contextUsage` events are local
//! session observations. They are **never** account allowance, remaining
//! quota, reset time, or dollar spend. The public payload is labelled
//! `observed_session_telemetry` so Usage Meters cannot absorb it.
//!
//! The Muse lifecycle task should call [`ingest_line`] for every
//! `MspTransport::events()` notification. Tests feed recorded NDJSON
//! fixtures into [`MuseTelemetryStore`] and assert the public snapshot.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

/// Tauri event emitted when a node's observed telemetry snapshot changes.
pub const MUSE_SESSION_TELEMETRY_EVENT: &str = "muse-session-telemetry";

/// Discriminator so consumers can refuse to treat this as a Usage Meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "ObservedMuseSessionTelemetry.ts")]
pub enum ObservedTelemetryKind {
    ObservedSessionTelemetry,
}

/// Server-computed context pressure. Open on the wire; unknown values map
/// to [`ContextPressureLevel::Unknown`] rather than failing the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "ObservedMuseSessionTelemetry.ts")]
pub enum ContextPressureLevel {
    Normal,
    Warning,
    Blocked,
    Unknown,
}

/// Counted-once per-completion counters plus raw provider cache fields.
/// `prompt_tokens` is the MSP counted-once derivation — not raw
/// `input_tokens`, which may include cache depending on the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "ObservedMuseSessionTelemetry.ts")]
pub struct ObservedTurnUsage {
    pub turn_id: String,
    #[ts(as = "i32")]
    pub prompt_tokens: i64,
    #[ts(as = "i32")]
    pub output_tokens: i64,
    #[ts(as = "i32")]
    pub total_tokens: i64,
    #[ts(as = "i32")]
    pub input_tokens: i64,
    #[ts(as = "i32")]
    pub reasoning_tokens: i64,
    #[ts(as = "i32")]
    pub cached_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<i32>")]
    pub cache_read_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<i32>")]
    pub cache_write_tokens: Option<i64>,
}

/// Session running totals of counted-once usage. Taken from the MSP
/// `cumulative` block; never re-summed from raw `inputTokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "ObservedMuseSessionTelemetry.ts")]
pub struct ObservedCumulativeUsage {
    #[ts(as = "i32")]
    pub prompt_tokens: i64,
    #[ts(as = "i32")]
    pub output_tokens: i64,
    #[ts(as = "i32")]
    pub total_tokens: i64,
}

/// Context-window occupancy. `pressure` is `used / window` when the host
/// reported a window; the window is omitted, never invented, when absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "ObservedMuseSessionTelemetry.ts")]
pub struct ObservedContextUsage {
    #[ts(as = "i32")]
    pub used_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<i32>")]
    pub window_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pressure: Option<f64>,
    pub pressure_level: ContextPressureLevel,
}

/// Public node telemetry payload. Distinct from [`crate::services::usage`]
/// Usage Meters: no remaining quota, reset time, spend, or currency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "ObservedMuseSessionTelemetry.ts")]
pub struct ObservedMuseSessionTelemetry {
    pub kind: ObservedTelemetryKind,
    #[ts(as = "i32")]
    pub node_id: i64,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn: Option<ObservedTurnUsage>,
    pub cumulative: ObservedCumulativeUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ObservedContextUsage>,
}

/// Node-scoped aggregator. Production uses [`global`]; tests construct a
/// local store so parallel `cargo test` workers cannot leak counters.
#[derive(Debug, Default)]
pub struct MuseTelemetryStore {
    entries: HashMap<i64, Entry>,
}

#[derive(Debug, Clone)]
struct Entry {
    session_id: String,
    model_id: Option<String>,
    last_turn: Option<ObservedTurnUsage>,
    cumulative: ObservedCumulativeUsage,
    context: Option<ObservedContextUsage>,
    last_token_cursor: Option<String>,
    last_context_cursor: Option<String>,
}

impl MuseTelemetryStore {
    /// Ingest one NDJSON notification line attributed to a Buildmesh node
    /// and MSP session. Unknown methods, malformed JSON, and session-id
    /// mismatches are dropped. Returns the new snapshot when state changes.
    pub fn ingest_line(
        &mut self,
        node_id: i64,
        attributed_session_id: &str,
        line: &str,
    ) -> Option<ObservedMuseSessionTelemetry> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let notification: JsonRpcNotification = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => {
                tracing::debug!(node_id, "dropped unparseable MSP telemetry line");
                return None;
            }
        };
        match notification.method.as_str() {
            "session/tokenUsage" => {
                let params: SessionTokenUsageParams =
                    match serde_json::from_value(notification.params) {
                        Ok(params) => params,
                        Err(_) => {
                            tracing::debug!(node_id, "dropped malformed session/tokenUsage");
                            return None;
                        }
                    };
                self.apply_token_usage(node_id, attributed_session_id, params)
            }
            "session/contextUsage" => {
                let params: SessionContextUsageParams =
                    match serde_json::from_value(notification.params) {
                        Ok(params) => params,
                        Err(_) => {
                            tracing::debug!(node_id, "dropped malformed session/contextUsage");
                            return None;
                        }
                    };
                self.apply_context_usage(node_id, attributed_session_id, params)
            }
            _ => None,
        }
    }

    /// Feed a recorded NDJSON sequence. Returns the last changed snapshot.
    pub fn ingest_ndjson(
        &mut self,
        node_id: i64,
        attributed_session_id: &str,
        ndjson: &str,
    ) -> Option<ObservedMuseSessionTelemetry> {
        let mut latest = None;
        for line in ndjson.lines() {
            if let Some(snapshot) = self.ingest_line(node_id, attributed_session_id, line) {
                latest = Some(snapshot);
            }
        }
        latest
    }

    pub fn snapshot(&self, node_id: i64) -> Option<ObservedMuseSessionTelemetry> {
        self.entries.get(&node_id).map(|entry| entry.to_public(node_id))
    }

    pub fn forget(&mut self, node_id: i64) {
        self.entries.remove(&node_id);
    }

    fn apply_token_usage(
        &mut self,
        node_id: i64,
        attributed_session_id: &str,
        params: SessionTokenUsageParams,
    ) -> Option<ObservedMuseSessionTelemetry> {
        if params.session_id != attributed_session_id {
            tracing::debug!(
                node_id,
                attributed = attributed_session_id,
                observed = params.session_id.as_str(),
                "dropped session/tokenUsage for a different MSP session"
            );
            return None;
        }
        if params.session_id.is_empty() || params.turn_id.is_empty() {
            return None;
        }
        let entry = self.entry_for(node_id, &params.session_id);
        if entry.last_token_cursor.as_deref() == Some(params.view_cursor.as_str()) {
            return None;
        }
        if params.cumulative.total_tokens < entry.cumulative.total_tokens {
            tracing::debug!(node_id, "dropped session/tokenUsage that would rewind cumulative");
            return None;
        }
        entry.last_token_cursor = Some(params.view_cursor);
        if params.model_id.is_some() {
            entry.model_id = params.model_id;
        }
        entry.cumulative = ObservedCumulativeUsage {
            prompt_tokens: params.cumulative.prompt_tokens,
            output_tokens: params.cumulative.output_tokens,
            total_tokens: params.cumulative.total_tokens,
        };
        entry.last_turn = Some(ObservedTurnUsage {
            turn_id: params.turn_id,
            prompt_tokens: params.prompt_tokens,
            output_tokens: params.usage.output_tokens,
            total_tokens: params.total_tokens,
            input_tokens: params.usage.input_tokens,
            reasoning_tokens: params.usage.reasoning_tokens,
            cached_tokens: params.usage.cached_tokens,
            cache_read_tokens: params.usage.cache_read_tokens,
            cache_write_tokens: params.usage.cache_write_tokens,
        });
        Some(entry.to_public(node_id))
    }

    fn apply_context_usage(
        &mut self,
        node_id: i64,
        attributed_session_id: &str,
        params: SessionContextUsageParams,
    ) -> Option<ObservedMuseSessionTelemetry> {
        if params.session_id != attributed_session_id {
            tracing::debug!(
                node_id,
                attributed = attributed_session_id,
                observed = params.session_id.as_str(),
                "dropped session/contextUsage for a different MSP session"
            );
            return None;
        }
        if params.session_id.is_empty() {
            return None;
        }
        let entry = self.entry_for(node_id, &params.session_id);
        if entry.last_context_cursor.as_deref() == Some(params.view_cursor.as_str()) {
            return None;
        }
        entry.last_context_cursor = Some(params.view_cursor);
        entry.context = Some(ObservedContextUsage {
            used_tokens: params.used_tokens,
            window_tokens: params.window_tokens,
            pressure: pressure_ratio(params.used_tokens, params.window_tokens),
            pressure_level: params
                .pressure
                .map(ContextPressureLevel::from)
                .unwrap_or(ContextPressureLevel::Unknown),
        });
        Some(entry.to_public(node_id))
    }

    fn entry_for(&mut self, node_id: i64, session_id: &str) -> &mut Entry {
        match self.entries.get(&node_id) {
            Some(existing) if existing.session_id == session_id => {}
            _ => {
                self.entries.insert(node_id, Entry::new(session_id));
            }
        }
        self.entries.get_mut(&node_id).expect("just inserted")
    }
}

impl Entry {
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            model_id: None,
            last_turn: None,
            cumulative: ObservedCumulativeUsage {
                prompt_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            context: None,
            last_token_cursor: None,
            last_context_cursor: None,
        }
    }

    fn to_public(&self, node_id: i64) -> ObservedMuseSessionTelemetry {
        ObservedMuseSessionTelemetry {
            kind: ObservedTelemetryKind::ObservedSessionTelemetry,
            node_id,
            session_id: self.session_id.clone(),
            model_id: self.model_id.clone(),
            last_turn: self.last_turn.clone(),
            cumulative: self.cumulative.clone(),
            context: self.context.clone(),
        }
    }
}

fn pressure_ratio(used_tokens: i64, window_tokens: Option<i64>) -> Option<f64> {
    match window_tokens {
        Some(window) if window > 0 => Some(used_tokens as f64 / window as f64),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcNotification {
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionTokenUsageParams {
    session_id: String,
    turn_id: String,
    view_cursor: String,
    #[serde(default)]
    model_id: Option<String>,
    prompt_tokens: i64,
    total_tokens: i64,
    usage: TokenUsage,
    cumulative: CumulativeTokenUsage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenUsage {
    input_tokens: i64,
    output_tokens: i64,
    #[serde(default)]
    reasoning_tokens: i64,
    #[serde(default)]
    cached_tokens: i64,
    #[serde(default)]
    cache_read_tokens: Option<i64>,
    #[serde(default)]
    cache_write_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CumulativeTokenUsage {
    prompt_tokens: i64,
    output_tokens: i64,
    total_tokens: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionContextUsageParams {
    session_id: String,
    view_cursor: String,
    used_tokens: i64,
    #[serde(default)]
    window_tokens: Option<i64>,
    #[serde(default)]
    pressure: Option<WirePressure>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WirePressure {
    Normal,
    Warning,
    Blocked,
    #[serde(other)]
    Unknown,
}

impl From<WirePressure> for ContextPressureLevel {
    fn from(value: WirePressure) -> Self {
        match value {
            WirePressure::Normal => ContextPressureLevel::Normal,
            WirePressure::Warning => ContextPressureLevel::Warning,
            WirePressure::Blocked => ContextPressureLevel::Blocked,
            WirePressure::Unknown => ContextPressureLevel::Unknown,
        }
    }
}

fn global_store() -> &'static Mutex<MuseTelemetryStore> {
    static STORE: OnceLock<Mutex<MuseTelemetryStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(MuseTelemetryStore::default()))
}

static APP: OnceLock<AppHandle> = OnceLock::new();

/// Bind the app handle so ingest can emit [`MUSE_SESSION_TELEMETRY_EVENT`].
pub fn bind_app(app: AppHandle) {
    let _ = APP.set(app);
}

/// Process-wide store used by IPC, Node Digest, and node deletion.
/// Tests of the aggregator construct a local [`MuseTelemetryStore`].
pub fn ingest_line(
    node_id: i64,
    attributed_session_id: &str,
    line: &str,
) -> Option<ObservedMuseSessionTelemetry> {
    let snapshot = {
        let mut store = global_store().lock().unwrap_or_else(|e| e.into_inner());
        store.ingest_line(node_id, attributed_session_id, line)
    };
    if let Some(ref snap) = snapshot {
        if let Some(app) = APP.get() {
            let _ = app.emit(MUSE_SESSION_TELEMETRY_EVENT, snap);
        }
    }
    snapshot
}

pub fn snapshot(node_id: i64) -> Option<ObservedMuseSessionTelemetry> {
    let store = global_store().lock().unwrap_or_else(|e| e.into_inner());
    store.snapshot(node_id)
}

/// Skip the process-wide store unless this is a Muse node. Coordinator
/// digest assembly must not take the telemetry mutex for every harness.
pub fn snapshot_if_muse(provider: &str, node_id: i64) -> Option<ObservedMuseSessionTelemetry> {
    if provider != "muse" {
        return None;
    }
    snapshot(node_id)
}

pub fn forget(node_id: i64) {
    let mut store = global_store().lock().unwrap_or_else(|e| e.into_inner());
    store.forget(node_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_AND_CONTEXT: &str = include_str!("fixtures/token_and_context.jsonl");
    const CACHE_TOKENS: &str = include_str!("fixtures/cache_tokens.jsonl");
    const NODE_A: &str = include_str!("fixtures/concurrent_node_a.jsonl");
    const NODE_B: &str = include_str!("fixtures/concurrent_node_b.jsonl");
    const CONTEXT_WITHOUT_WINDOW: &str = include_str!("fixtures/context_without_window.jsonl");
    const MISMATCHED_SESSION: &str = include_str!("fixtures/mismatched_session.jsonl");

    const QUOTA_SHAPED_KEYS: &[&str] = &[
        "remaining",
        "resetsAt",
        "resets_at",
        "usedPercent",
        "used_percent",
        "spend",
        "monthlySpend",
        "currency",
        "limit",
        "allowance",
        "quota",
    ];

    fn assert_not_quota_shaped(value: &serde_json::Value) {
        let obj = value
            .as_object()
            .expect("public telemetry payload is an object");
        for key in QUOTA_SHAPED_KEYS {
            assert!(
                !obj.contains_key(*key),
                "observed session telemetry must not carry quota-shaped key `{key}`"
            );
        }
        assert_eq!(
            obj.get("kind").and_then(|v| v.as_str()),
            Some("observed_session_telemetry")
        );
    }

    fn ingest(ndjson: &str, node_id: i64, session_id: &str) -> ObservedMuseSessionTelemetry {
        let mut store = MuseTelemetryStore::default();
        store
            .ingest_ndjson(node_id, session_id, ndjson)
            .expect("fixture should produce a snapshot")
    }

    #[test]
    fn token_usage_increments_cumulative_and_turn_counters() {
        let snapshot = ingest(TOKEN_AND_CONTEXT, 16801, "sess-aaaa-1111");
        assert_eq!(snapshot.kind, ObservedTelemetryKind::ObservedSessionTelemetry);
        assert_eq!(snapshot.node_id, 16801);
        assert_eq!(snapshot.session_id, "sess-aaaa-1111");
        assert_eq!(
            snapshot.model_id.as_deref(),
            Some("muse-spark-1.3-contributor")
        );

        let turn = snapshot.last_turn.expect("last turn");
        assert_eq!(turn.turn_id, "turn-2");
        assert_eq!(turn.prompt_tokens, 25);
        assert_eq!(turn.output_tokens, 15);
        assert_eq!(turn.total_tokens, 40);
        assert_ne!(
            turn.prompt_tokens, turn.input_tokens,
            "last-turn prompt is counted-once, not raw input"
        );

        assert_eq!(snapshot.cumulative.prompt_tokens, 45);
        assert_eq!(snapshot.cumulative.output_tokens, 25);
        assert_eq!(snapshot.cumulative.total_tokens, 70);
        assert_ne!(
            snapshot.cumulative.prompt_tokens, turn.prompt_tokens,
            "cumulative session totals stay distinct from the last turn"
        );
    }

    #[test]
    fn context_usage_updates_pressure_ratio() {
        let snapshot = ingest(TOKEN_AND_CONTEXT, 16802, "sess-aaaa-1111");
        let context = snapshot.context.expect("context");
        assert_eq!(context.used_tokens, 96000);
        assert_eq!(context.window_tokens, Some(128000));
        assert_eq!(context.pressure_level, ContextPressureLevel::Warning);
        let pressure = context.pressure.expect("ratio");
        assert!((pressure - 96000.0 / 128000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cache_read_and_write_do_not_inflate_prompt_tokens() {
        let snapshot = ingest(CACHE_TOKENS, 16803, "sess-cache-0001");
        let turn = snapshot.last_turn.expect("last turn");
        assert_eq!(turn.prompt_tokens, 20);
        assert_eq!(turn.input_tokens, 120);
        assert_eq!(turn.cached_tokens, 100);
        assert_eq!(turn.cache_read_tokens, Some(90));
        assert_eq!(turn.cache_write_tokens, Some(10));
        assert_eq!(
            snapshot.cumulative.prompt_tokens, 20,
            "session prompt totals use counted-once cumulative, not raw input + cache"
        );
        assert_ne!(turn.prompt_tokens, turn.input_tokens + turn.cached_tokens);
        assert_ne!(
            turn.prompt_tokens,
            turn.cache_read_tokens.unwrap_or(0) + turn.cache_write_tokens.unwrap_or(0)
        );
    }

    #[test]
    fn concurrent_nodes_keep_isolated_telemetry() {
        let mut store = MuseTelemetryStore::default();
        store.ingest_ndjson(16804, "sess-node-a", NODE_A);
        store.ingest_ndjson(16805, "sess-node-b", NODE_B);

        let a = store.snapshot(16804).expect("node a");
        let b = store.snapshot(16805).expect("node b");
        assert_eq!(a.session_id, "sess-node-a");
        assert_eq!(b.session_id, "sess-node-b");
        assert_eq!(a.cumulative.total_tokens, 16);
        assert_eq!(b.cumulative.total_tokens, 950);
        assert_eq!(a.context.as_ref().unwrap().used_tokens, 1100);
        assert_eq!(b.context.as_ref().unwrap().used_tokens, 9000);
        assert_eq!(
            b.context.as_ref().unwrap().pressure_level,
            ContextPressureLevel::Blocked
        );
        assert_ne!(a.cumulative.total_tokens, b.cumulative.total_tokens);
    }

    #[test]
    fn mismatched_session_id_is_dropped() {
        let mut store = MuseTelemetryStore::default();
        assert!(store
            .ingest_ndjson(16806, "sess-bound-0001", MISMATCHED_SESSION)
            .is_none());
        assert!(store.snapshot(16806).is_none());
    }

    #[test]
    fn missing_window_does_not_invent_a_limit_or_ratio() {
        let snapshot = ingest(CONTEXT_WITHOUT_WINDOW, 16807, "sess-nolimit-0001");
        let context = snapshot.context.as_ref().expect("context");
        assert_eq!(context.used_tokens, 4096);
        assert_eq!(context.window_tokens, None);
        assert_eq!(context.pressure, None);
        assert_eq!(context.pressure_level, ContextPressureLevel::Normal);
        let json = serde_json::to_value(&snapshot).unwrap();
        assert!(json["context"].get("window_tokens").is_none());
        assert!(json["context"].get("pressure").is_none());
    }

    #[test]
    fn public_payload_is_not_quota_shaped() {
        let snapshot = ingest(TOKEN_AND_CONTEXT, 16808, "sess-aaaa-1111");
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_not_quota_shaped(&json);
        assert!(crate::services::usage::catalog::dispatch("muse").is_none());
    }

    #[test]
    fn omitted_or_null_pressure_does_not_drop_context_usage() {
        let omitted = r#"{"jsonrpc":"2.0","method":"session/contextUsage","params":{"sessionId":"sess-aaaa-1111","viewCursor":"c-omit","sourceRange":{"first":1,"last":1,"stream":"s"},"usedTokens":12,"windowTokens":100}}"#;
        let null_pressure = r#"{"jsonrpc":"2.0","method":"session/contextUsage","params":{"sessionId":"sess-aaaa-1111","viewCursor":"c-null","sourceRange":{"first":1,"last":1,"stream":"s"},"usedTokens":12,"windowTokens":100,"pressure":null}}"#;
        for (line, node_id) in [(omitted, 16811), (null_pressure, 16812)] {
            let mut store = MuseTelemetryStore::default();
            let snapshot = store
                .ingest_line(node_id, "sess-aaaa-1111", line)
                .expect("open-wire context usage must ingest without pressure");
            let context = snapshot.context.expect("context");
            assert_eq!(context.used_tokens, 12);
            assert_eq!(context.window_tokens, Some(100));
            assert_eq!(context.pressure_level, ContextPressureLevel::Unknown);
        }
    }

    #[test]
    fn omitted_reasoning_and_cached_tokens_do_not_drop_token_usage() {
        let line = r#"{"jsonrpc":"2.0","method":"session/tokenUsage","params":{"sessionId":"sess-aaaa-1111","turnId":"turn-open","viewCursor":"c-open","sourceRange":{"first":1,"last":1,"stream":"s"},"promptTokens":5,"totalTokens":7,"usage":{"inputTokens":5,"outputTokens":2},"cumulative":{"promptTokens":5,"outputTokens":2,"totalTokens":7}}}"#;
        let mut store = MuseTelemetryStore::default();
        let snapshot = store
            .ingest_line(16813, "sess-aaaa-1111", line)
            .expect("token usage without optional cache/reasoning fields");
        let turn = snapshot.last_turn.expect("last turn");
        assert_eq!(turn.prompt_tokens, 5);
        assert_eq!(turn.output_tokens, 2);
        assert_eq!(turn.reasoning_tokens, 0);
        assert_eq!(turn.cached_tokens, 0);
        assert_eq!(turn.cache_read_tokens, None);
        assert_eq!(turn.cache_write_tokens, None);
    }

    #[test]
    fn snapshot_if_muse_skips_non_muse_providers() {
        assert!(snapshot_if_muse("anthropic", 16814).is_none());
        assert!(snapshot_if_muse("codex", 16814).is_none());
        assert!(snapshot_if_muse("muse", 16814).is_none());
    }

    #[test]
    fn ingest_ndjson_does_not_echo_a_prior_snapshot_when_every_line_is_ignored() {
        let mut store = MuseTelemetryStore::default();
        store.ingest_ndjson(16815, "sess-node-a", NODE_A);
        assert!(store.snapshot(16815).is_some());
        assert!(
            store
                .ingest_ndjson(16815, "sess-node-a", "not-json\n{\"jsonrpc\":\"2.0\",\"method\":\"turn/started\"}\n")
                .is_none(),
            "an unparseable batch must not masquerade as a new observation"
        );
        assert_eq!(store.snapshot(16815).unwrap().cumulative.total_tokens, 16);
    }

    #[test]
    fn unknown_pressure_level_is_preserved_as_unknown() {
        let line = r#"{"jsonrpc":"2.0","method":"session/contextUsage","params":{"sessionId":"sess-aaaa-1111","viewCursor":"c1","sourceRange":{"first":1,"last":1,"stream":"s"},"usedTokens":1,"windowTokens":10,"pressure":"critical"}}"#;
        let mut store = MuseTelemetryStore::default();
        let snapshot = store
            .ingest_line(16810, "sess-aaaa-1111", line)
            .expect("snapshot");
        assert_eq!(
            snapshot.context.expect("context").pressure_level,
            ContextPressureLevel::Unknown
        );
    }

    #[test]
    fn unknown_methods_and_malformed_lines_are_ignored() {
        let mut store = MuseTelemetryStore::default();
        let ndjson = concat!(
            "{\"jsonrpc\":\"2.0\",\"method\":\"turn/started\",\"params\":{\"sessionId\":\"sess-aaaa-1111\"}}\n",
            "not-json\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"session/tokenUsage\",\"params\":{\"sessionId\":\"sess-aaaa-1111\"}}\n",
        );
        assert!(store.ingest_ndjson(16809, "sess-aaaa-1111", ndjson).is_none());
        assert!(store.snapshot(16809).is_none());
    }

    #[test]
    fn redaction_audit_fixtures_carry_no_prompts_or_secrets() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/agent/provider/muse/fixtures");
        let mut scanned = 0usize;
        for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
            let path = entry.expect("dirent").path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            scanned += 1;
            let body = std::fs::read_to_string(&path).expect("fixture readable");
            assert_fixture_redacted(&path, &body);
        }
        assert!(scanned >= 6, "expected the recorded NDJSON fixtures to be present");
    }

    fn assert_fixture_redacted(path: &std::path::Path, body: &str) {
        let lowered = body.to_ascii_lowercase();
        for needle in [
            "bearer ",
            "sk-",
            "api_key",
            "authorization",
            "password",
            "-----begin",
            "secret",
        ] {
            assert!(
                !lowered.contains(needle),
                "{} must not contain `{needle}`",
                path.display()
            );
        }
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("fixture lines are JSON");
            walk_redacted(path, &value);
        }
    }

    fn walk_redacted(path: &std::path::Path, value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    let key_l = key.to_ascii_lowercase();
                    assert!(
                        !matches!(
                            key_l.as_str(),
                            "content" | "text" | "prompt" | "command" | "arguments" | "input"
                                | "auth" | "token" | "apikey" | "api_key"
                        ),
                        "{} must not carry key `{key}`",
                        path.display()
                    );
                    walk_redacted(path, child);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    walk_redacted(path, child);
                }
            }
            serde_json::Value::String(text) => {
                let words = text.split_whitespace().count();
                assert!(
                    words <= 4,
                    "{} string value looks like prose: {text}",
                    path.display()
                );
            }
            _ => {}
        }
    }
}
