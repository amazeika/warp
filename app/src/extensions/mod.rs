//! Warp's side of the out-of-process extension API.
//!
//! This module is the app-side counterpart to `crates/extension_host`: that
//! crate knows how to find, validate, start and gate an extension without
//! knowing anything about WarpUI, and everything here is the part that does
//! touch application state.
//!
//! It is unrelated to `crate::plugin`, which is Warp's internal JavaScript
//! plugin host for shell completions. That host runs Warp's own code; this one
//! runs code the user installed, which is why permissions, capability
//! negotiation and supervision exist at all.
//!
//! Threading:
//!
//! ```text
//!   plugin stdout --> reader thread (extension_host)
//!                          |
//!                          v
//!                     pump thread  --ModelSpawner--> main thread
//!                                                    ExtensionManager
//!                                                      |  Session gates
//!                                                      |  BridgeHost dispatch
//!                                                      v
//!                                                    plugin stdin
//! ```
//!
//! Reading happens off the main thread because a blocking read must never stall
//! the UI; everything else — gating, dispatch, and the write back — happens on
//! the main thread, so extension state needs no locks and a dispatch can touch
//! models directly.
mod context;
mod contributions;
mod dialog;
mod execution;
mod host;
mod manager;
mod permissions;

pub(crate) use contributions::{ContributedCommand, ContributedPanel};
pub(crate) use manager::{ExtensionManager, ExtensionManagerEvent};
