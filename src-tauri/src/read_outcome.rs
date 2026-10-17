//! The outcome vocabulary for an authoritative persistence read (issue #2330).
//!
//! A read of authoritative state has three genuinely different outcomes, and a
//! caller has to be able to tell them apart:
//!
//! * **empty** — the query succeeded and matched zero rows,
//! * **absent** — the identifier does not exist,
//! * **unavailable** — persistence could not answer at all.
//!
//! `String`, `Option<T>` and `.is_ok()` erase that distinction: a failed query
//! and a genuinely empty table both become `Ok(vec![])` and `Ok(())`, so a
//! client cannot tell "there are no agents" from "the database could not be
//! queried". On an HTTP read boundary that turns unavailable storage into a
//! *successful* empty answer, which is the failure this type exists to prevent.
//!
//! [`ReadOutcome`] is deliberately narrow. "Graceful degradation" stays
//! available for **optional** enrichment (a transcript that no reader supports
//! is a legitimate 200 `unavailable` envelope); what it must never do is stand
//! in for the authoritative answer itself. See
//! [`ReadOutcome::ok_or_unavailable`] for the collection case and
//! [`ReadOutcome::absent_or_unavailable`] for the by-id case.
//!
//! ## Transport mapping
//!
//! The HTTP mapping is deliberately *not* here — a status code is a transport
//! concern and this module is used by non-HTTP callers (the desktop Tauri
//! commands, background workers). [`crate::http::read_status`] owns the single
//! policy every route shares:
//!
//! | outcome | status |
//! |---|---|
//! | empty / present | `200` |
//! | absent | `404` |
//! | unavailable | `503` |
//!
//! `503` rather than `500` because unavailability is transient by definition —
//! a reader that retries must be able to distinguish "come back later" from
//! "this will never work".

use std::fmt;

/// Why an authoritative read did not produce a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome<T> {
    /// The read succeeded. For a collection this may legitimately be empty:
    /// *zero rows found* is a real, successful observation.
    Present(T),
    /// The requested resource does not exist. Only ever produced from a
    /// successful query that matched no row — never from a failure.
    Absent,
    /// Persistence could not answer. The payload is the concrete cause, kept
    /// for diagnostics; it is never matched against by callers.
    Unavailable(String),
}

impl<T> ReadOutcome<T> {
    /// The value, if the read succeeded.
    pub fn as_present(&self) -> Option<&T> {
        match self {
            Self::Present(value) => Some(value),
            _ => None,
        }
    }

    /// The collection read: any failure is unavailability.
    ///
    /// A list query cannot be "absent" — matching zero rows is a *success*
    /// with an empty vector — so every `Err` here is an unavailable read, and
    /// the empty `Ok` is the only path that produces an empty success. This
    /// is the one function a `GET /api/nodes`-style handler needs.
    pub fn ok_or_unavailable<E: fmt::Display>(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Self::Present(value),
            Err(error) => Self::Unavailable(error.to_string()),
        }
    }

    /// The by-id read: only "no such row" is absence.
    ///
    /// `QueryReturnedNoRows` is the *only* error that means the resource is
    /// absent, and it means that only because this is a lookup by primary key.
    /// Every other rusqlite failure — a broken query, a column that no longer
    /// exists, a type mismatch in a row, a checkout failure — is unavailability,
    /// and collapsing those to `Absent` is how a server fault became a 404.
    ///
    /// Matching the typed variant rather than the error *string* is deliberate:
    /// `to_string()` output is not a stable contract, and the old
    /// `if error.is_err() { 404 }` sites matched nothing at all.
    pub fn absent_or_unavailable(result: Result<T, rusqlite::Error>) -> Self {
        match result {
            Ok(value) => Self::Present(value),
            Err(rusqlite::Error::QueryReturnedNoRows) => Self::Absent,
            Err(error) => Self::Unavailable(error.to_string()),
        }
    }

    /// `Err` only when the read was unavailable, so a caller that already
    /// matched absence can use `?` on the remaining failure.
    pub fn into_result(self) -> Result<T, Unavailable> {
        match self {
            Self::Present(value) => Ok(value),
            Self::Absent => Err(Unavailable::Absent),
            Self::Unavailable(cause) => Err(Unavailable::Read(cause)),
        }
    }

    /// The failure, when this outcome is not a success.
    ///
    /// Lets a handler branch on `Present` for its own logic and still hand the
    /// non-present cases to one transport policy, without the awkward
    /// `into_failure().expect_err(...)` dance at each call site.
    pub fn failure(&self) -> Option<Unavailable> {
        match self {
            Self::Present(_) => None,
            Self::Absent => Some(Unavailable::Absent),
            Self::Unavailable(cause) => Some(Unavailable::Read(cause.clone())),
        }
    }
}

