//! Shared wire contract between Warp and out-of-process extensions.
//!
//! Everything that crosses the process boundary is defined here and nowhere
//! else: the message envelopes, the method and event names, the manifest that
//! Warp reads before starting a plugin, the permissions a manifest can request,
//! and the capability tokens Warp advertises during the handshake.
//!
//! The crate is deliberately free of app, UI and process-management types so
//! both sides of the boundary — the Warp host and any plugin SDK — can depend
//! on it. Process supervision lives in `extension_host`.
pub mod capabilities;
pub mod events;
pub mod manifest;
pub mod methods;
pub mod permissions;
pub mod protocol;

pub use capabilities::Capability;
pub use events::{
    ActionOrigin, CommandInvokedParams, ContextChangedParams, EventKind, PanelActionParams,
    SessionChangedParams,
};
pub use manifest::{
    Activation, CommandContribution, ExecutionPolicy, ExtensionManifest, MANIFEST_FILE_NAME,
    ManifestError, ManifestPermissions, PanelContribution, PanelLocation,
};
pub use methods::{
    DialogConfirmParams, DialogConfirmResult, DialogInputParams, DialogInputResult,
    DialogSelectOption, DialogSelectParams, DialogSelectResult, DiffComparison, DiffOpenFileParams,
    DiffOpenWorkingTreeParams, ExecutionRunParams, ExecutionRunResult, ExecutionTarget,
    FileOpenParams, InitializeParams, InitializeResult, Method, NotificationLevel,
    NotificationParams, PanelItem, PanelItemAction, PanelSection, PanelStatus, PanelViewState,
    PreferredView, WorkspaceContext,
};
pub use permissions::{Permission, PermissionSet};
pub use protocol::{
    API_VERSION, ErrorCode, EventEnvelope, ExtensionError, Message, PROTOCOL_VERSION,
    RequestEnvelope, ResponseEnvelope, ResponsePayload, decode_params,
};
