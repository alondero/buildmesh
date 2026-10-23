//! Attention-hook provisioning — the single read → compare → upsert →
//! atomic-write sequence every harness's hook file shares.
//!
//! [`managed_config`](crate::agent::managed_config) owns the deep primitives
//! (reading a document, the two merge shapes, the atomic writer). What was
//! copied four times beside them was the *orchestration*: read, build the
//! expected entries, compare, upsert, write atomically, report the outcome —
//! and, with it, the error policy and the idempotency rule. This module owns
//! that sequence once.
//!
//! A harness supplies a [`HookProvisioningSpec`]: which file, in which format,
//! under which root key, which events with which matcher rules, and which
//! stored entries count as Buildmesh's own. The spec is data; the two format
//! adapters ([`HookFormat::Json`] and [`HookFormat::Toml`]) sit behind the one
//! [`provision_attention_hooks`] entry point, so a caller never has to know
//! which format its harness writes.
//!
//! What stays with each adapter is genuinely per-harness and must not drift:
//! the callback command (platform- and runtime-dependent), the exact event
//! list, the handler shape, and the markers that identify an already-installed
//! config as Buildmesh's (issue #2235 — changing a marker would stop us
//! recognising our own prior installs).

use crate::agent::managed_config::{self, Document};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Serializes TOML provisions: Kimi's config is shared by every session, and
/// its merge is a read-modify-write rather than a per-entry upsert.
static TOML_CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One harness's declared hook-file shape plus the callback to write.
///
/// [`command`](Self::command) is bound per provision because the callback
/// depends on the launch platform and runtime; every other field is static
/// shape data declared beside its adapter.
pub struct HookProvisioningSpec<'a> {
    /// Harness id used in the trace line ("agy", "cursor", …).
    pub harness: &'static str,
    /// File name inside the directory passed to
    /// [`provision_attention_hooks`], e.g. `hooks.json`.
    pub file: &'static str,
    /// The file in the harness's own vocabulary, used in every rejection
    /// ("agy hooks.json"). The shared malformed refusal names the path; this
    /// label names the file when the harness owns the wording.
    pub label: &'static str,
    /// The callback written into every event that does not override it.
    pub command: &'a str,
    /// Commands that differ from [`command`](Self::command), keyed by event
    /// name. An event marked optional is provisioned only when named here
    /// (AGY's question gate is absent on a WSL host).
    pub event_commands: &'a [(&'static str, &'a str)],
    /// The document format and shape.
    pub format: HookFormat,
}

/// The two document formats behind one provisioning interface.
#[derive(Debug, Clone, Copy)]
pub enum HookFormat {
    Json(JsonHookShape),
    Toml(TomlHookShape),
}

/// A JSON hook document's shape.
#[derive(Debug, Clone, Copy)]
pub struct JsonHookShape {
    /// The key the provisioned events live under; `None` means the document
    /// root itself.
    pub root_key: Option<&'static str>,
    /// Replace that key's whole value with the declared events instead of
    /// merging event by event. AGY owns one namespace key outright; Cursor,
    /// Grok and Claude share their `hooks` object with the user.
    pub replace_root: bool,
    /// Root entries the file must carry. A missing entry is inserted; a
    /// different value is refused (Cursor's `"version": 1`).
    pub required_root: &'static [(&'static str, RequiredRoot)],
    /// The handler object written into every event.
    pub handler: HandlerShape,
    /// Which stored handler counts as Buildmesh's own.
    pub ownership: Ownership,
    /// Events to provision, in file order.
    pub events: &'static [EventRule],
}

/// A TOML hook document's shape: a top-level array of hook tables.
#[derive(Debug, Clone, Copy)]
pub struct TomlHookShape {
    /// The top-level key holding an array of hook tables (`hooks`).
    pub root_key: &'static str,
    /// Events to (re)append, in file order.
    pub events: &'static [TomlEventRule],
    /// Timeout written beside every event.
    pub timeout: i64,
    /// Which stored command counts as Buildmesh's own.
    pub ownership: Ownership,
}

