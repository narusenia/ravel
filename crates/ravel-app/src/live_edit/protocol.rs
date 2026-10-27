// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The line protocol of the live-edit socket: pure functions, no I/O, no GPUI.
//!
//! One line of JSON in, one line of JSON out:
//!
//! ```text
//! -> {"id": 1, "ops": [{"op": "add_composition", ...}]}
//! <- {"id": 1, "ok": {"created": [{"kind": "composition", "id": 3}]}}
//! <- {"id": 1, "error": {"code": "layer-not-found", "index": 0, "message": "..."}}
//! ```

use ravel_core::edit::{Applied, BatchError, Created, EditOp};
use serde_json::{Value, json};

/// The longest request line accepted, in bytes. A bound on what one
/// connection can make the process buffer.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// A well-formed request.
#[derive(Debug)]
pub struct Request {
    /// Echoed back untouched; any JSON.
    pub id: Value,
    pub ops: Vec<EditOp>,
}

/// One response line (without the newline).
fn error_line(id: &Value, code: &str, index: Option<usize>, message: &str) -> String {
    json!({"id": id, "error": {"code": code, "index": index, "message": message}}).to_string()
}

/// A `bad-request` response line saying why.
pub fn bad_request(message: &str) -> String {
    error_line(&Value::Null, "bad-request", None, message)
}

/// The request `line` holds, or the response line that refuses it.
pub fn parse_request(line: &str) -> Result<Request, String> {
    let bad = |e: String| bad_request(&e);
    let mut value: Value = serde_json::from_str(line).map_err(|e| bad(e.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| bad("the request must be a JSON object".into()))?;
    let ops = object
        .remove("ops")
        .ok_or_else(|| bad("missing field `ops`".into()))?;
    let ops = serde_json::from_value(ops).map_err(|e| bad(format!("ops: {e}")))?;
    Ok(Request {
        id: object.remove("id").unwrap_or(Value::Null),
        ops,
    })
}

/// The index of the first operation that removes something, if any.
///
/// Removal is refused until a confirmation dialog exists. `Disconnect` is
/// let through: one undo brings the edge back.
pub fn first_removal(ops: &[EditOp]) -> Option<usize> {
    ops.iter().position(|op| {
        matches!(
            op,
            EditOp::RemoveComposition { .. }
                | EditOp::RemoveLayer { .. }
                | EditOp::RemoveNode { .. }
        )
    })
}

/// The response refusing a batch that removes something.
pub fn confirmation_required(id: &Value, index: usize) -> String {
    error_line(
        id,
        "confirmation-required",
        Some(index),
        "operations that remove objects need user confirmation, which is not available yet",
    )
}

/// The success response for an applied batch.
pub fn ok_response(id: &Value, applied: &Applied) -> String {
    let created: Vec<Value> = applied
        .created
        .iter()
        .map(|c| match c {
            Created::Composition(id) => json!({"kind": "composition", "id": id}),
            Created::Layer(id) => json!({"kind": "layer", "id": id}),
            Created::Node(id) => json!({"kind": "node", "id": id}),
            Created::Edge(id) => json!({"kind": "edge", "id": id}),
        })
        .collect();
    json!({"id": id, "ok": {"created": created}}).to_string()
}

/// The failure response for a refused batch.
pub fn batch_error_response(id: &Value, err: &BatchError) -> String {
    error_line(
        id,
        err.error.code(),
        Some(err.index),
        &err.error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::edit::CompRef;
    use ravel_core::id::CompId;

    fn parsed(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn a_request_keeps_its_id_and_ops() {
        let req = parse_request(
            r#"{"id": {"n": 7}, "ops": [{"op": "add_layer", "comp": 1, "template": "solid"}]}"#,
        );
        let req = match req {
            Ok(req) => req,
            Err(line) => panic!("refused: {line}"),
        };
        assert_eq!(req.id, json!({"n": 7}));
        assert_eq!(req.ops.len(), 1);
    }

    #[test]
    fn broken_or_misshapen_requests_are_bad_requests_with_a_null_id() {
        for line in ["not json", r#"{"id": 1}"#, r#"{"ops": [{"op": "nope"}]}"#] {
            let response = parsed(&parse_request(line).unwrap_err());
            assert_eq!(response["id"], Value::Null, "{line}");
            assert_eq!(response["error"]["code"], "bad-request", "{line}");
        }
    }

    #[test]
    fn only_removals_are_held_back() {
        let comp = CompRef::Id(CompId::new(1));
        let add = EditOp::AddLayer {
            comp: comp.clone(),
            template: "solid".into(),
            alias: None,
        };
        let remove = EditOp::RemoveComposition { comp };
        assert_eq!(first_removal(std::slice::from_ref(&add)), None);
        assert_eq!(first_removal(&[add, remove]), Some(1));
    }

    #[test]
    fn the_refusal_names_its_code_and_index() {
        let response = parsed(&confirmation_required(&json!("a"), 2));
        assert_eq!(response["id"], "a");
        assert_eq!(response["error"]["code"], "confirmation-required");
        assert_eq!(response["error"]["index"], 2);
    }
}
