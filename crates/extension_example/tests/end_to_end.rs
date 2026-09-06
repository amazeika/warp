//! Drives the real `warp-extension-example` binary through the real host.
//!
//! This is the test that proves the acceptance criteria for the extension API
//! as a whole: discovery without recompiling Warp, a handshake, gated calls,
//! panel state, an action round trip, and a plugin failing without taking the
//! host down.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use extension_host::{
    DiscoveredExtension, Dispatched, ExtensionHost, ExtensionProcess, ExtensionRecord,
    ProcessEvent, Session, discover_in,
};
use extension_protocol::{
    ActionOrigin, Capability, EventEnvelope, EventKind, ExecutionRunResult, ExecutionTarget,
    ExtensionError, ExtensionManifest, Message, Method, PanelActionParams, PanelStatus,
    PanelViewState, WorkspaceContext,
};
use instant::Instant;
use serde_json::json;
use tempfile::TempDir;

/// Generous enough for a cold start on a loaded CI machine, short enough that a
/// genuine hang fails the test rather than the job.
const PUMP_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

fn example_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_warp-extension-example"))
}

/// Installs the built plugin into a throwaway extensions root, exactly as a
/// user would: a directory, a manifest, and an executable.
fn install(manifest: &str) -> (TempDir, PathBuf) {
    let root = TempDir::new().expect("temp extensions root");
    let directory = root.path().join("warp-extension-example");
    std::fs::create_dir_all(&directory).expect("extension directory");
    std::fs::write(directory.join("extension.toml"), manifest).expect("manifest is written");

    let installed = directory.join("warp-extension-example");
    std::fs::copy(example_binary(), &installed).expect("plugin binary is installed");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755))
            .expect("plugin binary is executable");
    }
    (root, directory)
}

fn shipped_manifest() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("extension.toml");
    std::fs::read_to_string(path).expect("the shipped example manifest is readable")
}

fn valid(discovered: &DiscoveredExtension) -> (&ExtensionManifest, &Path) {
    match &discovered.record {
        ExtensionRecord::Valid {
            manifest,
            executable,
        } => (manifest, executable.as_path()),
        ExtensionRecord::Invalid { reason } => panic!("extension is invalid: {reason}"),
    }
}

#[derive(Debug, Clone)]
struct Call {
    method: Method,
    params: serde_json::Value,
}

/// Stands in for Warp: records every gated call and answers with canned data.
struct FakeHost {
    context: WorkspaceContext,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl ExtensionHost for FakeHost {
    fn capabilities(&self) -> Vec<Capability> {
        Capability::ALL.to_vec()
    }

    fn dispatch(
        &mut self,
        _request_id: &str,
        method: Method,
        params: serde_json::Value,
    ) -> Dispatched {
        Dispatched::Answered(self.answer(method, params))
    }
}

impl FakeHost {
    fn answer(
        &mut self,
        method: Method,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ExtensionError> {
        self.calls
            .lock()
            .expect("call log is not poisoned")
            .push(Call {
                method,
                params: params.clone(),
            });

        match method {
            Method::WorkspaceGetContext => {
                Ok(serde_json::to_value(&self.context).expect("context"))
            }
            Method::ExecutionRun => Ok(serde_json::to_value(ExecutionRunResult {
                exit_code: Some(0),
                stdout: "hello from warp-extension-example\n".to_owned(),
                stderr: String::new(),
                duration_ms: 1,
                truncated: false,
            })
            .expect("result")),
            Method::ExtensionInitialize
            | Method::FileOpen
            | Method::DiffOpenWorkingTree
            | Method::DiffOpenFile
            | Method::NotificationShow
            | Method::DialogConfirm
            | Method::DialogInput
            | Method::DialogSelect
            | Method::PanelSetState => Ok(json!({})),
        }
    }
}

fn context() -> WorkspaceContext {
    WorkspaceContext {
        workspace_id: "ws-1".to_owned(),
        cwd: "/home/user/project".to_owned(),
        repository_root: Some("/home/user/project".to_owned()),
        active_pane_id: Some("pane-1".to_owned()),
        session_id: Some("ssh-123".to_owned()),
        execution_target: ExecutionTarget::Ssh {
            session_id: "ssh-123".to_owned(),
        },
    }
}

struct Harness {
    process: ExtensionProcess,
    session: Session,
    host: FakeHost,
    calls: Arc<Mutex<Vec<Call>>>,
    closed: bool,
    _root: TempDir,
}

impl Harness {
    fn start(manifest: &str) -> Self {
        let (root, directory) = install(manifest);
        let discovered = discover_in(root.path());
        assert_eq!(discovered.len(), 1, "the installed extension is discovered");
        let (manifest, executable) = valid(&discovered[0]);

        let process = ExtensionProcess::spawn("dev.warp.example", executable, &directory, None)
            .expect("the plugin starts");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let session = Session::new(manifest.clone());
        let host = FakeHost {
            context: context(),
            calls: Arc::clone(&calls),
        };
        Self {
            process,
            session,
            host,
            calls,
            closed: false,
            _root: root,
        }
    }

