use extension_protocol::{
    Capability, ErrorCode, ExecutionTarget, ExtensionManifest, InitializeResult, Message, Method,
    RequestEnvelope, ResponsePayload,
};
use serde_json::json;

use super::*;

const MANIFEST: &str = r#"
id = "dev.warp.git"
name = "Warp Git"
version = "0.1.0"
api_version = 1
command = "./warp-git"

[permissions]
workspace_read = true
process_execute = true

[execution]
allowed_executables = ["git"]

[[commands]]
id = "git.fetch"
title = "Git: Fetch"

[[panels]]
id = "git"
title = "Git"
location = "left"
"#;

/// Records what reached the host so tests can assert a call was gated *before*
/// dispatch rather than merely failing inside it.
#[derive(Default)]
struct RecordingHost {
    capabilities: Vec<Capability>,
    dispatched: Vec<Method>,
}

impl RecordingHost {
    fn with_all_capabilities() -> Self {
        Self {
            capabilities: Capability::ALL.to_vec(),
            dispatched: Vec::new(),
        }
    }
}

impl ExtensionHost for RecordingHost {
    fn capabilities(&self) -> Vec<Capability> {
        self.capabilities.clone()
    }

    fn dispatch(
        &mut self,
        method: Method,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, extension_protocol::ExtensionError> {
        self.dispatched.push(method);
        Ok(json!({ "dispatched": method.as_str() }))
    }
}

fn manifest() -> ExtensionManifest {
    let manifest = ExtensionManifest::parse(MANIFEST).expect("manifest parses");
    manifest.validate().expect("manifest is valid");
    manifest
}

/// A session and the host it dispatches into.
///
/// The session no longer owns its host, so the tests hold the pair together to
/// keep asserting what did and did not reach the host side of a gate.
struct Connected {
    session: Session,
    host: RecordingHost,
}

impl Connected {
    fn is_initialized(&self) -> bool {
        self.session.is_initialized()
    }

