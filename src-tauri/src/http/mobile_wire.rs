//! Canonical mobile-route wire fixtures (issue #2318).
//!
//! The Android client reads untyped JSON, so its tests used to mock the
//! desktop with bodies written by hand. When the desktop changed a response
//! shape, those hand-written bodies kept passing: the issues feed moving to
//! `{items, completeness}` (issue #2024) and the mesh-scoped Create PR and
//! saved Launch Configuration launches all broke while the Android tests
//! stayed green (issue #2318). The web client is protected because it imports
//! the ts-rs generated types; the phone had no equivalent.
//!
//! This module is the other half of that fix. It builds one canonical sample
//! of each mobile-route body the phone reads and sends **from the Rust wire
//! type the route actually sends**, and pins the result as a committed JSON
//! fixture that the Android
//! JVM tests and the instrumentation dispatcher read instead of inline
//! strings. A Rust wire change therefore fails a test on both sides: the
//! fixture gate below reports the drift, and — once the fixture is
//! regenerated — the Android test reads the new shape and fails against the
//! client that has not caught up.
//!
//! The fixtures live in `android/app/src/test/resources/mobile-wire/`, next
//! to the tests that read them. The gate compares them, so a stale committed
//! fixture fails the Rust suite rather than being silently rewritten, and
//! `the_fixture_directory_holds_only_derived_fixtures` fails when the
//! directory holds a file no Rust type produces, so a fixture left behind by a
//! rename fails the suite too.
//!
//! Regenerating after an intentional wire change:
//!
//! ```text
//! cd src-tauri && cargo test --lib http::mobile_wire -- --ignored
//! ```

use serde::Serialize;

/// The committed fixture directory, relative to the crate root.
const FIXTURE_DIR: &str = "../android/app/src/test/resources/mobile-wire";

/// One mobile-route body and the fixture file that carries it.
struct Fixture {
    /// The fixture file name, without the `.json` suffix.
    name: &'static str,
    /// The body, serialised from the wire type the route sends.
    body: serde_json::Value,
}

/// Serialise `value` the way the route does and keep it as a fixture.
fn fixture<T: Serialize>(name: &'static str, value: &T) -> Fixture {
    Fixture {
        name,
        body: serde_json::to_value(value).expect("mobile wire fixture serialises"),
    }
}

/// Serialise a request body the way the phone sends it.
///
/// A request struct carries the fields the route *accepts*, including the
/// ones it defaults. The phone omits the ones it has nothing to say about,
/// so the canonical body drops the `None` fields and the keys the route
/// defaults — the body on the wire, not the body in memory.
fn request_fixture<T: Serialize>(name: &'static str, value: &T, defaulted: &[&str]) -> Fixture {
    let mut body = serde_json::to_value(value).expect("mobile wire request fixture serialises");
    if let Some(object) = body.as_object_mut() {
        for key in defaulted {
            object.remove(*key);
        }
        object.retain(|_, value| !value.is_null());
    }
    Fixture { name, body }
}

// ----- the canonical samples -------------------------------------------

/// The mesh the phone lists: one row, with the fields the client reads.
fn mesh() -> crate::models::Mesh {
    crate::models::Mesh {
        id: 3,
        name: "QA mesh".to_string(),
        path: "/repo".to_string(),
        base_ref: "main".to_string(),
        ..Default::default()
    }
}

/// The agent node the phone lists, and the node the issue and PR spawn
/// routes answer with.
fn agent_node() -> crate::models::AgentNode {
    crate::models::AgentNode {
        id: 17,
        mesh_id: 3,
        name: "Created agent".to_string(),
        path: "/repo".to_string(),
        branch: "main".to_string(),
        env: crate::models::EnvType::Windows,
        provider: "claude".to_string(),
        status: crate::models::SessionStatus::Idle,
        ..Default::default()
    }
}

/// The capability descriptor every Spawn Menu row carries. The phone reads
/// `accepts_initial_prompt` to decide whether a row can take a backend-built
/// prompt.
fn harness_capabilities(
    harness_id: &str,
    plain_terminal: bool,
    supports_prefill: bool,
) -> crate::agent::capabilities::HarnessCapabilities {
    crate::agent::capabilities::HarnessCapabilities {
        harness_id: harness_id.to_string(),
        background_inference: None,
        supports_resume: !plain_terminal,
        auto_resume_on_startup: false,
        requires_attention_hook: false,
        attention_capability: crate::agent::capabilities::AttentionCapability::None,
        supports_passive_turn_watcher: false,
        supports_result_file_handoff: !plain_terminal,
        produces_readable_transcript: true,
        supports_model_override: true,
        supports_effort_override: false,
        supports_extra_args: true,
        supports_prefill,
        accepts_initial_prompt: supports_prefill || !plain_terminal,
        is_plain_terminal: plain_terminal,
        effort_control: crate::agent::capabilities::EffortControlKind::None,
        permission_modes: Vec::new(),
        default_permission_mode: None,
        available_on: vec!["windows".to_string()],
    }
}

