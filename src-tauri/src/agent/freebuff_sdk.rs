//! Controlled Freebuff SDK runs (issue #2638).
//!
//! The interactive `freebuff` CLI is a native launcher. It does not ship
//! `@codebuff/sdk`, and its terminal session has no print-mode stream.
//! This module is the separate execution path: persist the JSON `RunState`
//! a successful `client.run()` returns, then fold the `handleEvent` print-mode
//! events into Circuit lifecycle facts and a report.
//!
//! It does not launch the SDK. Installed CLI users do not have that package,
//! and `CodebuffClient` requires an API key or bring-your-own-key config. The
//! CLI bearer in `credentials.json` is not that key. See
//! `docs/learning/freebuff-sdk-surface.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::agent::managed_config::atomic_write;
use crate::agent::session_lifecycle::LifecycleKind;
use crate::circuit::observation::ObservedWorkFact;
use crate::circuit::strategy::submission_digest;

/// Print-mode event `type` values from Freebuff `common/src/types/print-mode.ts`
/// on 2026-10-28. An event outside this set can hide a child-agent transition,
/// so the fold refuses to claim a child registry.
pub const PRINT_MODE_EVENT_TYPES: &[&str] = &[
    "download",
    "error",
    "finish",
    "reasoning_delta",
    "start",
    "subagent_finish",
    "subagent_start",
    "text",
    "tool_call",
    "tool_result",
];

const REPORT_CHAR_CAP: usize = 256 * 1024;
const RETRYABLE_ERROR_SOURCES: &[&str] = &["stream-interrupted", "output-limit", "provider-error"];

/// What an installed Freebuff CLI provides relative to the public SDK.
/// Recorded from the CLI package and the SDK client on 2026-10-28.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdkAvailability {
    /// `npm install -g freebuff` launches a native binary. It does not depend
    /// on `@codebuff/sdk`.
    pub bundled_with_installed_cli: bool,
    pub package: &'static str,
    /// `CodebuffClient` throws unless `apiKey`, `CODEBUFF_API_KEY`, or `byok`
    /// is set. It does not read the CLI login by itself.
    pub requires_api_key_or_byok: bool,
    /// The CLI `authToken` is not accepted as the SDK API key.
    pub cli_bearer_is_sdk_api_key: bool,
}

