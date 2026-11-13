//! Read contract for the durable review recovery owner. Support describes the
//! graph; availability describes whether a continuation can execute now.

use super::lineage::RunLineage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ReviewContinuationRefusal {
    RunMissing,
    RunNotFailed,
    UnsupportedGraph,
    InvalidGraph,
    InvalidContext,
    InvalidAncestry,
    MissingAncestor,
    CancelledSuccessor,
    SourceMissing,
    SourceWrongMesh,
    BeforeReview,
    ReviewerDefinitionMissing,
    FeedbackChanged,
    RoundLimitMissing,
    RoundLimitReached,
    SourceStopping,
    ReviewerStopping,
    ReviewerControlledElsewhere,
}

impl ReviewContinuationRefusal {
    pub fn message(self) -> &'static str {
        use ReviewContinuationRefusal::*;
        match self {
            RunMissing => "This run no longer exists.",
            RunNotFailed => "Only failed reviews can be continued. Cancelled reviews require a fresh review.",
            UnsupportedGraph => "This custom circuit needs manual recovery. Open its implementation agent and inspect the failed step.",
            InvalidGraph => "The saved review graph is unavailable. Start a fresh review.",
            InvalidContext => "The saved review context is unavailable. Start a fresh review.",
            InvalidAncestry => "Review lineage is inconsistent. Start a fresh review.",
            MissingAncestor => "Review lineage is incomplete.",
            CancelledSuccessor => "The continued review was cancelled. Start a fresh review.",
            SourceMissing => "The implementation agent is no longer retained. Recover from the PR branch and start a new review.",
            SourceWrongMesh => "The implementation agent no longer belongs to this Mesh.",
            BeforeReview => "This run stopped before review. Open the implementation agent and resolve its failure first.",
            ReviewerDefinitionMissing => "The reviewer definition is unavailable.",
            FeedbackChanged => "The review feedback step has changed. Open the implementation agent to recover this custom circuit.",
            RoundLimitMissing => "The review round limit is unavailable.",
            RoundLimitReached => "Review round limit reached.",
            SourceStopping => "The implementation agent is still being stopped. Try Review again in a moment.",
            ReviewerStopping => "The previous reviewer is still being cleaned up. Try Review again in a moment.",
            ReviewerControlledElsewhere => "The reviewer is controlled by another active Circuit Run. Finish or cancel that run before continuing this review.",
        }
    }
}

/// Identities from the executable snapshot, including review-only extensions.
/// Clients use these to present recorded outcomes, never to infer approval.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ReviewRunNodes {
    pub verdict_node_id: String,
    pub retry_node_ids: Vec<String>,
    pub merge_node_id: Option<String>,
    pub merge_confirmed_node_id: Option<String>,
    pub merge_unconfirmed_node_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ReviewContinuation {
    pub lineage: RunLineage,
    pub supported: bool,
    pub available: bool,
    pub refusal_reason: Option<ReviewContinuationRefusal>,
    pub refusal_message: Option<String>,
    #[ts(as = "Option<i32>")]
    pub source_agent_node_id: Option<i64>,
    #[ts(as = "Option<i32>")]
    pub reviewer_agent_node_id: Option<i64>,
    #[ts(as = "Option<i32>")]
    pub target_run_id: Option<i64>,
    pub review_nodes: Option<ReviewRunNodes>,
}
