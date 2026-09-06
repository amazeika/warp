//! Reference Warp extension: the smallest plugin that exercises every v0.1
//! capability without any domain logic.
//!
//! It exists so the extension API can be proven end to end before a real plugin
//! is written, and so a regression in the host shows up as a failure here
//! rather than inside something as intricate as a Git client.
//!
//! stdout is the transport. All diagnostics go to stderr.
use extension_protocol::{
    DiffComparison, EventKind, ExecutionTarget, NotificationLevel, PanelActionParams, PanelItem,
    PanelItemAction, PanelSection, PanelStatus, PanelViewState, WorkspaceContext,
};
use extension_sdk::{Client, ClientError};

const EXTENSION_ID: &str = "dev.warp.example";
const PANEL_ID: &str = "example";

fn main() {
    if let Err(error) = run() {
        eprintln!("warp-extension-example: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), ClientError> {
    let mut client = Client::from_stdio();

    let handshake = client.initialize(EXTENSION_ID)?;
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

    let context = client.workspace_context()?;
    eprintln!(
        "workspace {} at {} target {}",
        context.workspace_id,
        context.cwd,
        describe(&context.execution_target)
    );

    let echoed = client.run(
        &context.execution_target,
        &context.cwd,
        "echo",
        &["hello from warp-extension-example".to_owned()],
    )?;
    eprintln!("echo exited with {:?}", echoed.exit_code);

    let sample_root = context
        .repository_root
        .clone()
        .unwrap_or_else(|| context.cwd.clone());
    client.open_file(
        &context.execution_target,
        &format!("{sample_root}/README.md"),
        Some(1),
    )?;

    if let Some(repository_root) = context.repository_root.clone() {
        client.open_working_tree_diff(&context.execution_target, &repository_root)?;
    }

    client.notify(
        "Warp Extension Example is running",
        Some(describe(&context.execution_target)),
        NotificationLevel::Info,
    )?;
    client.set_panel_state(&panel_state(&context))?;

    serve_events(&mut client, &context)
}

/// Handles host events until Warp asks the plugin to stop or closes the stream.
fn serve_events(client: &mut Client, context: &WorkspaceContext) -> Result<(), ClientError> {
    loop {
        let Some(event) = client.next_event()? else {
            return Ok(());
        };
        match event.event {
            EventKind::ExtensionShutdown => return Ok(()),
            EventKind::PanelAction => {
                let params: PanelActionParams = serde_json::from_value(event.params)
                    .map_err(|err| ClientError::Transport(err.to_string()))?;
                // The action carries its own origin, so a focus change while
                // the plugin was busy cannot retarget the diff.
                let Some(repository_root) = params.origin.repository_root.clone() else {
                    continue;
                };
                client.open_file_diff(
                    &params.origin.execution_target,
                    &repository_root,
                    &params.item_id,
                    DiffComparison::HeadToWorktree,
                )?;
            }
            EventKind::CommandInvoked => {
                client.notify(
                    "Hello from the example extension",
                    None,
                    NotificationLevel::Info,
                )?;
            }
            EventKind::WorkspaceChanged
            | EventKind::CwdChanged
            | EventKind::RepositoryChanged
            | EventKind::SessionChanged
            | EventKind::SshConnected
            | EventKind::SshDisconnected => {
                client.set_panel_state(&panel_state(context))?;
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