/// One event in a TOML hook document.
#[derive(Debug, Clone, Copy)]
pub struct TomlEventRule {
    pub name: &'static str,
    pub matcher: Option<&'static str>,
}

/// A root entry the harness's schema requires, e.g. Cursor's `"version": 1`.
///
/// Deliberately typed rather than declared as a JSON string: a malformed
/// declaration is a developer defect, so it is a compile error rather than a
/// runtime refusal aimed at the user. A harness needing another literal type
/// adds a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredRoot {
    Integer(i64),
}

impl RequiredRoot {
    fn to_json(self) -> serde_json::Value {
        match self {
            Self::Integer(value) => serde_json::json!(value),
        }
    }
}

/// One event in a JSON hook document and how Buildmesh's handler merges into
/// it.
#[derive(Debug, Clone, Copy)]
pub struct EventRule {
    pub name: &'static str,
    /// Provisioned only when [`HookProvisioningSpec::event_commands`] names
    /// it.
    pub optional: bool,
    pub merge: EventMerge,
}

/// How a JSON event stores its handlers.
#[derive(Debug, Clone, Copy)]
pub enum EventMerge {
    /// A flat array of handlers (Cursor's `stop`, AGY's `Stop`).
    Flat,
    /// Matcher groups; ours goes into the group that already holds one, or a
    /// new group carrying `matcher` when none does (Claude, AGY tool events).
    MatcherGroup { matcher: Option<&'static str> },
    /// Question events: drop ours from every group, then append one group
    /// carrying `matcher` (Grok's `^ask_user_question$`).
    Question { matcher: &'static str },
}

/// The handler object a harness stores in its hook arrays.
#[derive(Debug, Clone, Copy)]
pub enum HandlerShape {
    /// `{ "command": … }` — Cursor.
    Command,
    /// `{ "type": "command", "command": … }` — AGY, Claude Code, Grok.
    TypedCommand,
}

impl HandlerShape {
    fn build(&self, command: &str) -> serde_json::Value {
        match self {
            Self::Command => serde_json::json!({ "command": command }),
            Self::TypedCommand => serde_json::json!({ "type": "command", "command": command }),
        }
    }
}

/// What makes a stored entry Buildmesh's own.
///
/// The markers identify an already-installed config, so they stay declared by
/// the adapter that installed it: a changed marker would make a re-provision
/// append a second copy beside the first (issue #2235).
#[derive(Debug, Clone, Copy)]
pub struct Ownership {
    /// Handler fields that may carry the callback (`url` and `command` for
    /// Grok's legacy HTTP hooks).
    pub fields: &'static [&'static str],
    /// Anchors every Buildmesh callback carries.
    pub anchors: &'static [&'static str],
}

impl Ownership {
    /// True when `handler` is Buildmesh's own entry.
    pub fn owns_handler(&self, handler: &serde_json::Value) -> bool {
        self.fields.iter().any(|field| {
            handler
                .get(*field)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| self.owns_command(text))
        })
    }

    /// Anchors match either the command itself (including a bare
    /// `powershell.exe -EncodedCommand` payload) or an encoded PowerShell
    /// segment embedded in a wrapper — Kimi guards its callback with
    /// `if defined BUILDMESH_SESSION_ID …`, so the payload is only visible
    /// after decoding from wherever the segment starts.
    fn owns_command(&self, command: &str) -> bool {
        managed_config::carries_anchors(command, self.anchors)
            || command
                .find("powershell.exe ")
                .and_then(|start| crate::env::decode_powershell_command(&command[start..]))
                .is_some_and(|decoded| self.matches_anchors(&decoded))
    }

    fn matches_anchors(&self, text: &str) -> bool {
        self.anchors.iter().all(|anchor| text.contains(anchor))
    }
}

/// What a provision actually did. Re-provisioning over an already-correct file
/// reports `written: false`, which is issue #886's no-spurious-mtime
/// invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookProvisionOutcome {
    pub path: PathBuf,
    pub written: bool,
}

