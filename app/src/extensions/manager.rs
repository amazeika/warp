//! Discovery, permission gating, supervision and dispatch for installed extensions.
//!
//! One `ExtensionManager` owns every extension Warp knows about. It lives on
//! the main thread and holds each plugin's process handle and session, so all
//! state transitions happen in one place with no locking; the only work done
//! elsewhere is the blocking read of each plugin's stdout.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use extension_host::discovery::{DiscoveredExtension, ExtensionRecord};
use extension_host::{
    ExtensionProcess, ExtensionState, ProcessEvent, RestartDecision, Session, StateMachine,
    StopReason, extension_log_path, extensions_root,
};
use extension_protocol::{Activation, ExtensionManifest, Permission, PermissionSet};
use instant::Instant;
use warpui::r#async::Timer;
use warpui::{Entity, ModelContext, ModelSpawner, SingletonEntity};

use super::contributions::{ContributedCommand, ContributedPanel, Contributions};
use super::host::BridgeHost;
use super::permissions::{GrantStore, PermissionDecision};

/// How long a plugin has to answer `extension.initialize`.
///
/// A plugin that never completes the handshake is indistinguishable from one
/// that hung, and the difference does not matter: neither can serve a request.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a plugin is given to exit after being asked to.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// What the UI needs to know about the extensions Warp is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionManagerEvent {
    /// A command or panel appeared or disappeared.
    ContributionsChanged,
    /// The extension cannot start until the user answers.
    PermissionRequired {
        extension_id: String,
        extension_name: String,
        requested: PermissionSet,
        /// Empty on a first install; on an upgrade, only what is newly asked
        /// for — that is the part of a second prompt worth reading.
        added: Vec<Permission>,
    },
    /// Warp has given up restarting this extension.
    Failed {
        extension_id: String,
        extension_name: String,
        reason: String,
    },
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
    extensions: BTreeMap<String, ManagedExtension>,
    /// Directories that claimed to be extensions but cannot run, kept with
    /// their reason so the UI can say why rather than silently omitting them.
    rejected: Vec<(PathBuf, String)>,
    spawner: ModelSpawner<Self>,
    /// The directory activation conditions are evaluated against. `None` until
    /// workspace context exists, which keeps a `workspace_contains` extension
    /// inactive rather than guessing.
    workspace_directory: Option<PathBuf>,
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
            extensions: BTreeMap::new(),
            rejected: Vec::new(),
            spawner: ctx.spawner(),
            workspace_directory: None,
        };
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
        let Some(extension) = self.extensions.get(extension_id) else {
            return;
        };
        match self.grants.decide(&extension.manifest) {
            PermissionDecision::Granted => self.start(extension_id, ctx),
            PermissionDecision::Prompt { requested, added } => {
                ctx.emit(ExtensionManagerEvent::PermissionRequired {
                    extension_id: extension_id.to_owned(),
                    extension_name: extension.manifest.name.clone(),
                    requested,
                    added,
                });
            }
        }
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
        self.start(extension_id, ctx);
    }

    /// Forgets any grant and stops the extension if it is running.
    pub fn revoke(&mut self, extension_id: &str, ctx: &mut ModelContext<Self>) {
        if let Err(err) = self.grants.revoke(extension_id) {
            log::warn!("Failed to revoke the grant for {extension_id}: {err}");
        }
        self.stop(extension_id, StopReason::Requested, ctx);
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
        message: extension_protocol::Message,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(extension) = self.extensions.get_mut(extension_id) else {
            return false;
        };
        let Some(running) = extension.running.as_mut() else {
            return false;
        };
        let was_initialized = running.session.is_initialized();

        let mut host = BridgeHost {
            extension_name: extension.manifest.name.clone(),
            ctx,
        };
        let reply = running.session.handle_message(message, &mut host);

        if let Some(reply) = reply
            && let Err(err) = running.process.send(&reply)
        {
            log::warn!("Failed to answer extension {extension_id}: {err}");
        }

        let became_initialized = !was_initialized && running.session.is_initialized();
        if became_initialized {
            extension.state.running();
        }
        became_initialized
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
