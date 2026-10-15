//! Managed config documents — the single place Buildmesh reads, merges and
//! atomically rewrites a config file it shares with the user.
//!
//! Every harness adapter that injects a Buildmesh entry into a harness config
//! (AGY, Claude Code, Cursor, Grok, mcode, Muse, Codex, the workspace-trust
//! stores) does the same three things: read the user's document, merge
//! Buildmesh's own entry into it, and persist the result without ever losing
//! the user's content. Those three steps drifted across ten copy-pasted
//! writers — two of them disagreed about what to say when an existing file was
//! malformed, which is precisely the case where the user needs one answer.
//!
//! This module owns the invariants that are genuinely shared:
//!
//! - [`atomic_write`] is the only temp-file-then-rename writer, with a
//!   process-unique temp name so two concurrent provisions cannot collide.
//! - [`read_document`] treats a missing file as an empty document and refuses
//!   a malformed one with the single [`refuse_malformed`] wording.
//! - [`carries_anchors`] is the one ownership test: an entry is Buildmesh's
//!   when its stored command carries every declared marker.
//! - [`upsert_entry`] / [`upsert_matcher_group`] are the two merge shapes, both
//!   reporting whether anything changed so a re-provision can skip the write.
//!
//! What stays with the adapter is genuinely per-harness and stays declared
//! there: which file, which events, which handler shape, which markers make an
//! entry ours (issue #2235 — changing a marker would stop Buildmesh
//! recognising its own installed configs), and how the rejection names the
//! harness's own keys.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Backs the temp-file suffix in [`atomic_write`].
///
/// A module-level `AtomicU64` makes a name unique only inside one test
/// binary, and `scripts/rust-test-shards.mjs` runs several binaries against
/// the same directory at once, so the name carries the process id too.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A config document Buildmesh merges into, plus what a rewrite must preserve
/// about the bytes it came from.
#[derive(Debug, Clone)]
pub struct Document {
    /// The parsed document, or an empty object when no file exists yet.
    pub root: serde_json::Value,
    /// The user's file ended with a newline. Re-serialising must keep it, or
    /// every provision would strip the final newline off a hand-edited file
    /// and show up as an unrelated diff in the user's repository.
    pub trailing_newline: bool,
}

impl Document {
    /// Re-serialize the document as pretty JSON, restoring the trailing
    /// newline the user's file carried. A document Buildmesh created has none,
    /// so a fresh install stays the way the harness writes it.
    pub fn to_pretty_json(&self) -> Result<String, serde_json::Error> {
        let mut body = serde_json::to_string_pretty(&self.root)?;
        if self.trailing_newline {
            body.push('\n');
        }
        Ok(body)
    }
}

/// Why a config document could not be read into a [`Document`].
#[derive(Debug)]
pub struct DocumentError {
    path: PathBuf,
    kind: DocumentErrorKind,
}

/// The two ways reading a managed document can fail, kept apart because they
/// are different problems for the user: one is a file Buildmesh declines to
/// touch, the other is a file Buildmesh could not open at all.
#[derive(Debug)]
pub enum DocumentErrorKind {
    /// Readable bytes that are not a parseable document. Buildmesh will not
    /// overwrite them — the parse failure is carried so the refusal can name
    /// it.
    Malformed(String),
    /// The target could not be read: a directory sits in its place, permissions
    /// deny it, or its bytes are not UTF-8.
    Unreadable(std::io::Error),
}

impl DocumentError {
    /// The refusal Buildmesh gives for a config document it will not clobber:
    /// one wording for every harness, so a malformed file produces the same
    /// actionable answer whichever adapter found it.
    pub fn malformed(path: &Path, parse_error: serde_json::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: DocumentErrorKind::Malformed(parse_error.to_string()),
        }
    }

    pub fn unreadable(path: &Path, source: std::io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: DocumentErrorKind::Unreadable(source),
        }
    }

    /// True when the file was readable but is not a managed document. Adapters
    /// whose own prose differs only for this case branch on it and let every
    /// other read failure keep their own wording.
    pub fn is_malformed(&self) -> bool {
        matches!(self.kind, DocumentErrorKind::Malformed(_))
    }

    /// The underlying parse or I/O failure, for an adapter that words its own
    /// read-failure message instead of using [`Display`](fmt::Display).
    pub fn reason(&self) -> String {
        match &self.kind {
            DocumentErrorKind::Malformed(parse_error) => parse_error.clone(),
            DocumentErrorKind::Unreadable(source) => source.to_string(),
        }
    }
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            DocumentErrorKind::Malformed(parse_error) => {
                write!(f, "{}", refuse_malformed(&self.path, parse_error))
            }
            DocumentErrorKind::Unreadable(source) => {
                write!(f, "failed to read {:?}: {source}", self.path)
            }
        }
    }
}

