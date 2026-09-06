//! Running one allow-listed program, locally or on the session that was named.
//!
//! The target is bound to a transport on the main thread, before anything is
//! awaited, and the future that runs the command carries the transport with
//! it. That is what makes invariant 31 hold: a focus change while a command is
//! in flight cannot retarget it, because by then there is nothing left to
//! resolve.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use command::Stdio;
use extension_protocol::{
    ErrorCode, ExecutionRunParams, ExecutionRunResult, ExecutionTarget, ExtensionError,
};
use futures::FutureExt as _;
use futures::future::{Either, select};
use instant::Instant;
use warp_core::SessionId;
use warp_util::host_id::HostId;
use warpui::r#async::Timer;
use warpui::{AppContext, SingletonEntity as _};

use crate::remote_server::client::RemoteServerClient;
use crate::remote_server::manager::RemoteServerManager;
use crate::remote_server::proto::{RunCommandErrorCode, run_command_response};

/// How much of each stream a plugin is given back.
///
/// A plugin asking for `git log` on a large repository should get a truncated
/// answer it can act on, not a failure and not a message large enough to stall
/// the transport; the flag on the result is what tells it the difference.
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// The transport a resolved request will actually run over.
///
/// Resolution happens once, synchronously, and the result is owned by the
/// future — so a request is answered by the session it named or not at all.
pub(super) enum Runner {
    Local,
    Remote {
        session_id: SessionId,
        client: Arc<RemoteServerClient>,
    },
}

pub(super) struct ExecutionService;

impl ExecutionService {
    /// Binds an execution target to the transport that will carry it.
    ///
    /// Fails closed, and never falls back to running locally: a mutating
    /// command aimed at a remote checkout must not land on the same-named path
    /// on this machine.
    pub(super) fn resolve(
        target: &ExecutionTarget,
        ctx: &AppContext,
    ) -> Result<Runner, ExtensionError> {
        let ExecutionTarget::Ssh { session_id } = target else {
            return Ok(Runner::Local);
        };
        let session_id = parse_session_id(session_id)?;

        // A build with no remote manager tracks no sessions at all, which is
        // the same answer as one that never heard of this session.
        let (tracked, client) = match ctx.has_singleton_model::<RemoteServerManager>() {
            false => (false, None),
            true => {
                let manager = RemoteServerManager::as_ref(ctx);
                let client = manager
                    .client_for_session(session_id)
                    .filter(|_| manager.is_session_potentially_active(session_id))
                    .cloned();
                (manager.tracks_session(session_id), client)
            }
        };
        let Some(client) = client else {
            return Err(refusal(session_id, tracked));
        };
        Ok(Runner::Remote { session_id, client })
    }

    /// The host a session is running on.
    ///
    /// Separate from [`Self::resolve`] because a path is bound to a host while
    /// a command is bound to a transport, and the two are wanted by different
    /// callers — but they fail for the same reasons and say so identically, so
    /// the refusal is shared.
    pub(super) fn host_id(session_id: &str, ctx: &AppContext) -> Result<HostId, ExtensionError> {
        let session_id = parse_session_id(session_id)?;
        if !ctx.has_singleton_model::<RemoteServerManager>() {
            return Err(refusal(session_id, false));
        }
        let manager = RemoteServerManager::as_ref(ctx);
        manager
            .host_id_for_session(session_id)
            .cloned()
            .ok_or_else(|| refusal(session_id, manager.tracks_session(session_id)))
    }
}

pub(super) fn parse_session_id(session_id: &str) -> Result<SessionId, ExtensionError> {
    session_id.parse::<u64>().map(SessionId::from).map_err(|_| {
        ExtensionError::new(
            ErrorCode::InvalidRequest,
            format!("`{session_id}` is not a session id"),
        )
    })
}

/// Why a named session cannot carry a request.
///
/// The two codes say different things and a plugin acts on them differently:
/// a session Warp is still tracking but cannot reach has ended under a target
/// the plugin was right to hold, so `session_disconnected` invites it to ask
/// for the context again; one Warp never had is `target_stale`, which says the
/// target itself was never Warp's to run on.
pub(super) fn refusal(session_id: SessionId, tracked: bool) -> ExtensionError {
    let session_id = session_id.as_u64();
    match tracked {
        true => ExtensionError::new(
            ErrorCode::SessionDisconnected,
            format!("session {session_id} is no longer connected"),
        ),
        false => ExtensionError::new(
            ErrorCode::TargetStale,
            format!("session {session_id} is not a session Warp knows about"),
        ),
    }
}

