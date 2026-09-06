//! A protocol client: one outstanding request at a time, events buffered.
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Stdin, Stdout, Write};

use extension_protocol::{
    API_VERSION, Capability, DialogConfirmParams, DialogConfirmResult, DialogInputParams,
    DialogInputResult, DialogSelectOption, DialogSelectParams, DialogSelectResult, DiffComparison,
    DiffOpenFileParams, DiffOpenWorkingTreeParams, EventEnvelope, ExecutionRunParams,
    ExecutionRunResult, ExecutionTarget, ExtensionError, FileOpenParams, InitializeParams,
    InitializeResult, Message, Method, NotificationLevel, NotificationParams, PanelViewState,
    RequestEnvelope, WorkspaceContext,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Warp answered, and the answer was a refusal. A plugin is expected to
    /// handle these — a denied permission is a normal outcome, not a crash.
    #[error(transparent)]
    Extension(#[from] ExtensionError),
    /// Warp closed the stream. Nothing further will arrive.
    #[error("Warp closed the extension stream")]
    Closed,
    #[error("extension transport failed: {0}")]
    Transport(String),
}

/// Talks to Warp over a framed JSON stream.
///
/// Generic over its streams so a plugin's behavior can be tested against
/// in-memory buffers instead of a live host.
pub struct Client<R = BufReader<Stdin>, W = Stdout> {
    reader: R,
    writer: W,
    pending_events: VecDeque<EventEnvelope>,
    next_request_id: u64,
    closed: bool,
    capabilities: Vec<Capability>,
}

impl Client<BufReader<Stdin>, Stdout> {
    /// Connects over the process's own stdio, which is where Warp wires the
    /// transport. Plugins must therefore log to stderr; anything written to
    /// stdout is read as a protocol frame.
    pub fn from_stdio() -> Self {
        Self::new(BufReader::new(std::io::stdin()), std::io::stdout())
    }
}

impl<R: BufRead, W: Write> Client<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            pending_events: VecDeque::new(),
            next_request_id: 1,
            closed: false,
            capabilities: Vec::new(),
        }
    }

    /// Performs the handshake and records what this Warp build supports.
    pub fn initialize(&mut self, extension_id: &str) -> Result<InitializeResult, ClientError> {
        let result: InitializeResult = self.call(
            Method::ExtensionInitialize,
            &InitializeParams {
                extension_id: extension_id.to_owned(),
                api_version: API_VERSION,
            },
        )?;
        self.capabilities = result.capabilities.clone();
        Ok(result)
    }

    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    /// True when Warp advertised this capability during the handshake.
    ///
    /// Check before calling into an optional surface rather than treating an
    /// `unsupported_capability` error as a failure.
    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn call<P: Serialize, T: DeserializeOwned>(
        &mut self,
        method: Method,
        params: &P,
    ) -> Result<T, ClientError> {
        let request_id = self.next_request_id.to_string();
        self.next_request_id += 1;

        let request = RequestEnvelope::with_params(request_id.clone(), method, params)?;
        self.send(&Message::Request(request))?;

        loop {
            let Some(message) = self.receive()? else {
                return Err(ClientError::Closed);
            };
            match message {
                Message::Response(response) if response.request_id == request_id => {
                    return Ok(response.result_as()?);
                }
                Message::Event(event) => self.pending_events.push_back(event),
                Message::Request(_) | Message::Response(_) => {}
            }
        }
    }

    /// Blocks for the next event, returning `None` once Warp closes the stream.
    pub fn next_event(&mut self) -> Result<Option<EventEnvelope>, ClientError> {
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

    pub fn workspace_context(&mut self) -> Result<WorkspaceContext, ClientError> {
        self.call(Method::WorkspaceGetContext, &serde_json::json!({}))
    }

    pub fn run(
        &mut self,
        target: &ExecutionTarget,
        cwd: &str,
        executable: &str,
        args: &[String],
    ) -> Result<ExecutionRunResult, ClientError> {
        self.call(
            Method::ExecutionRun,
            &ExecutionRunParams {
                target: target.clone(),
                cwd: cwd.to_owned(),
                executable: executable.to_owned(),
                args: args.to_vec(),
                env: Default::default(),
                timeout_ms: None,
            },
        )
    }

    pub fn open_file(
        &mut self,
        target: &ExecutionTarget,
        path: &str,
        line: Option<u32>,
    ) -> Result<(), ClientError> {
        self.call_ignoring_result(
            Method::FileOpen,
            &FileOpenParams {
                target: target.clone(),
                path: path.to_owned(),
                line,
                column: None,
                preferred_view: None,
            },
        )
    }

    pub fn open_working_tree_diff(
        &mut self,
        target: &ExecutionTarget,
        repository_root: &str,
    ) -> Result<(), ClientError> {
        self.call_ignoring_result(
            Method::DiffOpenWorkingTree,
            &DiffOpenWorkingTreeParams {
                target: target.clone(),
                repository_root: repository_root.to_owned(),
            },
        )
    }

    pub fn open_file_diff(
        &mut self,
        target: &ExecutionTarget,
        repository_root: &str,
        path: &str,
        comparison: DiffComparison,
    ) -> Result<(), ClientError> {
        self.call_ignoring_result(
            Method::DiffOpenFile,
            &DiffOpenFileParams {
                target: target.clone(),
                repository_root: repository_root.to_owned(),
                path: path.to_owned(),
                comparison,
            },
        )
    }

    pub fn notify(
        &mut self,
        title: &str,
        body: Option<String>,
        level: NotificationLevel,
    ) -> Result<(), ClientError> {
        self.call_ignoring_result(
            Method::NotificationShow,
            &NotificationParams {
                title: title.to_owned(),
                body,
                level,
            },
        )
    }

    /// Asks Warp to render a confirmation. Warp owns the destructive treatment,
    /// so a plugin cannot make an irreversible action look routine.
    pub fn confirm(
        &mut self,
        title: &str,
        body: Option<String>,
        confirm_label: &str,
        destructive: bool,
    ) -> Result<bool, ClientError> {
        let result: DialogConfirmResult = self.call(
            Method::DialogConfirm,
            &DialogConfirmParams {
                title: title.to_owned(),
                body,
                confirm_label: confirm_label.to_owned(),
                cancel_label: "Cancel".to_owned(),
                destructive,
            },
        )?;
        Ok(result.confirmed)
    }

    /// `None` means the user dismissed the dialog.
    pub fn input(
        &mut self,
        title: &str,
        body: Option<String>,
        placeholder: Option<String>,
    ) -> Result<Option<String>, ClientError> {
        let result: DialogInputResult = self.call(
            Method::DialogInput,
            &DialogInputParams {
                title: title.to_owned(),
                body,
                placeholder,
                initial_value: None,
            },
        )?;
        Ok(result.value)
    }

    /// `None` means the user dismissed the dialog.
    pub fn select(
        &mut self,
        title: &str,
        body: Option<String>,
        options: Vec<DialogSelectOption>,
    ) -> Result<Option<String>, ClientError> {
        let result: DialogSelectResult = self.call(
            Method::DialogSelect,
            &DialogSelectParams {
                title: title.to_owned(),
                body,
                options,
            },
        )?;
        Ok(result.selected_id)
    }

    pub fn set_panel_state(&mut self, state: &PanelViewState) -> Result<(), ClientError> {
        self.call_ignoring_result(Method::PanelSetState, state)
    }

    fn call_ignoring_result<P: Serialize>(
        &mut self,
        method: Method,
        params: &P,
    ) -> Result<(), ClientError> {
        let _: serde_json::Value = self.call(method, params)?;
        Ok(())
    }

    fn send(&mut self, message: &Message) -> Result<(), ClientError> {
        let encoded = serde_json::to_string(message)
            .map_err(|err| ClientError::Transport(err.to_string()))?;
        writeln!(self.writer, "{encoded}")
            .map_err(|err| ClientError::Transport(err.to_string()))?;
        self.writer
            .flush()
            .map_err(|err| ClientError::Transport(err.to_string()))
    }

    fn receive(&mut self) -> Result<Option<Message>, ClientError> {
        if self.closed {
            return Ok(None);
        }
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|err| ClientError::Transport(err.to_string()))?;
            if read == 0 {
                self.closed = true;
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            let message: Message = serde_json::from_str(line.trim())
                .map_err(|err| ClientError::Transport(err.to_string()))?;
            return Ok(Some(message));
        }
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
