//! Handshake, gating and dispatch for one connected extension.
use extension_protocol::{
    API_VERSION, Capability, ErrorCode, ExecutionRunParams, ExtensionError, ExtensionManifest,
    InitializeParams, InitializeResult, Message, Method, PROTOCOL_VERSION, PanelViewState,
    PermissionSet, RequestEnvelope, ResponseEnvelope, decode_params,
};

/// What a host did with one request.
///
/// Some answers cannot be produced inside the call that asks for them: a dialog
/// is answered when the user clicks, which is many frames after the dispatch
/// returns. [`Dispatched::Deferred`] is the host saying it has taken the
/// request and will write the response for that `request_id` itself.
pub enum Dispatched {
    /// The answer is ready now, and the session writes it back.
    Answered(Result<serde_json::Value, ExtensionError>),
    /// Nothing is written now. The host owes the plugin exactly one response
    /// carrying the `request_id` it was given.
    Deferred,
}

impl From<Result<serde_json::Value, ExtensionError>> for Dispatched {
    fn from(result: Result<serde_json::Value, ExtensionError>) -> Self {
        Dispatched::Answered(result)
    }
}

/// The Warp side of the extension API.
///
/// The app implements this; everything above it — permissions, capabilities,
/// the handshake — is enforced before a call reaches an implementation, so an
/// implementor never has to re-check whether a plugin was allowed to ask.
pub trait ExtensionHost {
    /// Capabilities this build actually implements, advertised at handshake.
    fn capabilities(&self) -> Vec<Capability>;

    /// Carries out one request. `request_id` is only needed by a host that
    /// answers later; a host that answers every method inline can ignore it.
    fn dispatch(
        &mut self,
        request_id: &str,
        method: Method,
        params: serde_json::Value,
    ) -> Dispatched;
}

/// One extension's connection state and the gates its requests pass through.
///
/// The host is passed to each call rather than owned, because the app-side
/// implementation borrows live application state: it exists only for the
/// duration of one dispatch and cannot be stored alongside the session.
pub struct Session {
    manifest: ExtensionManifest,
    permissions: PermissionSet,
    initialized: bool,
}

impl Session {
    pub fn new(manifest: ExtensionManifest) -> Self {
        let permissions = manifest.effective_permissions();
        Self {
            manifest,
            permissions,
            initialized: false,
        }
    }

    pub fn manifest(&self) -> &ExtensionManifest {
        &self.manifest
    }

