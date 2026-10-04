//! The typed shape of a fatal startup failure (issue #1525).
//!
//! Why a type instead of a `Box<dyn Error>`
//! --------------------------------------
//! Every way startup can fail used to be a bare error string, a `.unwrap()`, or
//! an `.expect()` — so a user whose app never opened got no message and no log,
//! while the Boot Error Panel (which only exists *after* React boots) promised
//! that details had been written to `buildmesh.log`. A failure before the
//! database is open cannot be reported by the panel that needs the database.
//!
//! [`StartupFailure`] is the one type every fatal startup path produces. It
//! carries three separable things:
//!
//! * **stage** — *which* part of boot failed, so the message can name it
//!   instead of guessing from a string;
//! * **summary + remediation** — text written by us, therefore safe to show a
//!   user verbatim, and separate from the technical `detail`;
//! * **paths** — the resolved log file and profile directory, so the error
//!   surface can offer "Open log folder" and "Copy details" without
//!   re-resolving anything (and without touching a half-initialised process).
//!
//! The technical `detail` is passed through
//! [`SecretScrubber`](crate::secret_scrubber::SecretScrubber) on the way in: a
//! database path or a driver message can quote back a connection string, and
//! this text is destined for a modal dialog and a bug report.
//!
//! Nothing here deletes, moves, or replaces user data. A corrupt database is
//! reported with its path and instructions for the user to act on it
//! explicitly; the application never "recovers" by wiping a file it does not
//! own the provenance of.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::secret_scrubber::SecretScrubber;

/// Which part of startup failed.
///
/// Ordered by position in the boot sequence so the label alone tells a user
/// (or a log reader) how far the app got. The split that matters for
/// behaviour is [`is_retry_safe`](StartupStage::is_retry_safe): a stage that
/// fails *before* any process-global state is installed can be retried in
/// place, one that fails at or after the database cannot.
///
/// The list is exactly the stages that can end the process. Two things that
/// read like stages deliberately are not:
///
/// * **Preferences** — `preferences::init` only records the app-data
///   directory and cannot fail, and a `preferences.json` that will not parse
///   deliberately degrades to defaults (the resolver accessors log and carry
///   on). Refusing to start over a settings file would be a behaviour change
///   this issue does not ask for, so that condition is logged at `error` with
///   the stage named in the message rather than modelled as a fatal stage.
/// * **Schema migration** — `db::init` opens the connection *and* evolves the
///   schema, so a failed migration is a [`StartupStage::Database`] failure by
///   construction. Splitting it would claim a distinction the call graph does
///   not make. The post-preferences v19 repair passes *are* separate, and are
///   non-fatal by design (see `run_profile_startup`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupStage {
    /// Resolving or creating the app-data profile directory.
    AppData,
    /// Creating the `logs` directory, or opening the bounded `buildmesh.log`.
    LogDirectory,
    /// Establishing the one-process-per-profile claim (issue #1521).
    ProfileOwnership,
    /// Opening the SQLite database and evolving its schema.
    Database,
    /// Post-migration service bring-up: legacy retirement, crash recovery.
    Services,
}

impl StartupStage {
    /// The noun phrase a user reads: "the app data directory". Deliberately
    /// not an enum name or a module name — this is shown to a person.
    pub fn label(self) -> &'static str {
        match self {
            StartupStage::AppData => "the app data directory",
            StartupStage::LogDirectory => "the log directory",
            StartupStage::ProfileOwnership => "the app data profile",
            StartupStage::Database => "the local database",
            StartupStage::Services => "startup services",
        }
    }

    /// Whether re-running the failing step in place can succeed.
    ///
    /// True only for stages that run before the process installs any global
    /// state: the profile directory could not be created because a backup tool
    /// held a handle, or `logs\` was momentarily unwritable — both are fixed
    /// from outside the app, and re-running the step is then genuinely useful.
    ///
    /// False from [`StartupStage::Database`] onward. `db::init` installs the
    /// process-global connection singleton before it can fail, and a second
    /// call short-circuits to `Ok(())` (`db::init` returns early once `DB` is
    /// set), so an in-place "Retry" would report success for a database that is
    /// still unopenable — worse than not offering it. Those stages ask the user
    /// to fix the cause and start Buildmesh again.
    pub fn is_retry_safe(self) -> bool {
        matches!(self, StartupStage::AppData | StartupStage::LogDirectory)
    }
}