/// Read a managed JSON config document.
///
/// A missing file is the happy path (a fresh install) and reads as an empty
/// object. An existing file that does not parse is the user's data — a partial
/// edit, a trailing comma, a hand-written config — so it is reported through
/// the single [`refuse_malformed`] refusal rather than silently replaced with
/// `{}` (issue #2235, which recorded AGY and Cursor answering this same case
/// differently).
pub fn read_document(path: &Path) -> Result<Document, DocumentError> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let root = serde_json::from_str(&content)
                .map_err(|error| DocumentError::malformed(path, error))?;
            Ok(Document {
                root,
                trailing_newline: content.ends_with('\n'),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Document {
            root: serde_json::json!({}),
            trailing_newline: false,
        }),
        Err(error) => Err(DocumentError::unreadable(path, error)),
    }
}

/// The wording Buildmesh uses when it declines to overwrite a config document
/// it could not parse. One message for every harness and every document
/// format, so a malformed file gets the same answer whichever adapter found
/// it — and so does a non-JSON format such as Kimi's TOML, which reaches this
/// through the same refusal.
pub fn refuse_malformed(path: &Path, parse_error: &str) -> String {
    format!(
        "refusing to overwrite malformed {path:?}: {parse_error}. \
         Repair or remove the file and retry"
    )
}

/// The document root as an object map, refusing a valid-JSON top level that is
/// not an object (an array, a string, a number, `null`). `label` names the
/// location in the harness's own vocabulary — `"cursor hooks.json top-level"` —
/// so the rejection points at the file the user has to repair.
pub fn root_object<'a>(
    document: &'a mut Document,
    label: &str,
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>, String> {
    // The kind tag is read before the mutable borrow: naming the shape is
    // only needed on the rejection path, and `as_object_mut` borrows the whole
    // value, so a closure reading `document.root` would conflict with it.
    let kind = json_kind(&document.root);
    document
        .root
        .as_object_mut()
        .ok_or_else(|| format!("{label} must be a JSON object; got {kind}"))
}

/// `parent[key]` as an object map, inserting an empty object when the key is
/// absent and refusing a value of any other shape.
pub fn object_entry<'a>(
    parent: &'a mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    label: &str,
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>, String> {
    let value = parent.entry(key).or_insert_with(|| serde_json::json!({}));
    let kind = json_kind(value);
    value
        .as_object_mut()
        .ok_or_else(|| format!("{label} must be a JSON object; got {kind}"))
}

/// `parent[key]` as an array, inserting an empty array when the key is absent
/// and refusing a value of any other shape.
pub fn array_entry<'a>(
    parent: &'a mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    label: &str,
) -> Result<&'a mut Vec<serde_json::Value>, String> {
    let value = parent.entry(key).or_insert_with(|| serde_json::json!([]));
    let kind = json_kind(value);
    value
        .as_array_mut()
        .ok_or_else(|| format!("{label} must be an array; got {kind}"))
}

/// Human-readable JSON kind tag for a rejection, so the message names the shape
/// the user actually has on disk instead of `serde_json::Value`.
pub fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Object(_) => "Object",
        serde_json::Value::Array(_) => "Array",
        serde_json::Value::String(_) => "String",
        serde_json::Value::Number(_) => "Number",
        serde_json::Value::Bool(_) => "Boolean",
        serde_json::Value::Null => "Null",
    }
}

