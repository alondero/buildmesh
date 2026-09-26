//! Identity and provenance shared by Circuit observers. No harness payloads are
//! parsed at this boundary; adapters may assert only facts they can establish.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitObservationIdentity.ts")]
pub struct ObservationIdentity {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub step_id: String,
    pub attempt: i32,
    #[ts(as = "i32")]
    pub agent_node_id: i64,
    pub session_incarnation: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub report_revision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "ObservationDisposition.ts")]
pub enum ObservationDisposition {
    Accepted,
    ReducedConfidence,
    Duplicate,
    Rejected,
    Unavailable,
    Conflicting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "ObservedWorkFact.ts")]
pub enum ObservedWorkFact {
    Working,
    Yielded,
    NeedsInput,
    PermissionRequested,
    QuestionRequested,
    HumanWaitRequested { wait_kind: HumanWaitKind, request_id: String },
    HumanResponse { wait_kind: HumanWaitKind, request_id: String },
    ToolResponse { wait_kind: HumanWaitKind, request_id: String },
    ToolFailed { wait_kind: HumanWaitKind, request_id: String },
    ForegroundTerminated,
    ForegroundReconciled { conflict_id: String },
    OwnedStarted { work_id: String },
    OwnedTerminated { work_id: String },
    OwnershipCovered,
    OwnershipUnavailable { reason: String },
    OwnershipSnapshot { active_work: Vec<String> },
    AssistantReport { text: String, revision: String },
    AssignedWorkCompleted,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitObservation.ts")]
pub struct CircuitObservation {
    pub identity: ObservationIdentity,
    pub source: String,
    pub source_id: Option<String>,
    #[ts(as = "i32")]
    pub observed_at_ms: i64,
    pub authoritative: bool,
    pub fact: ObservedWorkFact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "RecordedCircuitObservation.ts")]
pub struct RecordedObservation {
    pub observation: CircuitObservation,
    pub disposition: ObservationDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "ReportInterpretation.ts")]
pub enum ReportInterpretation { Completed, Blocked, Working, Continue, InsufficientEvidence }

impl From<Option<crate::autopilot::evaluator::Classification>> for ReportInterpretation {
    fn from(value: Option<crate::autopilot::evaluator::Classification>) -> Self {
        use crate::autopilot::evaluator::Classification as C;
        match value { Some(C::Completed) => Self::Completed, Some(C::Blocked) => Self::Blocked,
            Some(C::Working) => Self::Working, Some(C::Continue) => Self::Continue, None => Self::InsufficientEvidence }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "RecordedClassification.ts")]
pub struct RecordedClassification {
    pub step_id: String,
    pub attempt: i32,
    pub interpretation: ReportInterpretation,
    pub report_revision: Option<String>,
    pub evidence_owner: Option<ObservationIdentity>,
    pub lifecycle_verified: bool,
    pub report_text: Option<String>,
    pub report_completeness: ReportCompleteness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "ReportCompleteness.ts")]
pub enum ReportCompleteness {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEvidence {
    pub text: String,
    pub revision: String,
    #[serde(default)]
    pub input_stamp: Option<String>,
    #[serde(default)]
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "HumanWaitKind.ts")]
pub enum HumanWaitKind { Input, Permission, Question, ReviewApproval }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitHumanWait.ts")]
pub struct HumanWait {
    pub wait_kind: HumanWaitKind,
    pub request_id: Option<String>,
    pub identity: ObservationIdentity,
    pub source: String,
    pub source_id: Option<String>,
    #[ts(as = "i32")]
    pub observed_at_ms: i64,
    pub authoritative: bool,
    #[ts(as = "Option<i32>")]
    pub resolved_at_ms: Option<i64>,
}