/// Why a hook file could not be provisioned. The message is the user-facing
/// answer: a document Buildmesh will not clobber names the path and says it is
/// the user's to repair.
#[derive(Debug)]
pub enum HookProvisionError {
    /// The document could not be read, merged or serialized; the message is
    /// the refusal shown at the spawn path.
    Refused(String),
    /// The merged document could not be persisted.
    Write { path: PathBuf, source: io::Error },
}

impl fmt::Display for HookProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(message) => f.write_str(message),
            Self::Write { path, source } => {
                write!(f, "failed to write {path:?}: {source}")
            }
        }
    }
}

impl std::error::Error for HookProvisionError {}

/// The command written into one event: a declared override, else the spec's
/// default callback. Both format adapters resolve commands through this one
/// rule.
impl<'a> HookProvisioningSpec<'a> {
    fn override_command(&self, event: &str) -> Option<&'a str> {
        self.event_commands
            .iter()
            .find(|(name, _)| *name == event)
            .map(|(_, command)| *command)
    }

    fn command_for(&self, event: &str) -> &'a str {
        self.override_command(event).unwrap_or(self.command)
    }
}

/// Read, merge and atomically write one harness's attention hooks.
///
/// The sequence is the same for every harness: a missing file is a fresh
/// install, a malformed file is refused with the one shared wording (the
/// user's bytes survive), an already-correct file is not rewritten, and a
/// changed one lands atomically.
///
/// `target_dir` is the directory that holds [`spec.file`](HookProvisioningSpec::file),
/// which is **not** always the project workspace: it is the project-level
/// config directory for a workspace harness (`.cursor`, `.claude`, `.agents`),
/// and the harness's own global config directory for one that stores hooks
/// outside the project (`~/.grok/hooks`, `~/.kimi-code`).
pub fn provision_attention_hooks(
    spec: &HookProvisioningSpec<'_>,
    target_dir: &Path,
) -> Result<HookProvisionOutcome, HookProvisionError> {
    let path = target_dir.join(spec.file);
    let written = match &spec.format {
        HookFormat::Json(shape) => provision_json(spec, shape, &path)?,
        HookFormat::Toml(shape) => provision_toml(spec, shape, &path)?,
    };
    if written {
        tracing::info!(
            "{} provision_attention_hooks: wrote {:?}",
            spec.harness,
            path
        );
    }
    Ok(HookProvisionOutcome { path, written })
}

fn provision_json(
    spec: &HookProvisioningSpec<'_>,
    shape: &JsonHookShape,
    path: &Path,
) -> Result<bool, HookProvisionError> {
    let mut document = read_json_document(spec, path)?;
    let events = resolve_events(spec, shape);
    let root = managed_config::root_object(&mut document, &format!("{} top-level", spec.label))
        .map_err(HookProvisionError::Refused)?;
    // A missing required root entry is a document change in its own right: it
    // must be persisted even when the hook events already match.
    let root_changed = apply_required_root(spec, shape, root)?;

    let events_changed = match shape.root_key {
        Some(key) if shape.replace_root => {
            let expected = expected_events(shape, &events);
            if root.get(key) == Some(&expected) {
                false
            } else {
                root.insert(key.to_string(), expected);
                true
            }
        }
        Some(key) => {
            let hooks = managed_config::object_entry(root, key, &format!("{} `{key}`", spec.label))
                .map_err(HookProvisionError::Refused)?;
            merge_events(spec, shape, hooks, &events)?
        }
        None => merge_events(spec, shape, root, &events)?,
    };

    let changed = root_changed || events_changed;
    if !changed {
        return Ok(false);
    }
    let content = document.to_pretty_json().map_err(|error| {
        HookProvisionError::Refused(format!("serialize {} failed: {error}", spec.label))
    })?;
    persist(path, &content)?;
    Ok(true)
}