/// True when `text` carries every marker in `anchors` — the one test for "this
/// entry is Buildmesh's".
///
/// PowerShell `-EncodedCommand` lines are decoded first. The WindowsInterop
/// relay bakes the callback into base64, so the anchors are invisible in the
/// stored command; without decoding, a re-provision would fail to recognise
/// its own entry and append a duplicate beside it.
///
/// Matching is on substrings rather than the whole command so a URL that
/// grows a query parameter, or a flag that moves, keeps the entry ours. The
/// anchors themselves stay with each adapter: they are what identifies an
/// already-installed config as Buildmesh's, so they must not drift.
pub fn carries_anchors(text: &str, anchors: &[&str]) -> bool {
    let decoded = crate::env::decode_powershell_command(text);
    let candidate = decoded.as_deref().unwrap_or(text);
    anchors.iter().all(|anchor| candidate.contains(anchor))
}

/// Upsert Buildmesh's own entry in a flat array of handlers, reporting whether
/// the array changed so the caller can skip an unnecessary rewrite.
///
/// `owned` picks the entry that is ours — never its position, which a user's
/// reordering or a sibling tool's entry would invalidate. A drifting Buildmesh
/// entry is replaced in place so the file keeps a single copy of it; an absent
/// one is appended.
pub fn upsert_entry(
    entries: &mut Vec<serde_json::Value>,
    owned: &impl Fn(&serde_json::Value) -> bool,
    entry: &serde_json::Value,
) -> bool {
    if let Some(existing) = entries.iter_mut().find(|candidate| owned(candidate)) {
        if *existing == *entry {
            return false;
        }
        *existing = entry.clone();
        return true;
    }
    entries.push(entry.clone());
    true
}

/// Upsert Buildmesh's handler inside one event's matcher groups, reporting
/// whether the groups changed.
///
/// Harnesses that document `hooks.<event>` as an array of matcher groups keep
/// unrelated groups, matchers and sibling handlers. Buildmesh's handler is
/// replaced in the group that already holds it, and a **new** group (carrying
/// `group_fields`, e.g. Claude Code's documented `"matcher": ""` catch-all) is
/// appended only when no group holds one. It is deliberately never appended
/// into a group that does not already hold ours: that group has its own
/// matcher, and inheriting it would silently scope Buildmesh's hook to a tool
/// the user filtered on.
pub fn upsert_matcher_group(
    groups: &mut Vec<serde_json::Value>,
    owned: &impl Fn(&serde_json::Value) -> bool,
    entry: &serde_json::Value,
    group_fields: &[(&str, &serde_json::Value)],
) -> bool {
    for group in groups.iter_mut() {
        let Some(handlers) = group
            .get_mut("hooks")
            .and_then(|value| value.as_array_mut())
        else {
            continue;
        };
        if let Some(index) = handlers.iter().position(owned) {
            if handlers[index] == *entry {
                // Already correct — stop before a later group appends a copy.
                return false;
            }
            handlers[index] = entry.clone();
            return true;
        }
    }
    let mut group = serde_json::Map::new();
    for (key, value) in group_fields {
        group.insert((*key).to_string(), (*value).clone());
    }
    group.insert("hooks".to_string(), serde_json::json!([entry.clone()]));
    groups.push(serde_json::Value::Object(group));
    true
}

/// The temp file [`atomic_write`] writes through: a sibling of the target (the
/// rename has to stay on one volume) named with this process's id and a
/// per-process counter, so a second concurrent write to the same target can
/// never land on the same name.
fn temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        "{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        counter
    ))
}