    fn methods(&self) -> Vec<Method> {
        self.calls
            .lock()
            .expect("call log is not poisoned")
            .iter()
            .map(|call| call.method)
            .collect()
    }

    fn call(&self, method: Method) -> Option<Call> {
        self.calls
            .lock()
            .expect("call log is not poisoned")
            .iter()
            .find(|call| call.method == method)
            .cloned()
    }

    /// Services the plugin until `done` holds, the plugin exits, or the timeout
    /// elapses.
    fn pump_until(&mut self, description: &str, done: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + PUMP_TIMEOUT;
        while Instant::now() < deadline {
            if done(self) {
                return;
            }
            match self.process.recv_timeout(POLL_INTERVAL) {
                Ok(ProcessEvent::Message(message)) => {
                    if let Some(reply) = self.session.handle_message(*message, &mut self.host) {
                        self.process.send(&reply).expect("the reply is written");
                    }
                }
                Ok(ProcessEvent::Decode(error)) => panic!("the plugin sent a bad frame: {error}"),
                Ok(ProcessEvent::Stderr(_)) => continue,
                Ok(ProcessEvent::Closed) => {
                    self.closed = true;
                    if done(self) {
                        return;
                    }
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.closed = true;
                    break;
                }
            }
        }
        assert!(
            done(self),
            "timed out waiting for {description}; observed calls: {:?}",
            self.methods()
        );
    }