    fn handle(&mut self, message: Message) -> Option<Message> {
        self.session.handle_message(message, &mut self.host)
    }
}

fn session(host: RecordingHost) -> Connected {
    Connected {
        session: Session::new(manifest()),
        host,
    }
}

fn initialized_session(host: RecordingHost) -> Connected {
    let mut session = session(host);
    let reply = request(
        &mut session,
        Method::ExtensionInitialize,
        json!({ "extension_id": "dev.warp.git", "api_version": 1 }),
    );
    assert!(matches!(reply, ResponsePayload::Ok { .. }));
    session
}

fn request(session: &mut Connected, method: Method, params: serde_json::Value) -> ResponsePayload {
    let message = Message::Request(RequestEnvelope::new("1", method, params));
    let Some(Message::Response(response)) = session.handle(message) else {
        panic!("a request must produce a response");
    };
    response.payload
}

fn error_code(payload: ResponsePayload) -> ErrorCode {
    match payload {
        ResponsePayload::Err { error } => error.code,
        ResponsePayload::Ok { result } => panic!("expected an error, got {result}"),
    }
}

fn run_params(executable: &str) -> serde_json::Value {
    json!({
        "target": { "kind": "local" },
        "cwd": "/home/user/project",
        "executable": executable,
        "args": ["status"]
    })
}

#[test]
fn the_handshake_reports_the_protocol_api_and_capabilities() {
    let mut session = session(RecordingHost::with_all_capabilities());
    let payload = request(
        &mut session,
        Method::ExtensionInitialize,
        json!({ "extension_id": "dev.warp.git", "api_version": 1 }),
    );

    let ResponsePayload::Ok { result } = payload else {
        panic!("the handshake must succeed");
    };
    let result: InitializeResult = serde_json::from_value(result).expect("result decodes");
    assert_eq!(result.protocol, extension_protocol::PROTOCOL_VERSION);
    assert_eq!(result.api_version, extension_protocol::API_VERSION);
    assert_eq!(result.capabilities, Capability::ALL);
    assert!(session.is_initialized());
}

#[test]
fn a_handshake_claiming_another_extension_is_rejected() {
    let mut session = session(RecordingHost::with_all_capabilities());
    let payload = request(
        &mut session,
        Method::ExtensionInitialize,
        json!({ "extension_id": "dev.warp.other", "api_version": 1 }),
    );
    assert_eq!(error_code(payload), ErrorCode::InvalidRequest);
    assert!(!session.is_initialized());
}

#[test]
fn a_handshake_for_another_api_generation_is_a_protocol_mismatch() {
    let mut session = session(RecordingHost::with_all_capabilities());
    let payload = request(
        &mut session,
        Method::ExtensionInitialize,
        json!({ "extension_id": "dev.warp.git", "api_version": 99 }),
    );
    assert_eq!(error_code(payload), ErrorCode::ProtocolMismatch);
}

#[test]
fn calls_before_the_handshake_are_refused() {
    let mut session = session(RecordingHost::with_all_capabilities());
    let payload = request(&mut session, Method::WorkspaceGetContext, json!({}));
    assert_eq!(error_code(payload), ErrorCode::InvalidRequest);
    assert!(session.host.dispatched.is_empty());
}

#[test]
fn a_wrong_protocol_version_is_rejected_before_anything_else() {
    let mut session = session(RecordingHost::with_all_capabilities());
    let mut envelope = RequestEnvelope::new("1", Method::ExtensionInitialize, json!({}));
    envelope.protocol = 99;

    let Some(Message::Response(response)) = session.handle(Message::Request(envelope)) else {
        panic!("a request must produce a response");
    };
    assert_eq!(error_code(response.payload), ErrorCode::ProtocolMismatch);
}

#[test]
fn a_granted_method_reaches_the_host() {
    let mut session = initialized_session(RecordingHost::with_all_capabilities());
    let payload = request(&mut session, Method::WorkspaceGetContext, json!({}));
    assert!(matches!(payload, ResponsePayload::Ok { .. }));
    assert_eq!(session.host.dispatched, [Method::WorkspaceGetContext]);
}

#[test]
fn an_undeclared_permission_is_denied_without_reaching_the_host() {
    let mut session = initialized_session(RecordingHost::with_all_capabilities());
    let payload = request(
        &mut session,
        Method::NotificationShow,
        json!({ "title": "hello", "level": "info" }),
    );
    assert_eq!(error_code(payload), ErrorCode::PermissionDenied);
    assert!(session.host.dispatched.is_empty());
}

#[test]
fn an_unadvertised_capability_is_reported_as_unsupported_not_denied() {
    let host = RecordingHost {
        capabilities: vec![Capability::WorkspaceContextV1],
        dispatched: Vec::new(),
    };
    let mut session = initialized_session(host);
    let payload = request(&mut session, Method::ExecutionRun, run_params("git"));
    assert_eq!(error_code(payload), ErrorCode::UnsupportedCapability);
    assert!(session.host.dispatched.is_empty());
}

#[test]
fn an_allowlisted_executable_runs() {
    let mut session = initialized_session(RecordingHost::with_all_capabilities());
    let payload = request(&mut session, Method::ExecutionRun, run_params("git"));
    assert!(matches!(payload, ResponsePayload::Ok { .. }));
    assert_eq!(session.host.dispatched, [Method::ExecutionRun]);
}

#[test]
fn an_executable_outside_the_allowlist_is_denied() {
    let mut session = initialized_session(RecordingHost::with_all_capabilities());
    for executable in ["sh", "bash", "/usr/bin/git", "./git", "git "] {
        let payload = request(&mut session, Method::ExecutionRun, run_params(executable));
        assert_eq!(
            error_code(payload),
            ErrorCode::PermissionDenied,
            "`{executable}` must be denied"
        );
    }
    assert!(session.host.dispatched.is_empty());
}

#[test]
fn a_malformed_execution_request_is_rejected_before_dispatch() {
    let mut session = initialized_session(RecordingHost::with_all_capabilities());
    let payload = request(
        &mut session,
        Method::ExecutionRun,
        json!({ "executable": "git" }),
    );
    assert_eq!(error_code(payload), ErrorCode::InvalidRequest);
    assert!(session.host.dispatched.is_empty());
}

#[test]
fn a_plugin_may_only_drive_a_panel_it_declared() {
    let mut session = initialized_session(RecordingHost::with_all_capabilities());

    let own = request(
        &mut session,
        Method::PanelSetState,
        json!({ "panel_id": "git", "status": { "state": "loading" } }),
    );
    assert!(matches!(own, ResponsePayload::Ok { .. }));

    let other = request(
        &mut session,
        Method::PanelSetState,
        json!({ "panel_id": "someone-elses-panel", "status": { "state": "loading" } }),
    );
    assert_eq!(error_code(other), ErrorCode::PermissionDenied);
    assert_eq!(session.host.dispatched, [Method::PanelSetState]);
}

#[test]
fn declaring_contributions_grants_the_matching_ui_permissions() {
    let session = session(RecordingHost::with_all_capabilities());
    assert!(
        session
            .session
            .permissions()
            .contains(extension_protocol::Permission::UiPanel)
    );
    assert!(
        session
            .session
            .permissions()
            .contains(extension_protocol::Permission::UiCommands)
    );
}

#[test]
fn events_and_responses_from_a_plugin_are_dropped_rather_than_answered() {
    let mut session = initialized_session(RecordingHost::with_all_capabilities());

    let event = Message::Event(extension_protocol::EventEnvelope::new(
        extension_protocol::EventKind::PanelAction,
        json!({}),
    ));
    assert!(session.handle(event).is_none());

    let response = Message::Response(extension_protocol::ResponseEnvelope::ok("9", json!({})));
    assert!(session.handle(response).is_none());
    assert!(session.host.dispatched.is_empty());
}

#[test]
fn a_remote_target_is_carried_through_unchanged() {
    let mut session = initialized_session(RecordingHost::with_all_capabilities());
    let params = json!({
        "target": { "kind": "ssh", "session_id": "ssh-123" },
        "cwd": "/home/user/project",
        "executable": "git",
        "args": ["fetch", "--prune"]
    });
    let decoded: extension_protocol::ExecutionRunParams =
        serde_json::from_value(params.clone()).expect("params decode");
    assert_eq!(
        decoded.target,
        ExecutionTarget::Ssh {
            session_id: "ssh-123".to_owned()
        }
    );

    let payload = request(&mut session, Method::ExecutionRun, params);
    assert!(matches!(payload, ResponsePayload::Ok { .. }));
}
