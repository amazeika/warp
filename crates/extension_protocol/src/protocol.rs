//! Message envelopes and the stable error vocabulary shared with plugins.
use serde::{Deserialize, Serialize};

use crate::events::EventKind;
use crate::methods::Method;

/// Wire format version carried by every message.
///
/// A mismatch is fatal for the message, not merely a warning: the two sides
/// disagree about what the remaining fields mean.
pub const PROTOCOL_VERSION: u32 = 1;

/// Extension API generation that manifests declare and the handshake reports.
///
/// This is versioned separately from [`PROTOCOL_VERSION`] so the set of methods
/// can grow without rewriting the envelope, and vice versa.
pub const API_VERSION: u32 = 1;

/// A plugin-initiated call into Warp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub protocol: u32,
    pub request_id: String,
    pub method: Method,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl RequestEnvelope {
    pub fn new(request_id: impl Into<String>, method: Method, params: serde_json::Value) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            request_id: request_id.into(),
            method,
            params,
        }
    }

    pub fn with_params<T: Serialize>(
        request_id: impl Into<String>,
        method: Method,
        params: &T,
    ) -> Result<Self, ExtensionError> {
        Ok(Self::new(
            request_id,
            method,
            encode_params(method, params)?,
        ))
    }
}

/// Warp's answer to exactly one [`RequestEnvelope`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub protocol: u32,
    pub request_id: String,
    #[serde(flatten)]
    pub payload: ResponsePayload,
}

impl ResponseEnvelope {
    pub fn ok(request_id: impl Into<String>, result: serde_json::Value) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            request_id: request_id.into(),
            payload: ResponsePayload::Ok { result },
        }
    }

    pub fn error(request_id: impl Into<String>, error: ExtensionError) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            request_id: request_id.into(),
            payload: ResponsePayload::Err { error },
        }
    }

    /// Decodes a successful result, turning both a failed response and a
    /// malformed payload into an [`ExtensionError`] the caller can propagate.
    pub fn result_as<T: serde::de::DeserializeOwned>(&self) -> Result<T, ExtensionError> {
        match &self.payload {
            ResponsePayload::Ok { result } => {
                serde_json::from_value(result.clone()).map_err(|err| {
                    ExtensionError::with_details(
                        ErrorCode::InvalidRequest,
                        "failed to decode result",
                        err.to_string(),
                    )
                })
            }
            ResponsePayload::Err { error } => Err(error.clone()),
        }
    }
}

/// Success or failure half of a [`ResponseEnvelope`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsePayload {
    Ok { result: serde_json::Value },
    Err { error: ExtensionError },
}

/// A one-way notification from Warp to a plugin. Events are never answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub protocol: u32,
    pub event: EventKind,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl EventEnvelope {
    pub fn new(event: EventKind, params: serde_json::Value) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            event,
            params,
        }
    }

    pub fn with_params<T: Serialize>(event: EventKind, params: &T) -> Result<Self, ExtensionError> {
        let params = serde_json::to_value(params).map_err(|err| {
            ExtensionError::with_details(
                ErrorCode::InvalidRequest,
                format!("failed to encode {} parameters", event.as_str()),
                err.to_string(),
            )
        })?;
        Ok(Self::new(event, params))
    }
}

/// Anything that can appear on the wire in either direction.
///
/// Variant order matters: `deny_unknown_fields` on the request and event
/// envelopes is what makes untagged discrimination unambiguous, and the
/// response — which cannot deny unknown fields because its payload is
/// flattened — is tried last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(RequestEnvelope),
    Event(EventEnvelope),
    Response(ResponseEnvelope),
}

impl From<RequestEnvelope> for Message {
    fn from(value: RequestEnvelope) -> Self {
        Message::Request(value)
    }
}

impl From<ResponseEnvelope> for Message {
    fn from(value: ResponseEnvelope) -> Self {
        Message::Response(value)
    }
}

impl From<EventEnvelope> for Message {
    fn from(value: EventEnvelope) -> Self {
        Message::Event(value)
    }
}

/// Structured failure returned to a plugin instead of a response result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ExtensionError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl ExtensionError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(
        code: ErrorCode,
        message: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            details: Some(details.into()),
        }
    }
}

/// Stable machine-readable failure reason. These strings are public API.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    PluginNotFound,
    PluginFailed,
    PermissionDenied,
    UnsupportedCapability,
    WorkspaceMissing,
    RepositoryMissing,
    SessionDisconnected,
    ExecutionFailed,
    TargetStale,
    OperationConflict,
    InvalidRequest,
    ProtocolMismatch,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::PluginNotFound => "plugin_not_found",
            ErrorCode::PluginFailed => "plugin_failed",
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::UnsupportedCapability => "unsupported_capability",
            ErrorCode::WorkspaceMissing => "workspace_missing",
            ErrorCode::RepositoryMissing => "repository_missing",
            ErrorCode::SessionDisconnected => "session_disconnected",
            ErrorCode::ExecutionFailed => "execution_failed",
            ErrorCode::TargetStale => "target_stale",
            ErrorCode::OperationConflict => "operation_conflict",
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::ProtocolMismatch => "protocol_mismatch",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) fn encode_params<T: Serialize>(
    method: Method,
    params: &T,
) -> Result<serde_json::Value, ExtensionError> {
    serde_json::to_value(params).map_err(|err| {
        ExtensionError::with_details(
            ErrorCode::InvalidRequest,
            format!("failed to encode {} parameters", method.as_str()),
            err.to_string(),
        )
    })
}

/// Decodes request parameters, reporting the offending method on failure.
pub fn decode_params<T: serde::de::DeserializeOwned>(
    method: Method,
    params: &serde_json::Value,
) -> Result<T, ExtensionError> {
    serde_json::from_value(params.clone()).map_err(|err| {
        ExtensionError::with_details(
            ErrorCode::InvalidRequest,
            format!("failed to decode {} parameters", method.as_str()),
            err.to_string(),
        )
    })
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