impl HumanWait {
    fn matches_request(&self, kind: HumanWaitKind, request_id: &Option<String>, identity: &ObservationIdentity) -> bool {
        self.wait_kind == kind && &self.request_id == request_id
            && self.identity.run_id == identity.run_id && self.identity.step_id == identity.step_id
            && self.identity.attempt == identity.attempt && self.identity.agent_node_id == identity.agent_node_id
            && self.identity.session_incarnation == identity.session_incarnation
            && self.identity.session_id == identity.session_id && self.identity.turn_id == identity.turn_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceConflictKind {
    Foreground,
    OwnedWork { work_id: String },
    Identity,
    LegacyUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceConflict {
    pub id: String,
    pub kind: EvidenceConflictKind,
    pub identity: ObservationIdentity,
    pub observed_at_ms: i64,
}

/// Persisted per attempt. An ownership item never disappears on parent yield.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkEvidence {
    pub identity: Option<ObservationIdentity>,
    #[serde(default)]
    pub human_waits: Vec<HumanWait>,
    #[serde(default)]
    pub report: Option<ReportEvidence>,
    #[serde(default)]
    pub lifecycle_invalidated: bool,
    pub foreground_terminated: bool,
    pub assignment_completed: bool,
    pub ownership_covered: bool,
    pub children: std::collections::BTreeMap<String, bool>,
    pub conflicted: bool,
    #[serde(default)]
    pub conflicts: Vec<EvidenceConflict>,
    pub latest: Option<CircuitObservation>,
    pub sources: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub source_watermarks: std::collections::BTreeMap<String, i64>,
}

impl WorkEvidence {
    fn record_conflict(&mut self, kind: EvidenceConflictKind, event: &CircuitObservation) {
        use sha2::{Digest, Sha256};
        // Old persisted booleans carry no resolvable cause. Keep that uncertainty.
        if self.conflicted && self.conflicts.is_empty() {
            self.conflicts.push(EvidenceConflict { id: "legacy-unknown".into(), kind: EvidenceConflictKind::LegacyUnknown,
                identity: event.identity.clone(), observed_at_ms: event.observed_at_ms });
        }
        let key = serde_json::to_vec(&(&event.source, &event.source_id, &event.identity, event.observed_at_ms, &kind)).expect("conflict identity");
        let id = hex::encode(Sha256::digest(key));
        if !self.conflicts.iter().any(|conflict| conflict.id == id) {
            self.conflicts.push(EvidenceConflict { id, kind, identity: event.identity.clone(), observed_at_ms: event.observed_at_ms });
        }
        self.conflicted = true;
    }

    pub fn has_human_wait(&self) -> bool {
        // Old ledgers also contain inferred waits from AwaitingInput, which
        // several harnesses use for ordinary yields. Only a request can wait
        // indefinitely; a status projection has no request to answer.
        self.human_waits.iter().any(|wait| wait.resolved_at_ms.is_none() && wait.source != "agent_status_projection")
    }

    pub fn completion_verified(&self) -> bool {
        self.lifecycle_verified() && self.assignment_completed
    }

    pub fn lifecycle_verified(&self) -> bool {
        !self.has_human_wait()
            && self.foreground_terminated
            && self.ownership_covered
            && self.children.values().all(|terminal| *terminal)
            && !self.conflicted
            && !self.lifecycle_invalidated
    }

    pub fn observe(
        &mut self,
        expected: &ObservationIdentity,
        event: &CircuitObservation,
    ) -> ObservationDisposition {
        let observed = &event.identity;
        let owned_terminal = matches!(&event.fact, ObservedWorkFact::OwnedTerminated { work_id } if self.children.contains_key(work_id));
        let mismatch = expected.run_id != observed.run_id
            || expected.step_id != observed.step_id
            || expected.attempt != observed.attempt
            || expected.agent_node_id != observed.agent_node_id
            || [
                (&expected.session_incarnation, &observed.session_incarnation),
                (&expected.session_id, &observed.session_id),
                (&expected.report_revision, &observed.report_revision),
            ]
            .iter()
            .any(|(a, b)| a.as_ref().zip(b.as_ref()).is_some_and(|(a, b)| a != b))
            || (!owned_terminal
                && expected
                    .turn_id
                    .as_ref()
                    .zip(observed.turn_id.as_ref())
                    .is_some_and(|(a, b)| a != b));
        if mismatch {
            return ObservationDisposition::Rejected;
        }
        if matches!(event.fact, ObservedWorkFact::OwnedTerminated { .. })
            && !owned_terminal
            && self
                .identity
                .as_ref()
                .and_then(|known| known.turn_id.as_ref())
                .zip(observed.turn_id.as_ref())
                .is_some_and(|(a, b)| a != b)
        {
            return ObservationDisposition::Rejected;
        }
        let mut identity = expected.clone();
        for (token, incoming, known) in [
            (
                &mut identity.session_incarnation,
                &observed.session_incarnation,
                self.identity
                    .as_ref()
                    .and_then(|i| i.session_incarnation.as_ref()),
            ),
            (
                &mut identity.session_id,
                &observed.session_id,
                self.identity.as_ref().and_then(|i| i.session_id.as_ref()),
            ),
            (
                &mut identity.turn_id,
                &observed.turn_id,
                self.identity.as_ref().and_then(|i| i.turn_id.as_ref()),
            ),
            (
                &mut identity.report_revision,
                &observed.report_revision,
                self.identity
                    .as_ref()
                    .and_then(|i| i.report_revision.as_ref()),
            ),
        ] {
            if token.is_none() {
                *token = incoming.as_ref().or(known).cloned();
            }
        }
        // Owned work may end in a later foreground turn. Its terminal receipt
        // closes that identity without rewinding the current foreground state.
        if owned_terminal {
            identity.turn_id = self
                .identity
                .as_ref()
                .and_then(|known| known.turn_id.clone())
                .or(identity.turn_id);
        }
        if self.identity.as_ref().is_some_and(|old| {
            [
                (&old.session_incarnation, &identity.session_incarnation),
                (&old.session_id, &identity.session_id),
            ]
            .iter()
            .any(|(a, b)| a.as_ref().zip(b.as_ref()).is_some_and(|(a, b)| a != b))
        }) {
            self.record_conflict(EvidenceConflictKind::Identity, event);
            return ObservationDisposition::Conflicting;
        }
        let key = event.source_id.as_ref().map(|id| {
            serde_json::to_string(&(
                &event.source,
                id,
                &observed.session_incarnation,
                &observed.turn_id,
            ))
            .expect("string tuple")
        });
        if key
            .as_ref()
            .is_some_and(|key| self.sources.contains_key(key))
        {
            return ObservationDisposition::Duplicate;
        }
        if key.is_none()
            && self.latest.as_ref().is_some_and(|last| {
                last.source == event.source
                    && last.identity == event.identity
                    && last.fact == event.fact
                    && last.authoritative == event.authoritative
            })
        {
            return ObservationDisposition::Duplicate;
        }
        if self
            .source_watermarks
            .get(&event.source)
            .is_some_and(|last| event.observed_at_ms < *last)
        {
            return ObservationDisposition::Rejected;
        }
        if self.identity.as_ref().is_some_and(|old| {
            old.turn_id
                .as_ref()
                .zip(identity.turn_id.as_ref())
                .is_some_and(|(a, b)| a != b)
        }) {
            self.foreground_terminated = false;
            self.assignment_completed = false;
            self.ownership_covered = false;
            self.report = None;

        }
        self.identity = Some(identity);
        self.source_watermarks
            .insert(event.source.clone(), event.observed_at_ms);
        if let Some(key) = key {
            self.sources.insert(key, event.observed_at_ms);
        }
        self.latest = Some(event.clone());
        if event.fact == ObservedWorkFact::Unavailable {
            self.lifecycle_invalidated = true;
            self.foreground_terminated = false;
            self.ownership_covered = false;
            return ObservationDisposition::Unavailable;
        }
        if matches!(event.fact, ObservedWorkFact::OwnershipUnavailable { .. }) {
            self.lifecycle_invalidated = true;
            self.ownership_covered = false;
            return ObservationDisposition::Unavailable;
        }
        let requested = match &event.fact {
            ObservedWorkFact::NeedsInput => Some((HumanWaitKind::Input, None)),
            ObservedWorkFact::PermissionRequested => Some((HumanWaitKind::Permission, None)),
            ObservedWorkFact::QuestionRequested => Some((HumanWaitKind::Question, None)),
            ObservedWorkFact::HumanWaitRequested { wait_kind, request_id } => {
                Some((*wait_kind, (!request_id.trim().is_empty()).then(|| request_id.clone())))
            }
            _ => None,
        };
        if let Some((wait_kind, request_id)) = requested.filter(|_| event.source != "agent_status_projection") {
            let same_agent = |wait: &HumanWait| {
                wait.identity.run_id == observed.run_id && wait.identity.step_id == observed.step_id
                    && wait.identity.attempt == observed.attempt && wait.identity.agent_node_id == observed.agent_node_id
                    && wait.identity.session_incarnation == observed.session_incarnation
                    && wait.identity.session_id == observed.session_id
            };
            // AwaitingInput is an aggregate projection of native requests, not
            // another request. Replace that inference when its native detail
            // arrives, preserving both source observations in history.
            if request_id.is_some() {
                self.human_waits.retain(|wait| !(same_agent(wait) && wait.source == "agent_status_projection"
                    && wait.request_id.is_none() && wait.resolved_at_ms.is_none()));
            }
            let projection_of_native = event.source == "agent_status_projection"
                && self.human_waits.iter().any(|wait| same_agent(wait) && wait.request_id.is_some()
                    && wait.resolved_at_ms.is_none_or(|resolved| resolved >= event.observed_at_ms));
            // Missing correlation still records an indefinite wait. A later
            // activity projection cannot manufacture the matching answer.
            if !projection_of_native && !self.human_waits.iter().any(|wait| wait.matches_request(wait_kind, &request_id, &event.identity)) {
                self.human_waits.push(HumanWait { wait_kind, request_id,
                    identity: event.identity.clone(), source: event.source.clone(),
                    source_id: event.source_id.clone(), observed_at_ms: event.observed_at_ms,
                    authoritative: event.authoritative, resolved_at_ms: None });
            }
        }
        if !event.authoritative
            || observed.session_incarnation.is_none()
            || observed.session_id.is_none()
            || observed.turn_id.is_none()
        {
            return ObservationDisposition::ReducedConfidence;
        }
        if let ObservedWorkFact::HumanResponse { wait_kind, request_id }
            | ObservedWorkFact::ToolResponse { wait_kind, request_id }
            | ObservedWorkFact::ToolFailed { wait_kind, request_id } = &event.fact {
            let tool_response = matches!(event.fact, ObservedWorkFact::ToolResponse { .. } | ObservedWorkFact::ToolFailed { .. });
            let mut matched = false;
            for wait in &mut self.human_waits {
                let kind = if tool_response && wait.wait_kind == HumanWaitKind::Permission {
                    HumanWaitKind::Permission
                } else { *wait_kind };
                if wait.matches_request(kind, &Some(request_id.clone()), observed)
                    && !request_id.is_empty() && wait.observed_at_ms <= event.observed_at_ms {
                    wait.resolved_at_ms.get_or_insert(event.observed_at_ms);
                    matched = true;
                }
            }
            if !matched { return ObservationDisposition::Rejected; }
        }
        match &event.fact {
            ObservedWorkFact::AssistantReport { text, revision } => {
                self.report = Some(ReportEvidence {
                    text: text.clone(),
                    revision: revision.clone(),
                    input_stamp: None,
                    observed_at_ms: event.observed_at_ms,
                });
                if let Some(identity) = self.identity.as_mut() {
                    identity.report_revision = Some(revision.clone());
                }
            }
            ObservedWorkFact::Working if self.foreground_terminated => self.record_conflict(EvidenceConflictKind::Foreground, event),
            ObservedWorkFact::ForegroundReconciled { conflict_id } => {
                let Some(index) = self.conflicts.iter().position(|conflict| conflict.id == *conflict_id
                    && conflict.kind == EvidenceConflictKind::Foreground
                    && conflict.identity.session_incarnation == observed.session_incarnation
                    && conflict.identity.session_id == observed.session_id
                    && conflict.identity.turn_id == observed.turn_id
                    && conflict.observed_at_ms <= event.observed_at_ms) else { return ObservationDisposition::Rejected; };
                self.conflicts.remove(index);
                self.conflicted = !self.conflicts.is_empty();
                self.foreground_terminated = true;
            },
            ObservedWorkFact::ForegroundTerminated => self.foreground_terminated = true,
            ObservedWorkFact::AssignedWorkCompleted => self.assignment_completed = true,
            ObservedWorkFact::OwnershipCovered => self.ownership_covered = true,
            ObservedWorkFact::OwnershipSnapshot { active_work } => {
                for id in active_work {
                    if self.children.get(id) == Some(&true) {
                        self.record_conflict(EvidenceConflictKind::OwnedWork { work_id: id.clone() }, event);
                    }
                }
                for id in active_work {
                    self.children.insert(id.clone(), false);
                }
                self.ownership_covered = true;
            }
            ObservedWorkFact::OwnedStarted { work_id } => {
                if self.children.get(work_id) == Some(&true) {
                    self.record_conflict(EvidenceConflictKind::OwnedWork { work_id: work_id.clone() }, event);
                }
                self.children.entry(work_id.clone()).or_insert(false);
            }
            ObservedWorkFact::OwnedTerminated { work_id } => {
                self.children.insert(work_id.clone(), true);
            }
            _ => {}
        }
        if matches!(event.fact, ObservedWorkFact::ForegroundTerminated | ObservedWorkFact::OwnershipCovered | ObservedWorkFact::OwnershipSnapshot { .. })
            && self.foreground_terminated && self.ownership_covered {
            self.lifecycle_invalidated = false;
        }
        if self.conflicted {
            ObservationDisposition::Conflicting
        } else {
            ObservationDisposition::Accepted
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_projection_is_not_an_indefinite_human_request() {
        let mut state = WorkEvidence::default();
        let mut projected = event(ObservedWorkFact::NeedsInput, 1);
        projected.source = "agent_status_projection".into();
        projected.authoritative = false;
        projected.identity.turn_id = None;
        state.observe(&identity(), &projected);
        assert!(!state.has_human_wait());
        assert!(!state.completion_verified());

        state.observe(&identity(), &event(ObservedWorkFact::PermissionRequested, 2));
        assert!(state.has_human_wait(), "native requests still require a response");
    }

    #[test]
    fn circuit_human_wait_response_requires_request_kind_and_full_identity() {
        let mut state = WorkEvidence::default();
        for (index, wait_kind) in [HumanWaitKind::Permission, HumanWaitKind::Question].into_iter().enumerate() {
            state.observe(&identity(), &event(ObservedWorkFact::HumanWaitRequested {
                wait_kind, request_id: "request".into() }, index as i64));
        }
        let mut duplicate = event(ObservedWorkFact::HumanWaitRequested {
            wait_kind: HumanWaitKind::Permission, request_id: "request".into() }, 1);
        duplicate.source_id = Some("different-delivery".into());
        duplicate.identity.report_revision = Some("new-report".into());
        state.observe(&identity(), &duplicate);
        assert_eq!(state.human_waits.len(), 2, "report revision is not request identity");
        let encoded = serde_json::to_string(&state).unwrap();
        state = serde_json::from_str(&encoded).unwrap();
        let mut response = event(ObservedWorkFact::HumanResponse {
            wait_kind: HumanWaitKind::Permission, request_id: "wrong".into() }, 2);
        assert_eq!(state.observe(&identity(), &response), ObservationDisposition::Rejected);
        assert_eq!(state.human_waits.iter().filter(|w| w.resolved_at_ms.is_none()).count(), 2);
        response = event(ObservedWorkFact::HumanResponse {
            wait_kind: HumanWaitKind::Permission, request_id: "request".into() }, 3);
        response.authoritative = false;
        assert_eq!(state.observe(&identity(), &response), ObservationDisposition::ReducedConfidence);
        assert!(state.human_waits.iter().all(|w| w.resolved_at_ms.is_none()));
        response = event(ObservedWorkFact::HumanResponse {
            wait_kind: HumanWaitKind::Permission, request_id: "request".into() }, 4);
        response.identity.session_incarnation = Some("replacement".into());
        assert_eq!(state.observe(&identity(), &response), ObservationDisposition::Rejected);
        response = event(ObservedWorkFact::HumanResponse {
            wait_kind: HumanWaitKind::Permission, request_id: "request".into() }, 5);
        assert_eq!(state.observe(&identity(), &response), ObservationDisposition::Accepted);
        assert_eq!(state.human_waits[0].resolved_at_ms, Some(5));
        assert!(state.has_human_wait(), "answering permission does not answer the question");
        assert_eq!(state.observe(&identity(), &response), ObservationDisposition::Duplicate);
        state.observe(&identity(), &event(ObservedWorkFact::HumanResponse {
            wait_kind: HumanWaitKind::Question, request_id: "request".into() }, 6));
        assert!(!state.has_human_wait());
        assert!(!state.completion_verified(), "answers never supply lifecycle completion");
    }

    #[test]
    fn owned_child_terminal_from_an_older_turn_preserves_current_foreground_evidence() {
        let mut state = WorkEvidence::default();
        state.observe(
            &identity(),
            &event(
                ObservedWorkFact::OwnedStarted {
                    work_id: "child".into(),
                },
                1,
            ),
        );
        let mut current = identity();
        current.turn_id = Some("next-turn".into());
        for (index, fact) in [
            ObservedWorkFact::Working,
            ObservedWorkFact::ForegroundTerminated,
            ObservedWorkFact::OwnershipCovered,
            ObservedWorkFact::AssignedWorkCompleted,
        ]
        .into_iter()
        .enumerate()
        {
            let mut observation = event(fact, index as i64 + 2);
            observation.identity = current.clone();
            state.observe(&current, &observation);
        }
        assert!(!state.completion_verified());
        let terminal = event(
            ObservedWorkFact::OwnedTerminated {
                work_id: "child".into(),
            },
            6,
        );
        assert_eq!(
            state.observe(&current, &terminal),
            ObservationDisposition::Accepted
        );
        assert_eq!(
            state.identity.as_ref().unwrap().turn_id.as_deref(),
            Some("next-turn")
        );
        assert!(state.completion_verified());
        let unrelated = event(
            ObservedWorkFact::OwnedTerminated {
                work_id: "unowned".into(),
            },
            7,
        );
        assert_eq!(
            state.observe(&current, &unrelated),
            ObservationDisposition::Rejected
        );
        assert!(!state.children.contains_key("unowned"));
    }

    #[test]
    fn empty_registry_does_not_end_previously_owned_work() {
        let mut state = WorkEvidence::default();
        for (index, fact) in [
            ObservedWorkFact::OwnedStarted {
                work_id: "child".into(),
            },
            ObservedWorkFact::ForegroundTerminated,
            ObservedWorkFact::AssignedWorkCompleted,
            ObservedWorkFact::OwnershipSnapshot {
                active_work: vec![],
            },
        ]
        .into_iter()
        .enumerate()
        {
            state.observe(&identity(), &event(fact, index as i64));
        }
        assert!(!state.completion_verified());
        assert_eq!(state.children.get("child"), Some(&false));
        state.observe(
            &identity(),
            &event(
                ObservedWorkFact::OwnedTerminated {
                    work_id: "child".into(),
                },
                5,
            ),
        );
        assert!(state.completion_verified());
    }

    #[test]
    fn unavailable_evidence_requires_fresh_foreground_and_ownership_proof() {
        let mut state = WorkEvidence::default();
        for (index, fact) in [
            ObservedWorkFact::ForegroundTerminated,
            ObservedWorkFact::OwnershipCovered,
            ObservedWorkFact::Unavailable,
            ObservedWorkFact::AssignedWorkCompleted,
        ]
        .into_iter()
        .enumerate()
        {
            state.observe(&identity(), &event(fact, index as i64));
        }
        assert!(!state.completion_verified());
        state.observe(
            &identity(),
            &event(ObservedWorkFact::ForegroundTerminated, 5),
        );
        assert!(!state.completion_verified());
        state.observe(&identity(), &event(ObservedWorkFact::OwnershipCovered, 6));
        assert!(state.completion_verified());
    }

    #[test]
    fn missing_tokens_cannot_splice_completion_across_turns() {
        let mut state = WorkEvidence::default();
        state.observe(
            &identity(),
            &event(ObservedWorkFact::ForegroundTerminated, 1),
        );
        state.observe(
            &identity(),
            &event(ObservedWorkFact::AssignedWorkCompleted, 2),
        );
        let mut missing = identity();
        missing.turn_id = None;
        let mut reduced = event(ObservedWorkFact::Yielded, 3);
        reduced.identity = missing.clone();
        assert_eq!(
            state.observe(&missing, &reduced),
            ObservationDisposition::ReducedConfidence
        );
        assert_eq!(
            state.identity.as_ref().unwrap().turn_id.as_deref(),
            Some("turn")
        );
        let mut next = identity();
        next.turn_id = Some("next-turn".into());
        let mut coverage = event(ObservedWorkFact::OwnershipCovered, 4);
        coverage.identity = next.clone();
        assert_eq!(
            state.observe(&next, &coverage),
            ObservationDisposition::Accepted
        );
        assert!(!state.completion_verified());
        assert!(!state.foreground_terminated);
        assert!(!state.assignment_completed);
    }

    #[test]
    fn independent_source_delay_does_not_discard_owned_terminal_evidence() {
        let mut state = WorkEvidence::default();
        state.observe(
            &identity(),
            &event(
                ObservedWorkFact::OwnedStarted {
                    work_id: "child".into(),
                },
                1,
            ),
        );
        state.observe(
            &identity(),
            &event(ObservedWorkFact::ForegroundTerminated, 10),
        );
        let mut terminal = event(
            ObservedWorkFact::OwnedTerminated {
                work_id: "child".into(),
            },
            5,
        );
        terminal.source = "child-adapter".into();
        assert_eq!(
            state.observe(&identity(), &terminal),
            ObservationDisposition::Accepted
        );
        assert_eq!(state.children.get("child"), Some(&true));
    }

    fn identity() -> ObservationIdentity {
        ObservationIdentity {
            run_id: 1,
            step_id: "work".into(),
            attempt: 2,
            agent_node_id: 9,
            session_incarnation: Some("spawn-2".into()),
            session_id: Some("session".into()),
            turn_id: Some("turn".into()),
            report_revision: None,
        }
    }
    fn event(fact: ObservedWorkFact, sequence: i64) -> CircuitObservation {
        CircuitObservation {
            identity: identity(),
            source: "fixture-adapter".into(),
            source_id: Some(sequence.to_string()),
            observed_at_ms: sequence,
            authoritative: true,
            fact,
        }
    }

    #[test]
    fn yield_and_parent_termination_cannot_hide_active_owned_work() {
        let mut state = WorkEvidence::default();
        for (i, fact) in [
            ObservedWorkFact::OwnedStarted {
                work_id: "child-7".into(),
            },
            ObservedWorkFact::Yielded,
            ObservedWorkFact::ForegroundTerminated,
            ObservedWorkFact::AssignedWorkCompleted,
            ObservedWorkFact::OwnershipCovered,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                state.observe(&identity(), &event(fact, i as i64)),
                ObservationDisposition::Accepted
            );
            assert!(!state.completion_verified());
        }
        let json = serde_json::to_string(&state).unwrap();
        state = serde_json::from_str(&json).unwrap();
        let terminal = event(
            ObservedWorkFact::OwnedTerminated {
                work_id: "child-7".into(),
            },
            6,
        );
        assert_eq!(
            state.observe(&identity(), &terminal),
            ObservationDisposition::Accepted
        );
        assert!(state.completion_verified());
        assert_eq!(
            state.observe(&identity(), &terminal),
            ObservationDisposition::Duplicate
        );
    }

    #[test]
    fn missing_optional_identity_is_retained_but_explicit_mismatch_is_rejected() {
        let mut state = WorkEvidence::default();
        let mut observation = event(ObservedWorkFact::ForegroundTerminated, 1);
        observation.identity.session_id = None;
        assert_eq!(
            state.observe(&identity(), &observation),
            ObservationDisposition::ReducedConfidence
        );
        assert_eq!(state.latest, Some(observation.clone()));
        assert!(!state.foreground_terminated);
        observation.identity.attempt = 1;
        assert_eq!(
            state.observe(&identity(), &observation),
            ObservationDisposition::Rejected
        );
        observation.identity = identity();
        observation.identity.session_id = Some("other-session".into());
        assert_eq!(
            state.observe(&identity(), &observation),
            ObservationDisposition::Rejected
        );
    }

    #[test]
    fn circuit_foreground_reconciliation_is_revision_scoped_and_preserves_other_obligations() {
        let mut state = WorkEvidence::default();
        state.observe(&identity(), &event(ObservedWorkFact::ForegroundTerminated, 1));
        state.observe(&identity(), &event(ObservedWorkFact::Working, 2));
        let conflict_id = state.conflicts[0].id.clone();
        state.observe(&identity(), &event(ObservedWorkFact::OwnedTerminated { work_id: "child".into() }, 3));
        state.observe(&identity(), &event(ObservedWorkFact::OwnedStarted { work_id: "child".into() }, 4));
        state.observe(&identity(), &event(ObservedWorkFact::HumanWaitRequested { wait_kind: HumanWaitKind::Permission, request_id: "approval".into() }, 5));
        state = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        let mut reply = event(ObservedWorkFact::ForegroundReconciled { conflict_id: conflict_id.clone() }, 6);
        reply.authoritative = false;
        assert_eq!(state.observe(&identity(), &reply), ObservationDisposition::ReducedConfidence);
        assert_eq!(state.conflicts.len(),2);
        reply.authoritative = true;
        reply.source_id = Some("authoritative-recheck".into());
        assert_eq!(state.observe(&identity(), &reply), ObservationDisposition::Conflicting);
        assert_eq!(state.conflicts.len(),1);
        assert!(matches!(state.conflicts[0].kind, EvidenceConflictKind::OwnedWork { .. }));
        assert!(state.has_human_wait());
        assert!(!state.completion_verified());
        assert_eq!(state.observe(&identity(), &reply), ObservationDisposition::Duplicate);
        reply.source_id = Some("old-conflict-again".into());
        assert_eq!(state.observe(&identity(), &reply), ObservationDisposition::Rejected);
        let mut legacy = WorkEvidence { conflicted: true, ..Default::default() };
        assert_eq!(legacy.observe(&identity(), &reply), ObservationDisposition::Rejected);
        assert!(legacy.conflicted);
    }

    #[test]
    fn conflicting_lifecycle_does_not_use_last_arrival_as_truth() {
        let mut state = WorkEvidence::default();
        state.observe(
            &identity(),
            &event(ObservedWorkFact::ForegroundTerminated, 1),
        );
        assert_eq!(
            state.observe(&identity(), &event(ObservedWorkFact::Working, 2)),
            ObservationDisposition::Conflicting
        );
        assert!(!state.completion_verified());
    }
}
