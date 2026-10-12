//! Ordered compatibility upgrades for persisted circuit graphs.

use super::model::CircuitNodeKind as K;
use super::model::{
    CircuitBlueprintKind, CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition,
    GithubActionKind, OpenPrPolicy, StepOutcome,
};
use super::model_node_review::LOCAL_APPROVED_MESSAGE;

pub(crate) const LEGACY_LOCAL_APPROVED_MESSAGE: &str =
    "Review approved for {{source.name}} (agent {{source.agent_id}}).";

/// Storage ownership determines which topology changes are applicable.
#[derive(Clone, Copy)]
pub(crate) enum UpgradeTarget {
    /// An authorable graph, including a copy of the local-review preset.
    Blueprint,
    /// The server-owned local-review preset may gain the publication flow.
    Preset,
    /// In-flight legacy review contracts wait until their runs finish.
    ActiveBlueprint { is_preset: bool },
    /// A pinned run keeps its topology apart from the dead feedback route.
    RunSnapshot,
    /// Older runs without a snapshot receive the pre-publication normalizations.
    UnpinnedRun { active: bool },
}

/// Apply applicable upgrades in dependency order and report any change.
pub(crate) fn upgrade(graph: &mut CircuitGraph, target: UpgradeTarget) -> bool {
    if matches!(target, UpgradeTarget::RunSnapshot) {
        return graph.repair_issue_review_feedback_route();
    }
    let is_preset = matches!(
        target,
        UpgradeTarget::Preset | UpgradeTarget::ActiveBlueprint { is_preset: true }
    );
    let mut changed = graph.upgrade_legacy_issue_review_first_turns();
    changed |= graph.upgrade_issue_review_verdict();
    if !matches!(
        target,
        UpgradeTarget::ActiveBlueprint { .. } | UpgradeTarget::UnpinnedRun { active: true }
    ) {
        changed |= if graph.is_issue_driven_autopilot_review() {
            graph.upgrade_legacy_issue_review_contract()
        } else {
            graph.upgrade_legacy_agent_review_prompts()
        };
    }
    if matches!(target, UpgradeTarget::UnpinnedRun { .. }) {
        return changed;
    }
    // Repair before matching stock topology: the stale route otherwise stops
    // the publication and merge-verification upgrades recognizing the graph.
    changed |= graph.repair_issue_review_feedback_route();
    changed |= if is_preset {
        graph.upgrade_local_review_publication_flow()
    } else {
        graph.upgrade_issue_review_publication_flow()
    };
    // Local copies receive stock text updates through the prompt contract;
    // an active copy must retain that deferral through the merge phase too.
    if is_preset || graph.is_issue_driven_autopilot_review() {
        changed |= graph.upgrade_merge_prompt();
    }
    if !is_preset {
        changed |= graph.upgrade_issue_review_merge_verification();
    }
    changed
}

impl CircuitGraph {
    /// The nodes that verify a merge before the implementer is closed.
    pub(crate) const MERGE_VERIFICATION_NODES: [&'static str; 5] = [
        "merge_wait",
        "merge_verify",
        "close_implementer",
        "merge_blocked",
        "merge_unconfirmed",
    ];
    pub(crate) const LEGACY_PR_APPROVED_MESSAGE: &'static str = "Review approved for PR #{{pr.number}} ({{issue.title}}). Check the current PR head and required checks before merging.";

    /// Normalize the original issue-review blueprint so already-saved circuits
    /// use their real task as the spawned agent's first turn. Match the node
    /// kinds and two-edge intermediary shape; copy the authored prompt
    /// verbatim so migration never replaces it with a server default.
    fn upgrade_legacy_issue_review_first_turns(&mut self) -> bool {
        let marker_changed = self.blueprint.is_none() && self.has_legacy_issue_review_shape();
        if marker_changed {
            self.blueprint = Some(CircuitBlueprintKind::IssueDrivenAutopilotReview);
        }
        if !self.is_issue_driven_autopilot_review() {
            return false;
        }

        // The original review blueprint predates the explicit OpenPr policy.
        // Preserve its require-existing contract when loading a persisted
        // graph that omitted the new optional field.
        let mut policy_changed = false;
        for node in &mut self.nodes {
            if let CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                open_pr_policy,
                ..
            } = &mut node.kind
            {
                if open_pr_policy.is_none() {
                    *open_pr_policy = Some(OpenPrPolicy::RequireExisting);
                    policy_changed = true;
                }
            }
        }