    pub fn permissions(&self) -> &PermissionSet {
        &self.permissions
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Handles one inbound message, returning the reply to write back.
    ///
    /// Messages a plugin should not be sending — responses and events, neither
    /// of which the host solicits in v0.1 — are dropped rather than answered,
    /// because there is nothing to answer.
    /// A request the host deferred yields no reply here: the host answers it
    /// once the user does, which is the only way a modal question can be
    /// carried over a protocol whose calls return immediately.
    pub fn handle_message<H: ExtensionHost>(
        &mut self,
        message: Message,
        host: &mut H,
    ) -> Option<Message> {
        match message {
            Message::Request(request) => {
                Some(Message::Response(self.handle_request(request, host)?))
            }
            Message::Event(_) | Message::Response(_) => None,
        }
    }

    fn handle_request<H: ExtensionHost>(
        &mut self,
        request: RequestEnvelope,
        host: &mut H,
    ) -> Option<ResponseEnvelope> {
        let request_id = request.request_id.clone();
        match self.dispatch_request(request, host) {
            Dispatched::Answered(Ok(result)) => Some(ResponseEnvelope::ok(request_id, result)),
            Dispatched::Answered(Err(error)) => Some(ResponseEnvelope::error(request_id, error)),
            Dispatched::Deferred => None,
        }
    }

    fn dispatch_request<H: ExtensionHost>(
        &mut self,
        request: RequestEnvelope,
        host: &mut H,
    ) -> Dispatched {
        if request.protocol != PROTOCOL_VERSION {
            return Dispatched::Answered(Err(ExtensionError::new(
                ErrorCode::ProtocolMismatch,
                format!(
                    "request declares protocol version {}, expected {PROTOCOL_VERSION}",
                    request.protocol
                ),
            )));
        }

        if request.method == Method::ExtensionInitialize {
            return self.initialize(&request.params, host).into();
        }
        if !self.initialized {
            return Dispatched::Answered(Err(ExtensionError::new(
                ErrorCode::InvalidRequest,
                format!(
                    "{} was called before extension.initialize",
                    request.method.as_str()
                ),
            )));
        }

        if let Err(error) = self.gate(request.method, &request.params, host) {
            return Dispatched::Answered(Err(error));
        }

        host.dispatch(&request.request_id, request.method, request.params)
    }

    /// Every check a request passes before the host sees it.
    fn gate<H: ExtensionHost>(
        &self,
        method: Method,
        params: &serde_json::Value,
        host: &H,
    ) -> Result<(), ExtensionError> {
        self.ensure_capability(method, host)?;
        self.ensure_permission(method)?;
        self.ensure_request_policy(method, params)
    }

    fn initialize<H: ExtensionHost>(
        &mut self,
        params: &serde_json::Value,
        host: &H,
    ) -> Result<serde_json::Value, ExtensionError> {
        let params: InitializeParams = decode_params(Method::ExtensionInitialize, params)?;
        if params.extension_id != self.manifest.id {
            return Err(ExtensionError::with_details(
                ErrorCode::InvalidRequest,
                "handshake identifies a different extension",
                format!(
                    "manifest declares `{}`, handshake claims `{}`",
                    self.manifest.id, params.extension_id
                ),
            ));
        }
        if params.api_version != API_VERSION {
            return Err(ExtensionError::new(
                ErrorCode::ProtocolMismatch,
                format!(
                    "extension requests api_version {}, this build supports {API_VERSION}",
                    params.api_version
                ),
            ));
        }

        self.initialized = true;
        let result = InitializeResult {
            protocol: PROTOCOL_VERSION,
            api_version: API_VERSION,
            capabilities: host.capabilities(),
        };
        serde_json::to_value(result).map_err(|err| {
            ExtensionError::with_details(
                ErrorCode::InvalidRequest,
                "failed to encode handshake result",
                err.to_string(),
            )
        })
    }

    fn ensure_capability<H: ExtensionHost>(
        &self,
        method: Method,
        host: &H,
    ) -> Result<(), ExtensionError> {
        let Some(required) = method.capability() else {
            return Ok(());
        };
        if host.capabilities().contains(&required) {
            return Ok(());
        }
        Err(ExtensionError::new(
            ErrorCode::UnsupportedCapability,
            format!(
                "{} requires capability {required}, which this Warp build does not advertise",
                method.as_str()
            ),
        ))
    }

    fn ensure_permission(&self, method: Method) -> Result<(), ExtensionError> {
        let Some(required) = method.permission() else {
            return Ok(());
        };
        if self.permissions.contains(required) {
            return Ok(());
        }
        Err(ExtensionError::new(
            ErrorCode::PermissionDenied,
            format!(
                "{} requires the {required} permission, which this extension did not declare",
                method.as_str()
            ),
        ))
    }

    /// Per-method policy that depends on the request body rather than only the
    /// method: the executable allowlist, and the rule that a plugin may only
    /// drive a panel it declared.
    fn ensure_request_policy(
        &self,
        method: Method,
        params: &serde_json::Value,
    ) -> Result<(), ExtensionError> {
        match method {
            Method::ExecutionRun => {
                let params: ExecutionRunParams = decode_params(method, params)?;
                if !self.manifest.allows_executable(&params.executable) {
                    return Err(ExtensionError::with_details(
                        ErrorCode::PermissionDenied,
                        format!("`{}` is not an allowed executable", params.executable),
                        format!(
                            "allowed: {}",
                            self.manifest.execution.allowed_executables.join(", ")
                        ),
                    ));
                }
                Ok(())
            }
            Method::PanelSetState => {
                let params: PanelViewState = decode_params(method, params)?;
                if !self.manifest.declares_panel(&params.panel_id) {
                    return Err(ExtensionError::new(
                        ErrorCode::PermissionDenied,
                        format!(
                            "panel `{}` was not declared by this extension",
                            params.panel_id
                        ),
                    ));
                }
                Ok(())
            }
            Method::ExtensionInitialize
            | Method::WorkspaceGetContext
            | Method::FileOpen
            | Method::DiffOpenWorkingTree
            | Method::DiffOpenFile
            | Method::NotificationShow
            | Method::DialogConfirm
            | Method::DialogInput
            | Method::DialogSelect => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