/// The saved Launch Configuration a menu row carries under `configuration`.
fn saved_configuration() -> crate::preferences::spawn_configurations::SpawnConfiguration {
    crate::preferences::spawn_configurations::SpawnConfiguration {
        id: "launch/405f11ec".to_string(),
        name: "Saved agent".to_string(),
        spawn_option_id: "claude".to_string(),
        harness_id: Some("claude".to_string()),
        ..Default::default()
    }
}

/// One Spawn Menu row. `configuration` marks a saved Launch Configuration;
/// `unavailable_reason` marks a row that cannot launch now.
///
/// A plain terminal is not resumable: the archive dialog only offers rows it
/// can resume, so the flag decides whether the terminal row is offered there.
struct MenuRow {
    /// The row's id: the Spawn Option, or the saved configuration's id.
    id: &'static str,
    /// The row's display label.
    label: &'static str,
    /// The harness behind the row.
    harness_id: &'static str,
    /// The proxied account, when the row is a proxied Spawn Option.
    provider_id: Option<&'static str>,
    /// A plain terminal is not resumable and cannot take a prompt.
    plain_terminal: bool,
    /// Whether the row can take a backend-built prompt.
    supports_prefill: bool,
    /// The saved Launch Configuration the row carries, when it has one.
    configuration: Option<crate::preferences::spawn_configurations::SpawnConfiguration>,
    /// Why the row cannot launch now.
    unavailable_reason: Option<&'static str>,
}

fn provider_row(row: MenuRow) -> crate::agent::provider::ProviderInfo {
    crate::agent::provider::ProviderInfo {
        id: row.id.to_string(),
        label: row.label.to_string(),
        color: "#5b6472".to_string(),
        icon: row.label.chars().next().unwrap_or('?').to_string(),
        resumable: !row.plain_terminal,
        harness_id: row.harness_id.to_string(),
        provider_id: row.provider_id.map(str::to_string),
        is_proxied: row.provider_id.is_some(),
        group_key: row.harness_id.to_string(),
        capabilities: harness_capabilities(
            row.harness_id,
            row.plain_terminal,
            row.supports_prefill,
        ),
        runtime: None,
        configurations: Vec::new(),
        configuration: row.configuration,
        unavailable_reason: row.unavailable_reason.map(str::to_string),
        effective_permission: None,
    }
}

/// The Spawn Menu the phone renders, in the backend's own order: a plain
/// terminal, a saved Launch Configuration, a proxied provider, and a row
/// whose executable is missing.
fn provider_menu() -> Vec<crate::agent::provider::ProviderInfo> {
    vec![
        provider_row(MenuRow {
            id: "terminal",
            label: "Terminal",
            harness_id: "terminal",
            provider_id: None,
            plain_terminal: true,
            supports_prefill: false,
            configuration: None,
            unavailable_reason: None,
        }),
        provider_row(MenuRow {
            id: "launch/405f11ec",
            label: "Saved agent",
            harness_id: "claude",
            provider_id: None,
            plain_terminal: false,
            supports_prefill: true,
            configuration: Some(saved_configuration()),
            unavailable_reason: None,
        }),
        provider_row(MenuRow {
            id: "claude:minimax",
            label: "MiniMax via Claude Code",
            harness_id: "claude",
            provider_id: Some("minimax"),
            plain_terminal: false,
            supports_prefill: true,
            configuration: None,
            unavailable_reason: None,
        }),
        provider_row(MenuRow {
            id: "codex",
            label: "Unavailable agent",
            harness_id: "codex",
            provider_id: None,
            plain_terminal: false,
            supports_prefill: true,
            configuration: None,
            unavailable_reason: Some("Missing executable"),
        }),
    ]
}

/// The completeness block every multi-page GitHub read carries.
fn completeness() -> crate::services::github::pagination::GitHubPageCompleteness {
    crate::services::github::pagination::GitHubPageCompleteness {
        returned: 1,
        pages_fetched: 1,
        complete: true,
        incomplete_reason: None,
        reported_total: None,
    }
}

