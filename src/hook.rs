use serde::Deserialize;

/// Deserialized tolerantly: unknown fields are ignored, and every field beyond
/// `hook_event_name` is optional, since several events (`PostToolUseFailure`,
/// `StopFailure`, `SessionEnd`) carry undocumented payload shapes berger doesn't need.
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct HookPayload {
    pub hook_event_name: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_payload() {
        let json = r#"{"hook_event_name":"Stop"}"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "Stop");
        assert_eq!(payload.session_id, None);
    }

    #[test]
    fn ignores_unknown_fields() {
        let json =
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "PreToolUse");
    }

    #[test]
    fn captures_session_id_when_present() {
        let json = r#"{"hook_event_name":"SessionStart","session_id":"abc-123","cwd":"/tmp"}"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.session_id, Some("abc-123".to_string()));
    }

    #[test]
    fn missing_hook_event_name_fails() {
        let json = r#"{"session_id":"abc"}"#;
        let result: Result<HookPayload, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn malformed_json_fails() {
        let result: Result<HookPayload, _> = serde_json::from_str("not json");
        assert!(result.is_err());
    }
}