pub fn validated_sdk_availability() -> SdkAvailability {
    SdkAvailability {
        bundled_with_installed_cli: false,
        package: "@codebuff/sdk",
        requires_api_key_or_byok: true,
        cli_bearer_is_sdk_api_key: false,
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SdkRunError {
    #[error("invalid Freebuff run key")]
    InvalidKey,
    #[error("{0}")]
    InvalidRunState(&'static str),
    #[error("could not store Freebuff run state: {0}")]
    Store(String),
}

/// Child agents named by `subagent_start` / `subagent_finish`. Ids are the
/// SDK `agentId` values. Work facts prefix them with `freebuff-agent:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildWork {
    Tracked {
        open: BTreeSet<String>,
        closed: BTreeSet<String>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldedSdkEvents {
    pub facts: Vec<ObservedWorkFact>,
    pub lifecycle: Vec<LifecycleKind>,
    pub child_work: ChildWork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegratedSdkRun {
    /// Run state already on disk before this call. Pass it as `previousRun`.
    pub previous_run: Option<Value>,
    pub persisted_run: Value,
    pub facts: Vec<ObservedWorkFact>,
    pub lifecycle: Vec<LifecycleKind>,
    pub child_work: ChildWork,
}

/// One JSON file per run key under `app_data/freebuff-sdk-runs`.
pub struct DirectoryRunStateStore {
    root: PathBuf,
}

impl DirectoryRunStateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn for_app_data(app_data: &Path) -> Self {
        Self::new(app_data.join("freebuff-sdk-runs"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load(&self, key: &str) -> Result<Option<Value>, SdkRunError> {
        let path = self.path_for(key)?;
        match fs::read_to_string(&path) {
            Ok(body) => serde_json::from_str(&body)
                .map(Some)
                .map_err(|_| SdkRunError::Store("stored run state is not JSON".to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SdkRunError::Store(error.to_string())),
        }
    }

    pub fn save(&self, key: &str, state: &Value) -> Result<(), SdkRunError> {
        let path = self.path_for(key)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| SdkRunError::Store(error.to_string()))?;
        }
        let body =
            serde_json::to_string(state).map_err(|error| SdkRunError::Store(error.to_string()))?;
        atomic_write(&path, &body).map_err(|error| SdkRunError::Store(error.to_string()))
    }

    fn path_for(&self, key: &str) -> Result<PathBuf, SdkRunError> {
        if !valid_run_key(key) {
            return Err(SdkRunError::InvalidKey);
        }
        Ok(self.root.join(format!("{key}.run.json")))
    }
}

fn valid_run_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut len = 1;
    for ch in chars {
        len += 1;
        if len > 128 || !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
            return false;
        }
    }
    true
}

/// Accept only a successful SDK `RunState`. An error output must not become
/// the next `previousRun`.
pub fn validate_run_state(state: &Value) -> Result<(), SdkRunError> {
    let Some(object) = state.as_object() else {
        return Err(SdkRunError::InvalidRunState(
            "Freebuff run state must be a JSON object",
        ));
    };
    let Some(output) = object.get("output").and_then(Value::as_object) else {
        return Err(SdkRunError::InvalidRunState(
            "Freebuff run state is missing an output object",
        ));
    };
    if output.get("type").and_then(Value::as_str) == Some("error") {
        return Err(SdkRunError::InvalidRunState(
            "Freebuff run state output is an error and cannot be resumed",
        ));
    }
    Ok(())
}

pub fn fold_sdk_events(events: &[Value]) -> FoldedSdkEvents {
    let mut fold = Fold::default();
    for event in events {
        fold.push(event);
    }
    fold.finish()
}

/// Load the previous run state, fold this run's events, and store the new
/// state when it can be resumed.
pub fn integrate_controlled_run(
    store: &DirectoryRunStateStore,
    key: &str,
    returned_state: &Value,
    events: &[Value],
) -> Result<IntegratedSdkRun, SdkRunError> {
    validate_run_state(returned_state)?;
    let previous_run = store.load(key)?;
    let folded = fold_sdk_events(events);
    store.save(key, returned_state)?;
    Ok(IntegratedSdkRun {
        previous_run,
        persisted_run: returned_state.clone(),
        facts: folded.facts,
        lifecycle: folded.lifecycle,
        child_work: folded.child_work,
    })
}

#[derive(Default)]
struct Fold {
    root_id: Option<String>,
    saw_root_start: bool,
    root_finished: bool,
    report: String,
    report_truncated: bool,
    open: BTreeSet<String>,
    closed: BTreeSet<String>,
    child_unavailable: Option<String>,
    /// Set when the last event is an `error` with no retry `source`. The SDK
    /// continues after that message, so it fails the run only if nothing follows.
    ended_on_bare_error: bool,
    facts: Vec<ObservedWorkFact>,
    lifecycle: Vec<LifecycleKind>,
}

impl Fold {
    fn push(&mut self, event: &Value) {
        let kind = event
            .as_object()
            .and_then(|object| object.get("type"))
            .and_then(Value::as_str);
        if kind != Some("error") {
            self.ended_on_bare_error = false;
        }
        let Some(object) = event.as_object() else {
            self.invalidate_children(
                "Freebuff event is not a JSON object; child work is unavailable",
            );
            return;
        };
        let Some(kind) = object.get("type").and_then(Value::as_str) else {
            self.invalidate_children("Freebuff event has no type; child work is unavailable");
            return;
        };
        if !PRINT_MODE_EVENT_TYPES.contains(&kind) {
            self.invalidate_children(format!(
                "Freebuff event type {kind} is outside the print-mode contract; child work is unavailable"
            ));
            return;
        }
        match kind {
            "start" => self.on_start(object),
            "text" => self.on_text(object),
            "subagent_start" => self.on_subagent_start(object),
            "subagent_finish" => self.on_subagent_finish(object),
            "finish" => self.on_finish(object),
            "error" => self.on_error(object),
            _ => {}
        }
    }

    fn on_start(&mut self, event: &Map<String, Value>) {
        if self.saw_root_start {
            return;
        }
        self.saw_root_start = true;
        self.root_id = agent_id(event);
        self.facts.push(ObservedWorkFact::Working);
        self.lifecycle.push(LifecycleKind::WorkResumed);
    }

    fn on_text(&mut self, event: &Map<String, Value>) {
        let Some(text) = event.get("text").and_then(Value::as_str) else {
            return;
        };
        if !self.is_root_agent(agent_id(event).as_deref()) {
            return;
        }
        self.append_report(text);
    }

    fn on_subagent_start(&mut self, event: &Map<String, Value>) {
        let Some(id) = agent_id(event) else {
            self.invalidate_children(
                "Freebuff subagent_start omitted agentId; child work is unavailable",
            );
            return;
        };
        if self.closed.contains(&id) {
            self.invalidate_children(format!(
                "Freebuff child {id} started again after it finished; child work is unavailable"
            ));
            return;
        }
        if self.child_unavailable.is_some() || !self.open.insert(id.clone()) {
            return;
        }
        self.facts.push(ObservedWorkFact::OwnedStarted {
            work_id: work_id(&id),
        });
    }

    fn on_subagent_finish(&mut self, event: &Map<String, Value>) {
        let Some(id) = agent_id(event) else {
            self.invalidate_children(
                "Freebuff subagent_finish omitted agentId; child work is unavailable",
            );
            return;
        };
        self.close_child(id);
    }

    fn on_finish(&mut self, event: &Map<String, Value>) {
        if let Some(id) = agent_id(event) {
            if self.open.contains(&id) {
                self.close_child(id);
                return;
            }
            if self.closed.contains(&id) {
                return;
            }
            if self.root_id.as_ref().is_some_and(|root| root != &id) {
                self.invalidate_children(format!(
                    "Freebuff finish named unknown agent {id}; child work is unavailable"
                ));
                return;
            }
        }
        self.finish_root();
    }

    fn on_error(&mut self, event: &Map<String, Value>) {
        // `source` is present only for the three auto-recovered stream endings.
        // An error without one is a tool message the SDK continues past. It
        // fails the run only when the event list ends on it.
        if retryable_error(event) {
            self.ended_on_bare_error = false;
            return;
        }
        self.ended_on_bare_error = true;
    }

    fn close_child(&mut self, id: String) {
        if self.child_unavailable.is_some() {
            return;
        }
        if !self.open.remove(&id) {
            self.invalidate_children(format!(
                "Freebuff child {id} finished without starting; child work is unavailable"
            ));
            return;
        }
        self.closed.insert(id.clone());
        self.facts.push(ObservedWorkFact::OwnedTerminated {
            work_id: work_id(&id),
        });
    }

    fn finish_root(&mut self) {
        if self.root_finished {
            return;
        }
        self.root_finished = true;
        if let Some(text) = self.report_text() {
            let revision = submission_digest(&text);
            self.facts
                .push(ObservedWorkFact::AssistantReport { text, revision });
        }
        if let Some(reason) = self.child_unavailable.clone() {
            self.facts
                .push(ObservedWorkFact::OwnershipUnavailable { reason });
            self.lifecycle.push(LifecycleKind::SignalUnavailable);
            return;
        }
        if !self.open.is_empty() {
            let active_work = self.open.iter().map(|id| work_id(id)).collect();
            self.facts.push(ObservedWorkFact::Yielded);
            self.facts
                .push(ObservedWorkFact::OwnershipSnapshot { active_work });
            self.lifecycle.push(LifecycleKind::BackgroundRunning);
            return;
        }
        self.facts.push(ObservedWorkFact::ForegroundTerminated);
        self.facts.push(ObservedWorkFact::OwnershipSnapshot {
            active_work: Vec::new(),
        });
        self.lifecycle.push(LifecycleKind::TurnCompleted);
    }

    fn finish(mut self) -> FoldedSdkEvents {
        if self.ended_on_bare_error && !self.root_finished {
            self.invalidate_children(
                "Freebuff run ended on an error before finish; child work is unavailable",
            );
            self.facts.push(ObservedWorkFact::Unavailable);
            self.lifecycle.push(LifecycleKind::Error);
        }
        FoldedSdkEvents {
            facts: self.facts,
            lifecycle: self.lifecycle,
            child_work: match self.child_unavailable {
                Some(reason) => ChildWork::Unavailable { reason },
                None => ChildWork::Tracked {
                    open: self.open,
                    closed: self.closed,
                },
            },
        }
    }

    fn is_root_agent(&self, id: Option<&str>) -> bool {
        match (id, self.root_id.as_deref()) {
            (None, _) => true,
            (Some(id), Some(root)) => id == root,
            (Some(id), None) => !self.open.contains(id) && !self.closed.contains(id),
        }
    }

    fn append_report(&mut self, text: &str) {
        if self.report_truncated {
            return;
        }
        let room = REPORT_CHAR_CAP.saturating_sub(self.report.chars().count());
        let incoming = text.chars().count();
        if incoming <= room {
            self.report.push_str(text);
            return;
        }
        self.report.extend(text.chars().take(room));
        const MARKER: &str = "\n[freebuff report truncated]";
        let overflow = self
            .report
            .chars()
            .count()
            .saturating_sub(REPORT_CHAR_CAP.saturating_sub(MARKER.chars().count()));
        if overflow > 0 {
            let keep = self.report.chars().count().saturating_sub(overflow);
            self.report = self.report.chars().take(keep).collect();
        }
        self.report.push_str(MARKER);
        self.report_truncated = true;
    }

    fn report_text(&self) -> Option<String> {
        if self.report.is_empty() {
            None
        } else {
            Some(self.report.clone())
        }
    }

    fn invalidate_children(&mut self, reason: impl Into<String>) {
        if self.child_unavailable.is_none() {
            self.child_unavailable = Some(reason.into());
        }
    }
}

fn agent_id(event: &Map<String, Value>) -> Option<String> {
    event
        .get("agentId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn work_id(agent_id: &str) -> String {
    format!("freebuff-agent:{agent_id}")
}

fn retryable_error(event: &Map<String, Value>) -> bool {
    event
        .get("source")
        .and_then(Value::as_str)
        .is_some_and(|source| RETRYABLE_ERROR_SOURCES.contains(&source))
}

#[cfg(test)]
#[path = "freebuff_sdk_tests.rs"]
mod tests;