impl Runner {
    /// Runs the command and returns what the plugin gets back.
    ///
    /// Times out only when the plugin asked for one: Warp has no opinion about
    /// how long an allow-listed program should take, and imposing a default
    /// would make a long build look like a failure.
    pub(super) async fn run(
        self,
        params: ExecutionRunParams,
    ) -> Result<ExecutionRunResult, ExtensionError> {
        let started = Instant::now();
        let timeout_ms = params.timeout_ms;
        let run = match self {
            Runner::Local => run_local(params).boxed(),
            Runner::Remote { session_id, client } => run_remote(session_id, client, params).boxed(),
        };
        let output = match timeout_ms {
            None => run.await?,
            Some(timeout_ms) => {
                let expiry = Timer::after(Duration::from_millis(timeout_ms)).boxed();
                match select(run, expiry).await {
                    Either::Left((output, _)) => output?,
                    // Dropping `run` here is what stops the command: the local
                    // child is killed on drop, and the remote request is
                    // abandoned rather than waited out.
                    Either::Right(_) => {
                        return Err(ExtensionError::new(
                            ErrorCode::ExecutionFailed,
                            format!("the command did not finish within {timeout_ms}ms"),
                        ));
                    }
                }
            }
        };

        let (stdout, stdout_truncated) = truncate(output.stdout);
        let (stderr, stderr_truncated) = truncate(output.stderr);
        Ok(ExecutionRunResult {
            exit_code: output.exit_code,
            stdout,
            stderr,
            duration_ms: started.elapsed().as_millis() as u64,
            truncated: stdout_truncated || stderr_truncated,
        })
    }
}

/// What both transports produce, before it is trimmed to the output limit.
struct RawOutput {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run_local(params: ExecutionRunParams) -> Result<RawOutput, ExtensionError> {
    let mut command = command::r#async::Command::new(&params.executable);
    command
        .args(&params.args)
        .current_dir(&params.cwd)
        // Added to Warp's environment rather than replacing it, which is what
        // the remote path does too: a `git` with no `HOME` and no `PATH` is not
        // the `git` the user would have run themselves.
        .envs(params.env.iter())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A command whose plugin gave up waiting for it must not keep running:
        // dropping the future on a timeout is the only cancellation this API
        // has, so it has to be the one that stops the process.
        .kill_on_drop(true);

    let output = command.output().await.map_err(|err| {
        ExtensionError::with_details(
            ErrorCode::ExecutionFailed,
            format!("failed to run `{}`", params.executable),
            err.to_string(),
        )
    })?;
    Ok(RawOutput {
        exit_code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

async fn run_remote(
    session_id: SessionId,
    client: Arc<RemoteServerClient>,
    params: ExecutionRunParams,
) -> Result<RawOutput, ExtensionError> {
    let environment: HashMap<String, String> = params.env.into_iter().collect();
    let response = client
        .run_command(
            session_id,
            shell_command(&params.executable, &params.args),
            Some(params.cwd),
            environment,
        )
        .await
        .map_err(|err| {
            ExtensionError::with_details(
                ErrorCode::SessionDisconnected,
                format!("session {} did not answer the command", session_id.as_u64()),
                err.to_string(),
            )
        })?;

    match response.result {
        Some(run_command_response::Result::Success(success)) => Ok(RawOutput {
            exit_code: success.exit_code,
            stdout: success.stdout,
            stderr: success.stderr,
        }),
        Some(run_command_response::Result::Error(error)) => {
            // A session the daemon has no executor for has ended as far as the
            // plugin is concerned, whatever the manager still believes.
            let code = match error.code() {
                RunCommandErrorCode::SessionNotFound => ErrorCode::SessionDisconnected,
                _ => ErrorCode::ExecutionFailed,
            };
            Err(ExtensionError::with_details(
                code,
                format!("the command failed on session {}", session_id.as_u64()),
                error.message,
            ))
        }
        None => Err(ExtensionError::new(
            ErrorCode::ExecutionFailed,
            format!("session {} returned an empty answer", session_id.as_u64()),
        )),
    }
}

/// Quotes an argv vector into the single shell string the remote proto takes.
///
/// This is the documented asymmetry in the design: the extension API is argv
/// all the way down, and the local path spawns argv directly, but
/// `RunCommandRequest` carries `command` as a string. Every word is wrapped in
/// single quotes, inside which the shell expands nothing at all, and an
/// embedded quote is closed, escaped and reopened — the only sequence a
/// single-quoted POSIX word cannot contain. The follow-up is a repeated `args`
/// field on the request, after which this function goes away without the
/// extension-facing API changing.
fn shell_command(executable: &str, args: &[String]) -> String {
    std::iter::once(executable)
        .chain(args.iter().map(String::as_str))
        .map(quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r#"'\''"#))
}

/// Cuts a stream to the output limit, saying whether anything was cut.
///
/// The cut is on bytes rather than characters, so a limit can be enforced
/// before anything is decoded; a multi-byte character split by the cut is
/// replaced by the lossy conversion rather than dropping the line around it.
fn truncate(mut output: Vec<u8>) -> (String, bool) {
    let truncated = output.len() > MAX_OUTPUT_BYTES;
    if truncated {
        output.truncate(MAX_OUTPUT_BYTES);
    }
    (String::from_utf8_lossy(&output).into_owned(), truncated)
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
