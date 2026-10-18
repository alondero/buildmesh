//! Whole-request command arguments for wide `#[command]` signatures.
//!
//! A Tauri command's parameter names **are** its wire contract: the macro
//! reads each declared parameter out of the invoke payload by name, and the
//! desktop wrappers in `src/lib/tauri.ts`, the `tests/e2e` specs and six
//! vitest suites assert on those exact camelCase keys. So a command that
//! accepts eight or more fields cannot be collapsed into a single struct
//! parameter without moving every one of those callers onto a nested
//! payload — a transport change, not a refactor.
//!
//! [`WirePayload`] keeps the wire exactly as it is and drops the Rust-side
//! parameter count to one. The command declares
//! `payload: WirePayload<CreateCircuitRequest>` and the request struct is
//! deserialized from the *whole* invoke body instead of from one key, so the
//! frontend keeps sending `{ meshId, name, ... }` flat.
//!
//! The wrapper deliberately does not implement `Deserialize`: Tauri has a
//! blanket `CommandArg` impl for every `Deserialize` type, and that impl
//! deserializes the single key named after the parameter. Staying out of
//! `Deserialize` is what lets this type opt into whole-body extraction, and
//! deriving `Deserialize` here later would be a compile error rather than a
//! silent behaviour change.
//!
//! Field naming is the caller's business: put
//! `#[serde(default)]` on every optional field so an absent key still means
//! `None` (what Tauri's own per-key extraction does for `Option`), and
//! `#[serde(rename_all = "camelCase")]` to match what the JS wrappers send.

use serde::de::DeserializeOwned;
use tauri::ipc::{CommandArg, CommandItem, InvokeError};
use tauri::Runtime;

/// One command argument read from the entire invoke payload rather than
/// from the single key named after the parameter.
pub struct WirePayload<T>(pub T);

impl<T> WirePayload<T> {
    /// The deserialized request, so a command body can destructure it into
    /// the locals the unchanged logic below it used to take as parameters.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<'de, R: Runtime, T: DeserializeOwned> CommandArg<'de, R> for WirePayload<T> {
    fn from_command(command: CommandItem<'de, R>) -> Result<Self, InvokeError> {
        let name = command.name;
        let payload = command.message.payload().clone();
        let request = match payload {
            tauri::ipc::InvokeBody::Json(value) => serde_json::from_value(value),
            // A raw-bytes IPC body is still the request document for the
            // desktop protocol; only Android can send this shape.
            tauri::ipc::InvokeBody::Raw(bytes) => serde_json::from_slice(&bytes),
        };
        request.map(WirePayload).map_err(|error| {
            // Report the parameter name rather than the empty per-key lookup
            // the macro would have used, so a rejected invoke still names the
            // argument the caller can fix.
            tauri::Error::InvalidArgs(name, "payload", error).into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::WirePayload;
    use serde::Deserialize;

    /// The shape `create_circuit` reads: five required fields, three
    /// optional ones the frontend may omit entirely.
    #[derive(Debug, Clone, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    struct Sample {
        mesh_id: i64,
        name: String,
        #[serde(default)]
        trigger_label: Option<String>,
        #[serde(default)]
        interval_seconds: Option<i64>,
    }

    #[test]
    fn reads_the_whole_camel_case_payload() {
        let payload: serde_json::Value = serde_json::json!({
            "meshId": 7,
            "name": "review",
            "triggerLabel": "ready",
            "intervalSeconds": 900
        });
        let parsed: Sample = serde_json::from_value(payload).expect("flat payload parses");
        assert_eq!(
            parsed,
            Sample {
                mesh_id: 7,
                name: "review".into(),
                trigger_label: Some("ready".into()),
                interval_seconds: Some(900),
            }
        );
    }

    #[test]
    fn an_absent_optional_key_is_none_like_taurus_own_extraction() {
        let parsed: Sample =
            serde_json::from_value(serde_json::json!({ "meshId": 1, "name": "n" }))
                .expect("omitted optionals default");
        assert_eq!(parsed.trigger_label, None);
        assert_eq!(parsed.interval_seconds, None);
    }

    #[test]
    fn a_missing_required_field_is_an_error() {
        assert!(serde_json::from_value::<Sample>(serde_json::json!({ "name": "n" })).is_err());
    }

    /// Guards the reason this type exists at all: a frontend that still
    /// sends the old flat payload must keep working unchanged.
    #[test]
    fn the_wire_does_not_move_under_the_struct() {
        let wire = serde_json::json!({ "meshId": 7, "name": "review", "triggerLabel": null });
        let parsed: Sample = serde_json::from_value(wire).expect("null optional is None");
        assert_eq!(parsed.mesh_id, 7);
        assert_eq!(parsed.trigger_label, None);
    }

    /// `into_inner` is the only way a command body reaches the value, and it
    /// must hand back exactly what was deserialized.
    #[test]
    fn into_inner_unwraps_the_payload() {
        let parsed: Sample =
            serde_json::from_value(serde_json::json!({ "meshId": 3, "name": "x" }))
                .expect("parses");
        assert_eq!(WirePayload(parsed.clone()).into_inner(), parsed);
    }
}
