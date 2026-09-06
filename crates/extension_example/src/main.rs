//! Reference Warp extension: the smallest plugin that exercises every v0.1
//! capability without any domain logic.
//!
//! It exists so the extension API can be proven end to end before a real plugin
//! is written, and so a regression in the host shows up as a failure here
//! rather than inside something as intricate as a Git client.
//!
//! It speaks the protocol directly over stdio using only `extension_protocol`,
//! which is the point: the wire format is the compatibility layer, and an SDK
//! is a convenience rather than a requirement.
//!
//! stdout is the transport. All diagnostics go to stderr.
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Stdin, StdoutLock, Write};

use extension_protocol::{
    API_VERSION, DiffComparison, DiffOpenFileParams, DiffOpenWorkingTreeParams, EventEnvelope,
    EventKind, ExecutionRunParams, ExecutionRunResult, ExecutionTarget, ExtensionError,
    FileOpenParams, InitializeParams, InitializeResult, Message, Method, NotificationLevel,
    NotificationParams, PanelActionParams, PanelItem, PanelItemAction, PanelSection, PanelStatus,
    PanelViewState, RequestEnvelope, WorkspaceContext,
};

const EXTENSION_ID: &str = "dev.warp.example";
const PANEL_ID: &str = "example";

fn main() {
    if let Err(error) = run() {
        eprintln!("warp-extension-example: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut client = Client::new();

    let handshake: InitializeResult = client.call(
        Method::ExtensionInitialize,
        &InitializeParams {
            extension_id: EXTENSION_ID.to_owned(),
            api_version: API_VERSION,
        },
    )?;
    eprintln!(
        "handshake: protocol {} api {} capabilities [{}]",
        handshake.protocol,
        handshake.api_version,
        handshake
            .capabilities
            .iter()
            .map(|capability| capability.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let context: WorkspaceContext =
        client.call(Method::WorkspaceGetContext, &serde_json::json!({}))?;
    eprintln!(
        "workspace {} at {} target {}",
        context.workspace_id,
        context.cwd,
        describe(&context.execution_target)
    );

    let echoed: ExecutionRunResult = client.call(
        Method::ExecutionRun,
        &ExecutionRunParams {
            target: context.execution_target.clone(),
            cwd: context.cwd.clone(),
            executable: "echo".to_owned(),
            args: vec!["hello from warp-extension-example".to_owned()],
            env: Default::default(),
            timeout_ms: None,
        },
    )?;
    eprintln!("echo exited with {:?}", echoed.exit_code);

    let sample_path = context
        .repository_root
        .clone()
        .unwrap_or_else(|| context.cwd.clone());

    let _: serde_json::Value = client.call(
        Method::FileOpen,
        &FileOpenParams {
            target: context.execution_target.clone(),
            path: format!("{sample_path}/README.md"),
            line: Some(1),
            column: None,
            preferred_view: None,
        },
    )?;

    if let Some(repository_root) = context.repository_root.clone() {
        let _: serde_json::Value = client.call(
            Method::DiffOpenWorkingTree,
            &DiffOpenWorkingTreeParams {
                target: context.execution_target.clone(),
                repository_root,
            },
        )?;
    }

    let _: serde_json::Value = client.call(
        Method::NotificationShow,
        &NotificationParams {
            title: "Warp Extension Example is running".to_owned(),
            body: Some(describe(&context.execution_target)),
            level: NotificationLevel::Info,
        },
    )?;

    let _: serde_json::Value = client.call(Method::PanelSetState, &panel_state(&context))?;

    serve_events(&mut client, &context)
}

/// Handles host events until Warp asks the plugin to stop or closes the stream.
fn serve_events(client: &mut Client, context: &WorkspaceContext) -> Result<(), String> {
    loop {
        let Some(event) = client.next_event()? else {
            return Ok(());
        };
        match event.event {
            EventKind::ExtensionShutdown => return Ok(()),
            EventKind::PanelAction => {
                let params: PanelActionParams = serde_json::from_value(event.params)
                    .map_err(|err| format!("malformed panel.action: {err}"))?;
                // The action carries its own origin, so a focus change while
                // the plugin was busy cannot retarget the diff.
                let Some(repository_root) = params.origin.repository_root.clone() else {
                    continue;
                };
                let _: serde_json::Value = client.call(
                    Method::DiffOpenFile,
                    &DiffOpenFileParams {
                        target: params.origin.execution_target.clone(),
                        repository_root,
                        path: params.item_id,
                        comparison: DiffComparison::HeadToWorktree,
                    },
                )?;
            }
            EventKind::CommandInvoked => {
                let _: serde_json::Value = client.call(
                    Method::NotificationShow,
                    &NotificationParams {
                        title: "Hello from the example extension".to_owned(),
                        body: None,
                        level: NotificationLevel::Info,
                    },
                )?;
            }
            EventKind::WorkspaceChanged
            | EventKind::CwdChanged
            | EventKind::RepositoryChanged
            | EventKind::SessionChanged
            | EventKind::SshConnected
            | EventKind::SshDisconnected => {
                let _: serde_json::Value =
                    client.call(Method::PanelSetState, &panel_state(context))?;
            }
        }
    }
}

fn panel_state(context: &WorkspaceContext) -> PanelViewState {
    let repository = context
        .repository_root
        .clone()
        .unwrap_or_else(|| "no repository".to_owned());
    PanelViewState {
        panel_id: PANEL_ID.to_owned(),
        status: PanelStatus::Ready,
        sections: vec![PanelSection {
            id: "example.context".to_owned(),
            title: "Workspace".to_owned(),
            badge: None,
            items: vec![
                PanelItem {
                    id: "cwd".to_owned(),
                    label: context.cwd.clone(),
                    description: Some("Working directory".to_owned()),
                    icon: None,
                    badge: None,
                    actions: Vec::new(),
                    children: Vec::new(),
                    expanded: false,
                },
                PanelItem {
                    id: "target".to_owned(),
                    label: describe(&context.execution_target),
                    description: Some("Execution target".to_owned()),
                    icon: None,
                    badge: None,
                    actions: Vec::new(),
                    children: Vec::new(),
                    expanded: false,
                },
                PanelItem {
                    id: "README.md".to_owned(),
                    label: repository,
                    description: Some("Repository".to_owned()),
                    icon: None,
                    badge: None,
                    actions: vec![PanelItemAction {
                        id: "open_diff".to_owned(),
                        title: "Open diff".to_owned(),
                        icon: None,
                        destructive: false,
                    }],
                    children: Vec::new(),
                    expanded: false,
                },
            ],
        }],
    }
}

fn describe(target: &ExecutionTarget) -> String {
    match target {
        ExecutionTarget::Local => "local".to_owned(),
        ExecutionTarget::Ssh { session_id } => format!("ssh session {session_id}"),
    }
}

/// Minimal protocol client: one outstanding request at a time, with events
/// buffered so a reply is never lost behind them.
struct Client {
    reader: BufReader<Stdin>,
    writer: StdoutLock<'static>,
    pending_events: VecDeque<EventEnvelope>,
    next_request_id: u64,
    closed: bool,
}

impl Client {
    fn new() -> Self {
        Self {
            reader: BufReader::new(std::io::stdin()),
            writer: std::io::stdout().lock(),
            pending_events: VecDeque::new(),
            next_request_id: 1,
            closed: false,
        }
    }

    fn call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &mut self,
        method: Method,
        params: &P,
    ) -> Result<R, String> {
        let request_id = self.next_request_id.to_string();
        self.next_request_id += 1;

        let request = RequestEnvelope::with_params(request_id.clone(), method, params)
            .map_err(|err: ExtensionError| err.to_string())?;
        self.send(&Message::Request(request))?;

        loop {
            let message = self
                .receive()?
                .ok_or_else(|| format!("stream closed while waiting for {method}"))?;
            match message {
                Message::Response(response) if response.request_id == request_id => {
                    return response.result_as().map_err(|err| err.to_string());
                }
                Message::Event(event) => self.pending_events.push_back(event),
                Message::Response(_) | Message::Request(_) => {}
            }
        }
    }

    /// Returns the next event, or `None` once Warp closes the stream.
    fn next_event(&mut self) -> Result<Option<EventEnvelope>, String> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(Some(event));
        }
        loop {
            match self.receive()? {
                Some(Message::Event(event)) => return Ok(Some(event)),
                Some(Message::Request(_) | Message::Response(_)) => {}
                None => return Ok(None),
            }
        }
    }

    fn send(&mut self, message: &Message) -> Result<(), String> {
        let encoded = serde_json::to_string(message).map_err(|err| err.to_string())?;
        writeln!(self.writer, "{encoded}").map_err(|err| err.to_string())?;
        self.writer.flush().map_err(|err| err.to_string())
    }

    fn receive(&mut self) -> Result<Option<Message>, String> {
        if self.closed {
            return Ok(None);
        }
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|err| err.to_string())?;
            if read == 0 {
                self.closed = true;
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            let message: Message =
                serde_json::from_str(line.trim()).map_err(|err| err.to_string())?;
            return Ok(Some(message));
        }
    }
}
