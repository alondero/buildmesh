//! Per-login Usage Meter rows for a provider with more than one Harness Login.
//!
//! The provider account (`codex`) still produces exactly one row through the
//! ordinary catalog path: the default login. Each additional login is the same
//! provider and the same billing relationship *kind*, but a different
//! subscription, so it is read from its own home and shown as its own row,
//! labelled with the login. "Usage follows the credential": two logins are two
//! credentials, so they are two meters.
//!
//! Assembly here is pure. The command owns the preference read, the fetch
//! fan-out and the last-known store; this module only decides which rows exist
//! and in what order, applying the same keep/drop and last-known rules
//! [`crate::commands::usage`] applies to the default row.

use std::collections::{HashMap, HashSet};

use super::last_known::LastKnown;
use super::outcome::UsageOutcome;
use super::types::{MeterLogin, ProviderMeters, ProviderUsage};

/// Provider whose additional logins get their own meter rows.
pub(crate) const LOGIN_PROVIDER: &str = "codex";

/// Label of the implicit default login on a meter row.
pub(crate) const DEFAULT_LOGIN_LABEL: &str = "Default";

/// The last-known store key for one additional login's reading. The default
/// login keeps the bare provider id, so readings remembered before logins
/// existed still belong to it.
pub(crate) fn last_known_key(login_id: &str) -> String {
    format!("{LOGIN_PROVIDER}@{login_id}")
}

/// One additional login's fetch result, as the command collected it.
pub(crate) struct LoginReading {
    pub login: MeterLogin,
    pub outcome: UsageOutcome,
    pub usage: ProviderUsage,
}

/// The row for one additional login, or `None` when it has nothing to show.
///
/// Mirrors the default row's rules: a usable outcome is shown live; a
/// transient failure prefers a remembered reading; a login with no usable
/// credential (never signed in, or signed out) shows its remembered reading,
/// else no row. A native login has no configured key, so a rejected token is
/// treated like a missing one — Settings shows the sign-in state.
pub(crate) fn login_row(
    reading: LoginReading,
    previous: &HashMap<String, LastKnown>,
) -> Option<ProviderMeters> {
    let remembered = previous.get(&last_known_key(&reading.login.id));
    let row = |usage: ProviderUsage, cached_at: Option<i64>| ProviderMeters {
        provider: LOGIN_PROVIDER.to_string(),
        usage_page: super::catalog::usage_page(LOGIN_PROVIDER, Some(&usage)),
        usage_tracked: true,
        usage: Some(usage),
        cached_at,
        login: Some(reading.login.clone()),
    };
    if reading.outcome.keep(&HashSet::new(), LOGIN_PROVIDER) {
        if reading.outcome.is_transient() {
            if let Some(last) = remembered {
                return Some(row(last.usage.clone(), Some(last.cached_at)));
            }
        }
        return Some(row(reading.usage, None));
    }
    remembered.map(|last| row(last.usage.clone(), Some(last.cached_at)))
}

/// Merge the additional-login rows into the assembled list.
///
/// With no additional rows the list is returned untouched — a single login
/// renders exactly as it did before logins existed. Otherwise the default
/// Codex row (when present) is labelled with `default_login` and the
/// additional rows follow it; when the default row was dropped they go where
/// it would have been, at the end of the list.
pub(crate) fn merge_login_rows(
    mut rows: Vec<ProviderMeters>,
    default_login: MeterLogin,
    additional: Vec<ProviderMeters>,
) -> Vec<ProviderMeters> {
    if additional.is_empty() {
        return rows;
    }
    match rows.iter().position(|row| row.provider == LOGIN_PROVIDER) {
        Some(index) => {
            rows[index].login = Some(default_login);
            let tail = rows.split_off(index + 1);
            rows.extend(additional);
            rows.extend(tail);
        }
        None => rows.extend(additional),
    }
    rows
}

#[cfg(test)]
#[path = "logins_tests.rs"]
mod tests;
