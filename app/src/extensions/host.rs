//! The [`ExtensionHost`] implementation: protocol methods turned into app calls.
//!
//! Constructed for the duration of one dispatch and thrown away, because it
//! borrows the live model context. Nothing here re-checks whether the extension
//! was allowed to ask — `Session` has already applied the protocol, capability
//! and permission gates by the time a call arrives.
use extension_host::{Dispatched, ExtensionHost};
use extension_protocol::{
    Capability, DialogConfirmParams, DiffOpenFileParams, DiffOpenWorkingTreeParams, ErrorCode,
    ExecutionRunParams, ExtensionError, FileOpenParams, Method, NotificationLevel,
    NotificationParams, PanelViewState, decode_params,
};
use warpui::windowing::WindowManager;
use warpui::{ModelContext, SingletonEntity as _};

use super::context::{ActiveContext, WorkspaceContextService};
use super::manager::ExtensionManager;
use crate::view_components::{DismissibleToast, ToastFlavor};
use crate::workspace::{ToastStack, WorkspaceAction};

/// Capabilities this build implements.
///
/// Advertising a capability Warp cannot honour would be worse than omitting it:
/// a plugin that sees the token stops offering the user a fallback. The list
/// grows as each surface is wired up.
/// `commands.v1` is here even though no method requires it: it is what tells a
/// plugin that a declared command will actually reach it as `command.invoked`,
/// which is the only way a plugin can know whether to offer that entry point.
pub(super) const IMPLEMENTED_CAPABILITIES: &[Capability] = &[
    Capability::CommandsV1,
    Capability::PanelTreeV1,
    Capability::NotificationV1,
    Capability::DialogConfirmV1,
    Capability::WorkspaceContextV1,
    Capability::ExecutionV1,
    Capability::DiffOpenV1,
    Capability::FileOpenV1,
];

pub(super) struct BridgeHost<'a, 'ctx> {
    pub extension_id: String,
    /// Shown as the notification's source, so a message a user did not expect
    /// names the extension that produced it rather than appearing to be Warp.
    pub extension_name: String,
    pub ctx: &'a mut ModelContext<'ctx, ExtensionManager>,
    /// A question raised by this dispatch, for the manager to queue once its
    /// own borrow of the extension has ended.
    ///
    /// It cannot be queued from here: the manager is already borrowed by the
    /// call that is being dispatched, which is the same reason this host exists
    /// only for the length of one call.
    pub question: Option<PendingConfirm>,
    /// A panel snapshot published by this dispatch, stored by the manager once
    /// its own borrow of the extension has ended, for the same reason.
    pub panel_state: Option<PanelViewState>,
    /// A command this dispatch accepted, for the manager to start once its own
    /// borrow of the extension has ended, for the same reason again.
    pub execution: Option<PendingExecution>,
    /// Something this dispatch is putting in front of the user, for the manager
    /// to open once its own borrow of the extension has ended. Opening a pane
    /// reaches through the workspace and back into extension state, so it is
    /// the one thing that must not happen while an extension is borrowed.
    pub open: Option<PendingOpen>,
}

/// A `dialog.confirm` waiting to be put in front of the user.
pub(super) struct PendingConfirm {
    pub request_id: String,
    pub params: DialogConfirmParams,
}

/// An `execution.run` whose target still has to be bound to a transport.
pub(super) struct PendingExecution {
    pub request_id: String,
    pub params: ExecutionRunParams,
}

/// A request to put something in front of the user, waiting for a window.
pub(super) struct PendingOpen {
    pub request_id: String,
    pub what: OpenRequest,
}

/// What a plugin asked Warp to show.
///
/// The three share one deferral because they share the one thing that makes
/// deferring necessary — a workspace view — not because they resolve alike.
pub(super) enum OpenRequest {
    File(FileOpenParams),
    DiffWorkingTree(DiffOpenWorkingTreeParams),
    DiffFile(DiffOpenFileParams),
}

impl ExtensionHost for BridgeHost<'_, '_> {
    fn capabilities(&self) -> Vec<Capability> {
        IMPLEMENTED_CAPABILITIES.to_vec()
    }

