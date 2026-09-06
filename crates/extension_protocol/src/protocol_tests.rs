use serde_json::json;

use super::*;
use crate::events::EventKind;
use crate::methods::Method;

#[test]
fn request_matches_the_documented_envelope() {
    let request = RequestEnvelope::new("42", Method::WorkspaceGetContext, json!({}));
    let encoded = serde_json::to_value(&request).expect("request serializes");
    assert_eq!(
        encoded,
        json!({
            "protocol": 1,
            "request_id": "42",
            "method": "workspace.getContext",
            "params": {}
        })
    );
}

#[test]
fn response_matches_the_documented_envelope() {
    let response = ResponseEnvelope::ok("42", json!({ "workspace_id": "abc" }));
    let encoded = serde_json::to_value(&response).expect("response serializes");
    assert_eq!(
        encoded,
        json!({
            "protocol": 1,
            "request_id": "42",
            "result": { "workspace_id": "abc" }
        })
    );
}

#[test]
fn event_matches_the_documented_envelope() {
    let event = EventEnvelope::new(
        EventKind::WorkspaceChanged,
        json!({ "workspace_id": "abc" }),
    );
    let encoded = serde_json::to_value(&event).expect("event serializes");
    assert_eq!(
        encoded,
        json!({
            "protocol": 1,
            "event": "workspace.changed",
            "params": { "workspace_id": "abc" }
        })
    );
}

#[test]
fn message_discriminates_requests_responses_and_events() {
    let request: Message = serde_json::from_value(json!({
        "protocol": 1,
        "request_id": "1",
        "method": "execution.run",
        "params": {}
    }))
    .expect("request decodes");
    assert!(matches!(request, Message::Request(_)));

    let event: Message = serde_json::from_value(json!({
        "protocol": 1,
        "event": "panel.action",
        "params": {}
    }))
    .expect("event decodes");
    assert!(matches!(event, Message::Event(_)));

    let ok: Message = serde_json::from_value(json!({
        "protocol": 1,
        "request_id": "1",
        "result": { "ok": true }
    }))
    .expect("ok response decodes");
    assert!(matches!(ok, Message::Response(_)));

    let failed: Message = serde_json::from_value(json!({
        "protocol": 1,
        "request_id": "1",
        "error": { "code": "permission_denied", "message": "denied" }
    }))
    .expect("error response decodes");
    let Message::Response(failed) = failed else {
        panic!("expected a response");
    };
    let ResponsePayload::Err { error } = failed.payload else {
        panic!("expected an error payload");
    };
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[test]
fn unknown_fields_are_rejected_rather_than_silently_dropped() {
    let decoded = serde_json::from_value::<RequestEnvelope>(json!({
        "protocol": 1,
        "request_id": "1",
        "method": "file.open",
        "params": {},
        "extra": true
    }));
    assert!(decoded.is_err());
}

#[test]
fn error_codes_round_trip_through_their_wire_names() {
    for code in [
        ErrorCode::PluginNotFound,
        ErrorCode::PluginFailed,
        ErrorCode::PermissionDenied,
        ErrorCode::UnsupportedCapability,
        ErrorCode::WorkspaceMissing,
        ErrorCode::RepositoryMissing,
        ErrorCode::SessionDisconnected,
        ErrorCode::ExecutionFailed,
        ErrorCode::TargetStale,
        ErrorCode::OperationConflict,
        ErrorCode::InvalidRequest,
        ErrorCode::ProtocolMismatch,
    ] {
        let encoded = serde_json::to_value(code).expect("code serializes");
        assert_eq!(encoded, json!(code.as_str()));
        let decoded: ErrorCode = serde_json::from_value(encoded).expect("code decodes");
        assert_eq!(decoded, code);
    }
}

#[test]
fn result_as_surfaces_the_remote_error_instead_of_a_decode_failure() {
    let response = ResponseEnvelope::error(
        "7",
        ExtensionError::new(ErrorCode::SessionDisconnected, "session ended"),
    );
    let decoded = response.result_as::<serde_json::Value>();
    let error = decoded.expect_err("an error response is not a result");
    assert_eq!(error.code, ErrorCode::SessionDisconnected);
}

#[test]
fn decode_params_names_the_offending_method() {
    let error = decode_params::<crate::methods::FileOpenParams>(Method::FileOpen, &json!({}))
        .expect_err("missing fields fail");
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert!(error.message.contains("file.open"), "{}", error.message);
}
