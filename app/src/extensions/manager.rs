//! Discovery, permission gating, supervision and dispatch for installed extensions.
//!
//! One `ExtensionManager` owns every extension Warp knows about. It lives on
//! the main thread and holds each plugin's process handle and session, so all
//! state transitions happen in one place with no locking; the only work done
//! elsewhere is the blocking read of each plugin's stdout.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use extension_host::discovery::{DiscoveredExtension, ExtensionRecord};
use extension_host::{
    ExtensionProcess, ExtensionState, ProcessEvent, RestartDecision, Session, StateMachine,
    StopReason, extension_log_path, extensions_root,
};
use extension_protocol::{
    ActionOrigin, Activation, CommandInvokedParams, DialogConfirmParams, DialogConfirmResult,
    ErrorCode, EventEnvelope, EventKind, ExecutionRunParams, ExecutionTarget, ExtensionError,
    ExtensionManifest, Message, PanelActionParams, PanelViewState, Permission, PermissionSet,
    ResponseEnvelope, SessionChangedParams,
};
use instant::Instant;
use warp_core::SessionId;
use warpui::WindowId;
use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, ModelSpawner, SingletonEntity};

use super::context::{ActiveContext, WorkspaceContextService, session_id_to_string};
use super::contributions::{ContributedCommand, ContributedPanel, Contributions};
use super::dialog::{self, Question};
use super::execution::ExecutionService;
use super::host::BridgeHost;
use super::permissions::{GrantStore, PermissionDecision};
use crate::remote_server::manager::{RemoteServerManager, RemoteServerManagerEvent};
use crate::workspace::Workspace;

/// How long a plugin has to answer `extension.initialize`.
///
/// A plugin that never completes the handshake is indistinguishable from one
/// that hung, and the difference does not matter: neither can serve a request.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a plugin is given to exit after being asked to.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// How long to wait before offering a question again that had nowhere to go.
///
/// Extensions are discovered while Warp is still starting, before a window
/// exists to prompt in. Nothing is waiting on a permission question, so it is
/// kept and retried rather than answered on the user's behalf.
const ASK_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// What the UI needs to know about the extensions Warp is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionManagerEvent {
    /// A command or panel appeared or disappeared.
    ContributionsChanged,
    /// An extension published a new snapshot of one of its panels.
    PanelStateChanged {
        extension_id: String,
        panel_id: String,
    },
    /// Warp has given up restarting this extension.
    Failed {
        extension_id: String,
        extension_name: String,
        reason: String,
    },
}

/// What answering the question currently on screen does.
///
/// The queue holds outcomes rather than closures so a question's effect can be
/// asserted without a window to click in, and so a question whose extension
/// went away can be neutralised in place.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// Start the extension if allowed; leave it inactive and unasked-about if
    /// not.
    Permission { extension_id: String },
    /// Answer one `dialog.confirm` request.
    Dialog {
        extension_id: String,
        request_id: String,
    },
    /// The extension stopped while its question was on screen. The click is
    /// still coming and now has nothing to act on, but the modal still has to
    /// close before the next question can be asked.
    Abandoned,
}

impl Outcome {
    fn extension_id(&self) -> Option<&str> {
        match self {
            Outcome::Permission { extension_id } | Outcome::Dialog { extension_id, .. } => {
                Some(extension_id)
            }
            Outcome::Abandoned => None,
        }
    }

    fn is_permission(&self) -> bool {
        matches!(self, Outcome::Permission { .. })
    }
}

/// One question waiting to be asked, or being asked right now.
struct PendingQuestion {
    question: Question,
    outcome: Outcome,
}

/// One extension Warp knows about, running or not.
struct ManagedExtension {
    manifest: ExtensionManifest,
    directory: PathBuf,
    executable: PathBuf,
    state: StateMachine,
    running: Option<RunningExtension>,
}

/// The parts that exist only while the plugin process does.
struct RunningExtension {
    process: ExtensionProcess,
    session: Session,
    started_at: Instant,
}

