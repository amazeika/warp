//! Newline-delimited JSON framing for the extension transport.
//!
//! Framing is kept separate from process I/O so protocol behavior — oversized
//! frames, truncated lines, malformed JSON, version mismatch — is exercised
//! against in-memory buffers rather than a spawned child.
use std::io::{BufRead, Write};

use extension_protocol::{ErrorCode, ExtensionError, Message, PROTOCOL_VERSION};

/// Largest single frame the host will accept.
///
/// A plugin that exceeds it has its message rejected rather than being allowed
/// to grow the host's memory without bound.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The peer closed the stream cleanly between frames.
    #[error("the extension stream ended")]
    Eof,
    #[error("frame of {size} bytes exceeds the {limit} byte limit")]
    FrameTooLarge { size: usize, limit: usize },
    #[error("frame is not valid protocol JSON: {0}")]
    Malformed(String),
    #[error("frame declares protocol version {found}, expected {expected}")]
    ProtocolMismatch { found: u32, expected: u32 },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl CodecError {
    /// The error a plugin should be told about, where one can be attributed.
    ///
    /// A frame that never decoded has no `request_id`, so the caller decides
    /// whether it can be answered at all.
    pub fn to_extension_error(&self) -> ExtensionError {
        match self {
            CodecError::Eof => ExtensionError::new(ErrorCode::PluginFailed, self.to_string()),
            CodecError::FrameTooLarge { .. } | CodecError::Malformed(_) => {
                ExtensionError::new(ErrorCode::InvalidRequest, self.to_string())
            }
            CodecError::ProtocolMismatch { .. } => {
                ExtensionError::new(ErrorCode::ProtocolMismatch, self.to_string())
            }
            CodecError::Io(_) => ExtensionError::new(ErrorCode::PluginFailed, self.to_string()),
        }
    }
}

/// Reads one frame, returning [`CodecError::Eof`] at a clean end of stream.
///
/// Blank lines are skipped rather than reported, so a plugin may use them as a
/// keepalive without the host treating them as malformed frames.
pub fn read_message(reader: &mut impl BufRead) -> Result<Message, CodecError> {
    let mut line = Vec::new();
    loop {
        line.clear();
        read_limited_line(reader, &mut line)?;
        while line
            .last()
            .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
        {
            line.pop();
        }
        if !line.is_empty() {
            break;
        }
    }

    let message: Message =
        serde_json::from_slice(&line).map_err(|err| CodecError::Malformed(err.to_string()))?;
    let protocol = message_protocol(&message);
    if protocol != PROTOCOL_VERSION {
        return Err(CodecError::ProtocolMismatch {
            found: protocol,
            expected: PROTOCOL_VERSION,
        });
    }
    Ok(message)
}

/// Writes one frame and flushes, so a plugin blocked on a reply is not waiting
/// on the host's buffer.
pub fn write_message(writer: &mut impl Write, message: &Message) -> Result<(), CodecError> {
    let encoded =
        serde_json::to_vec(message).map_err(|err| CodecError::Malformed(err.to_string()))?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(CodecError::FrameTooLarge {
            size: encoded.len(),
            limit: MAX_MESSAGE_BYTES,
        });
    }
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

pub(crate) fn message_protocol(message: &Message) -> u32 {
    match message {
        Message::Request(request) => request.protocol,
        Message::Event(event) => event.protocol,
        Message::Response(response) => response.protocol,
    }
}

/// Reads up to and including the next newline, buffering at most
/// [`MAX_MESSAGE_BYTES`].
///
/// An oversized frame is drained to its terminating newline before the error is
/// returned, so the stream resynchronises and one bad frame costs the plugin
/// that frame rather than the connection.
fn read_limited_line(reader: &mut impl BufRead, out: &mut Vec<u8>) -> Result<(), CodecError> {
    let mut size = 0usize;
    let mut overrun = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(CodecError::Io(err)),
        };
        if available.is_empty() {
            if overrun {
                return Err(CodecError::FrameTooLarge {
                    size,
                    limit: MAX_MESSAGE_BYTES,
                });
            }
            if out.is_empty() {
                return Err(CodecError::Eof);
            }
            return Ok(());
        }

        let (taken, complete) = match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => (index + 1, true),
            None => (available.len(), false),
        };
        size += taken;
        if size > MAX_MESSAGE_BYTES {
            overrun = true;
            out.clear();
        } else {
            out.extend_from_slice(&available[..taken]);
        }
        reader.consume(taken);
        if complete {
            if overrun {
                return Err(CodecError::FrameTooLarge {
                    size,
                    limit: MAX_MESSAGE_BYTES,
                });
            }
            return Ok(());
        }
    }
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