/// The issues feed: the `{items, completeness}` wrapper the desktop moved to
/// in issue #2024.
fn issues_feed() -> crate::commands::pr::GitHubIssueFeed {
    crate::commands::pr::GitHubIssueFeed {
        items: vec![crate::commands::pr::GitHubIssue {
            number: 42,
            title: "Fix deadline".to_string(),
            body: "Issue details".to_string(),
            url: "https://github.com/octo/r/issues/42".to_string(),
            state: "open".to_string(),
            labels: vec!["bug".to_string()],
            blocked_by: vec![43],
            author: "octo".to_string(),
        }],
        completeness: completeness(),
    }
}

/// The pull request feed: the same `{items, completeness}` wrapper.
fn pulls_feed() -> crate::commands::pr::GitHubPullRequestFeed {
    crate::commands::pr::GitHubPullRequestFeed {
        items: vec![crate::commands::pr::GitHubPullRequest {
            number: 42,
            title: "Fix deadline".to_string(),
            body: String::new(),
            url: "https://github.com/octo/r/pull/42".to_string(),
            state: "open".to_string(),
            draft: false,
            head_ref: "feat/deadline".to_string(),
            head_repo_owner: "octo".to_string(),
            head_repo_clone_url: "https://github.com/octo/r.git".to_string(),
            head_sha: "abc123".to_string(),
            author: "octo".to_string(),
            mergeable: None,
            mergeable_state: "unknown".to_string(),
            checks_state: None,
            review_decision: None,
        }],
        completeness: completeness(),
    }
}

/// The pull request feed with nothing in it: the mesh has no open pull
/// requests.
fn pulls_feed_empty() -> crate::commands::pr::GitHubPullRequestFeed {
    crate::commands::pr::GitHubPullRequestFeed {
        items: Vec::new(),
        completeness: crate::services::github::pagination::GitHubPageCompleteness {
            returned: 0,
            pages_fetched: 0,
            complete: true,
            incomplete_reason: None,
            reported_total: None,
        },
    }
}

/// The label catalog the Issues screen offers, with the autopilot triggers
/// that label carries.
fn issue_labels() -> crate::http::routes::issues::IssueLabelCatalog {
    crate::http::routes::issues::IssueLabelCatalog {
        labels: vec!["bug".to_string(), "buildmesh:run".to_string()],
        autopilot_triggers: [("buildmesh:run".to_string(), vec!["Ship fixes".to_string()])]
            .into_iter()
            .collect(),
    }
}

/// The archived sessions the Archive screen offers for resume.
fn archived_agent_nodes() -> Vec<crate::services::agent_node_discovery::ArchivedAgentNode> {
    vec![crate::services::agent_node_discovery::ArchivedAgentNode {
        session_id: "saved-session".to_string(),
        first_message: "Archived work".to_string(),
        branch: Some("topic".to_string()),
        cwd: Some("/repos/buildmesh".to_string()),
        timestamp: Some("2026-01-01T00:00:00Z".to_string()),
        worktree_name: Some("tree".to_string()),
        resumable: true,
        provider: Some("claude".to_string()),
        node_id: None,
    }]
}

/// The diff the phone renders for one changed file.
fn git_diff() -> crate::models::DiffResult {
    crate::models::DiffResult {
        files: vec![crate::models::FileDiff {
            path: "src/lib.rs".to_string(),
            hunks: vec![crate::models::DiffHunk {
                old_start: 10,
                old_lines: 2,
                new_start: 10,
                new_lines: 3,
                old_highlighted: String::new(),
                new_highlighted: String::new(),
                lines: vec![crate::models::DiffLine {
                    line_type: "add".to_string(),
                    content: "    let mesh = Mesh::default();".to_string(),
                    old_num: None,
                    new_num: Some(11),
                }],
                lines_highlighted: Vec::new(),
            }],
            status: "modified".to_string(),
            old_path: None,
            additions: 1,
            deletions: 0,
            binary: false,
        }],
    }
}

/// The branch the phone shows for an agent node.
fn git_branch() -> crate::http::routes::git::GitBranchResponse {
    crate::http::routes::git::GitBranchResponse {
        branch: "main".to_string(),
    }
}

/// Whether the GitHub CLI is authenticated on the desktop.
fn gh_auth() -> crate::http::routes::git::GitHubAuthStatus {
    crate::http::routes::git::GitHubAuthStatus { ok: true }
}

