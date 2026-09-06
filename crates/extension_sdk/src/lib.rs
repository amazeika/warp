//! Conveniences for writing a Warp extension in Rust.
//!
//! The SDK is optional. The wire protocol in `extension_protocol` is the
//! compatibility layer, and a plugin in any language that can frame JSON on
//! stdio is a first-class plugin. This crate only saves a Rust plugin from
//! rewriting request correlation and the typed call wrappers.
pub mod client;

pub use client::{Client, ClientError};
