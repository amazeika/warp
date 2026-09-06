//! Drives the real `warp-git` binary against a real Git repository.
//!
//! The parsers are the part of this plugin most likely to be wrong, and they
//! are wrong in ways that only show up against output Git actually produced.
//! This test therefore executes `git` for real rather than replaying fixtures.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use command::blocking::Command;
use extension_host::{
    ExtensionHost, ExtensionProcess, ExtensionRecord, ProcessEvent, Session, discover_in,
};
use extension_protocol::{
    ActionOrigin, Capability, CommandInvokedParams, EventEnvelope, EventKind, ExecutionRunParams,
    ExecutionRunResult, ExecutionTarget, ExtensionError, ExtensionManifest, Message, Method,
    NotificationParams, PanelActionParams, PanelSection, PanelStatus, PanelViewState,
    WorkspaceContext,
};
use instant::Instant;
use serde_json::json;
use tempfile::TempDir;

const PUMP_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Runs Git with the ambient environment neutralised.
///
/// A developer's `~/.gitconfig` can rename the default branch, enable signing,
/// or install hooks — any of which would make this test pass or fail for
/// reasons that have nothing to do with the plugin.
fn git(repo: &Path, args: &[&str]) -> ExecutionRunResult {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .args(args)
        .output()
        .expect("git runs");
    ExecutionRunResult {
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        duration_ms: 0,
        truncated: false,
    }
}

fn git_ok(repo: &Path, args: &[&str]) -> String {
    let result = git(repo, args);
    assert_eq!(
        result.exit_code,
        Some(0),
        "git {args:?} failed: {}",
        result.stderr
    );
    result.stdout
}

/// A repository with one of everything the panel has a section for.
fn repository() -> TempDir {
    let repo = TempDir::new().expect("temp repo");
    let path = repo.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);

    std::fs::write(path.join("kept.txt"), "one\n").expect("write");
    std::fs::write(path.join("edited.txt"), "before\n").expect("write");
    git_ok(path, &["add", "--all"]);
    git_ok(path, &["commit", "--quiet", "--message", "Initial commit"]);

    git_ok(path, &["branch", "feature/spike"]);
    std::fs::write(path.join("edited.txt"), "after\n").expect("write");
    std::fs::write(path.join("added.txt"), "new\n").expect("write");
    git_ok(path, &["add", "added.txt"]);
    std::fs::write(path.join("untracked file.txt"), "loose\n").expect("write");
    repo
}

fn install_plugin() -> (TempDir, PathBuf, ExtensionManifest) {
    let root = TempDir::new().expect("temp extensions root");
    let directory = root.path().join("warp-git");
    std::fs::create_dir_all(&directory).expect("extension directory");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("extension.toml"),
        directory.join("extension.toml"),
    )
    .expect("manifest is installed");

    let installed = directory.join("warp-git");
    std::fs::copy(env!("CARGO_BIN_EXE_warp-git"), &installed).expect("binary is installed");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755))
            .expect("binary is executable");
    }

    let discovered = discover_in(root.path());
    assert_eq!(discovered.len(), 1);
    let ExtensionRecord::Valid { manifest, .. } = &discovered[0].record else {
        panic!("the shipped manifest must be valid");
    };
    (root, directory, (**manifest).clone())
}

#[derive(Default)]
struct Recorded {
    panels: Vec<PanelViewState>,
    notifications: Vec<NotificationParams>,
    opened_diffs: Vec<serde_json::Value>,
    executed: Vec<Vec<String>>,
}

/// Stands in for Warp: executes Git for real, and answers dialogs from a script
/// so a flow that asks the user something still runs unattended.
struct GitHost {
    repo: PathBuf,
    recorded: Arc<Mutex<Recorded>>,
    dialog_answers: Arc<Mutex<VecDeque<serde_json::Value>>>,
}

impl ExtensionHost for GitHost {
    fn capabilities(&self) -> Vec<Capability> {
        Capability::ALL.to_vec()
    }

