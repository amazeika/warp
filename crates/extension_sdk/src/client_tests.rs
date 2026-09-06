use std::io::Cursor;

use extension_protocol::{ErrorCode, EventKind, PROTOCOL_VERSION};
use serde_json::json;

use super::*;

/// Builds a client whose "host" has already queued these frames.
fn client(frames: &[serde_json::Value]) -> Client<Cursor<Vec<u8>>, Vec<u8>> {
    let payload = frames
        .iter()
        .map(|frame| frame.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    Client::new(Cursor::new(payload.into_bytes()), Vec::new())
}

fn ok(request_id: &str, result: serde_json::Value) -> serde_json::Value {
    json!({ "protocol": PROTOCOL_VERSION, "request_id": request_id, "result": result })
}

fn handshake_result() -> serde_json::Value {
    json!({
        "protocol": PROTOCOL_VERSION,
        "api_version": 1,
        "capabilities": ["workspace.context.v1", "execution.v1"]
    })
}

#[test]
fn the_handshake_records_what_the_host_advertised() {
    let mut client = client(&[ok("1", handshake_result())]);
    let result = client
        .initialize("dev.warp.git")
        .expect("handshake succeeds");

    assert_eq!(result.api_version, 1);
    assert_eq!(
        client.capabilities(),
        [Capability::WorkspaceContextV1, Capability::ExecutionV1]
    );
    assert!(client.supports(Capability::ExecutionV1));
    assert!(
        !client.supports(Capability::DiffOpenV1),
        "a capability the host never advertised must not be assumed"
    );
}

#[test]
fn a_call_sends_the_declared_method_and_decodes_its_result() {
    let mut client = client(&[
        ok("1", handshake_result()),
        ok(
            "2",
            json!({
                "workspace_id": "ws-1",
                "cwd": "/repo",
                "execution_target": { "kind": "local" }
            }),
        ),
    ]);
    client
        .initialize("dev.warp.git")
        .expect("handshake succeeds");
    let context = client.workspace_context().expect("context is returned");

    assert_eq!(context.workspace_id, "ws-1");
    assert_eq!(context.execution_target, ExecutionTarget::Local);
}

#[test]
fn a_refusal_surfaces_as_an_extension_error_rather_than_a_transport_failure() {
    let mut client = client(&[
        ok("1", handshake_result()),
        json!({
            "protocol": PROTOCOL_VERSION,
            "request_id": "2",
            "error": { "code": "permission_denied", "message": "not allowed" }
        }),
    ]);
    client
        .initialize("dev.warp.git")
        .expect("handshake succeeds");

    let error = client
        .workspace_context()
        .expect_err("a refused call fails");
    let ClientError::Extension(error) = error else {
        panic!("a refusal must be an Extension error, not a transport error");
    };
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[test]
fn events_arriving_mid_call_are_buffered_rather_than_lost() {
    let mut client = client(&[
        ok("1", handshake_result()),
        json!({
            "protocol": PROTOCOL_VERSION,
            "event": "repository.changed",
            "params": { "workspace_id": "ws-1" }
        }),
        ok(
            "2",
            json!({
                "workspace_id": "ws-1",
                "cwd": "/repo",
                "execution_target": { "kind": "local" }
            }),
        ),
    ]);
    client
        .initialize("dev.warp.git")
        .expect("handshake succeeds");
    client.workspace_context().expect("context is returned");

    let event = client
        .next_event()
        .expect("reading events succeeds")
        .expect("the buffered event is delivered");
    assert_eq!(event.event, EventKind::RepositoryChanged);
}

#[test]
fn a_closed_stream_ends_the_event_loop_instead_of_erroring() {
    let mut client = client(&[ok("1", handshake_result())]);
    client
        .initialize("dev.warp.git")
        .expect("handshake succeeds");
    assert!(client.next_event().expect("reading succeeds").is_none());
}

#[test]
fn a_call_against_a_closed_stream_is_reported_as_closed() {
    let mut client = client(&[]);
    let error = client
        .initialize("dev.warp.git")
        .expect_err("a handshake with no host fails");
    assert!(matches!(error, ClientError::Closed));
}