/// Persist `content` to `path` atomically: write a process-unique sibling temp
/// file, fsync it, then rename it over the target.
///
/// A reader — a harness reloading its config, a file watcher, a second
/// Buildmesh runtime — sees either the previous file or the new one, never a
/// half-written one. On Windows `std::fs::rename` is
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, atomic on the same volume, which
/// is why the temp file must be a sibling. A crash before the rename leaves
/// the user's file untouched and the temp file as the only residue; a rename
/// failure cleans the temp file up and returns the error.
///
/// On unix the temp file is created `0o600`, which the rename carries onto the
/// target. `File::create` would inherit `0o666 & !umask` — typically `0o644` —
/// and four of the writers this replaced built their temp file with
/// `tempfile::NamedTempFile`, which is `0o600` by design. Without the explicit
/// mode this would have silently widened `.claude/settings.local.json` inside
/// the user's own repository, `~/.grok/hooks/hooks.json`, Cline's hook script
/// and `.codex/{config.toml,hooks.json}` to world-readable. Picking the
/// stricter of the two keeps every managed file at or below the permissions it
/// had before.
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;

    let temp = temp_path(path);
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        if let Err(cleanup) = std::fs::remove_file(&temp) {
            tracing::warn!(
                "managed_config: failed to clean up temp file {:?}: {cleanup}",
                temp
            );
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn temp_file(dir: &TempDir, name: &str) -> PathBuf {
        dir.path().join(name)
    }

    fn temp_files(dir: &TempDir) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The merge every managed document performs: read, upsert Buildmesh's
    /// handler into one event's array, write only when something changed.
    fn provision(path: &Path, command: &str) -> Result<bool, String> {
        let mut document = read_document(path).map_err(|error| error.to_string())?;
        let hooks = root_object(&mut document, "hooks.json top-level")
            .and_then(|root| object_entry(root, "hooks", "hooks.json `hooks`"))?;
        let handlers = array_entry(hooks, "stop", "hooks.json `hooks.stop`")?;
        let owned = |candidate: &serde_json::Value| {
            candidate
                .get("command")
                .and_then(|value| value.as_str())
                .is_some_and(|value| carries_anchors(value, ANCHORS))
        };
        if !upsert_entry(handlers, &owned, &json!({ "command": command })) {
            return Ok(false);
        }
        atomic_write(path, &document.to_pretty_json().map_err(|e| e.to_string())?)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    const ANCHORS: &[&str] = &["/api/attention/", "BUILDMESH_PORT"];

    /// A user's other tools and automation share this file. Merging must add
    /// Buildmesh's entry beside them, never in place of them.
    #[test]
    fn merge_preserves_unrelated_user_entries() {
        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "hooks.json");
        std::fs::write(
            &path,
            r#"{"userTool":{"enabled":true},"hooks":{"stop":[{"command":"echo user"}]}}"#,
        )
        .unwrap();

        assert!(provision(&path, "curl $BUILDMESH_PORT/api/attention/").unwrap());

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["userTool"]["enabled"], json!(true));
        let stop = written["hooks"]["stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "user entry must survive: {stop:?}");
        assert_eq!(stop[0]["command"], json!("echo user"));
        assert_eq!(
            stop[1]["command"],
            json!("curl $BUILDMESH_PORT/api/attention/")
        );
    }

    /// Issue #886's idempotency invariant: a re-provision that finds our entry
    /// already correct must not rewrite the file, so repeated spawns leave no
    /// spurious mtime churn in the user's repository.
    #[test]
    fn re_provision_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "hooks.json");

        assert!(provision(&path, "curl $BUILDMESH_PORT/api/attention/").unwrap());
        let after_first = std::fs::read(&path).unwrap();

        assert!(
            !provision(&path, "curl $BUILDMESH_PORT/api/attention/").unwrap(),
            "an unchanged entry must report no write"
        );
        assert_eq!(std::fs::read(&path).unwrap(), after_first);
        assert_eq!(temp_files(&dir), vec!["hooks.json"], "no temp residue");
    }

    /// A drifting Buildmesh entry (an older callback shape) is replaced in
    /// place, not duplicated, and the user's sibling handlers stay put.
    #[test]
    fn re_provision_replaces_a_drifted_entry_in_place() {
        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "hooks.json");
        std::fs::write(
            &path,
            r#"{"hooks":{"stop":[{"command":"echo user"},{"command":"stale-buildmesh $BUILDMESH_PORT/api/attention/"}]}}"#,
        )
        .unwrap();

        assert!(provision(&path, "curl $BUILDMESH_PORT/api/attention/").unwrap());

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let stop = written["hooks"]["stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "drift must not append a duplicate: {stop:?}");
        assert_eq!(stop[0]["command"], json!("echo user"));
        assert_eq!(
            stop[1]["command"],
            json!("curl $BUILDMESH_PORT/api/attention/")
        );
    }

    /// The malformed case two adapters used to answer differently (issue
    /// #2235). Whatever the corruption, the file is the user's to repair: it
    /// is refused with the one message and left byte-identical, with no temp
    /// residue beside it.
    #[test]
    fn malformed_document_is_refused_and_left_byte_identical() {
        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "hooks.json");
        for original in [
            "{\"user-tool\":",
            "[]",
            "null",
            "\"user configuration\"",
            "{\"hooks\": {\"stop\": \"not-an-array\"}}",
        ] {
            std::fs::write(&path, original).unwrap();

            let error = provision(&path, "curl $BUILDMESH_PORT/api/attention/").unwrap_err();

            assert!(
                error.starts_with("refusing to overwrite malformed")
                    || error.contains("hooks.json top-level must be a JSON object")
                    || error.contains("`hooks.stop` must be an array"),
                "unexpected rejection for {original}: {error}"
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                original,
                "the user's bytes must survive {original}"
            );
            assert_eq!(temp_files(&dir), vec!["hooks.json"], "no temp residue");
        }
    }

    /// A file that cannot be read at all — a directory in the target's place,
    /// or bytes that are not UTF-8 — is reported without replacing anything.
    #[test]
    fn unreadable_target_is_reported_without_writing() {
        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "hooks.json");
        std::fs::create_dir(&path).unwrap();
        let sentinel = path.join("user-data");
        std::fs::write(&sentinel, "keep").unwrap();

        let error = provision(&path, "curl $BUILDMESH_PORT/api/attention/").unwrap_err();

        assert!(error.starts_with("failed to read"), "{error}");
        assert!(path.is_dir());
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "keep");
    }

    /// The two read failures are different problems for the user. An adapter
    /// whose own read-failure prose is pinned by its tests (AGY) branches on
    /// `is_malformed` so the malformed-document refusal stays the shared one
    /// while a directory-in-place or non-UTF-8 file keeps its own wording.
    #[test]
    fn read_failures_are_distinguishable() {
        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "hooks.json");

        std::fs::write(&path, "{not json").unwrap();
        let malformed = read_document(&path).unwrap_err();
        assert!(malformed.is_malformed());
        assert!(malformed
            .to_string()
            .starts_with("refusing to overwrite malformed"));
        assert!(!malformed.reason().is_empty());

        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        let unreadable = read_document(&path).unwrap_err();
        assert!(!unreadable.is_malformed());
        assert!(unreadable.to_string().starts_with("failed to read"));
        assert!(!unreadable.reason().is_empty());
    }

    /// The rename carries the temp file's mode onto the target, and these are
    /// config files — one of them (`.claude/settings.local.json`) lives inside
    /// the user's own repository. `File::create` would land `0o644`.
    #[cfg(unix)]
    #[test]
    fn written_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "settings.local.json");
        atomic_write(&path, "{}").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "atomic_write must not widen the target");
    }

    /// The reason two writers cannot both be the implementation: two threads
    /// writing the same target concurrently would share a fixed temp name, and
    /// on Windows one's rename would then clobber the other's half-written
    /// file. Every write must land whole, and nothing may be left behind.
    ///
    /// Pair this with `temp_names_are_unique_per_call`: a shared name fails
    /// deterministically on Windows, but on Linux a writer may win by luck if
    /// the threads happen to serialise, so this test alone is not the proof.
    #[test]
    fn concurrent_writes_do_not_share_a_temp_name() {
        let dir = TempDir::new().unwrap();
        let path = Arc::new(temp_file(&dir, "hooks.json"));
        let payloads: Vec<String> = (0..8)
            .map(|index| format!("{{\"writer\":{index},\"pad\":\"{}\"}}", "x".repeat(4096)))
            .collect();

        let workers: Vec<_> = payloads
            .iter()
            .map(|payload| {
                let path = Arc::clone(&path);
                let payload = payload.clone();
                std::thread::spawn(move || atomic_write(&path, &payload).map(|()| payload))
            })
            .collect();

        let mut written = Vec::new();
        for worker in workers {
            written.push(
                worker
                    .join()
                    .expect("no writer may panic")
                    .expect("no writer may fail"),
            );
        }

        let final_content = std::fs::read_to_string(path.as_ref()).unwrap();
        assert!(
            written.contains(&final_content),
            "the surviving file must be one writer's complete payload, not a blend"
        );
        assert_eq!(temp_files(&dir), vec!["hooks.json"], "no temp residue");
    }

    /// The temp name is what makes concurrent writes safe, so the naming rule
    /// itself is pinned: two calls for the same target never collide, and the
    /// name carries this process id.
    #[test]
    fn temp_names_are_unique_per_call() {
        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "hooks.json");

        let first = temp_path(&path);
        let second = temp_path(&path);

        assert_ne!(first, second);
        assert_eq!(
            first.parent(),
            path.parent(),
            "the rename must stay on one volume"
        );
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(std::process::id().to_string().as_str()),
            "the temp name must carry the process id: {first:?}"
        );
    }

    /// A matcher-group merge keeps the harness's own group shape: our handler
    /// goes into a group of its own, never into a user's filtered group, and
    /// re-running an event that already carries our handler appends nothing.
    #[test]
    fn matcher_group_merge_is_idempotent_and_keeps_foreign_groups() {
        let mut groups = vec![json!({ "matcher": "Bash", "hooks": [{ "command": "echo user" }] })];
        let owned = |candidate: &serde_json::Value| {
            candidate
                .get("command")
                .and_then(|value| value.as_str())
                .is_some_and(|value| carries_anchors(value, ANCHORS))
        };
        let entry = json!({ "command": "curl $BUILDMESH_PORT/api/attention/" });

        assert!(upsert_matcher_group(&mut groups, &owned, &entry, &[]));
        assert!(!upsert_matcher_group(&mut groups, &owned, &entry, &[]));

        assert_eq!(groups.len(), 2, "{groups:?}");
        // Appending into the user's group would inherit its matcher and scope
        // Buildmesh's hook to the tools they filtered on.
        assert_eq!(groups[0]["matcher"], json!("Bash"));
        assert_eq!(groups[0]["hooks"].as_array().unwrap().len(), 1);
        assert_eq!(groups[1]["hooks"][0], entry);

        // A drifted handler inside our own group is replaced in place.
        let drifted = json!({ "command": "stale $BUILDMESH_PORT/api/attention/" });
        assert!(upsert_matcher_group(&mut groups, &owned, &drifted, &[]));
        assert_eq!(groups.len(), 2, "no duplicate group: {groups:?}");
        assert_eq!(groups[1]["hooks"][0], drifted);
    }

    /// Claude Code documents an empty `matcher` as its catch-all on
    /// `Notification`; the merge must carry that field onto the group it
    /// appends.
    #[test]
    fn matcher_group_merge_carries_declared_fields() {
        let mut groups: Vec<serde_json::Value> = Vec::new();
        let owned = |candidate: &serde_json::Value| {
            candidate
                .get("command")
                .and_then(|value| value.as_str())
                .is_some_and(|value| carries_anchors(value, ANCHORS))
        };
        let matcher_all = serde_json::Value::String(String::new());

        assert!(upsert_matcher_group(
            &mut groups,
            &owned,
            &json!({ "command": "curl $BUILDMESH_PORT/api/attention/" }),
            &[("matcher", &matcher_all)],
        ));

        assert_eq!(groups[0]["matcher"], json!(""));
    }

    /// The WindowsInterop relay stores the callback base64-encoded inside a
    /// `powershell.exe -EncodedCommand` line. Ownership must survive that, or
    /// a re-provision would append a second copy of our own hook instead of
    /// replacing the first.
    #[test]
    fn ownership_survives_an_encoded_powershell_command() {
        let payload = "curl $BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID";
        let command = format!(
            "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
            crate::env::encode_powershell(payload)
        );

        assert!(carries_anchors(&command, ANCHORS));

        // Every anchor is required, decoded or not: a user's own automation
        // that merely POSTs somewhere is not ours to replace.
        assert!(!carries_anchors("curl http://example.com", ANCHORS));
        assert!(carries_anchors(
            "curl $BUILDMESH_PORT/api/attention/",
            ANCHORS
        ));
    }

    /// A user-written trailing newline must not become a spurious diff on the
    /// first re-provision.
    #[test]
    fn rewrite_preserves_the_users_trailing_newline() {
        let document = Document {
            root: json!({ "a": 1 }),
            trailing_newline: true,
        };
        assert!(document.to_pretty_json().unwrap().ends_with("}\n"));

        let fresh = Document {
            root: json!({ "a": 1 }),
            trailing_newline: false,
        };
        assert!(!fresh.to_pretty_json().unwrap().ends_with('\n'));
    }
}
