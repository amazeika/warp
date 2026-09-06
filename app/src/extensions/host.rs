//! The [`ExtensionHost`] implementation: protocol methods turned into app calls.
//!
//! Constructed for the duration of one dispatch and thrown away, because it
//! borrows the live model context. Nothing here re-checks whether the extension
//! was allowed to ask — `Session` has already applied the protocol, capability
//! and permission gates by the time a call arrives.
use extension_host::ExtensionHost;
use extension_protocol::{
    Capability, ErrorCode, ExtensionError, Method, NotificationLevel, NotificationParams,
    decode_params,
};
use warpui::windowing::WindowManager;
use warpui::{ModelContext, SingletonEntity as _};

use super::manager::ExtensionManager;
use crate::view_components::{DismissibleToast, ToastFlavor};
use crate::workspace::{ToastStack, WorkspaceAction};

/// Capabilities this build implements.
///
/// Advertising a capability Warp cannot honour would be worse than omitting it:
/// a plugin that sees the token stops offering the user a fallback. The list
/// grows as each surface is wired up.
pub(super) const IMPLEMENTED_CAPABILITIES: &[Capability] = &[Capability::NotificationV1];

pub(super) struct BridgeHost<'a, 'ctx> {
    /// Shown as the notification's source, so a message a user did not expect
    /// names the extension that produced it rather than appearing to be Warp.
    pub extension_name: String,
    pub ctx: &'a mut ModelContext<'ctx, ExtensionManager>,
}

impl ExtensionHost for BridgeHost<'_, '_> {
    fn capabilities(&self) -> Vec<Capability> {
        IMPLEMENTED_CAPABILITIES.to_vec()
    }

    fn dispatch(
        &mut self,
        method: Method,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ExtensionError> {
        match method {
            Method::NotificationShow => self.show_notification(decode_params(method, &params)?),
            // Every other method is gated by a capability this build does not
            // advertise, so `Session` refuses it before dispatch. Answering
            // here as well keeps the failure honest if that ever changes.
            Method::ExtensionInitialize
            | Method::WorkspaceGetContext
            | Method::ExecutionRun
            | Method::FileOpen
            | Method::DiffOpenWorkingTree
            | Method::DiffOpenFile
            | Method::DialogConfirm
            | Method::DialogInput
            | Method::DialogSelect
            | Method::PanelSetState => Err(ExtensionError::new(
                ErrorCode::UnsupportedCapability,
                format!("{method} is not implemented by this Warp build"),
            )),
        }
    }
}

impl BridgeHost<'_, '_> {
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