    fn dispatch(
        &mut self,
        request_id: &str,
        method: Method,
        params: serde_json::Value,
    ) -> Dispatched {
        match method {
            Method::NotificationShow => Dispatched::Answered(
                decode_params(method, &params).and_then(|params| self.show_notification(params)),
            ),
            // The answer arrives when the user clicks, which is long after this
            // returns, so nothing is written back here.
            Method::DialogConfirm => match decode_params(method, &params) {
                Ok(params) => {
                    self.question = Some(PendingConfirm {
                        request_id: request_id.to_owned(),
                        params,
                    });
                    Dispatched::Deferred
                }
                Err(error) => Dispatched::Answered(Err(error)),
            },
            // `Session` has already checked that the extension declared this
            // panel, so the snapshot only has to be handed on. It is answered
            // straight away rather than deferred: storing it cannot fail, and
            // a plugin that had to wait for a redraw before publishing the next
            // snapshot would be paying for Warp's frame rate.
            Method::PanelSetState => match decode_params(method, &params) {
                Ok(state) => {
                    self.panel_state = Some(state);
                    Dispatched::Answered(Ok(serde_json::Value::Object(Default::default())))
                }
                Err(error) => Dispatched::Answered(Err(error)),
            },
            Method::WorkspaceGetContext => Dispatched::Answered(self.workspace_context()),
            // Nothing is written back here either: the command has not run yet,
            // and the target it names is bound to a transport by the manager,
            // which then owes the answer.
            Method::ExecutionRun => match decode_params(method, &params) {
                Ok(params) => {
                    self.execution = Some(PendingExecution {
                        request_id: request_id.to_owned(),
                        params,
                    });
                    Dispatched::Deferred
                }
                Err(error) => Dispatched::Answered(Err(error)),
            },
            // Nothing is written back for the three below either. Each has to
            // reach the workspace, and the workspace can reach back into
            // extension state, so the open happens after the manager's borrow
            // of this extension has ended and the manager owes the answer.
            Method::FileOpen => self.defer(request_id, method, &params, OpenRequest::File),
            Method::DiffOpenWorkingTree => {
                self.defer(request_id, method, &params, OpenRequest::DiffWorkingTree)
            }
            Method::DiffOpenFile => self.defer(request_id, method, &params, OpenRequest::DiffFile),
            // `dialog.input` and `dialog.select` share the `dialog.confirm.v1`
            // token with the method above, so they reach dispatch rather than
            // being refused by the capability gate. The code is still the right
            // one: this build does not implement them.
            //
            // Everything else is gated by a capability this build does not
            // advertise at all, so `Session` refuses it before dispatch.
            // Answering here as well keeps the failure honest if that changes.
            Method::ExtensionInitialize | Method::DialogInput | Method::DialogSelect => {
                Dispatched::Answered(Err(ExtensionError::new(
                    ErrorCode::UnsupportedCapability,
                    format!("{method} is not implemented by this Warp build"),
                )))
            }
        }
    }
}

impl BridgeHost<'_, '_> {
    /// Decodes an open request and hands it to the manager to perform.
    ///
    /// Decoding still happens here so a malformed request is refused by the
    /// same gate as every other one, and only a request that is at least
    /// well-formed survives long enough to reach a window.
    fn defer<P: serde::de::DeserializeOwned>(
        &mut self,
        request_id: &str,
        method: Method,
        params: &serde_json::Value,
        into: impl FnOnce(P) -> OpenRequest,
    ) -> Dispatched {
        match decode_params(method, params) {
            Ok(params) => {
                self.open = Some(PendingOpen {
                    request_id: request_id.to_owned(),
                    what: into(params),
                });
                Dispatched::Deferred
            }
            Err(error) => Dispatched::Answered(Err(error)),
        }
    }

    /// Answers `workspace.getContext` with the state at the moment it was
    /// asked, rather than with whatever the last context event described.
    ///
    /// A window with no working directory yet is reported as missing rather
    /// than answered with an invented one: a plugin that runs `git status`
    /// against a directory Warp guessed is worse off than one that retries.
    fn workspace_context(&mut self) -> Result<serde_json::Value, ExtensionError> {
        let context = WorkspaceContextService::current(self.ctx)
            .as_ref()
            .and_then(ActiveContext::to_wire)
            .ok_or_else(|| {
                ExtensionError::new(
                    ErrorCode::WorkspaceMissing,
                    "there is no active workspace with a working directory",
                )
            })?;
        serde_json::to_value(context).map_err(|err| {
            ExtensionError::with_details(
                ErrorCode::InvalidRequest,
                "failed to encode the workspace context",
                err.to_string(),
            )
        })
    }

    fn show_notification(
        &mut self,
        params: NotificationParams,
    ) -> Result<serde_json::Value, ExtensionError> {
        let window_id = WindowManager::handle(self.ctx)
            .as_ref(self.ctx)
            .active_window()
            .ok_or_else(|| {
                ExtensionError::new(
                    ErrorCode::WorkspaceMissing,
                    "there is no window to show a notification in",
                )
            })?;

        let text = match &params.body {
            Some(body) => format!("{}: {} — {body}", self.extension_name, params.title),
            None => format!("{}: {}", self.extension_name, params.title),
        };
        let toast: DismissibleToast<WorkspaceAction> =
            DismissibleToast::new(text, flavor_for(params.level));

        ToastStack::handle(self.ctx).update(self.ctx, |toast_stack, ctx| {
            toast_stack.add_ephemeral_toast(toast, window_id, ctx);
        });
        Ok(serde_json::Value::Object(Default::default()))
    }
}

/// Warp owns the presentation, so a plugin cannot render a failure as a
/// success; the level it sends only selects among Warp's own treatments.
fn flavor_for(level: NotificationLevel) -> ToastFlavor {
    match level {
        NotificationLevel::Info => ToastFlavor::Default,
        // Warp has no distinct warning treatment; a warning is closer to an
        // error than to a neutral message, so it takes the error treatment
        // rather than being silently downgraded.
        NotificationLevel::Warning | NotificationLevel::Error => ToastFlavor::Error,
    }
}
