//! Compatibility boundary for run lineage. Origin, extension and ancestry are
//! independent facts; none of them is evidence of review approval.

use super::{context::CircuitContext, model::CircuitGraph};

pub(crate) const REVIEW_PRESET_KEY: &str = "source.review_preset";
pub(crate) const REVIEW_EXTENDED_KEY: &str = "review.extended";
pub(crate) const RECOVERY_PARENT_KEY: &str = "recovery.from_run_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOrigin {
    LocalPreset,
    IssueDriven,
    ReviewDerived,
    Ordinary,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoveryAncestry {
    None,
    FromRun {
        #[ts(as = "i32")]
        run_id: i64,
    },
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct RunLineage {
    pub review_origin: ReviewOrigin,
    pub extended: bool,
    pub recovery: RecoveryAncestry,
}

impl RunLineage {
    pub fn read<'a>(get: impl Fn(&str) -> Option<&'a str>, graph: &CircuitGraph) -> Self {
        let extended = get(REVIEW_EXTENDED_KEY) == Some("1");
        let review_origin = if get(REVIEW_PRESET_KEY) == Some("1") {
            ReviewOrigin::LocalPreset
        } else if Self::is_issue_review(&get, graph) {
            ReviewOrigin::IssueDriven
        } else if graph.has_local_review_contract() {
            ReviewOrigin::ReviewDerived
        } else {
            ReviewOrigin::Ordinary
        };
        let recovery = Self::recovery_ancestry(&get);
        Self {
            review_origin,
            extended,
            recovery,
        }
    }

    /// Decode only stored facts. Worker hot paths do not need topology matching.
    pub fn decode<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Self {
        Self {
            review_origin: if get(REVIEW_PRESET_KEY) == Some("1") {
                ReviewOrigin::LocalPreset
            } else if Self::is_issue_review_extension(&get) {
                ReviewOrigin::IssueDriven
            } else {
                ReviewOrigin::Unknown
            },
            extended: Self::is_extended(&get),
            recovery: Self::recovery_ancestry(&get),
        }
    }

    pub fn recovery_ancestry<'a>(get: impl Fn(&str) -> Option<&'a str>) -> RecoveryAncestry {
        match get(RECOVERY_PARENT_KEY) {
            None => RecoveryAncestry::None,
            Some(value) => match value.parse::<i64>().ok().filter(|id| *id > 0) {
                Some(run_id) => RecoveryAncestry::FromRun { run_id },
                None => RecoveryAncestry::Invalid,
            },
        }
    }

    pub fn is_extended<'a>(get: impl Fn(&str) -> Option<&'a str>) -> bool {
        get(REVIEW_EXTENDED_KEY) == Some("1")
    }

    /// Review-only continuations replace the original graph but retain its scope.
    pub(crate) fn is_issue_review<'a>(
        get: impl Fn(&str) -> Option<&'a str>,
        graph: &CircuitGraph,
    ) -> bool {
        graph.is_issue_driven_autopilot_review() || Self::is_issue_review_extension(&get)
    }

    fn is_issue_review_extension<'a>(get: impl Fn(&str) -> Option<&'a str>) -> bool {
        Self::is_extended(&get) && get("review.source_was_circuit_owned") == Some("1")
    }

    pub fn is_local_preset(self) -> bool {
        self.review_origin == ReviewOrigin::LocalPreset
    }

    pub fn uses_borrowed_review_turns(self) -> bool {
        self.is_local_preset()
            || (self.extended && self.review_origin == ReviewOrigin::IssueDriven)
            || matches!(self.recovery, RecoveryAncestry::FromRun { .. })
    }

    pub fn mark_local_preset(context: &mut CircuitContext) {
        context.set(REVIEW_PRESET_KEY, "1");
    }

    pub fn record_parent(context: &mut CircuitContext, run_id: i64) {
        context.set(RECOVERY_PARENT_KEY, run_id.to_string());
    }
}

#[cfg(test)]
#[path = "lineage_tests.rs"]
mod tests;