    fn dispatch(
        &mut self,
        method: Method,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ExtensionError> {
        match method {
            Method::WorkspaceGetContext => Ok(serde_json::to_value(WorkspaceContext {
                workspace_id: "ws-1".to_owned(),
                cwd: self.repo.display().to_string(),
                repository_root: Some(self.repo.display().to_string()),
                active_pane_id: Some("pane-1".to_owned()),
                session_id: None,
                execution_target: ExecutionTarget::Local,
            })
            .expect("context encodes")),
            Method::ExecutionRun => {
                let params: ExecutionRunParams =
                    serde_json::from_value(params).expect("run params decode");
                self.recorded
                    .lock()
                    .expect("recorder")
                    .executed
                    .push(params.args.clone());
                let args: Vec<&str> = params.args.iter().map(String::as_str).collect();
                Ok(serde_json::to_value(git(&self.repo, &args)).expect("result encodes"))
            }
            Method::PanelSetState => {
                self.recorded
                    .lock()
                    .expect("recorder")
                    .panels
                    .push(serde_json::from_value(params).expect("panel state decodes"));
                Ok(json!({}))
            }
            Method::NotificationShow => {
                self.recorded
                    .lock()
                    .expect("recorder")
                    .notifications
                    .push(serde_json::from_value(params).expect("notification decodes"));
                Ok(json!({}))
            }
            Method::DiffOpenFile | Method::DiffOpenWorkingTree => {
                self.recorded
                    .lock()
                    .expect("recorder")
                    .opened_diffs
                    .push(params);
                Ok(json!({}))
            }
            Method::DialogConfirm | Method::DialogInput | Method::DialogSelect => Ok(self
                .dialog_answers
                .lock()
                .expect("answers")
                .pop_front()
                .unwrap_or_else(|| json!({ "confirmed": false }))),
            Method::FileOpen | Method::ExtensionInitialize => Ok(json!({})),
        }
    }
}

struct Harness {
    process: ExtensionProcess,
    session: Session<GitHost>,
    recorded: Arc<Mutex<Recorded>>,
    dialog_answers: Arc<Mutex<VecDeque<serde_json::Value>>>,
    repo: PathBuf,
    _extensions: TempDir,
    _repo: TempDir,
}

impl Harness {
    fn start() -> Self {
        let repo = repository();
        let (extensions, directory, manifest) = install_plugin();
        let executable = directory.join("warp-git");

        let process = ExtensionProcess::spawn("dev.warp.git", &executable, &directory, None)
            .expect("the plugin starts");
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let dialog_answers = Arc::new(Mutex::new(VecDeque::new()));
        let session = Session::new(
            manifest,
            GitHost {
                repo: repo.path().to_path_buf(),
                recorded: Arc::clone(&recorded),
                dialog_answers: Arc::clone(&dialog_answers),
            },
        );
        Self {
            process,
            session,
            recorded,
            dialog_answers,
            repo: repo.path().to_path_buf(),
            _extensions: extensions,
            _repo: repo,
        }
    }

    fn answer(&self, response: serde_json::Value) {
        self.dialog_answers
            .lock()
            .expect("answers")
            .push_back(response);
    }

    /// The most recent panel with content, skipping the loading frame the
    /// plugin publishes before each refresh.
    fn latest_ready_panel(&self) -> Option<PanelViewState> {
        self.recorded
            .lock()
            .expect("recorder")
            .panels
            .iter()
            .rev()
            .find(|panel| !matches!(panel.status, PanelStatus::Loading))
            .cloned()
    }

    fn executed(&self) -> Vec<Vec<String>> {
        self.recorded.lock().expect("recorder").executed.clone()
    }