/// Read a JSON hook document. Both read failures are worded here, once:
/// malformed bytes keep the shared [`managed_config::refuse_malformed`]
/// refusal, and an unreadable file names the harness's own file so the user
/// knows which one to look at.
fn read_json_document(
    spec: &HookProvisioningSpec<'_>,
    path: &Path,
) -> Result<Document, HookProvisionError> {
    match managed_config::read_document(path) {
        Ok(document) => Ok(document),
        Err(error) if error.is_malformed() => Err(HookProvisionError::Refused(error.to_string())),
        Err(error) => Err(HookProvisionError::Refused(format!(
            "failed to read {} at {}: {}",
            spec.label,
            path.display(),
            error.reason()
        ))),
    }
}

/// Drop optional events that no override supplies, then pair every remaining
/// event with its command.
fn resolve_events<'a>(
    spec: &'a HookProvisioningSpec<'a>,
    shape: &'a JsonHookShape,
) -> Vec<(&'static EventRule, &'a str)> {
    shape
        .events
        .iter()
        .filter_map(|event| {
            let overridden = spec.override_command(event.name).is_some();
            (overridden || !event.optional).then(|| (event, spec.command_for(event.name)))
        })
        .collect()
}

/// Root entries the harness's schema requires (Cursor's `version: 1`). A
/// missing entry is inserted; any other value is the user's schema mismatch.
/// Returns whether an entry had to be inserted, so the caller persists it.
fn apply_required_root(
    spec: &HookProvisioningSpec<'_>,
    shape: &JsonHookShape,
    root: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<bool, HookProvisionError> {
    let mut inserted = false;
    for (key, declared) in shape.required_root {
        let expected = declared.to_json();
        match root.get(*key) {
            None => {
                root.insert((*key).to_string(), expected);
                inserted = true;
            }
            Some(actual) if *actual == expected => {}
            Some(actual) => {
                return Err(HookProvisionError::Refused(format!(
                    "{} `{key}` must be {expected}; got {actual}",
                    spec.label
                )))
            }
        }
    }
    Ok(inserted)
}

/// The exact object a replace-root namespace must hold.
fn expected_events(
    shape: &JsonHookShape,
    events: &[(&'static EventRule, &str)],
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    for (event, command) in events {
        let handler = shape.handler.build(command);
        let value = match event.merge {
            EventMerge::Flat => serde_json::json!([handler]),
            EventMerge::MatcherGroup { matcher } => {
                let mut group = serde_json::Map::new();
                if let Some(matcher) = matcher {
                    group.insert("matcher".to_string(), serde_json::json!(matcher));
                }
                group.insert("hooks".to_string(), serde_json::json!([handler]));
                serde_json::json!([serde_json::Value::Object(group)])
            }
            EventMerge::Question { .. } => {
                unreachable!("a replace-root namespace declares flat or matcher-group events")
            }
        };
        object.insert(event.name.to_string(), value);
    }
    serde_json::Value::Object(object)
}

/// Merge the declared events into a shared `hooks` object, reporting whether
/// anything changed.
fn merge_events(
    spec: &HookProvisioningSpec<'_>,
    shape: &JsonHookShape,
    hooks: &mut serde_json::Map<String, serde_json::Value>,
    events: &[(&'static EventRule, &str)],
) -> Result<bool, HookProvisionError> {
    let mut changed = false;
    for (event, command) in events {
        let handler = shape.handler.build(command);
        let label = match shape.root_key {
            Some(root) => format!("{} `{root}.{}`", spec.label, event.name),
            None => format!("{} `{}`", spec.label, event.name),
        };
        let entries = managed_config::array_entry(hooks, event.name, &label)
            .map_err(HookProvisionError::Refused)?;
        let owned = |candidate: &serde_json::Value| shape.ownership.owns_handler(candidate);
        changed |= match event.merge {
            EventMerge::Flat => managed_config::upsert_entry(entries, &owned, &handler),
            EventMerge::MatcherGroup { matcher } => {
                let matcher = matcher.map(|text| serde_json::Value::String(text.to_string()));
                let fields: Vec<(&str, &serde_json::Value)> =
                    matcher.iter().map(|value| ("matcher", value)).collect();
                managed_config::upsert_matcher_group(entries, &owned, &handler, &fields)
            }
            EventMerge::Question { matcher } => {
                merge_question_handler(entries, &owned, &handler, matcher)
            }
        };
    }
    Ok(changed)
}

/// Grok's question-event merge: Buildmesh's handler is removed from every
/// group, groups left empty are dropped, and one group carrying `matcher` is
/// appended. Comparing the groups before and after is the idempotency test.
pub(crate) fn merge_question_handler(
    groups: &mut Vec<serde_json::Value>,
    owned: &impl Fn(&serde_json::Value) -> bool,
    handler: &serde_json::Value,
    matcher: &str,
) -> bool {
    let original = groups.clone();
    groups.retain_mut(|group| {
        let Some(handlers) = group
            .get_mut("hooks")
            .and_then(|value| value.as_array_mut())
        else {
            return true;
        };
        let carried_ours = handlers.iter().any(owned);
        handlers.retain(|candidate| !owned(candidate));
        !carried_ours || !handlers.is_empty()
    });
    groups.push(serde_json::json!({ "matcher": matcher, "hooks": [handler] }));
    *groups != original
}

fn provision_toml(
    spec: &HookProvisioningSpec<'_>,
    shape: &TomlHookShape,
    path: &Path,
) -> Result<bool, HookProvisionError> {
    use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

    let _guard = TOML_CONFIG_LOCK.lock().map_err(|error| {
        HookProvisionError::Refused(format!("{} lock poisoned: {error}", spec.label))
    })?;
    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(HookProvisionError::Refused(format!(
                "failed to read {} at {}: {error}",
                spec.label,
                path.display()
            )))
        }
    };
    let mut config = original.parse::<DocumentMut>().map_err(|error| {
        HookProvisionError::Refused(managed_config::refuse_malformed(path, &error.to_string()))
    })?;

    // Normalize the root into an editable array of tables. An inline empty
    // array or a whole inline array is accepted; any other shape is the
    // user's to repair.
    if !config.contains_key(shape.root_key)
        || config[shape.root_key]
            .as_array()
            .is_some_and(|array| array.is_empty())
    {
        config[shape.root_key] = Item::ArrayOfTables(ArrayOfTables::new());
    } else if !config[shape.root_key].is_array_of_tables() {
        let hooks = config[shape.root_key]
            .clone()
            .into_array_of_tables()
            .map_err(|_| invalid_toml_root(spec, shape))?;
        config[shape.root_key] = Item::ArrayOfTables(hooks);
    }
    let hooks = config[shape.root_key]
        .as_array_of_tables_mut()
        .ok_or_else(|| invalid_toml_root(spec, shape))?;

    hooks.retain(|hook| {
        !hook
            .get("command")
            .and_then(Item::as_str)
            .is_some_and(|command| shape.ownership.owns_command(command))
    });
    for event in shape.events {
        let mut hook = Table::new();
        hook["event"] = value(event.name);
        if let Some(matcher) = event.matcher {
            hook["matcher"] = value(matcher);
        }
        hook["command"] = value(spec.command_for(event.name));
        hook["timeout"] = value(shape.timeout);
        hooks.push(hook);
    }

    let updated = config.to_string();
    if updated == original {
        return Ok(false);
    }
    persist(path, &updated)?;
    Ok(true)
}

fn invalid_toml_root(spec: &HookProvisioningSpec<'_>, shape: &TomlHookShape) -> HookProvisionError {
    HookProvisionError::Refused(format!(
        "{} `{}` must be an array of tables",
        spec.label, shape.root_key
    ))
}

/// Persist a merged document: create the harness's directory on a fresh
/// install, then write atomically through the one managed writer.
fn persist(path: &Path, content: &str) -> Result<(), HookProvisionError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| HookProvisionError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    managed_config::atomic_write(path, content).map_err(|source| HookProvisionError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
#[path = "attention_hooks_tests.rs"]
mod tests;
