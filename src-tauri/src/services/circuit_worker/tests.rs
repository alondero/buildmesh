//! Worker test inventory, grouped by responsibility.
//!
//! Includes retain the historical `circuit_worker::tests::*` paths used by CI
//! filters and focused commands; helpers remain shared within this test scope.

use super::admission::*;
use super::github::*;
use super::observation::*;
use super::restart::*;
use super::spawn::*;
use super::turn_classify::*;
use super::*;
use crate::agent::spawn::ExplicitSpawnOverrides;
use crate::circuit::evaluator;
use crate::circuit::model::{CircuitNode, StepOutcome, CIRCUIT_GRAPH_VERSION};
use crate::circuit::strategy::PassiveWatcher;
use crate::circuit::test_support::advance_with_report_evidence;
use rusqlite::{Connection, OptionalExtension};

include!("tests/effect_commit.rs");
include!("tests/terminal_cleanup.rs");
include!("tests/observer_recovery.rs");
include!("tests/review_reports.rs");
include!("tests/classifier_retry.rs");
include!("tests/review_routing.rs");
include!("tests/continuation.rs");
include!("tests/parked_evidence.rs");
include!("tests/result_support.rs");
include!("tests/result_delivery.rs");
include!("tests/result_repair.rs");
include!("tests/result_handoff.rs");
include!("tests/quiet_observation.rs");
include!("tests/admission.rs");
include!("tests/capacity.rs");
include!("tests/lineage_recovery.rs");
include!("tests/watchdog.rs");
include!("tests/github.rs");
include!("tests/spawn.rs");
include!("tests/waits.rs");
include!("tests/naming.rs");