/// Every fixture the Android client reads, in the order the routes are
/// documented.
fn fixtures() -> Vec<Fixture> {
    vec![
        // ----- responses -------------------------------------------------
        fixture("meshes", &vec![mesh()]),
        fixture("nodes", &vec![agent_node()]),
        fixture("providers", &provider_menu()),
        fixture(
            "usage",
            &vec![crate::http::routes::providers::MobileProviderUsage {
                name: "Claude subscription".to_string(),
                meter: crate::services::usage::ProviderMeters {
                    provider: "anthropic".to_string(),
                    usage_tracked: true,
                    usage_page: None,
                    cached_at: Some(1_700_000_000),
                    usage: Some(crate::services::usage::ProviderUsage {
                        provider: "anthropic".to_string(),
                        logged_in: true,
                        windows: vec![crate::services::usage::types::UsageWindow {
                            label: "Weekly".to_string(),
                            used_percent: Some(0.0),
                            resets_at: Some("2026-11-01T12:00:00Z".to_string()),
                        }],
                        balance: Some(crate::services::usage::types::BillingBalance {
                            remaining: 0.0,
                            monthly_spend: Some(12.0),
                            currency: "USD".to_string(),
                        }),
                        meters: vec![crate::services::usage::types::UsageMeter::Unlimited],
                        detail: Some("Pro plan".to_string()),
                        error: None,
                    }),
                },
            }],
        ),
        fixture("agent_node", &agent_node()),
        fixture("issues_feed", &issues_feed()),
        fixture("pulls_feed", &pulls_feed()),
        fixture("pulls_feed_empty", &pulls_feed_empty()),
        fixture("issue_labels", &issue_labels()),
        fixture(
            "pr_source",
            &crate::commands::pr::PrSource {
                head_branch: "topic".to_string(),
                base_branch: "main".to_string(),
            },
        ),
        fixture(
            "pr_mergeability",
            &crate::commands::pr::PrMergeability {
                mergeable: Some(true),
                mergeable_state: "clean".to_string(),
            },
        ),
        fixture(
            "create_pr_result",
            &crate::commands::pr::CreatePrResult {
                url: "https://example.invalid/pull/17".to_string(),
                head_branch: "topic".to_string(),
                base_branch: "main".to_string(),
            },
        ),
        fixture(
            "create_node_outcome_active",
            &crate::services::create_operation::CreateNodeOutcome::Active {
                node: agent_node(),
                operation_key: Some("op-1".to_string()),
            },
        ),
        fixture(
            "create_node_outcome_start_pending",
            &crate::services::create_operation::CreateNodeOutcome::StartPending {
                node: agent_node(),
                operation_key: Some("op-1".to_string()),
                reason: "App not ready yet".to_string(),
            },
        ),
        fixture(
            "create_node_outcome_start_failed",
            &crate::services::create_operation::CreateNodeOutcome::StartFailed {
                node: agent_node(),
                operation_key: Some("op-1".to_string()),
                start_error: "claude CLI not found".to_string(),
            },
        ),
        fixture("archived_agent_nodes", &archived_agent_nodes()),
        fixture("git_diff", &git_diff()),
        fixture("git_branch", &git_branch()),
        fixture("gh_auth", &gh_auth()),
        fixture(
            "ws_ticket",
            &crate::http::ws_ticket::WsTicket {
                ticket: "ticket".to_string(),
            },
        ),
        // ----- requests the phone sends -----------------------------------
        // A saved Launch Configuration travels as `configuration_id` beside
        // its own Spawn Option.
        request_fixture(
            "create_node_request_saved_configuration",
            &crate::http::routes::nodes::CreateNodeRequest {
                mesh_id: 3,
                prompt: Some("Fix it".to_string()),
                provider: Some("claude".to_string()),
                configuration_id: Some("launch/405f11ec".to_string()),
                rows: Some(24),
                cols: Some(80),
                operation_key: Some("op-1".to_string()),
            },
            &[],
        ),
        // A proxied Spawn Option has no configuration of its own.
        request_fixture(
            "create_node_request_proxied",
            &crate::http::routes::nodes::CreateNodeRequest {
                mesh_id: 3,
                prompt: None,
                provider: Some("claude:minimax".to_string()),
                configuration_id: None,
                rows: Some(24),
                cols: Some(80),
                operation_key: Some("op-1".to_string()),
            },
            &[],
        ),
        // The mesh default sends no launch fields: the desktop resolves its
        // own explicit > mesh > app-wide chain.
        request_fixture(
            "create_node_request_mesh_default",
            &crate::http::routes::nodes::CreateNodeRequest {
                mesh_id: 3,
                prompt: Some("Fix it".to_string()),
                provider: None,
                configuration_id: None,
                rows: Some(24),
                cols: Some(80),
                operation_key: Some("op-1".to_string()),
            },
            &[],
        ),
        request_fixture(
            "issue_spawn_request_saved_configuration",
            &crate::http::routes::issues::SpawnRequest {
                title: "Fix deadline".to_string(),
                provider: Some("claude".to_string()),
                configuration_id: Some("launch/405f11ec".to_string()),
            },
            &[],
        ),
        request_fixture(
            "issue_spawn_request_proxied",
            &crate::http::routes::issues::SpawnRequest {
                title: "Fix deadline".to_string(),
                provider: Some("claude:minimax".to_string()),
                configuration_id: None,
            },
            &[],
        ),
        request_fixture(
            "issue_spawn_request_mesh_default",
            &crate::http::routes::issues::SpawnRequest {
                title: "Fix deadline".to_string(),
                provider: None,
                configuration_id: None,
            },
            &[],
        ),
        request_fixture(
            "pull_spawn_request_saved_configuration",
            &crate::http::routes::pr::PullSpawnRequest {
                title: "Fix deadline".to_string(),
                head_ref: "feat/deadline".to_string(),
                head_sha: "abc123".to_string(),
                head_repo_owner: Some("octo".to_string()),
                head_repo_clone_url: Some("https://github.com/octo/r.git".to_string()),
                provider: Some("claude".to_string()),
                configuration_id: Some("launch/405f11ec".to_string()),
            },
            &[],
        ),
        request_fixture(
            "pull_spawn_request_mesh_default",
            &crate::http::routes::pr::PullSpawnRequest {
                title: "Fix deadline".to_string(),
                head_ref: "feat/deadline".to_string(),
                head_sha: "abc123".to_string(),
                head_repo_owner: Some("octo".to_string()),
                head_repo_clone_url: Some("https://github.com/octo/r.git".to_string()),
                provider: None,
                configuration_id: None,
            },
            &[],
        ),
        // A proxied Spawn Option names its own account and has no
        // configuration of its own.
        request_fixture(
            "pull_spawn_request_proxied",
            &crate::http::routes::pr::PullSpawnRequest {
                title: "Fix deadline".to_string(),
                head_ref: "feat/deadline".to_string(),
                head_sha: "abc123".to_string(),
                head_repo_owner: Some("octo".to_string()),
                head_repo_clone_url: Some("https://github.com/octo/r.git".to_string()),
                provider: Some("claude:minimax".to_string()),
                configuration_id: None,
            },
            &[],
        ),
        request_fixture(
            "create_pr_request",
            &crate::http::routes::pr::CreatePrRequest {
                title: "Native change".to_string(),
                body: "Explains the change".to_string(),
                node_id: 17,
                base_branch: Some("main".to_string()),
                // The same head the `pr_source` fixture previews: the create
                // body asserts the worktree is still on the branch the client
                // saw.
                head_branch: Some("topic".to_string()),
            },
            &[],
        ),
        request_fixture(
            "create_pr_request_without_base",
            &crate::http::routes::pr::CreatePrRequest {
                title: "Native change".to_string(),
                body: String::new(),
                node_id: 17,
                base_branch: None,
                head_branch: Some("topic".to_string()),
            },
            &[],
        ),
        request_fixture(
            "node_input_request",
            &crate::http::routes::nodes::InputRequest {
                seq: "yes\r".to_string(),
            },
            &[],
        ),
        request_fixture(
            "issue_label_request",
            &crate::http::routes::issues::LabelRequest {
                label: "bug".to_string(),
                present: true,
            },
            &[],
        ),
        // `rows` and `cols` are route defaults: the phone sends neither.
        // `provider` is the Spawn Option the archive dialog preselected: the
        // first menu row that is available, resumable and has no saved
        // configuration of its own.
        request_fixture(
            "import_and_resume_request",
            &crate::http::routes::agent_nodes::ImportAndResumeRequest {
                cli_session_id: "saved-session".to_string(),
                branch: "topic".to_string(),
                worktree_name: Some("tree".to_string()),
                provider: Some("claude:minimax".to_string()),
                rows: 24,
                cols: 80,
            },
            &["rows", "cols"],
        ),
    ]
}

/// The directory the committed fixtures live in.
fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_DIR)
}

#[cfg(test)]
#[path = "mobile_wire_tests.rs"]
mod tests;