impl fmt::Display for StartupStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// What the user can do about a failure. The set is computed per failure
/// (see [`StartupFailure::actions`]) rather than hard-coded in the presenter,
/// so "is there anything to retry" is a property of the failure and not of
/// whichever dialog happens to render it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAction {
    /// Put the full detail (stage, message, log path, build) on the clipboard.
    CopyDetails,
    /// Open the profile's `logs` directory in the OS file manager.
    OpenLogFolder,
    /// Re-run the failed step. Only present when
    /// [`StartupStage::is_retry_safe`].
    Retry,
    /// Stop here. Always present, and the dialog's escape/close path.
    Quit,
}

impl fmt::Display for StartupAction {
    /// Short imperative label, used in the action list of the message body
    /// (the Win32 message box has fixed Yes/No/Cancel captions, so the body
    /// is what tells the user what each button actually does).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            StartupAction::CopyDetails => "copy these details",
            StartupAction::OpenLogFolder => "open the log folder",
            StartupAction::Retry => "try again",
            StartupAction::Quit => "quit",
        };
        f.write_str(text)
    }
}

/// Where a failure's records live, when they are known.
///
/// Boxed inside [`StartupFailure`] rather than held as two `Option<PathBuf>`
/// fields. The reason is a lint with teeth: `StartupFailure` is the `Err` type
/// of every startup `Result`, and at 144 bytes it tripped clippy's
/// `result_large_err` at four call sites. Grouping the two paths behind one
/// pointer takes the error under the threshold *and* removes the possibility of
/// one being set without the other being thought about.
#[derive(Debug, Clone, Default)]
struct FailurePaths {
    /// Resolved absolute path of `logs\buildmesh.log`, present only when that
    /// log actually holds this failure's record.
    log: Option<PathBuf>,
    /// Resolved absolute path of the app-data profile directory.
    profile: Option<PathBuf>,
}

/// A fatal startup failure, safe to show to a user and to log verbatim.
#[derive(Debug, Clone)]
pub struct StartupFailure {
    stage: StartupStage,
    /// One sentence in plain language, authored here (not derived from a
    /// driver error), so it is safe to display.
    summary: String,
    /// What the user can do. Empty when there is nothing useful to say.
    remediation: String,
    /// The technical error text. Scrubbed on the way in.
    detail: String,
    /// Resolved locations, filled in as they become knowable.
    paths: Option<Box<FailurePaths>>,
    /// Set by [`StartupFailure::database`]; only true when the failure is a
    /// corrupt / not-a-database image rather than a permission or I/O problem.
    /// Drives the "your data is untouched" wording, and is asserted in tests so
    /// the "we never touched your file" promise cannot be widened by accident.
    user_data_untouched: bool,
}

impl StartupFailure {
    /// Build a failure from a stage, a user-facing summary, and a technical
    /// detail. `detail` is scrubbed here, so no call site can leak a token by
    /// forgetting to.
    pub fn new(
        stage: StartupStage,
        summary: impl Into<String>,
        detail: impl fmt::Display,
    ) -> Self {
        Self {
            stage,
            summary: summary.into(),
            remediation: String::new(),
            detail: SecretScrubber::scrub(&detail.to_string()),
            paths: None,
            user_data_untouched: false,
        }
    }

    /// The app-data directory could not be resolved or created.
    ///
    /// `dir_hint` is the directory we *tried* to use, when we got far enough
    /// to have one. It is worth showing even though creating it is what
    /// failed: it is the path a user has to fix permissions on, and the
    /// platform-resolved value is not otherwise recoverable after the failure.
    pub fn app_data(
        summary: impl Into<String>,
        detail: impl fmt::Display,
        dir_hint: Option<PathBuf>,
    ) -> Self {
        let mut failure = Self::new(StartupStage::AppData, summary, detail);
        failure.set_paths(None, dir_hint.clone());
        failure.remediation = format!(
            "Check that {} exists and that you have permission to write to it, \
             then start Buildmesh again.",
            dir_hint
                .as_ref()
                .map(|dir| dir.display().to_string())
                .unwrap_or_else(|| "Buildmesh's app data directory".to_string())
        );
        failure
    }

