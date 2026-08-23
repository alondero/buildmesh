//! Mustache-style context resolution for circuit templates (issue #1206,
//! slice 1 of the Autopilot Circuits spec #1205).
//!
//! Milestone 1 implements the *basics*: dotted-path lookup against a
//! namespaced string map (`circuit.*`, `node.*`), substituted into
//! `{{ path }}` placeholders. Whitespace inside the braces is tolerated
//! (`{{circuit.name}}` == `{{ circuit.name }}`). Unknown paths resolve to
//! the empty string — a template referencing a not-yet-populated namespace
//! must not wedge a run, and the milestone-2 namespaces (`issue.*`,
//! `pr.*`, `verification.*`, `retry.*`) simply resolve empty today.
//!
//! Deliberately NOT the full Mustache spec: no sections, no inverted
//! sections, no partials, no HTML escaping (the consumers are PTY prompt
//! payloads and notification strings, not web pages). A tiny hand-rolled
//! resolver keeps the dependency surface flat — the repo's only other
//! templating is the hand-rolled placeholder substitution in
//! `autopilot::finish::render`.

use std::collections::BTreeMap;

/// Namespaced template context for one run. A `BTreeMap<String, String>`
/// with dotted keys is the whole model; builders below populate the
/// namespaces so call sites never hand-assemble `"circuit.run_id"`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CircuitContext {
    vars: BTreeMap<String, String>,
}

impl CircuitContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one dotted path.
    pub fn set(&mut self, path: &str, value: impl Into<String>) {
        self.vars.insert(path.to_string(), value.into());
    }

    pub fn get(&self, path: &str) -> Option<&str> {
        self.vars.get(path).map(|s| s.as_str())
    }

    /// Populate the `circuit.*` namespace for a run.
    pub fn with_circuit(&mut self, circuit_id: i64, name: &str, mesh_id: i64) -> &mut Self {
        self.set("circuit.id", circuit_id.to_string());
        self.set("circuit.name", name);
        self.set("circuit.mesh_id", mesh_id.to_string());
        self
    }

    /// Populate the per-run identity (`circuit.run_id`).
    pub fn with_run(&mut self, run_id: i64) -> &mut Self {
        self.set("circuit.run_id", run_id.to_string());
        self
    }

    /// Populate the `node.*` namespace for the circuit node currently
    /// executing. Values are transient — set just before resolving that
    /// node's templates.
    pub fn with_node(&mut self, node_id: &str) -> &mut Self {
        self.set("node.id", node_id);
        self
    }

    /// Serialise to the `context_json` column form.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(&self.vars).map_err(|e| format!("could not encode context: {}", e))
    }

    /// Parse back from `context_json`.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let vars: BTreeMap<String, String> =
            serde_json::from_str(json).map_err(|e| format!("invalid context_json: {}", e))?;
        Ok(Self { vars })
    }

    /// Resolve every `{{ path }}` placeholder in `template` against this
    /// context. See the module doc for the exact rules.
    pub fn resolve(&self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after_open = &rest[start + 2..];
            match after_open.find("}}") {
                Some(end) => {
                    let path = after_open[..end].trim();
                    out.push_str(self.get(path).unwrap_or(""));
                    rest = &after_open[end + 2..];
                }
                None => {
                    // No closing braces: emit the rest verbatim (an
                    // unterminated `{{` is user text, not a placeholder).
                    out.push_str("{{");
                    out.push_str(after_open);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_context() -> CircuitContext {
        let mut ctx = CircuitContext::new();
        ctx.with_circuit(7, "nightly-sweep", 3);
        ctx.with_run(42);
        ctx.with_node("spawn");
        ctx
    }

    #[test]
    fn resolves_dotted_paths_in_both_brace_styles() {
        let ctx = sample_context();
        assert_eq!(ctx.resolve("run {{circuit.name}} now"), "run nightly-sweep now");
        assert_eq!(ctx.resolve("run {{ circuit.name }} now"), "run nightly-sweep now");
    }

    #[test]
    fn multiple_placeholders_resolve_in_one_pass() {
        let ctx = sample_context();
        assert_eq!(
            ctx.resolve("{{circuit.name}}/{{circuit.run_id}}"),
            "nightly-sweep/42"
        );
    }

    #[test]
    fn unknown_namespace_resolves_empty_not_error() {
        let ctx = sample_context();
        // issue.* arrives in milestone 2 — a template using it today must
        // degrade to an empty interpolation, never wedge the run.
        assert_eq!(ctx.resolve("fix {{issue.number}} please"), "fix  please");
    }

    #[test]
    fn unterminated_placeholder_is_user_text() {
        let ctx = sample_context();
        assert_eq!(ctx.resolve("code like {{x"), "code like {{x");
    }

    #[test]
    fn empty_template_and_no_placeholders_pass_through_verbatim() {
        let ctx = sample_context();
        assert_eq!(ctx.resolve(""), "");
        assert_eq!(ctx.resolve("no placeholders here"), "no placeholders here");
    }

    #[test]
    fn whitespace_only_path_resolves_to_empty() {
        let ctx = sample_context();
        assert_eq!(ctx.resolve("a {{   }} b"), "a  b");
    }

    #[test]
    fn node_namespace_carries_the_executing_circuit_node() {
        let ctx = sample_context();
        assert_eq!(ctx.resolve("[{{node.id}}]"), "[spawn]");
    }

    #[test]
    fn context_json_round_trips_all_namespaces() {
        let ctx = sample_context();
        let json = ctx.to_json().unwrap();
        let back = CircuitContext::from_json(&json).unwrap();
        assert_eq!(back, ctx);
    }

    #[test]
    fn invalid_context_json_is_an_error() {
        assert!(CircuitContext::from_json("{oops").is_err());
    }
}