        let implementer_changed = self.replace_legacy_injected_first_turn(
            "implementer",
            "implementation_prompt",
            "implementation_classifier",
        );
        let reviewer_changed = self.replace_legacy_injected_first_turn(
            "reviewer",
            "review_prompt",
            "review_classifier",
        );
        marker_changed || policy_changed || implementer_changed || reviewer_changed
    }

    /// Upgrade the legacy review topology structurally. User-authored prompt
    /// text and spawn overrides are never rewritten; a graph whose spawn
    /// already has a non-empty prompt keeps its intermediary injection so a
    /// custom first turn cannot be lost.
    fn upgrade_issue_review_verdict(&mut self) -> bool {
        if !self.is_issue_driven_autopilot_review()
            || !matches!(
                self.node("review_classifier").map(|n| &n.kind),
                Some(
                    CircuitNodeKind::LlmTurnClassifier { .. }
                        | CircuitNodeKind::ReviewVerdict { .. }
                )
            )
            || !matches!(
                self.node("review_retry").map(|n| &n.kind),
                Some(CircuitNodeKind::RetryLimit { .. })
            )
        {
            return false;
        }
        let has_legacy_route = |from: &str, to: &str, condition: EdgeCondition| {
            self.edges
                .iter()
                .any(|edge| edge.from == from && edge.to == to && edge.condition == condition)
        };
        if !has_legacy_route(
            "review_classifier",
            "follow_feedback",
            EdgeCondition::OnOutcome(StepOutcome::Completed),
        ) || !has_legacy_route(
            "review_retry",
            "finish",
            EdgeCondition::OnOutcome(StepOutcome::Completed),
        ) || !has_legacy_route(
            "review_retry",
            "complete",
            EdgeCondition::OnOutcome(StepOutcome::Failed),
        ) {
            return false;
        }
        let canonical = Self::issue_driven_autopilot_review("");
        let mut changed = false;
        if matches!(
            self.node("review_classifier").map(|n| &n.kind),
            Some(CircuitNodeKind::LlmTurnClassifier { .. })
        ) {
            if let Some(node) = self
                .nodes
                .iter_mut()
                .find(|node| node.id == "review_classifier")
            {
                node.kind = canonical
                    .node("review_classifier")
                    .expect("canonical review node")
                    .kind
                    .clone();
                changed = true;
            }
        }
        for id in ["close_approved", "review_exhausted"] {
            if self.node(id).is_none() {
                self.nodes
                    .push(canonical.node(id).expect("canonical review node").clone());
                changed = true;
            }
        }
        let before_nodes = self.nodes.len();
        self.nodes.retain(|node| node.id != "review_blocked");
        changed |= self.nodes.len() != before_nodes;
        let before_edges = self.edges.len();
        self.edges.retain(|edge| {
            let obsolete_review_route = edge.from == "review_classifier"
                && matches!(edge.to.as_str(), "review_exhausted" | "review_blocked");
            let obsolete_retry_route = edge.from == "review_retry"
                && edge.to == "complete"
                && edge.condition == EdgeCondition::OnOutcome(StepOutcome::Failed);
            !(obsolete_review_route || obsolete_retry_route)
        });
        changed |= self.edges.len() != before_edges;
        let missing_edge =
            |from: &str, to: &str, condition: EdgeCondition, edges: &[CircuitEdge]| {
                !edges
                    .iter()
                    .any(|edge| edge.from == from && edge.to == to && edge.condition == condition)
            };
        for edge in canonical.edges.iter().filter(|edge| {
            matches!(
                edge.from.as_str(),
                "review_classifier" | "review_retry" | "close_approved"
            ) && matches!(
                edge.to.as_str(),
                "follow_feedback" | "close_approved" | "review_exhausted" | "complete"
            )
        }) {
            if missing_edge(&edge.from, &edge.to, edge.condition, &self.edges) {
                self.edges.push(edge.clone());
                changed = true;
            }
        }
        changed |= self.repair_issue_review_feedback_route();
        changed
    }

    /// Drop the pre-verdict `completed -> follow_feedback` route from a graph
    /// whose classifier is already the verdict gate. There `completed` means
    /// approved and goes to `close_approved`; kept beside the `working` route
    /// it makes `follow_feedback` wait for two outcomes of one verdict, so a
    /// changes-requested review could never start the fix round and the run
    /// failed. The leftover route also stopped the publication-flow and
    /// merge-verification upgrades recognising the graph. Graphs without both
    /// routes are left alone.
    fn repair_issue_review_feedback_route(&mut self) -> bool {
        let is_route = |edge: &CircuitEdge, outcome: StepOutcome| {
            edge.from == "review_classifier"
                && edge.to == "follow_feedback"
                && edge.condition == EdgeCondition::OnOutcome(outcome)
        };
        if !self.is_issue_driven_autopilot_review()
            || !matches!(
                self.node("review_classifier").map(|n| &n.kind),
                Some(CircuitNodeKind::ReviewVerdict { .. })
            )
            || !self.edges.iter().any(|e| is_route(e, StepOutcome::Working))
            || !self
                .edges
                .iter()
                .any(|e| is_route(e, StepOutcome::Completed))
        {
            return false;
        }
        self.edges
            .retain(|edge| !is_route(edge, StepOutcome::Completed));
        true
    }

    fn replace_legacy_injected_first_turn(
        &mut self,
        spawn_id: &str,
        prompt_id: &str,
        next_id: &str,
    ) -> bool {
        let spawn_matches = matches!(
            self.node(spawn_id).map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt.trim().is_empty()
        );
        let prompt_matches = matches!(
            self.node(prompt_id).map(|node| &node.kind),
            Some(CircuitNodeKind::InjectPty { prompt, target_node_id })
                if !prompt.trim().is_empty() && target_node_id.as_deref() == Some(spawn_id)
        );
        let incident_edges: Vec<&CircuitEdge> = self
            .edges
            .iter()
            .filter(|edge| edge.from == prompt_id || edge.to == prompt_id)
            .collect();
        let topology_matches = incident_edges.len() == 2
            && incident_edges.iter().any(|edge| {
                edge.from == spawn_id
                    && edge.to == prompt_id
                    && edge.condition == EdgeCondition::Always
            })
            && incident_edges.iter().any(|edge| {
                edge.from == prompt_id
                    && edge.to == next_id
                    && edge.condition == EdgeCondition::Always
            });

        if !(spawn_matches && prompt_matches && topology_matches) {
            return false;
        }

        let migrated_prompt = match self.node(prompt_id).map(|node| &node.kind) {
            Some(CircuitNodeKind::InjectPty { prompt, .. }) => prompt.clone(),
            _ => return false,
        };
        if let Some(CircuitNode {
            kind: CircuitNodeKind::SpawnAgentNode { prompt, .. },
            ..
        }) = self.nodes.iter_mut().find(|node| node.id == spawn_id)
        {
            *prompt = migrated_prompt;
        }
        self.nodes.retain(|node| node.id != prompt_id);
        for edge in &mut self.edges {
            if edge.from == spawn_id && edge.to == prompt_id {
                edge.to = next_id.to_string();
            }
        }
        self.edges
            .retain(|edge| !(edge.from == prompt_id && edge.to == next_id));
        true
    }

    fn upgrade_legacy_agent_review_prompts(&mut self) -> bool {
        let old_review = crate::review_contract::LEGACY_LOCAL_REVIEW_PROMPT;
        let old_feedback = crate::review_contract::LEGACY_FEEDBACK_PROMPT;
        let old_merge =
            crate::review_contract::legacy_merge_prompt("your pull request for this work", "");
        let mut changed = false;
        for node in &mut self.nodes {
            match &mut node.kind {
                CircuitNodeKind::SpawnAgentNode { prompt, .. }
                    if node.id == "reviewer" && prompt == old_review =>
                {
                    *prompt = Self::local_review_prompt();
                    changed = true;
                }
                CircuitNodeKind::InjectPty { prompt, .. }
                    if node.id == "feedback" && prompt == old_feedback =>
                {
                    *prompt =
                        Self::review_feedback_prompt(crate::review_contract::LOCAL_FEEDBACK_SCOPE);
                    changed = true;
                }
                CircuitNodeKind::InjectPty { prompt, .. }
                    if node.id == "merge" && prompt == &old_merge =>
                {
                    *prompt = crate::review_contract::merge_approved_pr(
                        "your pull request for this work",
                        "",
                    );
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    fn upgrade_legacy_issue_review_contract(&mut self) -> bool {
        if !self.is_issue_driven_autopilot_review() {
            return false;
        }
        // Match only the two exact prompts shipped before PR_REVIEW_PROMPT
        // was split from REVIEW_POLICY. A user-authored extension of either
        // prompt is not stock and must remain intact.
        let old_feedback = "Follow the feedback comments on PR #{{pr.number}} ({{pr.url}}). Reviewer report: {{node.reviewer.output}}. Address every valid comment, run the relevant tests, and update the PR. Do not ignore architectural or clean-code concerns; report what you changed.";
        let old_merge = crate::review_contract::legacy_merge_prompt(
            "PR #{{pr.number}} ({{pr.url}})",
            " {{pr.number}}",
        );
        let mut changed = false;
        for node in &mut self.nodes {
            match &mut node.kind {
                CircuitNodeKind::SpawnAgentNode { prompt, .. }
                    if node.id == "reviewer"
                        && (prompt == crate::review_contract::LEGACY_PR_REVIEW_PROMPT
                            || prompt == crate::review_contract::LEGACY_PR_REVIEW_WITH_VERDICT) =>
                {
                    *prompt = Self::pr_review_prompt();
                    changed = true;
                }
                CircuitNodeKind::InjectPty { prompt, .. }
                    if node.id == "follow_feedback" && prompt == old_feedback =>
                {
                    *prompt =
                        Self::review_feedback_prompt(crate::review_contract::PR_FEEDBACK_SCOPE);
                    changed = true;
                }
                CircuitNodeKind::InjectPty { prompt, .. }
                    if node.id == "merge" && prompt == &old_merge =>
                {
                    *prompt = Self::pr_merge_prompt();
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    /// Move a stored issue-review graph from the per-round reviewer to the
    /// publication flow: one reviewer re-prompted each round, and approval
    /// handing the PR to the implementer to merge. Only the exact stock
    /// topology is rewritten; prompts and launch settings are kept, apart
    /// from stock texts whose meaning the new flow changes.
    fn upgrade_issue_review_publication_flow(&mut self) -> bool {
        if !self.is_issue_driven_autopilot_review() || self.node("close_reviewer").is_none() {
            return false;
        }
        let mut upgraded = self.clone();
        let rewired =
            upgraded.retarget_edge("follow_feedback", "close_reviewer", "feedback_classifier")
                && upgraded.remove_edge("close_reviewer", "feedback_classifier")
                && upgraded.retarget_edge("review_retry", "finish", "re_review")
                && upgraded.retarget_edge("reviewer", "review_classifier", "review_round")
                && upgraded.retarget_edge("close_approved", "complete", "merge");
        if !rewired {
            return false;
        }
        upgraded
            .nodes
            .retain(|node| node.id != "close_reviewer" && node.id != "review_blocked");
        upgraded
            .edges
            .retain(|edge| edge.from != "review_blocked" && edge.to != "review_blocked");
        let canonical = Self::issue_driven_autopilot_review("");
        for id in ["review_round", "re_review", "merge"] {
            upgraded
                .nodes
                .push(canonical.node(id).expect("canonical review node").clone());
        }
        for (from, to) in [
            ("review_round", "review_classifier"),
            ("merge", "complete"),
            ("re_review", "review_round"),
        ] {
            upgraded.edges.push(CircuitEdge {
                from: from.into(),
                to: to.into(),
                condition: EdgeCondition::Always,
            });
        }
        upgraded.replace_stock_text(
            "follow_feedback",
            &Self::review_feedback_prompt(crate::review_contract::LEGACY_PR_FEEDBACK_SCOPE),
            &Self::review_feedback_prompt(crate::review_contract::PR_FEEDBACK_SCOPE),
        );
        upgraded.replace_stock_text(
            "complete",
            Self::LEGACY_PR_APPROVED_MESSAGE,
            Self::PR_APPROVED_MESSAGE,
        );
        if !upgraded.has_review_topology_of(&Self::publication_flow_template()) {
            return false;
        }
        // Continue to today's shape: the publication flow is not a place to stop.
        upgraded.upgrade_issue_review_merge_verification();
        *self = upgraded;
        true
    }

    /// The stock issue-review topology shipped before merge verification:
    /// approval handed the PR to the implementer and the run completed as soon
    /// as the prompt was delivered.
    pub(crate) fn publication_flow_template() -> Self {
        let mut graph = Self::issue_driven_autopilot_review("");
        graph
            .nodes
            .retain(|node| !Self::MERGE_VERIFICATION_NODES.contains(&node.id.as_str()));
        graph.edges.retain(|edge| {
            edge.from != "merge" && !Self::MERGE_VERIFICATION_NODES.contains(&edge.from.as_str())
        });
        graph.edges.push(CircuitEdge {
            from: "merge".into(),
            to: "complete".into(),
            condition: EdgeCondition::Always,
        });
        graph.replace_stock_text(
            "complete",
            Self::PR_MERGED_MESSAGE,
            Self::PR_APPROVED_MESSAGE,
        );
        graph
    }

    /// Move a stored issue-review graph from "asked the implementer to merge,
    /// done" to verifying the merge on GitHub and closing the implementer only
    /// then. Only the exact stock publication-flow topology is rewritten, and a
    /// customized prompt or message is kept; anything else is left untouched.
    fn upgrade_issue_review_merge_verification(&mut self) -> bool {
        if !self.is_issue_driven_autopilot_review()
            || self.node("merge_wait").is_some()
            || !self.has_review_topology_of(&Self::publication_flow_template())
        {
            return false;
        }
        let canonical = Self::issue_driven_autopilot_review("");
        let mut upgraded = self.clone();
        upgraded.remove_edge("merge", "complete");
        for id in Self::MERGE_VERIFICATION_NODES {
            upgraded
                .nodes
                .push(canonical.node(id).expect("canonical merge node").clone());
        }
        for edge in &canonical.edges {
            if !upgraded.edges.contains(edge) {
                upgraded.edges.push(edge.clone());
            }
        }
        upgraded.replace_stock_text(
            "complete",
            Self::PR_APPROVED_MESSAGE,
            Self::PR_MERGED_MESSAGE,
        );
        if !upgraded.has_review_topology_of(&canonical) {
            return false;
        }
        *self = upgraded;
        true
    }

    /// Replace the stock merge hand-off text with the one that updates an
    /// out-of-date branch first. A customized merge prompt is never rewritten.
    fn upgrade_merge_prompt(&mut self) -> bool {
        let (subject, argument) = if self.is_issue_driven_autopilot_review() {
            ("PR #{{pr.number}} ({{pr.url}})", " {{pr.number}}")
        } else {
            ("your pull request for this work", "")
        };
        let before = self.clone();
        self.replace_stock_text(
            "merge",
            &crate::review_contract::legacy_merge_approved_pr(subject, argument),
            &crate::review_contract::merge_approved_pr(subject, argument),
        );
        *self != before
    }

    /// Point the first `from -> to` edge at `new_to`, keeping its position so
    /// a RetryLimit's first child (its loop re-entry) is preserved.
    pub(crate) fn retarget_edge(&mut self, from: &str, to: &str, new_to: &str) -> bool {
        let Some(edge) = self
            .edges
            .iter_mut()
            .find(|edge| edge.from == from && edge.to == to)
        else {
            return false;
        };
        edge.to = new_to.to_string();
        true
    }

    pub(crate) fn remove_edge(&mut self, from: &str, to: &str) -> bool {
        let before = self.edges.len();
        self.edges
            .retain(|edge| !(edge.from == from && edge.to == to));
        self.edges.len() != before
    }

    /// Replace a prompt or message only while it is still the shipped text.
    pub(crate) fn replace_stock_text(&mut self, node_id: &str, stock: &str, replacement: &str) {
        let Some(node) = self.nodes.iter_mut().find(|node| node.id == node_id) else {
            return;
        };
        match &mut node.kind {
            CircuitNodeKind::InjectPty { prompt: text, .. }
            | CircuitNodeKind::SpawnAgentNode { prompt: text, .. }
            | CircuitNodeKind::Notify { message: text }
                if text == stock =>
            {
                *text = replacement.to_string();
            }
            _ => {}
        }
    }

    /// Move a stored local review graph from the per-round reviewer to the
    /// publication flow. Only the exact pre-publication topology is rewritten;
    /// the reviewer definition and customized texts are kept.
    fn upgrade_local_review_publication_flow(&mut self) -> bool {
        if self.node("close_reviewer").is_none() || self.node("publish").is_some() {
            return false;
        }
        let Some(K::RetryLimit { max_retries }) = self.node("retry").map(|node| &node.kind) else {
            return false;
        };
        let canonical = Self::agent_review(None, None, *max_retries);
        let mut upgraded = self.clone();
        let rewired = upgraded.retarget_edge("source_ready", "reviewer", "publish")
            && upgraded.retarget_edge("reviewer", "verdict", "review_round")
            && upgraded.retarget_edge("feedback", "close_reviewer", "await_fixes")
            && upgraded.remove_edge("close_reviewer", "await_fixes")
            && upgraded.retarget_edge("retry", "reviewer", "re_review")
            && upgraded.retarget_edge("close_approved", "approved", "merge");
        if !rewired {
            return false;
        }
        upgraded.nodes.retain(|node| node.id != "close_reviewer");
        for id in [
            "publish",
            "await_publish",
            "confirm_publish",
            "publish_ready",
            "review_round",
            "re_review",
            "merge",
        ] {
            upgraded
                .nodes
                .push(canonical.node(id).expect("canonical review node").clone());
        }
        let added = canonical.edges.iter().filter(|edge| {
            matches!(
                edge.from.as_str(),
                "publish"
                    | "await_publish"
                    | "confirm_publish"
                    | "publish_ready"
                    | "review_round"
                    | "re_review"
                    | "merge"
            )
        });
        upgraded.edges.extend(added.cloned());
        upgraded.replace_stock_text(
            "feedback",
            &Self::review_feedback_prompt(crate::review_contract::LEGACY_LOCAL_FEEDBACK_SCOPE),
            &Self::review_feedback_prompt(crate::review_contract::LOCAL_FEEDBACK_SCOPE),
        );
        upgraded.replace_stock_text(
            "approved",
            LEGACY_LOCAL_APPROVED_MESSAGE,
            LOCAL_APPROVED_MESSAGE,
        );
        if !upgraded.has_review_topology_of(&canonical) {
            return false;
        }
        *self = upgraded;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_point_matches_ordered_legacy_upgrades_and_is_idempotent() {
        use crate::circuit::test_support::{
            pre_publication_issue_review, pre_publication_local_review,
            publication_flow_issue_review, stuck_issue_review,
        };
        let legacy = CircuitGraph::from_json(include_str!(
            "../../tests/fixtures/legacy-issue-review-circuit.json"
        ))
        .unwrap();
        let mut injected = legacy.clone();
        injected.blueprint = None;
        for (spawn_id, prompt_id, next_id, first_turn) in [
            (
                "implementer",
                "implementation_prompt",
                "implementation_classifier",
                "{{issue.prefill}}",
            ),
            (
                "reviewer",
                "review_prompt",
                "review_classifier",
                crate::review_contract::LEGACY_PR_REVIEW_PROMPT,
            ),
        ] {
            if let CircuitNodeKind::SpawnAgentNode { prompt, .. } = &mut injected
                .nodes
                .iter_mut()
                .find(|node| node.id == spawn_id)
                .unwrap()
                .kind
            {
                prompt.clear();
            }
            assert!(injected.retarget_edge(spawn_id, next_id, prompt_id));
            injected.nodes.push(CircuitNode {
                id: prompt_id.into(),
                kind: CircuitNodeKind::InjectPty {
                    prompt: first_turn.into(),
                    target_node_id: Some(spawn_id.into()),
                },
            });
            injected.edges.push(CircuitEdge {
                from: prompt_id.into(),
                to: next_id.into(),
                condition: EdgeCondition::Always,
            });
        }
        for (before, target) in [
            (injected, UpgradeTarget::Blueprint),
            (legacy, UpgradeTarget::Blueprint),
            (stuck_issue_review(), UpgradeTarget::Blueprint),
            (pre_publication_issue_review(), UpgradeTarget::Blueprint),
            (publication_flow_issue_review(), UpgradeTarget::Blueprint),
            (pre_publication_local_review(3), UpgradeTarget::Preset),
            (pre_publication_local_review(3), UpgradeTarget::Blueprint),
        ] {
            let mut expected = before.clone();
            expected.upgrade_legacy_issue_review_first_turns();
            expected.upgrade_issue_review_verdict();
            if expected.is_issue_driven_autopilot_review() {
                expected.upgrade_legacy_issue_review_contract();
            } else {
                expected.upgrade_legacy_agent_review_prompts();
            }
            expected.repair_issue_review_feedback_route();
            if matches!(target, UpgradeTarget::Preset) {
                expected.upgrade_local_review_publication_flow();
            } else {
                expected.upgrade_issue_review_publication_flow();
            }
            expected.upgrade_merge_prompt();
            if !matches!(target, UpgradeTarget::Preset) {
                expected.upgrade_issue_review_merge_verification();
            }

            let mut actual = before.clone();
            assert_eq!(upgrade(&mut actual, target), expected != before);
            assert_eq!(actual, expected);
            actual.validate().unwrap();
            let after = actual.clone();
            assert!(!upgrade(&mut actual, target), "second pass changes nothing");
            assert_eq!(actual, after);
        }
    }

    #[test]
    fn snapshot_repairs_only_the_dead_route_and_keeps_prompts_and_topology() {
        let mut snapshot = crate::circuit::test_support::stuck_issue_review();
        let mut expected = snapshot.clone();
        assert!(expected.repair_issue_review_feedback_route());
        assert!(upgrade(&mut snapshot, UpgradeTarget::RunSnapshot));
        assert_eq!(snapshot, expected);
        assert!(snapshot.node("close_reviewer").is_some());
        assert!(snapshot.node("merge_verify").is_none());
        assert!(!upgrade(&mut snapshot, UpgradeTarget::RunSnapshot));
    }

    #[test]
    fn active_blueprint_defers_review_contract_until_its_runs_finish() {
        let mut graph = CircuitGraph::agent_review(None, None, 3);
        graph.replace_stock_text(
            "reviewer",
            &CircuitGraph::local_review_prompt(),
            crate::review_contract::LEGACY_LOCAL_REVIEW_PROMPT,
        );
        let before = graph.clone();
        assert!(!upgrade(
            &mut graph,
            UpgradeTarget::ActiveBlueprint { is_preset: true },
        ));
        assert_eq!(graph, before);
        assert!(upgrade(&mut graph, UpgradeTarget::Preset));
        assert!(matches!(
            &graph.node("reviewer").unwrap().kind,
            CircuitNodeKind::SpawnAgentNode { prompt, .. }
                if prompt == &CircuitGraph::local_review_prompt()
        ));
        assert!(!upgrade(&mut graph, UpgradeTarget::Preset));
    }

    #[test]
    fn active_review_copy_defers_legacy_merge_text_until_its_runs_finish() {
        let mut graph = CircuitGraph::agent_review(None, None, 3);
        graph.replace_stock_text(
            "reviewer",
            &CircuitGraph::local_review_prompt(),
            "Use the team's review checklist.",
        );
        graph.replace_stock_text(
            "feedback",
            &CircuitGraph::review_feedback_prompt(crate::review_contract::LOCAL_FEEDBACK_SCOPE),
            "Apply the team's requested fixes.",
        );
        graph.replace_stock_text(
            "merge",
            &crate::review_contract::merge_approved_pr("your pull request for this work", ""),
            &crate::review_contract::legacy_merge_prompt("your pull request for this work", ""),
        );
        let before = graph.clone();
        assert!(!upgrade(
            &mut graph,
            UpgradeTarget::ActiveBlueprint { is_preset: false },
        ));
        assert_eq!(graph, before);
        assert!(upgrade(&mut graph, UpgradeTarget::Blueprint));
        let mut expected = before;
        expected.replace_stock_text(
            "merge",
            &crate::review_contract::legacy_merge_prompt("your pull request for this work", ""),
            &crate::review_contract::merge_approved_pr("your pull request for this work", ""),
        );
        assert_eq!(graph, expected);
        assert!(!upgrade(&mut graph, UpgradeTarget::Blueprint));
    }

    #[test]
    fn stock_publication_flow_issue_review_upgrades_to_verified_merge_and_keeps_customizations() {
        let mut stored = crate::circuit::test_support::publication_flow_issue_review();
        assert!(stored.node("merge_wait").is_none());
        let canonical = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");

        assert!(stored.upgrade_issue_review_merge_verification());
        stored.validate().unwrap();
        assert!(stored.has_review_topology_of(&canonical));
        assert!(matches!(
            stored.node("complete").map(|n| &n.kind),
            Some(CircuitNodeKind::Notify { message }) if message == CircuitGraph::PR_MERGED_MESSAGE
        ));
        assert!(
            !stored.upgrade_issue_review_merge_verification(),
            "the upgrade is idempotent"
        );

        // A customized completion message survives.
        let mut custom = crate::circuit::test_support::publication_flow_issue_review();
        if let Some(CircuitNode {
            kind: CircuitNodeKind::Notify { message },
            ..
        }) = custom.nodes.iter_mut().find(|node| node.id == "complete")
        {
            *message = "Shipped!".into();
        }
        assert!(custom.upgrade_issue_review_merge_verification());
        assert!(matches!(
            custom.node("complete").map(|n| &n.kind),
            Some(CircuitNodeKind::Notify { message }) if message == "Shipped!"
        ));

        // A customized topology is left exactly as it was.
        let mut extra = crate::circuit::test_support::publication_flow_issue_review();
        extra.nodes.push(CircuitNode {
            id: "announce".into(),
            kind: CircuitNodeKind::Notify {
                message: "custom".into(),
            },
        });
        extra.edges.push(CircuitEdge {
            from: "complete".into(),
            to: "announce".into(),
            condition: EdgeCondition::Always,
        });
        let before = extra.clone();
        assert!(!extra.upgrade_issue_review_merge_verification());
        assert_eq!(extra, before);
    }

    #[test]
    fn stock_merge_prompt_upgrades_for_both_review_graphs_but_a_custom_one_is_kept() {
        let mut issue = crate::circuit::test_support::publication_flow_issue_review();
        assert!(issue.upgrade_merge_prompt());
        assert!(matches!(
            issue.node("merge").map(|n| &n.kind),
            Some(CircuitNodeKind::InjectPty { prompt, .. }) if prompt.contains("gh pr update-branch {{pr.number}}")
        ));
        assert!(!issue.upgrade_merge_prompt(), "idempotent");

        let mut local = crate::circuit::test_support::pre_publication_local_review(3);
        assert!(local.upgrade_local_review_publication_flow());
        // The publication-flow upgrade already writes the current prompt, so
        // put the previous stock text back to model a graph saved before it.
        local.replace_stock_text(
            "merge",
            &crate::review_contract::merge_approved_pr("your pull request for this work", ""),
            &crate::review_contract::legacy_merge_approved_pr(
                "your pull request for this work",
                "",
            ),
        );
        assert!(local.upgrade_merge_prompt());
        assert!(matches!(
            local.node("merge").map(|n| &n.kind),
            Some(CircuitNodeKind::InjectPty { prompt, .. }) if prompt.contains("gh pr update-branch")
        ));

        let mut custom = crate::circuit::test_support::publication_flow_issue_review();
        custom.replace_stock_text(
            "merge",
            &crate::review_contract::legacy_merge_approved_pr(
                "PR #{{pr.number}} ({{pr.url}})",
                " {{pr.number}}",
            ),
            "Merge it however you like.",
        );
        assert!(!custom.upgrade_merge_prompt());
        assert!(matches!(
            custom.node("merge").map(|n| &n.kind),
            Some(CircuitNodeKind::InjectPty { prompt, .. }) if prompt == "Merge it however you like."
        ));
    }

    #[test]
    fn stock_pre_publication_issue_review_upgrades_to_the_publication_flow() {
        let mut stored = crate::circuit::test_support::pre_publication_issue_review();
        if let Some(CircuitNode {
            kind: CircuitNodeKind::SpawnAgentNode { model, .. },
            ..
        }) = stored.nodes.iter_mut().find(|node| node.id == "reviewer")
        {
            *model = Some("custom-reviewer-model".into());
        }

        assert!(stored.upgrade_issue_review_publication_flow());
        stored.validate().unwrap();
        let canonical = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        assert!(stored.has_review_topology_of(&canonical));
        assert_eq!(
            stored.children("review_retry").first().map(String::as_str),
            Some("re_review")
        );
        for id in ["follow_feedback", "re_review", "merge", "complete"] {
            assert_eq!(
                stored.node(id),
                canonical.node(id),
                "{id} uses the current stock text"
            );
        }
        assert!(
            matches!(
                stored.node("reviewer").map(|node| &node.kind),
                Some(CircuitNodeKind::SpawnAgentNode { model: Some(model), .. }) if model == "custom-reviewer-model"
            ),
            "reviewer launch settings survive the upgrade"
        );
        assert!(
            !stored.upgrade_issue_review_publication_flow(),
            "the upgrade is idempotent"
        );
    }

    #[test]
    fn publication_upgrade_keeps_custom_prompts_and_skips_custom_topology() {
        let mut custom_prompt = crate::circuit::test_support::pre_publication_issue_review();
        let custom = "Follow the PR comments and also update the changelog.";
        if let Some(CircuitNode {
            kind: CircuitNodeKind::InjectPty { prompt, .. },
            ..
        }) = custom_prompt
            .nodes
            .iter_mut()
            .find(|node| node.id == "follow_feedback")
        {
            *prompt = custom.into();
        }
        assert!(custom_prompt.upgrade_issue_review_publication_flow());
        assert!(matches!(
            custom_prompt.node("follow_feedback").map(|node| &node.kind),
            Some(CircuitNodeKind::InjectPty { prompt, .. }) if prompt == custom
        ));

        let mut extra_step = crate::circuit::test_support::pre_publication_issue_review();
        extra_step.nodes.push(CircuitNode {
            id: "announce".into(),
            kind: CircuitNodeKind::Notify {
                message: "custom".into(),
            },
        });
        extra_step.edges.push(CircuitEdge {
            from: "complete".into(),
            to: "announce".into(),
            condition: EdgeCondition::Always,
        });
        let before = extra_step.clone();
        assert!(!extra_step.upgrade_issue_review_publication_flow());
        assert_eq!(
            extra_step, before,
            "a customized topology is left untouched"
        );
    }

    fn feedback_routes(graph: &CircuitGraph) -> Vec<EdgeCondition> {
        graph
            .edges
            .iter()
            .filter(|edge| edge.from == "review_classifier" && edge.to == "follow_feedback")
            .map(|edge| edge.condition)
            .collect()
    }

    /// A changes-requested verdict reached `follow_feedback` through a second,
    /// `completed` route left over from the pre-verdict classifier. A node needs
    /// every incoming edge satisfied and one verdict yields one outcome, so the
    /// feedback step could never start and the run failed.
    #[test]
    fn verdict_upgrade_replaces_the_legacy_completed_feedback_route() {
        let mut graph = CircuitGraph::from_json(include_str!(
            "../../tests/fixtures/legacy-issue-review-circuit.json"
        ))
        .unwrap();
        let mut custom = graph.clone();
        custom.edges.pop();
        let before = custom.clone();
        assert!(!custom.upgrade_issue_review_verdict());
        assert_eq!(custom, before, "custom wiring requires explicit editing");
        assert_eq!(
            feedback_routes(&graph),
            [EdgeCondition::OnOutcome(StepOutcome::Completed)],
            "the saved legacy graph routes a finished review turn to the feedback step"
        );

        assert!(graph.upgrade_issue_review_verdict());
        assert_eq!(
            feedback_routes(&graph),
            [EdgeCondition::OnOutcome(StepOutcome::Working)],
            "only a changes-requested verdict feeds the implementer"
        );
        assert!(graph.node("review_blocked").is_none());
        assert!(!graph
            .edges
            .iter()
            .any(|edge| { edge.from == "review_classifier" && edge.to == "review_blocked" }));
        graph.validate().unwrap();
    }

    #[test]
    fn stuck_issue_review_graph_is_repaired_and_reaches_the_current_flow() {
        let mut graph = CircuitGraph::from_json(include_str!(
            "../../tests/fixtures/stuck-issue-review-circuit.json"
        ))
        .unwrap();
        assert_eq!(
            feedback_routes(&graph),
            [
                EdgeCondition::OnOutcome(StepOutcome::Completed),
                EdgeCondition::OnOutcome(StepOutcome::Working)
            ],
            "the saved graph carries both routes"
        );

        assert!(graph.repair_issue_review_feedback_route());
        assert_eq!(
            feedback_routes(&graph),
            [EdgeCondition::OnOutcome(StepOutcome::Working)]
        );
        assert!(
            !graph.repair_issue_review_feedback_route(),
            "the repair is idempotent"
        );

        assert!(graph.upgrade_issue_review_publication_flow());
        graph.validate().unwrap();
        assert!(
            graph.has_review_topology_of(&CircuitGraph::issue_driven_autopilot_review(
                "buildmesh:run"
            ))
        );
    }

    #[test]
    fn feedback_route_repair_leaves_unrelated_graphs_alone() {
        // The canonical graph has only the `working` route.
        let mut canonical = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let before = canonical.clone();
        assert!(!canonical.repair_issue_review_feedback_route());
        assert_eq!(canonical, before);

        // Before the verdict upgrade `completed` is the only feedback route and
        // it is the correct one for that classifier.
        let mut legacy = CircuitGraph::from_json(include_str!(
            "../../tests/fixtures/legacy-issue-review-circuit.json"
        ))
        .unwrap();
        let before = legacy.clone();
        assert!(!legacy.repair_issue_review_feedback_route());
        assert_eq!(legacy, before);
    }

    #[test]
    fn legacy_pr_review_prompt_with_custom_suffix_is_preserved() {
        let mut graph = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let custom = "review PR {{pr.number}} as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture. Add the review comments to the PR as a comment. The pull request URL is {{pr.url}}. Also check our release checklist.";
        if let Some(node) = graph.nodes.iter_mut().find(|node| node.id == "reviewer") {
            if let CircuitNodeKind::SpawnAgentNode { prompt, .. } = &mut node.kind {
                *prompt = custom.into();
            }
        }
        assert!(!graph.upgrade_legacy_issue_review_contract());
        assert!(matches!(
            graph.node("reviewer").map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. }) if prompt == custom
        ));
    }

    #[test]
    fn legacy_merge_prompts_upgrade_to_branch_sync_text_and_custom_merge_is_preserved() {
        for (mut graph, upgrade) in [
            (
                CircuitGraph::issue_driven_autopilot_review("buildmesh:run"),
                "issue",
            ),
            (CircuitGraph::agent_review(None, None, 3), "local"),
        ] {
            let node = graph
                .nodes
                .iter_mut()
                .find(|node| node.id == "merge")
                .expect("merge exists");
            if let CircuitNodeKind::InjectPty { prompt, .. } = &mut node.kind {
                *prompt = if upgrade == "issue" {
                    crate::review_contract::legacy_merge_prompt(
                        "PR #{{pr.number}} ({{pr.url}})",
                        " {{pr.number}}",
                    )
                } else {
                    crate::review_contract::legacy_merge_prompt(
                        "your pull request for this work",
                        "",
                    )
                };
            }
            let changed = if upgrade == "issue" {
                graph.upgrade_legacy_issue_review_contract()
            } else {
                graph.upgrade_legacy_agent_review_prompts()
            };
            assert!(changed, "{upgrade} legacy merge text must upgrade");
            assert!(
                matches!(
                    graph.node("merge").map(|node| &node.kind),
                    Some(CircuitNodeKind::InjectPty { prompt, .. })
                        if prompt.contains("gh pr update-branch")
                            && prompt.contains("gh pr merge")
                            && !prompt.contains("Do not make further changes")
                ),
                "{upgrade} merge node carries the branch-sync text"
            );
        }

        let mut custom = CircuitGraph::issue_driven_autopilot_review("buildmesh:run");
        let custom_merge = "Squash and merge when ready. Also update the changelog.";
        if let Some(node) = custom.nodes.iter_mut().find(|node| node.id == "merge") {
            if let CircuitNodeKind::InjectPty { prompt, .. } = &mut node.kind {
                *prompt = custom_merge.into();
            }
        }
        assert!(!custom.upgrade_legacy_issue_review_contract());
        assert!(matches!(
            custom.node("merge").map(|node| &node.kind),
            Some(CircuitNodeKind::InjectPty { prompt, .. }) if prompt == custom_merge
        ));
    }

    #[test]
    fn stored_issue_review_blueprint_upgrades_injected_first_turns_to_spawn_prompts() {
        let mut graph = crate::circuit::test_support::pre_publication_issue_review();

        let implementer = graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "implementer")
            .unwrap();
        if let CircuitNodeKind::SpawnAgentNode { prompt, .. } = &mut implementer.kind {
            prompt.clear();
        }
        graph.nodes.push(CircuitNode {
            id: "implementation_prompt".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: "{{issue.prefill}}".into(),
                target_node_id: Some("implementer".into()),
            },
        });
        let implementer_edge = graph
            .edges
            .iter_mut()
            .find(|edge| edge.from == "implementer" && edge.to == "implementation_classifier")
            .unwrap();
        implementer_edge.to = "implementation_prompt".into();
        graph.edges.push(CircuitEdge {
            from: "implementation_prompt".into(),
            to: "implementation_classifier".into(),
            condition: EdgeCondition::Always,
        });

        let reviewer = graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "reviewer")
            .unwrap();
        if let CircuitNodeKind::SpawnAgentNode { prompt, .. } = &mut reviewer.kind {
            *prompt =
                "The pull request URL is {{pr.url}}. Use it as additional review context.".into();
        }
        graph.nodes.push(CircuitNode {
            id: "review_prompt".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: CircuitGraph::PR_REVIEW_PROMPT.into(),
                target_node_id: Some("reviewer".into()),
            },
        });
        let reviewer_edge = graph
            .edges
            .iter_mut()
            .find(|edge| edge.from == "reviewer" && edge.to == "review_classifier")
            .unwrap();
        reviewer_edge.to = "review_prompt".into();
        graph.edges.push(CircuitEdge {
            from: "review_prompt".into(),
            to: "review_classifier".into(),
            condition: EdgeCondition::Always,
        });

        let mut parsed = CircuitGraph::from_json(&graph.to_json().unwrap()).unwrap();
        assert!(parsed.upgrade_legacy_issue_review_first_turns());

        assert!(parsed.node("implementation_prompt").is_none());
        // The saved reviewer spawn prompt is an authored override, so its
        // intermediary remains intact rather than being silently replaced.
        assert!(parsed.node("review_prompt").is_some());
        assert!(matches!(
            parsed.node("implementer").map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt == "{{issue.prefill}}"
        ));
        assert!(matches!(
            parsed.node("reviewer").map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt == "The pull request URL is {{pr.url}}. Use it as additional review context."
        ));
        parsed.validate().unwrap();
    }

    #[test]
    fn legacy_review_graphs_get_a_marker_without_inspecting_prompt_text() {
        let mut graph = crate::circuit::test_support::pre_publication_issue_review();
        graph.nodes.push(CircuitNode {
            id: "review_prompt".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: "author-customized review instruction".into(),
                target_node_id: Some("reviewer".into()),
            },
        });
        let mut raw: serde_json::Value = serde_json::from_str(&graph.to_json().unwrap()).unwrap();
        raw.as_object_mut().unwrap().remove("blueprint");
        raw["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["id"] == "open_pr")
            .unwrap()["type"]
            .as_object_mut()
            .unwrap()
            .remove("open_pr_policy");

        let mut parsed = CircuitGraph::from_json(&serde_json::to_string(&raw).unwrap()).unwrap();
        assert!(parsed.upgrade_legacy_issue_review_first_turns());
        assert_eq!(
            parsed.blueprint,
            Some(CircuitBlueprintKind::IssueDrivenAutopilotReview)
        );
        assert!(parsed.is_issue_driven_autopilot_review());
        assert!(matches!(
            parsed.node("open_pr").map(|node| &node.kind),
            Some(CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                open_pr_policy: Some(OpenPrPolicy::RequireExisting),
                ..
            })
        ));
    }

    #[test]
    fn legacy_custom_injected_review_prompt_is_copied_verbatim() {
        let mut graph = crate::circuit::test_support::pre_publication_issue_review();
        if let Some(node) = graph.nodes.iter_mut().find(|node| node.id == "reviewer") {
            if let CircuitNodeKind::SpawnAgentNode { prompt, .. } = &mut node.kind {
                prompt.clear();
            }
        }
        graph.nodes.push(CircuitNode {
            id: "review_prompt".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: "Use our internal security checklist and preserve this wording.".into(),
                target_node_id: Some("reviewer".into()),
            },
        });
        let reviewer_edge = graph
            .edges
            .iter_mut()
            .find(|edge| edge.from == "reviewer" && edge.to == "review_classifier")
            .unwrap();
        reviewer_edge.to = "review_prompt".into();
        graph.edges.push(CircuitEdge {
            from: "review_prompt".into(),
            to: "review_classifier".into(),
            condition: EdgeCondition::Always,
        });
        graph.blueprint = None;

        assert!(graph.upgrade_legacy_issue_review_first_turns());
        assert!(graph.node("review_prompt").is_none());
        assert!(matches!(
            graph.node("reviewer").map(|node| &node.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
                if prompt == "Use our internal security checklist and preserve this wording."
        ));
    }
    #[test]
    fn stored_preset_upgrades_to_the_publication_flow_keeping_its_reviewer() {
        let mut stored = crate::circuit::test_support::pre_publication_local_review(4);
        if let CircuitNodeKind::SpawnAgentNode { prompt, .. } = &mut stored
            .nodes
            .iter_mut()
            .find(|n| n.id == "reviewer")
            .unwrap()
            .kind
        {
            *prompt = "Review the security boundaries".into();
        }
        assert!(stored.upgrade_local_review_publication_flow());
        stored.validate().unwrap();
        let canonical = CircuitGraph::agent_review(None, None, 4);
        assert!(stored.has_review_topology_of(&canonical));
        assert_eq!(
            stored.children("retry").first().map(String::as_str),
            Some("re_review")
        );
        for id in ["feedback", "publish", "re_review", "merge", "approved"] {
            assert_eq!(
                stored.node(id),
                canonical.node(id),
                "{id} uses the current stock text"
            );
        }
        assert!(matches!(&stored.node("reviewer").unwrap().kind,
            CircuitNodeKind::SpawnAgentNode { prompt, .. } if prompt == "Review the security boundaries"));
        assert!(
            !stored.upgrade_local_review_publication_flow(),
            "the upgrade is idempotent"
        );
    }
}