/// A read that did not yield a value, without the `T`.
///
/// Splitting this out lets [`ReadOutcome::into_result`] be used by handlers
/// that return `Result<(), _>` without naming a value type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// The resource does not exist → `404`.
    Absent,
    /// Persistence could not answer → `503`.
    Read(String),
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => write!(f, "not found"),
            Self::Read(cause) => write!(f, "{cause}"),
        }
    }
}

/// Map an offload failure into the same vocabulary.
///
/// `crate::blocking::run_blocking_typed` needs `E: From<String>`, and a
/// blocking task that failed to run is unavailability — not absence. Without
/// this, a task failure would have to be re-derived at every call site.
impl From<String> for Unavailable {
    fn from(message: String) -> Self {
        Self::Read(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_successful_empty_query_is_present_not_absent() {
        // The load-bearing distinction from issue #2330: an empty collection
        // is an *observation*, and must stay distinguishable from a failure.
        let outcome = ReadOutcome::ok_or_unavailable(Ok::<Vec<i64>, String>(vec![]));
        assert_eq!(outcome, ReadOutcome::Present(vec![]));
        assert_eq!(outcome.as_present(), Some(&vec![]));
        assert_eq!(outcome.failure(), None, "a success has no failure");
        assert_eq!(outcome.into_result(), Ok(vec![]));
    }

    #[test]
    fn a_failed_collection_query_is_unavailable_not_empty() {
        let outcome =
            ReadOutcome::ok_or_unavailable(Err::<Vec<i64>, &str>("no such table: agent_nodes"));
        assert_eq!(outcome.as_present(), None);
        assert_eq!(
            outcome.failure(),
            Some(Unavailable::Read("no such table: agent_nodes".into())),
            "a failed query must never present as an empty collection"
        );
        assert_eq!(
            outcome.into_result(),
            Err(Unavailable::Read("no such table: agent_nodes".into()))
        );
    }

    #[test]
    fn only_no_rows_is_absence_for_a_by_id_read() {
        let absent: ReadOutcome<i64> =
            ReadOutcome::absent_or_unavailable(Err::<i64, rusqlite::Error>(
                rusqlite::Error::QueryReturnedNoRows,
            ));
        assert_eq!(absent.failure(), Some(Unavailable::Absent));
    }

    #[test]
    fn a_broken_query_is_unavailable_even_on_a_by_id_read() {
        // Every non-`QueryReturnedNoRows` rusqlite failure is a fault, not a
        // missing resource. This is the case that used to answer 404.
        for error in [
            rusqlite::Error::InvalidQuery,
            rusqlite::Error::InvalidColumnName("agent_nodes.nope".into()),
            rusqlite::Error::InvalidParameterName("mesh".into()),
            rusqlite::Error::InvalidColumnType(0, "cols[0]".into(), rusqlite::types::Type::Null),
            rusqlite::Error::InvalidPath(PathBuf::from("/tmp/nope.sqlite")),
        ] {
            let label = format!("{error:?}");
            let outcome: ReadOutcome<i64> =
                ReadOutcome::absent_or_unavailable(Err::<i64, rusqlite::Error>(error));
            assert!(
                outcome
                    .failure()
                    .is_some_and(|f| matches!(f, Unavailable::Read(_))),
                "{label} must be unavailability, not an absent resource"
            );
            assert_ne!(
                outcome.failure(),
                Some(Unavailable::Absent),
                "{label} must not be a 404"
            );
        }
    }

    #[test]
    fn a_present_row_is_never_a_failure() {
        let outcome: ReadOutcome<i64> =
            ReadOutcome::absent_or_unavailable(Ok::<i64, rusqlite::Error>(7));
        assert_eq!(outcome.as_present(), Some(&7));
        assert_eq!(outcome.failure(), None);
        assert_eq!(outcome.into_result(), Ok(7));
    }

    /// The `From<String>` impl exists so `run_blocking_typed` can carry a
    /// typed error across the offload hop; a task failure there must never
    /// degrade into "the row was absent".
    #[test]
    fn a_task_failure_converted_from_string_is_not_absence() {
        let failure = Unavailable::from("task failed".to_string());
        assert_eq!(failure, Unavailable::Read("task failed".into()));
        assert_ne!(failure, Unavailable::Absent);
    }
}