pub struct ExtensionManager {
    root: PathBuf,
    grants: GrantStore,
    contributions: Contributions,
    /// The latest snapshot each running extension published, keyed by extension
    /// and panel id. Warp keeps only the newest: the model is a whole picture
    /// of the panel, not a stream of edits, so an older one has no use.
    panel_states: BTreeMap<(String, String), PanelViewState>,
    extensions: BTreeMap<String, ManagedExtension>,
    /// Directories that claimed to be extensions but cannot run, kept with
    /// their reason so the UI can say why rather than silently omitting them.
    rejected: Vec<(PathBuf, String)>,
    spawner: ModelSpawner<Self>,
    /// The directory activation conditions are evaluated against. `None` until
    /// workspace context exists, which keeps a `workspace_contains` extension
    /// inactive rather than guessing.
    workspace_directory: Option<PathBuf>,
    /// The last context Warp described to its extensions.
    ///
    /// Its job is to work out which events a change is worth. An action's
    /// origin is assembled afresh instead, because a value that lags by a
    /// frame is how an action ends up attached to the pane the user has just
    /// left — this is only what that falls back to when the workspace cannot
    /// be read at all.
    context: Option<ActiveContext>,
    /// Questions waiting for the user, and the one they are looking at.
    ///
    /// Warp asks one at a time: the modal surface holds a single dialog, so a
    /// second question shown over the first would replace it, and the answer
    /// the plugin is blocked on would never arrive.
    questions: VecDeque<PendingQuestion>,
    asking: Option<Outcome>,
    /// Set while a retry of an unaskable question is already pending, so a
    /// second question does not queue a second timer.
    retry_scheduled: bool,
    /// Extensions the user declined. A refusal is an answer, so Warp stops
    /// asking until the user comes back to the extension themselves.
    declined: BTreeSet<String>,
}

impl Entity for ExtensionManager {
    type Event = ExtensionManagerEvent;
}

impl SingletonEntity for ExtensionManager {}