    fn pump_until(&mut self, description: &str, done: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + PUMP_TIMEOUT;
        while Instant::now() < deadline {
            if done(self) {
                return;
            }
            match self.process.recv_timeout(POLL_INTERVAL) {
                Ok(ProcessEvent::Message(message)) => {
                    if let Some(reply) = self.session.handle_message(*message) {
                        self.process.send(&reply).expect("the reply is written");
                    }
                }
                Ok(ProcessEvent::Decode(error)) => panic!("the plugin sent a bad frame: {error}"),
                Ok(ProcessEvent::Stderr(_)) => continue,
                Ok(ProcessEvent::Closed) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(done(self), "timed out waiting for {description}");
    }

    /// Services the plugin until it goes quiet.
    ///
    /// Waiting for another panel would not work here: a flow the user cancels
    /// correctly does nothing at all, so quiescence is the only observable
    /// signal that the plugin finished handling the event.
    fn settle(&mut self, description: &str) {
        const QUIET: Duration = Duration::from_millis(500);
        let deadline = Instant::now() + PUMP_TIMEOUT;
        let mut last_activity = Instant::now();
        let mut saw_activity = false;

        while Instant::now() < deadline {
            match self.process.recv_timeout(POLL_INTERVAL) {
                Ok(ProcessEvent::Message(message)) => {
                    saw_activity = true;
                    last_activity = Instant::now();
                    if let Some(reply) = self.session.handle_message(*message) {
                        self.process.send(&reply).expect("the reply is written");
                    }
                }
                Ok(ProcessEvent::Decode(error)) => panic!("the plugin sent a bad frame: {error}"),
                Ok(ProcessEvent::Stderr(_)) => continue,
                Ok(ProcessEvent::Closed) => return,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if saw_activity && Instant::now().duration_since(last_activity) >= QUIET {
                        return;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        panic!("timed out waiting for {description}");
    }

    fn send_panel_action(&mut self, section: &str, item: &str, action: &str) {
        let event = EventEnvelope::with_params(
            EventKind::PanelAction,
            &PanelActionParams {
                panel_id: "git".to_owned(),
                section_id: section.to_owned(),
                item_id: item.to_owned(),
                action_id: action.to_owned(),
                origin: self.origin(),
            },
        )
        .expect("event encodes");
        self.process
            .send(&Message::Event(event))
            .expect("the event is written");
    }

    fn send_command(&mut self, command_id: &str) {
        let event = EventEnvelope::with_params(
            EventKind::CommandInvoked,
            &CommandInvokedParams {
                command_id: command_id.to_owned(),
                origin: self.origin(),
            },
        )
        .expect("event encodes");
        self.process
            .send(&Message::Event(event))
            .expect("the event is written");
    }

    fn origin(&self) -> ActionOrigin {
        ActionOrigin {
            workspace_id: "ws-1".to_owned(),
            session_id: None,
            repository_root: Some(self.repo.display().to_string()),
            execution_target: ExecutionTarget::Local,
        }
    }
}

fn started() -> Harness {
    let mut harness = Harness::start();
    harness.pump_until("the first panel", |harness| {
        harness
            .latest_ready_panel()
            .is_some_and(|panel| !panel.sections.is_empty())
    });
    harness
}

fn section<'a>(panel: &'a PanelViewState, id: &str) -> Option<&'a PanelSection> {
    panel.sections.iter().find(|section| section.id == id)
}

fn item_ids(panel: &PanelViewState, section_id: &str) -> Vec<String> {
    section(panel, section_id)
        .map(|section| section.items.iter().map(|item| item.id.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn the_panel_reflects_a_real_repository() {
    let harness = started();
    let panel = harness.latest_ready_panel().expect("a panel was published");

    assert_eq!(panel.panel_id, "git");
    assert_eq!(panel.status, PanelStatus::Ready);
    assert_eq!(item_ids(&panel, "git.staged"), ["added.txt"]);
    assert_eq!(item_ids(&panel, "git.unstaged"), ["edited.txt"]);
    assert_eq!(
        item_ids(&panel, "git.untracked"),
        ["untracked file.txt"],
        "a path containing a space must survive the round trip through Git"
    );

    let mut branches = item_ids(&panel, "git.branches");
    branches.sort();
    assert_eq!(branches, ["feature/spike", "main"]);

    let summary = section(&panel, "git.summary").expect("a summary is shown");
    assert!(
        summary.items[0].label.starts_with("main"),
        "the summary names the checked-out branch: {}",
        summary.items[0].label
    );
    assert_eq!(
        section(&panel, "git.history")
            .expect("history is shown")
            .items[0]
            .label,
        "Initial commit"
    );
}

#[test]
fn staging_from_the_panel_changes_the_real_index() {
    let mut harness = started();
    harness.send_panel_action("git.unstaged", "edited.txt", "stage");
    harness.pump_until("the panel to show the file as staged", |harness| {
        harness
            .latest_ready_panel()
            .is_some_and(|panel| item_ids(&panel, "git.staged").contains(&"edited.txt".to_owned()))
    });

    let staged = git_ok(&harness.repo, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.contains("edited.txt"),
        "the file must actually be staged in Git, not only in the panel: {staged}"
    );
    let panel = harness.latest_ready_panel().expect("a panel");
    assert!(!item_ids(&panel, "git.unstaged").contains(&"edited.txt".to_owned()));
}

#[test]
fn a_diff_request_names_the_comparison_for_the_section_it_came_from() {
    let mut harness = started();
    harness.send_panel_action("git.staged", "added.txt", "open_diff");
    harness.pump_until("a diff to be opened", |harness| {
        !harness
            .recorded
            .lock()
            .expect("recorder")
            .opened_diffs
            .is_empty()
    });

    let diffs = harness
        .recorded
        .lock()
        .expect("recorder")
        .opened_diffs
        .clone();
    assert_eq!(diffs[0]["path"], json!("added.txt"));
    assert_eq!(
        diffs[0]["comparison"],
        json!("head_to_index"),
        "a staged row compares the index against HEAD, not the worktree"
    );
    assert_eq!(diffs[0]["target"], json!({ "kind": "local" }));
}

#[test]
fn committing_writes_a_real_commit_with_the_message_the_user_typed() {
    let mut harness = started();
    // A message with a quote and spaces proves it crosses as one argv entry.
    harness.answer(json!({ "value": "Add \"added\" file" }));
    harness.send_command("git.commit");
    harness.pump_until("the commit to land", |harness| {
        git(&harness.repo, &["log", "--oneline"])
            .stdout
            .contains("Add \"added\" file")
    });

    assert_eq!(
        git_ok(&harness.repo, &["log", "--format=%s", "--max-count=1"]).trim(),
        "Add \"added\" file"
    );
}

#[test]
fn a_dismissed_commit_dialog_leaves_the_repository_untouched() {
    let mut harness = started();
    let before = git_ok(&harness.repo, &["rev-parse", "HEAD"]);

    harness.answer(json!({}));
    harness.send_command("git.commit");
    harness.settle("the plugin to handle the dismissal");

    assert_eq!(
        git_ok(&harness.repo, &["rev-parse", "HEAD"]),
        before,
        "dismissing the dialog must not commit"
    );
    assert!(
        !harness
            .executed()
            .iter()
            .any(|argv| argv.first().is_some_and(|arg| arg == "commit")),
        "no commit should have been attempted"
    );
}

#[test]
fn discarding_asks_first_and_only_then_restores_the_file() {
    let mut harness = started();

    harness.answer(json!({ "confirmed": false }));
    harness.send_panel_action("git.unstaged", "edited.txt", "discard");
    harness.settle("the refusal to be handled");
    assert_eq!(
        std::fs::read_to_string(harness.repo.join("edited.txt")).expect("read"),
        "after\n",
        "declining the confirmation must leave the file alone"
    );
    assert!(
        !harness
            .executed()
            .iter()
            .any(|argv| argv.first().is_some_and(|arg| arg == "checkout")),
        "nothing destructive runs before the user agrees"
    );

    harness.answer(json!({ "confirmed": true }));
    harness.send_panel_action("git.unstaged", "edited.txt", "discard");
    harness.pump_until("the discard to take effect", |harness| {
        std::fs::read_to_string(harness.repo.join("edited.txt"))
            .is_ok_and(|contents| contents == "before\n")
    });
}

#[test]
fn creating_and_switching_branches_moves_head_for_real() {
    let mut harness = started();

    harness.answer(json!({ "value": "feature/from-plugin" }));
    harness.send_command("git.createBranch");
    harness.pump_until("the branch to be created", |harness| {
        git(&harness.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
            .stdout
            .trim()
            == "feature/from-plugin"
    });

    harness.answer(json!({ "selected_id": "main" }));
    harness.send_command("git.switchBranch");
    harness.pump_until("the switch to take effect", |harness| {
        git(&harness.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
            .stdout
            .trim()
            == "main"
    });
}

#[test]
fn deleting_an_unmerged_branch_needs_a_second_confirmation() {
    let mut harness = started();

    // Put a commit on the branch that exists nowhere else, so `branch -d` must
    // refuse and the plugin has to escalate.
    git_ok(&harness.repo, &["switch", "--quiet", "feature/spike"]);
    std::fs::write(harness.repo.join("only-here.txt"), "x\n").expect("write");
    git_ok(&harness.repo, &["add", "only-here.txt"]);
    git_ok(
        &harness.repo,
        &["commit", "--quiet", "--message", "Only on the branch"],
    );
    git_ok(&harness.repo, &["switch", "--quiet", "main"]);

    harness.answer(json!({ "confirmed": true }));
    harness.answer(json!({ "confirmed": false }));
    harness.send_panel_action("git.branches", "feature/spike", "delete_branch");
    harness.pump_until("the safe delete to be attempted", |harness| {
        harness
            .executed()
            .iter()
            .any(|argv| argv.contains(&"--delete".to_owned()))
    });
    harness.settle("the escalation to be declined");

    let branches = git_ok(&harness.repo, &["branch", "--list"]);
    assert!(
        branches.contains("feature/spike"),
        "declining the force delete must keep the branch: {branches}"
    );
    assert!(
        !harness
            .executed()
            .iter()
            .any(|argv| argv.contains(&"--force".to_owned())),
        "a force delete must never run without its own confirmation"
    );
}

#[test]
fn every_command_the_plugin_runs_is_git_with_an_argv() {
    let mut harness = started();
    harness.answer(json!({ "value": "Commit from the test" }));
    harness.send_command("git.commit");
    harness.pump_until("the commit to land", |harness| {
        git(&harness.repo, &["log", "--oneline"])
            .stdout
            .contains("Commit from the test")
    });

    for argv in harness.executed() {
        for arg in &argv {
            assert!(
                !matches!(arg.as_str(), "-c" | "sh" | "bash" | "zsh"),
                "`{arg}` in {argv:?} would introduce a shell"
            );
        }
    }
}

#[test]
fn a_workspace_without_a_repository_is_reported_as_empty() {
    let cwd = TempDir::new().expect("temp dir");
    let (_extensions, directory, manifest) = install_plugin();

    let mut process = ExtensionProcess::spawn(
        "dev.warp.git",
        &directory.join("warp-git"),
        &directory,
        None,
    )
    .expect("the plugin starts");
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let mut session = Session::new(
        manifest,
        NoRepositoryHost {
            cwd: cwd.path().to_path_buf(),
            recorded: Arc::clone(&recorded),
        },
    );

    let deadline = Instant::now() + PUMP_TIMEOUT;
    while Instant::now() < deadline {
        let reported_empty = recorded
            .lock()
            .expect("recorder")
            .panels
            .iter()
            .any(|panel| matches!(panel.status, PanelStatus::Empty { .. }));
        if reported_empty {
            return;
        }
        match process.recv_timeout(POLL_INTERVAL) {
            Ok(ProcessEvent::Message(message)) => {
                if let Some(reply) = session.handle_message(*message) {
                    process.send(&reply).expect("reply is written");
                }
            }
            Ok(ProcessEvent::Decode(error)) => panic!("the plugin sent a bad frame: {error}"),
            Ok(ProcessEvent::Stderr(_)) => continue,
            Ok(ProcessEvent::Closed) => break,
            // A poll that found nothing is not the plugin going away.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("the plugin never reported an empty panel");
}

/// A host whose workspace holds no repository at all.
struct NoRepositoryHost {
    cwd: PathBuf,
    recorded: Arc<Mutex<Recorded>>,
}

impl ExtensionHost for NoRepositoryHost {
    fn capabilities(&self) -> Vec<Capability> {
        Capability::ALL.to_vec()
    }

    fn dispatch(
        &mut self,
        method: Method,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ExtensionError> {
        match method {
            Method::WorkspaceGetContext => Ok(serde_json::to_value(WorkspaceContext {
                workspace_id: "ws-1".to_owned(),
                cwd: self.cwd.display().to_string(),
                repository_root: None,
                active_pane_id: None,
                session_id: None,
                execution_target: ExecutionTarget::Local,
            })
            .expect("context encodes")),
            Method::PanelSetState => {
                self.recorded
                    .lock()
                    .expect("recorder")
                    .panels
                    .push(serde_json::from_value(params).expect("panel decodes"));
                Ok(json!({}))
            }
            _ => Ok(json!({})),
        }
    }
}
