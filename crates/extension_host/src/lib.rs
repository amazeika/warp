//! Host-side machinery for running Warp extensions out of process.
//!
//! This crate finds extensions on disk, validates their manifests, frames
//! messages, supervises the child process, and enforces the permission and
//! capability gates before anything reaches Warp. It deliberately does not
//! depend on `warpui`, so lifecycle, gating and framing are testable without an
//! app harness — the same split `crates/local_control` uses.
//!
//! The app-side adapter implements [`ExtensionHost`] and owns everything that
//! touches WarpUI state.
pub mod codec;
pub mod discovery;
pub mod lifecycle;
pub mod logging;
pub mod process;
pub mod session;

pub use codec::{CodecError, MAX_MESSAGE_BYTES, read_message, write_message};
pub use discovery::{
    DiscoveredExtension, ExtensionRecord, ValidationError, discover, discover_in,
    extension_directory_is_candidate, extensions_root,
};
pub use lifecycle::{ExtensionState, RestartDecision, RestartPolicy, StateMachine, StopReason};
pub use logging::{RedactingWriter, extension_log_path, extension_log_path_in, extension_log_root};
pub use process::{ExtensionProcess, ProcessError, ProcessEvent};
pub use session::{ExtensionHost, Session};