    /// The `logs` directory or the bounded `buildmesh.log` could not be opened.
    ///
    /// This is the one failure where there is no log to point the user at, so
    /// the message says so explicitly rather than naming a file that does not
    /// exist. A user who cannot write `logs\` still gets a reachable sink: the
    /// detail is on the screen and on stderr for a console launch.
    ///
    /// `log_path` is deliberately left unset. It means "the log that holds this
    /// failure's record", and by definition this failure has none — the
    /// directory is still named, in the remediation, which is the thing the
    /// user can actually go and fix.
    pub fn log_directory(
        summary: impl Into<String>,
        detail: impl fmt::Display,
        log_dir: Option<PathBuf>,
        profile_dir: Option<PathBuf>,
    ) -> Self {
        let mut failure = Self::new(StartupStage::LogDirectory, summary, detail);
        failure.set_paths(None, profile_dir);
        failure.remediation = match &log_dir {
            Some(dir) => format!(
                "Check that you can create files in {} (antivirus and backup \
                 tools sometimes lock it), then start Buildmesh again.",
                dir.display()
            ),
            None => String::new(),
        };
        failure
    }

    /// The one-process-per-profile claim failed (issue #1521).
    ///
    /// The message keeps that issue's reasoning: continuing would risk a
    /// second process rewriting a live instance's Agent Nodes.
    pub fn profile_ownership(
        error: &crate::instance_guard::OwnershipError,
        profile_dir: &Path,
        identifier: &str,
    ) -> Self {
        let mut failure = Self::new(
            StartupStage::ProfileOwnership,
            "Buildmesh cannot confirm that it owns its app data profile, so it \
             will not start. Carrying on would risk a second Buildmesh \
             rewriting a running instance's Agent Nodes.",
            error,
        );
        failure.set_paths(None, Some(profile_dir.to_path_buf()));
        failure.remediation = format!(
            "Check that {}\\ownership.lock is not held by another Buildmesh \
             process, then start Buildmesh again. (Profile: {identifier})",
            profile_dir.display()
        );
        failure
    }

    /// The database could not be opened, evolved, or migrated.
    ///
    /// Corruption and permission failure get different messages because the
    /// user's next step differs: corruption needs a decision about the file,
    /// while a permission error needs a decision about the folder. Neither
    /// ever leads to Buildmesh touching the file — a corrupt image is
    /// reported with its path and the user moves it aside themselves.
    pub fn database(db_path: &Path, error: &rusqlite::Error) -> Self {
        let corrupt = is_corruption(error);
        let (summary, remediation) = if corrupt {
            (
                format!(
                    "Buildmesh's local database at {} could not be read — the \
                     file is not a valid database. Nothing has been deleted or \
                     changed.",
                    db_path.display()
                ),
                format!(
                    "To start with an empty Buildmesh, move the file aside \
                     yourself and start Buildmesh again:\n\n    move \"{}\" \
                     \"{}.corrupt\"\n\nBuildmesh will not rename or delete it \
                     for you. If the database matters, copy it somewhere safe \
                     before moving it.",
                    db_path.display(),
                    db_path.display()
                ),
            )
        } else {
            (
                format!(
                    "Buildmesh's local database at {} could not be opened. \
                     Nothing has been deleted or changed.",
                    db_path.display()
                ),
                format!(
                    "Check free disk space and that you have permission to \
                     write to {} and to its folder, then start Buildmesh \
                     again.",
                    db_path.display()
                ),
            )
        };
        let mut failure = Self::new(StartupStage::Database, summary, error);
        failure.remediation = remediation;
        failure.user_data_untouched = true;
        failure
    }

    /// Post-migration service bring-up failed.
    pub fn services(what: &str, error: impl fmt::Display) -> Self {
        let mut failure = Self::new(
            StartupStage::Services,
            format!("Buildmesh could not start {what}."),
            error,
        );
        failure.remediation =
            "Restarting usually clears this. If it persists, attach the log \
             file to a bug report."
                .to_string();
        failure
    }