    fn send_event(&mut self, event: EventEnvelope) {
        self.process
            .send(&Message::Event(event))
            .expect("the event is written");
    }
}

#[test]
fn the_example_plugin_completes_the_documented_startup_sequence() {
    let mut harness = Harness::start(&shipped_manifest());
    harness.pump_until("the plugin to publish panel state", |harness| {
        harness.call(Method::PanelSetState).is_some()
    });

    assert!(
        harness.session.is_initialized(),
        "the plugin must complete the handshake first"
    );
    assert_eq!(
        harness.methods(),
        [
            Method::WorkspaceGetContext,
            Method::ExecutionRun,
            Method::FileOpen,
            Method::DiffOpenWorkingTree,
            Method::NotificationShow,
            Method::PanelSetState,
        ],
        "the plugin exercises every v0.1 surface in order"
    );

    let run = harness
        .call(Method::ExecutionRun)
        .expect("execution recorded");
    assert_eq!(run.params["executable"], json!("echo"));
    assert_eq!(
        run.params["target"],
        json!({ "kind": "ssh", "session_id": "ssh-123" }),
        "the plugin runs against the target Warp reported, not a local guess"
    );

    let panel = harness.call(Method::PanelSetState).expect("panel recorded");
    let state: PanelViewState = serde_json::from_value(panel.params).expect("panel state decodes");
    assert_eq!(state.panel_id, "example");
    assert_eq!(state.status, PanelStatus::Ready);
    assert_eq!(state.sections[0].items.len(), 3);
    assert_eq!(
        state.sections[0].items[1].label, "ssh session ssh-123",
        "the panel reflects the execution target it was given"
    );

    let status = harness.process.stop(Duration::from_secs(5));
    assert!(
        status.is_some_and(|status| status.success()),
        "the plugin exits cleanly when asked to shut down"
    );
}

#[test]
fn a_panel_action_round_trips_back_into_a_diff_request() {
    let mut harness = Harness::start(&shipped_manifest());
    harness.pump_until("the plugin to publish panel state", |harness| {
        harness.call(Method::PanelSetState).is_some()
    });

    let action = EventEnvelope::with_params(
        EventKind::PanelAction,
        &PanelActionParams {
            panel_id: "example".to_owned(),
            section_id: "example.context".to_owned(),
            item_id: "README.md".to_owned(),
            action_id: "open_diff".to_owned(),
            origin: ActionOrigin {
                workspace_id: "ws-1".to_owned(),
                session_id: Some("ssh-123".to_owned()),
                repository_root: Some("/home/user/project".to_owned()),
                execution_target: ExecutionTarget::Ssh {
                    session_id: "ssh-123".to_owned(),
                },
            },
        },
    )
    .expect("event encodes");
    harness.send_event(action);

    harness.pump_until("the plugin to open a diff", |harness| {
        harness.call(Method::DiffOpenFile).is_some()
    });

    let diff = harness.call(Method::DiffOpenFile).expect("diff recorded");
    assert_eq!(diff.params["path"], json!("README.md"));
    assert_eq!(diff.params["repository_root"], json!("/home/user/project"));
    assert_eq!(
        diff.params["target"],
        json!({ "kind": "ssh", "session_id": "ssh-123" }),
        "the diff opens against the target the action originated in"
    );
}

#[test]
fn an_undeclared_permission_is_denied_and_the_host_survives_the_plugin_exiting() {
    let manifest = shipped_manifest().replace("ui_notifications = true\n", "");
    let mut harness = Harness::start(&manifest);

    harness.pump_until("the plugin to be denied and exit", |harness| harness.closed);

    assert_eq!(
        harness.methods(),
        [
            Method::WorkspaceGetContext,
            Method::ExecutionRun,
            Method::FileOpen,
            Method::DiffOpenWorkingTree,
        ],
        "the denied call never reaches the host at all"
    );
    assert!(
        harness.call(Method::NotificationShow).is_none(),
        "the gate runs before dispatch, not inside it"
    );
    assert!(
        harness.call(Method::PanelSetState).is_none(),
        "nothing after the denial runs"
    );

    let status = harness.process.stop(Duration::from_secs(5));
    assert!(
        status.is_some_and(|status| !status.success()),
        "the plugin exits with a failure status"
    );
    assert!(
        harness.session.is_initialized(),
        "the host session is still intact after the plugin failed"
    );
}

#[test]
fn the_shipped_manifest_is_valid_and_declares_only_what_the_plugin_uses() {
    let manifest =
        ExtensionManifest::parse(&shipped_manifest()).expect("the shipped manifest parses");
    manifest.validate().expect("the shipped manifest is valid");

    assert_eq!(manifest.id, "dev.warp.example");
    assert_eq!(manifest.execution.allowed_executables, ["echo"]);
    assert!(
        !manifest.allows_executable("sh"),
        "the example must not be able to reach a shell"
    );

    let declared: BTreeMap<&str, bool> = manifest
        .effective_permissions()
        .iter()
        .map(|permission| (permission.as_str(), true))
        .collect();
    assert!(declared.contains_key("ui.panel"));
    assert!(declared.contains_key("ui.commands"));
    assert!(
        !declared.contains_key("workspace.write"),
        "the example never writes, so it must not ask to"
    );
}

#[test]
fn a_handshake_that_claims_a_different_extension_is_refused() {
    let manifest =
        shipped_manifest().replace("id = \"dev.warp.example\"", "id = \"dev.warp.impostor\"");
    let mut harness = Harness::start(&manifest);

    harness.pump_until("the plugin to be refused and exit", |harness| {
        harness.closed
    });

    assert!(
        !harness.session.is_initialized(),
        "a mismatched handshake must not initialize the session"
    );
    assert!(
        harness.methods().is_empty(),
        "no method runs before a successful handshake"
    );
    let status = harness.process.stop(Duration::from_secs(5));
    assert!(status.is_some_and(|status| !status.success()));
}
