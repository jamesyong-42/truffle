//! Versioned JSON envelope helpers for `--json` output.
//!
//! Every JSON response uses a common envelope with `"version": 1` and
//! `"node"` so that consumers can detect schema changes and attribute
//! output to a specific mesh node.

use serde_json::{Map, Value};

/// Build a base envelope with `version` and `node` fields.
pub fn envelope(node_name: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("version".into(), 1.into());
    map.insert("node".into(), node_name.into());
    map
}

/// Build a JSON error envelope per RFC 013 Section 11.6:
/// `{"version": 1, "error": {"code": N, "type": "...", "message": "...", "suggestion": "..."}}`
pub fn error_envelope(
    code: i32,
    error_type: &str,
    message: &str,
    suggestion: &str,
) -> Value {
    let mut error_obj = Map::new();
    error_obj.insert("code".into(), code.into());
    error_obj.insert("type".into(), error_type.into());
    error_obj.insert("message".into(), message.into());
    if !suggestion.is_empty() {
        error_obj.insert("suggestion".into(), suggestion.into());
    }

    let mut map = Map::new();
    map.insert("version".into(), 1.into());
    map.insert("error".into(), Value::Object(error_obj));
    Value::Object(map)
}

/// Pretty-print a JSON value to stdout.
pub fn print_json(value: &Value) {
    println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_has_version_and_node() {
        let env = envelope("my-laptop");
        assert_eq!(env["version"], 1);
        assert_eq!(env["node"], "my-laptop");
    }

    #[test]
    fn test_error_envelope_structure() {
        let err = error_envelope(3, "not_found", "Node not found", "truffle ls");
        assert_eq!(err["version"], 1);
        assert_eq!(err["error"]["code"], 3);
        assert_eq!(err["error"]["type"], "not_found");
        assert_eq!(err["error"]["message"], "Node not found");
        assert_eq!(err["error"]["suggestion"], "truffle ls");
    }

    #[test]
    fn test_error_envelope_no_suggestion() {
        let err = error_envelope(1, "error", "Something failed", "");
        assert!(err["error"].get("suggestion").is_none());
    }
}