    /// Record where the log and the profile live. Safe to call more than once;
    /// later calls fill in blanks rather than overwrite, because the earliest
    /// caller is the one that knows when the paths first became resolvable.
    pub fn with_paths(mut self, log_path: Option<PathBuf>, profile_dir: Option<PathBuf>) -> Self {
        self.set_paths(log_path, profile_dir);
        self
    }

    /// Fill in whichever locations are not already known, never overwriting.
    ///
    /// Shared by the constructors and [`with_paths`](Self::with_paths) so
    /// "first answer wins" is one rule rather than one per call site.
    fn set_paths(&mut self, log_path: Option<PathBuf>, profile_dir: Option<PathBuf>) {
        if log_path.is_none() && profile_dir.is_none() {
            return;
        }
        let paths = self.paths.get_or_insert_with(Box::default);
        if paths.log.is_none() {
            paths.log = log_path;
        }
        if paths.profile.is_none() {
            paths.profile = profile_dir;
        }
    }

    /// The resolved log file, if one exists.
    ///
    /// The only accessor the error surface needs, and the reason the others
    /// are not here: everything else a caller could want is already in
    /// [`user_text`](Self::user_text) or
    /// [`clipboard_text`](Self::clipboard_text), and a second way to read the
    /// same field is a second thing that can drift from what was shown.
    pub fn log_path(&self) -> Option<&Path> {
        self.paths.as_ref()?.log.as_deref()
    }

    /// The resolved app-data profile directory, when known.
    pub(crate) fn profile_path(&self) -> Option<&Path> {
        self.paths.as_ref()?.profile.as_deref()
    }

    /// The actions available for this failure, in the order the presenter
    /// offers them. `Quit` is always last and always present, so the dialog
    /// can never strand the user with no way out.
    ///
    /// The list is bounded at three entries, which is the number of buttons a
    /// Win32 message box offers alongside the dismiss path — so the native
    /// surface can offer *every* action as a real button instead of dropping
    /// one.
    ///
    /// The three-way `if/else if` is the enforcement, not a formatting
    /// choice: a Win32 message box has room for three buttons alongside the
    /// dismiss path, so a fourth action would be silently discarded by the
    /// presenter. `OpenLogFolder` and `Retry` are the two that could collide —
    /// a retry-safe failure *can* carry a `log_path` (nothing forbids a caller
    /// passing one with `with_paths`) — and `Retry` wins, because the only
    /// retry-safe stages are `AppData` and `LogDirectory`, which by definition
    /// have no log holding their record for the folder to open.
    pub fn actions(&self) -> Vec<StartupAction> {
        let mut actions = vec![StartupAction::CopyDetails];
        if self.stage.is_retry_safe() {
            actions.push(StartupAction::Retry);
        } else if self.log_path().is_some() {
            // `log_path` means "the log that holds this failure's record", so
            // offering the folder is only honest when one exists. A
            // `LogDirectory` failure leaves it unset: sending the user to a
            // folder that could not be written would look like a second,
            // different bug.
            actions.push(StartupAction::OpenLogFolder);
        }
        actions.push(StartupAction::Quit);
        actions
    }

    /// The user-facing body of the error surface.
    ///
    /// The user-facing description of the failure: what failed, what it means,
    /// and where the record of it is.
    ///
    /// Deliberately says nothing about *buttons*. The native presenter is a
    /// Win32 message box whose captions are fixed (Yes/No/Cancel), so "which
    /// button does what" is presentation, and it is built once in
    /// `startup::present` from the same binding that maps the answer. Putting
    /// a bare action list here instead would have read as if it described the
    /// buttons while never saying which was which — and it is what
    /// [`actions_summary`](Self::actions_summary) is for.
    pub fn user_text(&self) -> String {
        let mut text = format!(
            "Buildmesh could not start.\n\nFailed while preparing {}.\n\n{}",
            self.stage.label(),
            self.summary
        );
        if !self.remediation.is_empty() {
            text.push_str("\n\n");
            text.push_str(&self.remediation);
        }
        text.push_str("\n\n");
        match self.log_path() {
            Some(path) => text.push_str(&format!("Details were written to:\n{}\n", path.display())),
            None => text.push_str(
                "Buildmesh could not open a log file for this failure, so the \
                 details are on this screen only.\n",
            ),
        }
        if let Some(dir) = self.profile_path() {
            text.push_str(&format!("App data folder:\n{}\n", dir.display()));
        }
        text
    }

