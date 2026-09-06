//! Spawning and supervising one extension's child process.
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use command::blocking::Command;
use extension_protocol::{EventEnvelope, EventKind, Message};
use instant::Instant;

use crate::codec::{CodecError, read_message, write_message};
use crate::logging::RedactingWriter;

/// How often the exit wait re-checks the child.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Environment variable naming the extension to its own process.
pub const EXTENSION_ID_ENV: &str = "WARP_EXTENSION_ID";

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("failed to start extension executable {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("extension stdio was not available")]
    MissingStdio,
    #[error("failed to write to the extension: {0}")]
    Write(String),
    #[error("the extension is no longer running")]
    NotRunning,
}

/// Something that arrived from the plugin's stdout.
#[derive(Debug)]
pub enum ProcessEvent {
    Message(Box<Message>),
    /// A frame that could not be used. The connection survives a single bad
    /// frame; the supervisor decides when a run of them is a violation.
    Decode(CodecError),
    /// stdout reached end of file: the plugin is finished talking.
    Closed,
}

/// A running plugin, its transport, and the threads draining it.
pub struct ExtensionProcess {
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    events: Receiver<ProcessEvent>,
    reader: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<()>>,
}

impl ExtensionProcess {
    /// Starts `executable` with its stdio wired up as the transport.
    ///
    /// The plugin's stdout is the protocol stream, so plugins must log to
    /// stderr; anything they print to stdout is read as a frame.
    pub fn spawn(
        extension_id: &str,
        executable: &Path,
        working_directory: &Path,
        log_path: Option<&Path>,
    ) -> Result<Self, ProcessError> {
        let mut command = Command::new(executable);
        command
            .current_dir(working_directory)
            .env(EXTENSION_ID_ENV, extension_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // A plugin must not outlive the Warp process that started it.
        #[cfg(windows)]
        command.kill_on_parent_process_close();

        let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
            path: executable.to_path_buf(),
            source,
        })?;

        let stdin = child.stdin.take().ok_or(ProcessError::MissingStdio)?;
        let stdout = child.stdout.take().ok_or(ProcessError::MissingStdio)?;
        let stderr = child.stderr.take().ok_or(ProcessError::MissingStdio)?;

        let (sender, events) = channel();
        let reader = std::thread::Builder::new()
            .name(format!("warp-extension-{extension_id}-stdout"))
            .spawn(move || read_loop(BufReader::new(stdout), sender))
            .ok();

        let stderr_log = log_path.map(Path::to_path_buf);
        let stderr = std::thread::Builder::new()
            .name(format!("warp-extension-{extension_id}-stderr"))
            .spawn(move || drain_stderr(BufReader::new(stderr), stderr_log))
            .ok();

        Ok(Self {
            child,
            stdin: Some(BufWriter::new(stdin)),
            events,
            reader,
            stderr,
        })
    }

    pub fn send(&mut self, message: &Message) -> Result<(), ProcessError> {
        let stdin = self.stdin.as_mut().ok_or(ProcessError::NotRunning)?;
        write_message(stdin, message).map_err(|err| ProcessError::Write(err.to_string()))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<ProcessEvent, RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }

    /// Returns the child's exit status if it has already exited.
    pub fn try_exit_status(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Asks the plugin to stop, then waits, then kills it.
    ///
    /// Warp's own shutdown must not be held up by a plugin that ignores the
    /// request, so the timeout is a hard bound rather than a hint.
    pub fn stop(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let shutdown = Message::Event(EventEnvelope::new(
            EventKind::ExtensionShutdown,
            serde_json::Value::Object(Default::default()),
        ));
        let _ = self.send(&shutdown);
        // Closing stdin is what unblocks a plugin parked on a blocking read.
        self.stdin = None;

        if let Some(status) = self.wait_for_exit(timeout) {
            return Some(status);
        }
        let _ = self.child.kill();
        self.child.wait().ok()
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_exit_status() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(EXIT_POLL_INTERVAL);
        }
    }
}

impl Drop for ExtensionProcess {
    /// A dropped handle must not leave an orphaned plugin behind.
    fn drop(&mut self) {
        self.stdin = None;
        if self.try_exit_status().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        for handle in [self.reader.take(), self.stderr.take()]
            .into_iter()
            .flatten()
        {
            let _ = handle.join();
        }
    }
}

fn read_loop(mut reader: impl std::io::BufRead, sender: Sender<ProcessEvent>) {
    loop {
        match read_message(&mut reader) {
            Ok(message) => {
                if sender
                    .send(ProcessEvent::Message(Box::new(message)))
                    .is_err()
                {
                    return;
                }
            }
            Err(CodecError::Eof) => {
                let _ = sender.send(ProcessEvent::Closed);
                return;
            }
            Err(err) => {
                let fatal = matches!(err, CodecError::Io(_));
                if sender.send(ProcessEvent::Decode(err)).is_err() || fatal {
                    return;
                }
            }
        }
    }
}

fn drain_stderr(reader: impl std::io::BufRead, log_path: Option<PathBuf>) {
    let mut log = log_path.and_then(|path| {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(RedactingWriter::new)
    });

    for line in reader.lines() {
        let Ok(line) = line else { return };
        if let Some(log) = log.as_mut() {
            let _ = log.write_line(&line);
        }
    }
    if let Some(log) = log.as_mut() {
        let _ = log.flush();
    }
}