impl ExtensionManager {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        Self::with_paths(extensions_root(), GrantStore::load(), ctx)
    }

    /// Builds a manager over an explicit extensions root and grant store.
    ///
    /// Both are parameters rather than looked up here so the manager can be
    /// exercised against a temporary directory without mutating process
    /// environment, which is neither safe nor deterministic under a test runner.
    fn with_paths(root: PathBuf, grants: GrantStore, ctx: &mut ModelContext<Self>) -> Self {
        let mut manager = Self {
            root,
            grants,
            contributions: Contributions::default(),
            panel_states: BTreeMap::new(),
            extensions: BTreeMap::new(),
            rejected: Vec::new(),
            spawner: ctx.spawner(),
            workspace_directory: None,
            context: None,
            questions: VecDeque::new(),
            asking: None,
            retry_scheduled: false,
            declined: BTreeSet::new(),
        };
        manager.watch_remote_sessions(ctx);
        manager.context_changed(ctx);
        manager.refresh(ctx);
        manager
    }

    /// Re-reads the extensions directory and starts whatever is eligible.
    ///
    /// Extensions already running are left alone: rediscovering the same
    /// manifest is not a reason to interrupt a plugin mid-request.
    pub fn refresh(&mut self, ctx: &mut ModelContext<Self>) {
        self.rejected.clear();
        for discovered in extension_host::discover_in(&self.root) {
            self.reconcile(discovered);
        }
        self.activate_eligible(ctx);
    }

    fn reconcile(&mut self, discovered: DiscoveredExtension) {
        match discovered.record {
            ExtensionRecord::Invalid { reason } => {
                self.rejected
                    .push((discovered.directory, reason.to_string()));
            }
            ExtensionRecord::Valid {
                manifest,
                executable,
            } => {
                let id = manifest.id.clone();
                match self.extensions.get_mut(&id) {
                    Some(existing) if existing.running.is_some() => {}
                    Some(existing) => {
                        existing.manifest = *manifest;
                        existing.directory = discovered.directory;
                        existing.executable = executable;
                        existing.state.validated();
                    }
                    None => {
                        let mut state = StateMachine::new(Default::default());
                        state.validated();
                        self.extensions.insert(
                            id,
                            ManagedExtension {
                                manifest: *manifest,
                                directory: discovered.directory,
                                executable,
                                state,
                                running: None,
                            },
                        );
                    }
                }
            }
        }
    }

    fn activate_eligible(&mut self, ctx: &mut ModelContext<Self>) {
        let candidates: Vec<String> = self
            .extensions
            .iter()
            .filter(|(_, extension)| extension.running.is_none())
            .filter(|(_, extension)| !extension.state.state().needs_user_action())
            .filter(|(_, extension)| {
                should_activate(
                    extension.manifest.activation.as_ref(),
                    self.workspace_directory.as_deref(),
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in candidates {
            self.activate(&id, ctx);
        }
    }

    /// Starts an extension if the user has already allowed it, and asks
    /// otherwise. Nothing runs before the answer: the prompt is the gate, not a
    /// notice about something that already happened.
    pub fn activate(&mut self, extension_id: &str, ctx: &mut ModelContext<Self>) {
        if self.declined.contains(extension_id) || self.is_awaiting_permission(extension_id) {
            return;
        }
        let Some(extension) = self.extensions.get(extension_id) else {
            return;
        };
        match self.grants.decide(&extension.manifest) {
            PermissionDecision::Granted => self.start(extension_id, ctx),
            PermissionDecision::Prompt {
                requested,
                executables,
                added,
                added_executables,
            } => {
                let question = permission_question(
                    &extension.manifest.name,
                    &requested,
                    &executables,
                    &added,
                    &added_executables,
                );
                self.ask(
                    PendingQuestion {
                        question,
                        outcome: Outcome::Permission {
                            extension_id: extension_id.to_owned(),
                        },
                    },
                    ctx,
                );
            }
        }
    }

    /// Asks again about an extension the user previously declined.
    ///
    /// Declining is remembered for the session, so this is the only way back:
    /// a refusal must not be undone by a directory rescan the user did not ask
    /// for.
    pub fn retry(&mut self, extension_id: &str, ctx: &mut ModelContext<Self>) {
        self.declined.remove(extension_id);
        self.activate(extension_id, ctx);
    }

    /// Records the user's consent and starts the extension.
    pub fn allow(&mut self, extension_id: &str, ctx: &mut ModelContext<Self>) {
        let Some(extension) = self.extensions.get(extension_id) else {
            return;
        };
        if let Err(err) = self.grants.grant(&extension.manifest) {
            // The extension still starts: the user answered, and failing to
            // write the record only means they will be asked again next time.
            log::warn!("Failed to record the grant for {extension_id}: {err}");
        }
        self.declined.remove(extension_id);
        self.start(extension_id, ctx);
    }

    /// Forgets any grant and stops the extension if it is running.
    pub fn revoke(&mut self, extension_id: &str, ctx: &mut ModelContext<Self>) {
        if let Err(err) = self.grants.revoke(extension_id) {
            log::warn!("Failed to revoke the grant for {extension_id}: {err}");
        }
        self.stop(extension_id, StopReason::Requested, ctx);
    }

    /// Queues one question and shows it if nothing else is being asked.
    fn ask(&mut self, pending: PendingQuestion, ctx: &mut ModelContext<Self>) {
        self.questions.push_back(pending);
        self.show_next_question(ctx);
    }

    fn show_next_question(&mut self, ctx: &mut ModelContext<Self>) {
        while self.asking.is_none() {
            let Some(pending) = self.questions.pop_front() else {
                return;
            };
            let shown = dialog::show(
                &pending.question,
                |confirmed, ctx: &mut AppContext| {
                    let manager = Self::handle(&*ctx);
                    manager.update(ctx, |manager, ctx| manager.answered(confirmed, ctx));
                },
                ctx,
            );
            if shown {
                self.asking = Some(pending.outcome);
                return;
            }

            match pending.outcome {
                // A plugin is blocked on this one, so it gets an answer now,
                // and a confirmation nobody gave is a refusal.
                outcome @ (Outcome::Dialog { .. } | Outcome::Abandoned) => {
                    self.resolve(outcome, false, ctx);
                }
                // Nothing is waiting on a permission prompt, so it keeps its
                // place until Warp has somewhere to show it. Answering it here
                // would decline an extension the user was never shown.
                outcome => {
                    self.questions.push_front(PendingQuestion {
                        question: pending.question,
                        outcome,
                    });
                    self.schedule_retry(ctx);
                    return;
                }
            }
        }
    }

    fn schedule_retry(&mut self, ctx: &mut ModelContext<Self>) {
        if self.retry_scheduled {
            return;
        }
        self.retry_scheduled = true;
        ctx.spawn(
            async move {
                Timer::after(ASK_RETRY_INTERVAL).await;
            },
            |manager, _, ctx| {
                manager.retry_scheduled = false;
                manager.show_next_question(ctx);
            },
        );
    }

    /// The user answered the question currently on screen.
    fn answered(&mut self, confirmed: bool, ctx: &mut ModelContext<Self>) {
        let Some(outcome) = self.asking.take() else {
            return;
        };
        self.resolve(outcome, confirmed, ctx);
        self.show_next_question(ctx);
    }

    fn resolve(&mut self, outcome: Outcome, confirmed: bool, ctx: &mut ModelContext<Self>) {
        match outcome {
            Outcome::Permission { extension_id } => {
                if confirmed {
                    self.allow(&extension_id, ctx);
                } else {
                    self.declined.insert(extension_id);
                }
            }
            Outcome::Dialog {
                extension_id,
                request_id,
            } => self.answer_dialog(&extension_id, request_id, confirmed),
            Outcome::Abandoned => {}
        }
    }

    /// True when this extension already has a permission question outstanding,
    /// so a rescan while the prompt is up does not stack a second one.
    fn is_awaiting_permission(&self, extension_id: &str) -> bool {
        let outstanding = self
            .questions
            .iter()
            .map(|pending| &pending.outcome)
            .chain(self.asking.iter());
        outstanding
            .filter(|outcome| outcome.is_permission())
            .any(|outcome| outcome.extension_id() == Some(extension_id))
    }

    /// Drops the questions belonging to an extension that is going away.
    ///
    /// The one already on screen cannot be withdrawn, so it is neutralised
    /// instead: the click still closes the modal and lets the next question
    /// through, but it no longer starts a plugin or answers a request that no
    /// longer exists.
    fn abandon_questions(&mut self, extension_id: &str) {
        self.questions
            .retain(|pending| pending.outcome.extension_id() != Some(extension_id));
        if self
            .asking
            .as_ref()
            .is_some_and(|outcome| outcome.extension_id() == Some(extension_id))
        {
            self.asking = Some(Outcome::Abandoned);
        }
    }

    /// Asks the user a plugin's `dialog.confirm` question.
    ///
    /// The answer is written back from [`Self::answer_dialog`] once they click,
    /// which is why the dispatch that got here returned `Deferred`.
    pub(super) fn confirm(
        &mut self,
        extension_id: &str,
        request_id: String,
        params: DialogConfirmParams,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(extension) = self.extensions.get(extension_id) else {
            return;
        };
        self.ask(
            PendingQuestion {
                question: confirm_question(&extension.manifest.name, params),
                outcome: Outcome::Dialog {
                    extension_id: extension_id.to_owned(),
                    request_id,
                },
            },
            ctx,
        );
    }

    fn answer_dialog(&mut self, extension_id: &str, request_id: String, confirmed: bool) {
        let result = match serde_json::to_value(DialogConfirmResult { confirmed }) {
            Ok(result) => result,
            Err(err) => {
                log::warn!("Failed to encode a dialog answer for {extension_id}: {err}");
                return;
            }
        };
        self.respond(extension_id, request_id, Ok(result));
    }

    /// Tells an extension that the user picked one of its palette entries.
    ///
    /// The lookup is against the live contribution registry rather than the
    /// manifest, so an entry that outlived its extension by a frame invokes
    /// nothing instead of reaching a plugin that is not there to answer.
    pub fn invoke_command(
        &mut self,
        extension_id: &str,
        command_id: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        if self
            .contributions
            .command(extension_id, command_id)
            .is_none()
        {
            return;
        }
        let origin = self.origin(ctx);
        let event = match EventEnvelope::with_params(
            EventKind::CommandInvoked,
            &CommandInvokedParams {
                command_id: command_id.to_owned(),
                origin,
            },
        ) {
            Ok(event) => Message::Event(event),
            Err(err) => {
                log::warn!("Failed to encode {command_id} for extension {extension_id}: {err}");
                return;
            }
        };
        let Some(running) = self
            .extensions
            .get_mut(extension_id)
            .and_then(|extension| extension.running.as_mut())
        else {
            return;
        };
        if let Err(err) = running.process.send(&event) {
            log::warn!("Failed to deliver {command_id} to extension {extension_id}: {err}");
        }
    }

    /// Where an action the user just took is anchored.
    ///
    /// Assembled at invocation and carried in the event, so a plugin that is
    /// slow to respond acts on the workspace, session and repository the user
    /// was looking at rather than the ones they moved to.
    ///
    /// Falls back to the last context Warp described, and then to the window
    /// alone. A build with no workspace to read — a launch mode that never
    /// opens one, or a test harness — still targets the local machine, which
    /// is the one target that cannot be aimed at the wrong host; guessing a
    /// session id would not be.
    fn origin(&self, ctx: &AppContext) -> ActionOrigin {
        WorkspaceContextService::current(ctx)
            .or_else(|| self.context.clone())
            .map(|context| context.origin())
            .unwrap_or_else(|| ActionOrigin {
                workspace_id: self.workspace_id(ctx),
                session_id: None,
                repository_root: None,
                execution_target: ExecutionTarget::Local,
            })
    }

    fn workspace_id(&self, ctx: &AppContext) -> String {
        ctx.windows()
            .active_window()
            .map(|window_id| window_id.to_string())
            .unwrap_or_default()
    }

    /// Re-reads the workspace context and tells every running extension what
    /// moved.
    fn context_changed(&mut self, ctx: &mut ModelContext<Self>) {
        let current = WorkspaceContextService::current(ctx);
        self.context_moved(current, ctx);
    }

    /// The active workspace saying its context may have moved.
    ///
    /// The workspace assembles its own context and hands it over rather than
    /// being read back out of the registry, because it is borrowed by the call
    /// that got here.
    ///
    /// Diffing here rather than having each caller say what changed is what
    /// keeps the events honest: a `cd` inside one repository says only that
    /// the directory moved, and moving between two windows open on the same
    /// repository says nothing about the repository at all.
    pub fn workspace_context_changed(
        &mut self,
        workspace: &Workspace,
        window_id: WindowId,
        ctx: &mut ModelContext<Self>,
    ) {
        let current = WorkspaceContextService::of(workspace, window_id, ctx);
        self.context_moved(Some(current), ctx);
    }

    fn context_moved(&mut self, current: Option<ActiveContext>, ctx: &mut ModelContext<Self>) {
        let events = WorkspaceContextService::changes(self.context.as_ref(), current.as_ref());
        self.context = current;
        self.adopt_workspace_directory(ctx);
        for event in events {
            match event.encode() {
                Ok(envelope) => self.broadcast(&Message::Event(envelope)),
                Err(err) => log::warn!("Failed to encode {}: {err}", event.kind),
            }
        }
    }

    /// Updates the directory activation conditions are evaluated against.
    ///
    /// Only a local path qualifies: `workspace_contains` is answered by looking
    /// at this machine's file system, and a remote checkout is not on it. The
    /// repository root is preferred over the working directory so that moving
    /// between two directories of one repository does not re-evaluate anything.
    fn adopt_workspace_directory(&mut self, ctx: &mut ModelContext<Self>) {
        let directory = self
            .context
            .as_ref()
            .and_then(|context| context.repository_root.as_ref().or(context.cwd.as_ref()))
            .and_then(|path| path.to_local_path())
            .map(Path::to_path_buf);
        if directory == self.workspace_directory {
            return;
        }
        self.workspace_directory = directory;
        self.activate_eligible(ctx);
    }

    /// Follows the SSH sessions Warp holds, so a plugin hears that a target it
    /// is holding has gone before it tries to use it.
    ///
    /// These two events are about the transport, not about focus. A user
    /// moving to a local tab has disconnected nothing, and saying otherwise
    /// would make a plugin abandon a session that is still there; that move is
    /// `session.changed`, which is what the context diff emits.
    fn watch_remote_sessions(&mut self, ctx: &mut ModelContext<Self>) {
        if !ctx.has_singleton_model::<RemoteServerManager>() {
            return;
        }
        let remote = RemoteServerManager::handle(ctx);
        ctx.subscribe_to_model(&remote, |manager, _, event, ctx| {
            let (kind, session_id) = match event {
                RemoteServerManagerEvent::SessionConnected { session_id, .. } => {
                    (EventKind::SshConnected, *session_id)
                }
                RemoteServerManagerEvent::SessionDisconnected { session_id, .. } => {
                    (EventKind::SshDisconnected, *session_id)
                }
                _ => return,
            };
            manager.announce_session(kind, session_id, ctx);
        });
    }

    /// Tells every running extension that one SSH session came or went.
    ///
    /// The target is named in full rather than only by id, so a plugin can
    /// compare it against the one it is holding without reconstructing it.
    fn announce_session(
        &mut self,
        kind: EventKind,
        session_id: SessionId,
        ctx: &mut ModelContext<Self>,
    ) {
        let session_id = session_id_to_string(session_id);
        let params = SessionChangedParams {
            workspace_id: self.workspace_id(ctx),
            session_id: Some(session_id.clone()),
            execution_target: ExecutionTarget::Ssh { session_id },
        };
        match EventEnvelope::with_params(kind, &params) {
            Ok(envelope) => self.broadcast(&Message::Event(envelope)),
            Err(err) => log::warn!("Failed to encode {kind}: {err}"),
        }
    }

    /// Sends one message to every extension that has finished its handshake.
    ///
    /// A plugin that has not negotiated capabilities yet is skipped: it has
    /// not been told what this build supports, so it has no way to read an
    /// event against the version of the API it expected.
    fn broadcast(&mut self, message: &Message) {
        for (extension_id, extension) in &mut self.extensions {
            let Some(running) = extension.running.as_mut() else {
                continue;
            };
            if !running.session.is_initialized() {
                continue;
            }
            if let Err(err) = running.process.send(message) {
                log::warn!("Failed to deliver an event to extension {extension_id}: {err}");
            }
        }
    }

    /// Runs a command an extension asked for, on the target it named.
    ///
    /// The target is bound to its transport here, on the main thread, before
    /// anything is awaited. That is what invariant 31 costs: once the command
    /// is in flight there is nothing left to resolve, so a focus change cannot
    /// move it, and a session that ends underneath it fails rather than being
    /// retried somewhere else.
    pub(super) fn run_execution(
        &mut self,
        extension_id: &str,
        request_id: String,
        params: ExecutionRunParams,
        ctx: &mut ModelContext<Self>,
    ) {
        let runner = match ExecutionService::resolve(&params.target, ctx) {
            Ok(runner) => runner,
            Err(error) => {
                self.respond(extension_id, request_id, Err(error));
                return;
            }
        };
        let extension_id = extension_id.to_owned();
        ctx.spawn(
            async move { runner.run(params).await },
            move |manager, result, _| {
                let result = result.and_then(|result| {
                    serde_json::to_value(result).map_err(|err| {
                        ExtensionError::with_details(
                            ErrorCode::ExecutionFailed,
                            "failed to encode the command result",
                            err.to_string(),
                        )
                    })
                });
                manager.respond(&extension_id, request_id, result);
            },
        );
    }

    /// Writes one deferred answer back to the plugin waiting for it.
    ///
    /// An extension that stopped while its answer was being produced is not
    /// written to: there is no longer a request to answer, and the process the
    /// handle named is gone.
    fn respond(
        &mut self,
        extension_id: &str,
        request_id: String,
        result: Result<serde_json::Value, ExtensionError>,
    ) {
        let Some(running) = self
            .extensions
            .get_mut(extension_id)
            .and_then(|extension| extension.running.as_mut())
        else {
            return;
        };
        let response = match result {
            Ok(result) => ResponseEnvelope::ok(request_id, result),
            Err(error) => ResponseEnvelope::error(request_id, error),
        };
        if let Err(err) = running.process.send(&Message::Response(response)) {
            log::warn!("Failed to answer extension {extension_id}: {err}");
        }
    }

    fn start(&mut self, extension_id: &str, ctx: &mut ModelContext<Self>) {
        let Some(extension) = self.extensions.get_mut(extension_id) else {
            return;
        };
        if extension.running.is_some() || !extension.state.start() {
            return;
        }

        let mut process = match ExtensionProcess::spawn(
            extension_id,
            &extension.executable,
            &extension.directory,
            Some(&extension_log_path(extension_id)),
        ) {
            Ok(process) => process,
            Err(err) => {
                let detail = err.to_string();
                log::warn!("Failed to start extension {extension_id}: {detail}");
                self.stopped(extension_id, StopReason::Crashed { detail }, ctx);
                return;
            }
        };

        let Some(events) = process.take_events() else {
            return;
        };
        spawn_pump(extension_id.to_owned(), events, self.spawner.clone());
        extension.running = Some(RunningExtension {
            process,
            session: Session::new(extension.manifest.clone()),
            started_at: Instant::now(),
        });

        let id = extension_id.to_owned();
        ctx.spawn(
            async move {
                Timer::after(START_TIMEOUT).await;
            },
            move |manager, _, ctx| manager.enforce_start_timeout(&id, ctx),
        );
    }

    /// Stops a plugin that started but never finished the handshake.
    fn enforce_start_timeout(&mut self, extension_id: &str, ctx: &mut ModelContext<Self>) {
        let still_starting = self
            .extensions
            .get(extension_id)
            .and_then(|extension| extension.running.as_ref())
            .is_some_and(|running| {
                !running.session.is_initialized() && running.started_at.elapsed() >= START_TIMEOUT
            });
        if still_starting {
            self.stop(extension_id, StopReason::StartTimeout, ctx);
        }
    }

    /// Asks the plugin to exit, then applies the restart policy.
    pub fn stop(&mut self, extension_id: &str, reason: StopReason, ctx: &mut ModelContext<Self>) {
        let Some(extension) = self.extensions.get_mut(extension_id) else {
            return;
        };
        let Some(mut running) = extension.running.take() else {
            return;
        };
        extension.state.stopping();
        self.abandon_questions(extension_id);
        running.process.stop(STOP_TIMEOUT);
        // Dropping the process closes the event channel, which is what ends the
        // pump thread. The pump is deliberately not joined here: a stop is
        // usually decided while handling an event the pump is still blocked on,
        // so waiting for that thread from this one would deadlock the moment a
        // plugin exits.
        drop(running.process);
        self.stopped(extension_id, reason, ctx);
    }

    fn stopped(&mut self, extension_id: &str, reason: StopReason, ctx: &mut ModelContext<Self>) {
        self.contributions.unregister(extension_id);
        // The snapshots go with the panels they belonged to. Keeping them would
        // leave a panel showing a repository state nothing is maintaining any
        // more, which reads as current and is not.
        self.panel_states
            .retain(|(owner, _), _| owner != extension_id);
        ctx.emit(ExtensionManagerEvent::ContributionsChanged);

        let Some(extension) = self.extensions.get_mut(extension_id) else {
            return;
        };
        let name = extension.manifest.name.clone();
        match extension.state.stopped(reason, Instant::now()) {
            RestartDecision::StayInactive => {}
            RestartDecision::Restart => self.start(extension_id, ctx),
            RestartDecision::GiveUp { reason } => {
                ctx.emit(ExtensionManagerEvent::Failed {
                    extension_id: extension_id.to_owned(),
                    extension_name: name,
                    reason,
                });
            }
        }
    }

    /// Handles one thing the plugin said, on the main thread.
    fn on_process_event(
        &mut self,
        extension_id: &str,
        event: ProcessEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            ProcessEvent::Message(message) => {
                if self.handle_message(extension_id, *message, ctx) {
                    self.register_contributions(extension_id, ctx);
                }
            }
            ProcessEvent::Decode(error) => {
                // One unusable frame is not fatal: the codec resynchronises at
                // the next newline, and a plugin that only ever sends garbage
                // will fail the handshake anyway.
                log::warn!("Extension {extension_id} sent an unusable frame: {error}");
            }
            ProcessEvent::Stderr(line) => {
                log::info!("Extension {extension_id}: {line}");
            }
            ProcessEvent::Closed => {
                let detail = self.exit_detail(extension_id);
                self.stop(extension_id, StopReason::Crashed { detail }, ctx);
            }
        }
    }

    /// Returns true when this message completed the handshake.
    fn handle_message(
        &mut self,
        extension_id: &str,
        message: Message,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let (became_initialized, question, panel_state, execution) = {
            let Some(extension) = self.extensions.get_mut(extension_id) else {
                return false;
            };
            let Some(running) = extension.running.as_mut() else {
                return false;
            };
            let was_initialized = running.session.is_initialized();

            let mut host = BridgeHost {
                extension_id: extension_id.to_owned(),
                extension_name: extension.manifest.name.clone(),
                ctx,
                question: None,
                panel_state: None,
                execution: None,
            };
            let reply = running.session.handle_message(message, &mut host);
            let question = host.question;
            let panel_state = host.panel_state;
            let execution = host.execution;

            if let Some(reply) = reply
                && let Err(err) = running.process.send(&reply)
            {
                log::warn!("Failed to answer extension {extension_id}: {err}");
            }

            let became_initialized = !was_initialized && running.session.is_initialized();
            if became_initialized {
                extension.state.running();
            }
            (became_initialized, question, panel_state, execution)
        };

        // Applied out here because each touches the manager as a whole, and the
        // dispatch above held it borrowed for one extension.
        if let Some(state) = panel_state {
            self.set_panel_state(extension_id, state, ctx);
        }
        if let Some(question) = question {
            self.confirm(extension_id, question.request_id, question.params, ctx);
        }
        if let Some(execution) = execution {
            self.run_execution(extension_id, execution.request_id, execution.params, ctx);
        }
        became_initialized
    }

    /// Records the snapshot an extension published for one of its panels.
    ///
    /// The panel view reads this back at render time rather than being pushed
    /// its own copy, so there is exactly one record of what a panel shows and
    /// no way for the two to disagree.
    fn set_panel_state(
        &mut self,
        extension_id: &str,
        state: PanelViewState,
        ctx: &mut ModelContext<Self>,
    ) {
        let panel_id = state.panel_id.clone();
        self.panel_states
            .insert((extension_id.to_owned(), panel_id.clone()), state);
        ctx.emit(ExtensionManagerEvent::PanelStateChanged {
            extension_id: extension_id.to_owned(),
            panel_id,
        });
    }

    /// What a contributed panel is currently showing, if it has said yet.
    ///
    /// `None` covers both a panel whose extension has not published anything
    /// and one whose extension has stopped; the panel renders as loading in the
    /// first case and disappears in the second, so the two never look alike.
    pub fn panel_state(&self, extension_id: &str, panel_id: &str) -> Option<&PanelViewState> {
        self.panel_states
            .get(&(extension_id.to_owned(), panel_id.to_owned()))
    }

    /// Tells an extension that the user activated a row in one of its panels.
    ///
    /// Checked against the live contribution registry, exactly as an invoked
    /// command is: a row rendered from a snapshot that outlived its extension
    /// by a frame must not reach a plugin that is not there to answer, and a
    /// panel id the extension never declared must not reach it at all.
    pub fn invoke_panel_action(
        &mut self,
        extension_id: &str,
        panel_id: &str,
        section_id: &str,
        item_id: &str,
        action_id: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.contributions.panel(extension_id, panel_id).is_none() {
            return;
        }
        let origin = self.origin(ctx);
        let event = match EventEnvelope::with_params(
            EventKind::PanelAction,
            &PanelActionParams {
                panel_id: panel_id.to_owned(),
                section_id: section_id.to_owned(),
                item_id: item_id.to_owned(),
                action_id: action_id.to_owned(),
                origin,
            },
        ) {
            Ok(event) => Message::Event(event),
            Err(err) => {
                log::warn!("Failed to encode {action_id} for extension {extension_id}: {err}");
                return;
            }
        };
        let Some(running) = self
            .extensions
            .get_mut(extension_id)
            .and_then(|extension| extension.running.as_mut())
        else {
            return;
        };
        if let Err(err) = running.process.send(&event) {
            log::warn!("Failed to deliver {action_id} to extension {extension_id}: {err}");
        }
    }

    fn register_contributions(&mut self, extension_id: &str, ctx: &mut ModelContext<Self>) {
        let Some(extension) = self.extensions.get(extension_id) else {
            return;
        };
        self.contributions.register(&extension.manifest);
        ctx.emit(ExtensionManagerEvent::ContributionsChanged);
    }

    fn exit_detail(&mut self, extension_id: &str) -> String {
        let status = self
            .extensions
            .get_mut(extension_id)
            .and_then(|extension| extension.running.as_mut())
            .and_then(|running| running.process.try_exit_status());
        match status {
            Some(status) => format!("the plugin exited with {status}"),
            None => "the plugin closed its output stream".to_owned(),
        }
    }

    /// Stops every running extension. Called on shutdown, where a plugin that
    /// outlives Warp would be a stray process the user cannot see.
    pub fn stop_all(&mut self, ctx: &mut ModelContext<Self>) {
        let running: Vec<String> = self
            .extensions
            .iter()
            .filter(|(_, extension)| extension.running.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in running {
            self.stop(&id, StopReason::Requested, ctx);
        }
    }

    pub fn commands(&self) -> impl Iterator<Item = &ContributedCommand> {
        self.contributions.commands()
    }

    pub fn panels(&self) -> impl Iterator<Item = &ContributedPanel> {
        self.contributions.panels()
    }

    pub fn state(&self, extension_id: &str) -> Option<&ExtensionState> {
        Some(self.extensions.get(extension_id)?.state.state())
    }

    /// Extensions that could not be validated, with the reason to show.
    pub fn rejected(&self) -> &[(PathBuf, String)] {
        &self.rejected
    }
}

/// The permission prompt's wording.
///
/// A first install lists everything, because the user has agreed to nothing
/// yet. An upgrade lists only what is new: the whole point of asking a second
/// time is the difference, and re-reading the unchanged half is how a prompt
/// stops being read at all.
fn permission_question(
    extension_name: &str,
    requested: &PermissionSet,
    executables: &[String],
    added: &[Permission],
    added_executables: &[String],
) -> Question {
    let is_upgrade = !added.is_empty() || !added_executables.is_empty();
    let (title, lead, permissions, programs) = if is_upgrade {
        (
            format!("{extension_name} is asking for more access"),
            "This version additionally wants to:",
            added.to_vec(),
            added_executables.to_vec(),
        )
    } else {
        (
            format!("Allow {extension_name} to run?"),
            "This extension will be able to:",
            requested.iter().collect(),
            executables.to_vec(),
        )
    };

    let mut body = vec![lead.to_owned()];
    body.extend(
        permissions
            .iter()
            .map(|permission| format!("  • {}", permission.description())),
    );
    if !programs.is_empty() {
        // Named rather than summarised: `process.execute` says nothing on its
        // own, and the list is the whole of what limits it.
        body.push(format!(
            "  • Run only these programs: {}",
            programs.join(", ")
        ));
    }

    Question {
        title,
        body: body.join("\n"),
        confirm_label: "Allow".to_owned(),
        cancel_label: "Don't allow".to_owned(),
    }
}

/// A plugin's own confirmation, worded by the plugin but rendered by Warp.
///
/// The extension is named in the title because a question a user did not
/// expect has to say who is asking, and a destructive one says so in the
/// body: Warp owns this presentation precisely so a plugin cannot make an
/// irreversible action look routine.
fn confirm_question(extension_name: &str, params: DialogConfirmParams) -> Question {
    let mut body = Vec::new();
    if let Some(text) = params.body {
        body.push(text);
    }
    if params.destructive {
        body.push("This cannot be undone.".to_owned());
    }
    Question {
        title: format!("{extension_name}: {}", params.title),
        body: body.join("\n"),
        confirm_label: params.confirm_label,
        cancel_label: params.cancel_label,
    }
}

/// Whether an extension's activation condition holds.
///
/// No `[activation]` table means "always", which is what makes a plugin that
/// only contributes a command work without ceremony. A `workspace_contains`
/// condition needs a directory to test, and until Warp supplies one the honest
/// answer is no — starting the plugin anyway would activate it in a workspace
/// nobody has checked.
fn should_activate(activation: Option<&Activation>, workspace: Option<&Path>) -> bool {
    let Some(activation) = activation else {
        return true;
    };
    if activation.workspace_contains.is_empty() {
        return true;
    }
    let Some(workspace) = workspace else {
        return false;
    };
    workspace.ancestors().any(|directory| {
        activation
            .workspace_contains
            .iter()
            .any(|entry| directory.join(entry).exists())
    })
}

/// Moves everything the plugin says onto the main thread.
///
/// The read is blocking, so it cannot happen on the main thread; the handling
/// must happen there, because it touches models. The thread ends when the
/// process is dropped and the channel closes.
fn spawn_pump(
    extension_id: String,
    events: Receiver<ProcessEvent>,
    spawner: ModelSpawner<ExtensionManager>,
) {
    let extension_id_for_error = extension_id.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("warp-extension-{extension_id}-pump"))
        .spawn(move || {
            for event in events {
                let id = extension_id.clone();
                let delivered = warpui::r#async::block_on(spawner.spawn(move |manager, ctx| {
                    manager.on_process_event(&id, event, ctx);
                }));
                if delivered.is_err() {
                    // The manager is gone, so there is nothing left to tell.
                    return;
                }
            }
        });
    if let Err(err) = spawned {
        log::warn!("Failed to start the reader for extension {extension_id_for_error}: {err}");
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