    /// The affordances on offer, as a comma list.
    ///
    /// For "Copy details", where recording what the user *could* have done is
    /// useful context in a bug report. The button captions are deliberately not
    /// included: they are the presenter's business, and a clipboard payload
    /// naming "Yes" and "No" out of context helps nobody.
    pub fn actions_summary(&self) -> String {
        self.actions()
            .iter()
            .map(|action| action.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The full text for "Copy details": everything needed to reproduce the
    /// failure in an issue, including the build and the timestamp. Scrubbed
    /// detail travels with it, so the clipboard cannot become a worse leak
    /// than the log.
    pub fn clipboard_text(&self) -> String {
        let mut text = self.user_text();
        text.push_str(&format!("\nAvailable actions: {}.", self.actions_summary()));
        text.push_str("\n\n--- technical detail ---\n");
        text.push_str(&self.detail);
        text.push_str(&format!("\nbuild: {}", env!("GIT_SHA")));
        text.push_str(&format!("\ntime: {}", chrono::Utc::now().to_rfc3339()));
        text.push_str(&format!("\nos: {}", std::env::consts::OS));
        text
    }

    /// One structured line for `buildmesh.log`, so a user who reports "it
    /// didn't start" hands over a log that already names the stage.
    pub fn log_line(&self) -> String {
        format!(
            "STARTUP_FAILURE stage={} data_untouched={} log={} profile={} summary={:?} detail={:?}",
            self.stage.label(),
            self.user_data_untouched,
            self.log_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<none>".to_string()),
            self.profile_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<none>".to_string()),
            self.summary,
            self.detail,
        )
    }
}

impl fmt::Display for StartupFailure {
    /// The same text the user sees. Tauri's `setup` returns
    /// `Box<dyn Error>`, and this is what reaches the process stderr if the
    /// native surface is unavailable (non-Windows, or a headless session).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_text())
    }
}

impl std::error::Error for StartupFailure {}

/// Whether a SQLite failure means *the file is not a usable database*, as
/// opposed to "the process could not reach the file".
///
/// The distinction is the whole difference between "your data is damaged, and
/// you must decide what to do with it" and "fix the folder permissions". Only
/// the two image-level codes qualify: `CannotOpen` and `PermissionDenied` are
/// filesystem problems, and a schema/version error is a migration problem —
/// both of those would be misreported to a user as data loss if folded in
/// here.
///
/// Private because it is not a separate decision point: the only caller is
/// [`StartupFailure::database`], and the classifier's own tests exercise it
/// directly. Making it `pub` would only create a second way to ask the same
/// question and risk one of them drifting.
fn is_corruption(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(inner, _) => matches!(
            inner.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A `rusqlite::Error` carrying a raw SQLite result code, built the way
    /// the real driver builds one.
    ///
    /// `ffi::Error::new` takes SQLite's **raw primary result code** and maps it
    /// to an `ErrorCode` itself. Passing an `ErrorCode` would be a silent bug:
    /// the enum has no SQLite numbering, so its ordinal would be reinterpreted
    /// as a completely unrelated result code. The `corruption_classifier…` test
    /// is what proves these raw values still map to the intended codes.
    fn sqlite_error(raw_code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(raw_code), None)
    }

    /// SQLite's documented primary result codes, restated because
    /// `libsqlite3-sys` keeps its own copies private (they are regenerated by
    /// bindgen per SQLite version). These five are part of SQLite's stable
    /// documented interface.
    mod code {
        pub const ERROR: i32 = 1;
        pub const PERM: i32 = 3;
        pub const CORRUPT: i32 = 11;
        pub const CANTOPEN: i32 = 14;
        pub const NOTADB: i32 = 26;
    }

    /// A variant whose `Display` does carry caller-supplied text, for the
    /// scrubbing test. `SqliteFailure` cannot be used there because the driver's
    /// message is derived from the code.
    fn message_error(message: &str) -> rusqlite::Error {
        rusqlite::Error::InvalidParameterName(message.to_string())
    }

    /// A non-`SqliteFailure` variant, so the classifier is proven to look at
    /// the code rather than at the message text.
    fn non_sqlite_error() -> rusqlite::Error {
        rusqlite::Error::InvalidQuery
    }

    #[test]
    fn corruption_classifier_separates_image_damage_from_reachability() {
        // A file whose header is not SQLite, and a damaged image.
        assert!(is_corruption(&sqlite_error(code::NOTADB)));
        assert!(is_corruption(&sqlite_error(code::CORRUPT)));

        // The two a user would fix by touching the folder, not the file.
        assert!(!is_corruption(&sqlite_error(code::CANTOPEN)));
        assert!(!is_corruption(&sqlite_error(code::PERM)));
        // A migration/schema error is not damage either.
        assert!(!is_corruption(&sqlite_error(code::ERROR)));
        assert!(!is_corruption(&non_sqlite_error()));
    }

    /// The real-world corruption case, against a real file: write a
    /// non-database into `buildmesh.db` and classify the driver's answer.
    ///
    /// This is the "corrupt SQLite header" verification item from #1525, and
    /// it also pins the data-safety promise — the classification path must
    /// leave the file's bytes exactly as it found them.
    #[test]
    fn corrupt_database_file_classifies_as_corruption_and_is_left_byte_identical() {
        let dir = std::env::temp_dir().join(format!("bm-startup-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("buildmesh.db");

        // Not a SQLite header: a valid file that is not a database.
        let original_bytes: &[u8] = b"this is not a sqlite database, not even close\n";
        let mut file = std::fs::File::create(&db_path).unwrap();
        file.write_all(original_bytes).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let error = rusqlite::Connection::open(&db_path)
            .expect("opening a non-database file succeeds; SQLite checks the header on first use")
            // A query that actually reads the file. `SELECT 1` would be
            // answered from the expression tree without ever touching the
            // header, so it succeeds even on a corrupt image.
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect_err("the corrupt file must not answer a schema query");
        assert!(
            is_corruption(&error),
            "expected a corruption classification, got {error:?}"
        );

        // The promise: reporting a failure never rewrites user data.
        let after = std::fs::read(&db_path).unwrap();
        assert_eq!(
            after, original_bytes,
            "classifying a corrupt database must not modify it"
        );
        // And no journal/WAL sidecar was created either.
        assert!(!db_path.with_extension("db-journal").exists());
        assert!(!PathBuf::from(format!("{}-wal", db_path.display())).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corruption_message_names_the_file_and_promises_no_deletion() {
        let failure = StartupFailure::database(
            Path::new("/profiles/stable/buildmesh.db"),
            &sqlite_error(code::NOTADB),
        );

        let text = failure.user_text();
        assert!(
            text.contains("Failed while preparing the local database"),
            "must name the failing stage in prose: {text}"
        );
        assert!(
            text.contains("not a valid database"),
            "must name the condition: {text}"
        );
        assert!(
            text.contains("Nothing has been deleted or changed"),
            "must state the data-safety promise: {text}"
        );
        // The user's next step is explicit, and hands the decision to them.
        assert!(
            text.contains("move the file aside"),
            "must route corruption to explicit recovery: {text}"
        );
        assert!(
            text.contains("will not rename or delete it"),
            "must not offer automatic recovery: {text}"
        );
        // The same promise has to reach the log and the clipboard, not just
        // the dialog: a user who attaches only the log must see it.
        assert!(failure.log_line().contains("data_untouched=true"));
    }

    /// A non-corruption database failure must not accuse the user's data of
    /// being damaged — that sends them off to delete a perfectly good database.
    #[test]
    fn permission_failure_does_not_claim_corruption() {
        let failure = StartupFailure::database(
            Path::new("/profiles/stable/buildmesh.db"),
            &sqlite_error(code::CANTOPEN),
        );
        let text = failure.user_text();
        assert!(
            !text.contains("not a valid database"),
            "an unopenable file is not a damaged one: {text}"
        );
        assert!(text.contains("permission"), "should name the real problem: {text}");
    }

    #[test]
    fn stage_labels_are_prose_not_identifiers() {
        // The label reaches a user in a modal dialog, so it must not leak a
        // module name or an enum variant.
        for stage in [
            StartupStage::AppData,
            StartupStage::LogDirectory,
            StartupStage::ProfileOwnership,
            StartupStage::Database,
            StartupStage::Services,
        ] {
            let label = stage.label();
            assert!(!label.is_empty());
            assert!(
                label.starts_with("the ") || label == "startup services",
                "label reads as prose, got {label:?}"
            );
            assert!(!label.contains('_'), "no identifier text: {label:?}");
        }
    }

    /// Retry is offered only before any process-global state is installed.
    /// A `Retry` button on a database failure would be a lie: `db::init`
    /// short-circuits to `Ok(())` once the global connection is set, so the
    /// retry would "succeed" for a database that is still unopenable.
    #[test]
    fn retry_is_offered_only_for_pre_global_stages() {
        assert!(StartupStage::AppData.is_retry_safe());
        assert!(StartupStage::LogDirectory.is_retry_safe());
        for stage in [
            StartupStage::ProfileOwnership,
            StartupStage::Database,
            StartupStage::Services,
        ] {
            assert!(!stage.is_retry_safe(), "{stage:?} must not offer retry");
        }

        let retryable = StartupFailure::app_data(
            "Buildmesh could not create its app data directory.",
            "Access is denied",
            Some(PathBuf::from("/profiles/stable")),
        );
        assert!(retryable.actions().contains(&StartupAction::Retry));

        let db_failure = StartupFailure::database(
            Path::new("/profiles/stable/buildmesh.db"),
            &sqlite_error(code::NOTADB),
        );
        assert!(
            !db_failure.actions().contains(&StartupAction::Retry),
            "a database failure must not offer a retry that cannot work"
        );
    }

    /// `Quit` is the invariant that matters most: there must never be a
    /// failure surface with no way out.
    #[test]
    fn quit_is_always_offered_and_always_last() {
        let failures = [
            StartupFailure::app_data("no dir", "denied", Some(PathBuf::from("/p"))),
            StartupFailure::log_directory("no logs", "denied", None, None),
            StartupFailure::services("the crash-recovery sweep", "no rows table"),
        ];
        for failure in &failures {
            let actions = failure.actions();
            assert_eq!(
                actions.last(),
                Some(&StartupAction::Quit),
                "quit must be offered and last for {failure:?}"
            );
            assert!(actions.contains(&StartupAction::CopyDetails));
        }
    }

    /// A `LogDirectory` failure names the folder in its remediation but cannot
    /// offer to open it (nothing was written there), while a database failure
    /// — whose record *is* in that log — can.
    #[test]
    fn open_log_folder_is_offered_only_when_the_log_holds_this_failure() {
        let db_failure = StartupFailure::database(
            Path::new("/profiles/stable/buildmesh.db"),
            &sqlite_error(code::NOTADB),
        )
        .with_paths(
            Some(PathBuf::from("/profiles/stable/logs/buildmesh.log")),
            Some(PathBuf::from("/profiles/stable")),
        );
        assert!(db_failure.actions().contains(&StartupAction::OpenLogFolder));
        assert_eq!(
            db_failure.log_path(),
            Some(Path::new("/profiles/stable/logs/buildmesh.log"))
        );

        // The log directory exists, but the log could not be opened — so it
        // does not contain this failure, and the text says so instead of
        // promising a file that was never written.
        let log_failure = StartupFailure::log_directory(
            "Buildmesh could not open its log file.",
            "Access is denied",
            Some(PathBuf::from("/profiles/stable/logs")),
            Some(PathBuf::from("/profiles/stable")),
        );
        assert_eq!(log_failure.log_path(), None);
        assert!(!log_failure.actions().contains(&StartupAction::OpenLogFolder));
        let text = log_failure.user_text();
        assert!(
            text.contains("could not open a log file for this failure"),
            "must not promise a log it could not write: {text}"
        );
        // The folder is still named, because that is the thing to go and fix.
        assert!(text.contains("/profiles/stable/logs"), "{text}");

        // App-data resolution failed: there is no folder to open at all.
        let no_log = StartupFailure::app_data(
            "Buildmesh could not find its app data directory.",
            "APPDATA is not set",
            None,
        );
        assert!(!no_log.actions().contains(&StartupAction::OpenLogFolder));
    }

    /// The native surface can offer one button per action, so the action list
    /// must never exceed what its buttons can express. This is the invariant
    /// that makes "Quit is always present" reachable rather than aspirational.
    #[test]
    fn every_failure_fits_in_three_offered_actions() {
        let corrupt = sqlite_error(code::NOTADB);
        let log = Some(PathBuf::from("/p/logs/buildmesh.log"));
        let profile = Some(PathBuf::from("/p"));
        let cases = [
            // Retry-safe stages, with and without a resolvable log path.
            StartupFailure::app_data("no dir", "denied", profile.clone()),
            StartupFailure::app_data("no dir", "denied", None),
            StartupFailure::log_directory(
                "no logs",
                "denied",
                Some(PathBuf::from("/p/logs")),
                profile.clone(),
            ),
            StartupFailure::log_directory("no logs", "denied", None, None),
            // The collision this invariant exists for: a retry-safe stage that
            // a caller has *also* given a log path. An earlier version of this
            // test claimed to cover this and did not, because it passed
            // directories where a log path was required.
            StartupFailure::log_directory(
                "no logs",
                "denied",
                Some(PathBuf::from("/p/logs")),
                profile.clone(),
            )
            .with_paths(log.clone(), profile.clone()),
            StartupFailure::app_data("no dir", "denied", profile.clone())
                .with_paths(log.clone(), profile.clone()),
            // Post-global stages, which also carry a log path.
            StartupFailure::database(Path::new("/p/buildmesh.db"), &corrupt)
                .with_paths(log.clone(), profile.clone()),
            StartupFailure::services("the recovery sweep", "no such table")
                .with_paths(log.clone(), profile.clone()),
        ];
        for failure in &cases {
            let actions = failure.actions();
            assert!(
                actions.len() <= 3,
                "a Win32 message box has room for three: {failure:?} -> {actions:?}"
            );
            assert!(actions.contains(&StartupAction::Quit), "{failure:?}");
        }
    }

    /// The detail reaches a modal dialog, a bug report, and the clipboard, so
    /// it must be scrubbed on the way in — a driver message can quote back a
    /// connection string containing a token.
    #[test]
    fn detail_is_scrubbed_before_it_can_be_displayed_or_copied() {
        // `SqliteFailure` derives its message from the result code, so the
        // token has to ride in on a variant that carries caller text.
        let failure = StartupFailure::database(
            Path::new("/profiles/stable/buildmesh.db"),
            &message_error("unable to open: api_key=sk-live-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"),
        );
        let token = "sk-live-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345";
        for (sink, text) in [
            ("the dialog", failure.user_text()),
            ("the clipboard", failure.clipboard_text()),
            ("the log", failure.log_line()),
        ] {
            assert!(
                !text.contains(token),
                "a token must not reach {sink}: {text}"
            );
        }
        // The token is masked, not dropped: the shape of the error survives so
        // a reader can still tell what went wrong.
        assert!(failure.clipboard_text().contains("api_key="));
    }

    /// `with_paths` fills blanks but never overwrites an earlier answer: the
    /// first caller is the one that saw the path while it was still true.
    #[test]
    fn with_paths_fills_blanks_without_overwriting() {
        let failure = StartupFailure::app_data("no dir", "denied", Some(PathBuf::from("/first")))
            .with_paths(Some(PathBuf::from("/first/logs/buildmesh.log")), None);
        assert_eq!(failure.log_path(), Some(Path::new("/first/logs/buildmesh.log")));
        let text = failure.user_text();
        assert!(
            text.contains("App data folder:\n/first"),
            "the first profile dir must survive: {text}"
        );
    }

    /// The log line is what a user hands over when reporting "it didn't start",
    /// so it must name the stage and the data-safety promise in one line.
    #[test]
    fn log_line_names_stage_and_data_promise() {
        let failure = StartupFailure::database(
            Path::new("/profiles/stable/buildmesh.db"),
            &sqlite_error(code::NOTADB),
        )
        .with_paths(
            Some(PathBuf::from("/profiles/stable/logs/buildmesh.log")),
            Some(PathBuf::from("/profiles/stable")),
        );
        let line = failure.log_line();
        assert!(line.contains("STARTUP_FAILURE"));
        assert!(line.contains("the local database"));
        assert!(line.contains("data_untouched=true"));
        assert!(line.contains("/profiles/stable/logs/buildmesh.log"));
    }
}
