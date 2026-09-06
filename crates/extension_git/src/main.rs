//! `warp-git`: day-to-day Git inside Warp, as an extension rather than as part
//! of Warp itself.
//!
//! Every Git command runs through Warp's `execution.run`, so the plugin behaves
//! identically against a local repository and one reached through an SSH
//! session — it never opens a connection of its own, and it never assumes the
//! repository is on the machine it is running on.
//!
//! The installed `git` binary does the work. Reimplementing Git's primitives
//! would mean reimplementing `.gitconfig`, credential helpers, commit signing
//! and hooks along with them, and diverging from what the user's own terminal
//! does in the same repository.
//!
//! stdout is the protocol transport. Diagnostics go to stderr.
mod git;
mod panel;
mod repository;
mod state;

use extension_protocol::{
    CommandInvokedParams, DiffComparison, EventKind, ExecutionTarget, NotificationLevel,
    PanelActionParams, WorkspaceContext,
};
use extension_sdk::{Client, ClientError};

use crate::git::operations;
use crate::repository::{GitError, Repository};
use crate::state::RepositoryState;

const EXTENSION_ID: &str = "dev.warp.git";

type Stdio = Client<std::io::BufReader<std::io::Stdin>, std::io::Stdout>;

fn main() {
    if let Err(error) = run() {
        eprintln!("warp-git: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), ClientError> {
    let mut client = Client::from_stdio();
    let handshake = client.initialize(EXTENSION_ID)?;
    eprintln!(
        "warp-git ready: protocol {} api {}",
        handshake.protocol, handshake.api_version
    );

    let mut plugin = Plugin::default();
    plugin.refresh(&mut client)?;

    loop {
        let Some(event) = client.next_event()? else {
            return Ok(());
        };
        match event.event {
            EventKind::ExtensionShutdown => return Ok(()),
            EventKind::WorkspaceChanged
            | EventKind::CwdChanged
            | EventKind::RepositoryChanged
            | EventKind::SessionChanged
            | EventKind::SshConnected => plugin.refresh(&mut client)?,
            EventKind::SshDisconnected => {
                // The repository lived on the far side of that session, so the
                // last state read is no longer about anything reachable.
                plugin.context = None;
                client.set_panel_state(&panel::no_repository())?;
            }
            EventKind::PanelAction => {
                let params: PanelActionParams = serde_json::from_value(event.params)
                    .map_err(|err| ClientError::Transport(err.to_string()))?;
                plugin.on_panel_action(&mut client, params)?;
            }
            EventKind::CommandInvoked => {
                let params: CommandInvokedParams = serde_json::from_value(event.params)
                    .map_err(|err| ClientError::Transport(err.to_string()))?;
                plugin.on_command(&mut client, params)?;
            }
        }
    }
}

#[derive(Default)]
struct Plugin {
    context: Option<WorkspaceContext>,
    state: Option<RepositoryState>,
}

impl Plugin {
    /// Re-reads the repository and republishes the panel.
    ///
    /// Called on every context event rather than polled, so an idle repository
    /// costs nothing.
    fn refresh(&mut self, client: &mut Stdio) -> Result<(), ClientError> {
        client.set_panel_state(&panel::loading())?;

        let context = client.workspace_context()?;
        let Some(root) = context.repository_root.clone() else {
            self.context = Some(context);
            self.state = None;
            return client.set_panel_state(&panel::no_repository());
        };

        let target = context.execution_target.clone();
        self.context = Some(context);

        let mut repository = Repository::new(client, target, root);
        match repository.read_state() {
            Ok(state) => {
                let view = panel::render(&state);
                self.state = Some(state);
                client.set_panel_state(&view)
            }
            Err(error) => {
                eprintln!("warp-git: refresh failed: {error}");
                self.state = None;
                client.set_panel_state(&panel::error(error.summary()))
            }
        }
    }

    /// Opens a repository bound to the origin the action came from.
    ///
    /// The action's own origin is used rather than the plugin's current context,
    /// so an action stays attached to the repository it started in even if the
    /// user moved on while a dialog was open.
    fn repository_for<'a>(
        &self,
        client: &'a mut Stdio,
        target: &ExecutionTarget,
        repository_root: Option<&str>,
    ) -> Option<Repository<'a, std::io::BufReader<std::io::Stdin>, std::io::Stdout>> {
        let root = repository_root
            .map(str::to_owned)
            .or_else(|| self.state.as_ref().map(|state| state.root.clone()))?;
        Some(Repository::new(client, target.clone(), root))
    }

    fn on_panel_action(
        &mut self,
        client: &mut Stdio,
        params: PanelActionParams,
    ) -> Result<(), ClientError> {
        let target = params.origin.execution_target.clone();
        let root = params.origin.repository_root.clone();
        let item = params.item_id.clone();

        let outcome = match params.action_id.as_str() {
            panel::ACTION_OPEN_DIFF => {
                let Some(root) = root.clone() else {
                    return Ok(());
                };
                let comparison = comparison_for_section(&params.section_id);
                client.open_file_diff(&target, &root, &item, comparison)?;
                return Ok(());
            }
            panel::ACTION_OPEN_FILE => {
                let Some(root) = root.clone() else {
                    return Ok(());
                };
                client.open_file(&target, &format!("{root}/{item}"), None)?;
                return Ok(());
            }
            panel::ACTION_OPEN_COMMIT => {
                // Opening a specific commit needs a comparison the diff API
                // does not carry yet, so the working-tree diff is opened and
                // the user is told which commit they asked for.
                let Some(root) = root.clone() else {
                    return Ok(());
                };
                client.open_working_tree_diff(&target, &root)?;
                client.notify(
                    "Commit diffs are not available yet",
                    Some(format!(
                        "Opened the working tree instead. Viewing {} needs diff.openCommit.",
                        &item[..item.len().min(8)]
                    )),
                    NotificationLevel::Info,
                )?;
                return Ok(());
            }
            panel::ACTION_STAGE => {
                self.run_action(client, &target, root.as_deref(), operations::stage(&[item]))
            }
            panel::ACTION_UNSTAGE => self.run_action(
                client,
                &target,
                root.as_deref(),
                operations::unstage(&[item]),
            ),
            panel::ACTION_DISCARD => {
                let confirmed = client.confirm(
                    &format!("Discard changes to {item}?"),
                    Some("This cannot be undone.".to_owned()),
                    "Discard",
                    true,
                )?;
                if !confirmed {
                    return Ok(());
                }
                self.run_action(
                    client,
                    &target,
                    root.as_deref(),
                    operations::discard(&[item]),
                )
            }
            panel::ACTION_DELETE_FILE => {
                let confirmed = client.confirm(
                    &format!("Delete {item}?"),
                    Some(
                        "This untracked file is not in Git, so it cannot be recovered.".to_owned(),
                    ),
                    "Delete",
                    true,
                )?;
                if !confirmed {
                    return Ok(());
                }
                self.run_action(
                    client,
                    &target,
                    root.as_deref(),
                    operations::delete_untracked(&[item]),
                )
            }
            panel::ACTION_SWITCH_BRANCH => self.run_action(
                client,
                &target,
                root.as_deref(),
                operations::switch_branch(&item),
            ),
            panel::ACTION_CHECKOUT_REMOTE => {
                let local = item.split_once('/').map_or(item.as_str(), |(_, rest)| rest);
                let argv = operations::switch_to_remote_branch(local, &item);
                self.run_action(client, &target, root.as_deref(), argv)
            }
            panel::ACTION_RENAME_BRANCH => {
                let Some(new_name) = client.input(
                    &format!("Rename {item}"),
                    None,
                    Some("new branch name".to_owned()),
                )?
                else {
                    return Ok(());
                };
                let argv = operations::rename_branch(&item, new_name.trim());
                self.run_action(client, &target, root.as_deref(), argv)
            }
            panel::ACTION_DELETE_BRANCH => {
                return self.delete_branch(client, &target, root.as_deref(), &item);
            }
            _ => return Ok(()),
        };

        self.report(client, outcome)?;
        self.refresh(client)
    }

    fn on_command(
        &mut self,
        client: &mut Stdio,
        params: CommandInvokedParams,
    ) -> Result<(), ClientError> {
        let target = params.origin.execution_target.clone();
        let root = params.origin.repository_root.clone();

        let outcome = match params.command_id.as_str() {
            "git.refresh" => return self.refresh(client),
            "git.stageAll" => {
                self.run_action(client, &target, root.as_deref(), operations::stage_all())
            }
            "git.unstageAll" => {
                self.run_action(client, &target, root.as_deref(), operations::unstage_all())
            }
            "git.commit" => return self.commit(client, &target, root.as_deref()),
            "git.fetch" => self.run_action(client, &target, root.as_deref(), operations::fetch()),
            "git.pull" => self.run_action(client, &target, root.as_deref(), operations::pull()),
            "git.push" => return self.push(client, &target, root.as_deref()),
            "git.forcePush" => return self.force_push(client, &target, root.as_deref()),
            "git.createBranch" => {
                let Some(name) =
                    client.input("Create branch", None, Some("branch name".to_owned()))?
                else {
                    return Ok(());
                };
                let argv = operations::create_branch(name.trim());
                self.run_action(client, &target, root.as_deref(), argv)
            }
            "git.switchBranch" => return self.switch_branch(client, &target, root.as_deref()),
            "git.rebase" => return self.rebase(client, &target, root.as_deref()),
            "git.rebaseContinue" => self.run_action(
                client,
                &target,
                root.as_deref(),
                operations::rebase_continue(),
            ),
            "git.rebaseAbort" => {
                self.run_action(client, &target, root.as_deref(), operations::rebase_abort())
            }
            "git.mergeAbort" => {
                self.run_action(client, &target, root.as_deref(), operations::merge_abort())
            }
            _ => return Ok(()),
        };

        self.report(client, outcome)?;
        self.refresh(client)
    }

    fn commit(
        &mut self,
        client: &mut Stdio,
        target: &ExecutionTarget,
        root: Option<&str>,
    ) -> Result<(), ClientError> {
        let staged = self
            .state
            .as_ref()
            .map(|state| state.status.staged.len())
            .unwrap_or_default();
        if staged == 0 {
            client.notify(
                "Nothing staged to commit",
                Some("Stage a file first.".to_owned()),
                NotificationLevel::Warning,
            )?;
            return Ok(());
        }

        let Some(message) = client.input(
            &format!("Commit {staged} staged file(s)"),
            None,
            Some("commit message".to_owned()),
        )?
        else {
            return Ok(());
        };
        if message.trim().is_empty() {
            client.notify(
                "Commit cancelled",
                Some("A commit message is required.".to_owned()),
                NotificationLevel::Warning,
            )?;
            return Ok(());
        }

        let outcome = self.run_action(client, target, root, operations::commit(message.trim()));
        self.report(client, outcome)?;
        self.refresh(client)
    }

    /// Pushes, setting an upstream when the branch has none.
    ///
    /// A plain `git push` on an unconfigured branch fails with advice the user
    /// would have to read in a terminal; asking once is better than surfacing
    /// that as an error.
    fn push(
        &mut self,
        client: &mut Stdio,
        target: &ExecutionTarget,
        root: Option<&str>,
    ) -> Result<(), ClientError> {
        let (has_upstream, head, remote) = match &self.state {
            Some(state) => (
                state.status.branch.upstream.is_some(),
                state.status.branch.head.clone(),
                state.default_remote().map(str::to_owned),
            ),
            None => (false, None, None),
        };

        let argv = if has_upstream {
            operations::push()
        } else {
            let (Some(head), Some(remote)) = (head, remote) else {
                client.notify(
                    "Cannot push",
                    Some("This branch has no upstream and no default remote.".to_owned()),
                    NotificationLevel::Warning,
                )?;
                return Ok(());
            };
            let confirmed = client.confirm(
                &format!("Push {head} to {remote}?"),
                Some(format!(
                    "{head} has no upstream. This will set {remote}/{head}."
                )),
                "Push",
                false,
            )?;
            if !confirmed {
                return Ok(());
            }
            operations::push_setting_upstream(&remote, &head)
        };

        let outcome = self.run_action(client, target, root, argv);
        self.report(client, outcome)?;
        self.refresh(client)
    }

    /// Force-updates the remote branch.
    ///
    /// Never `--force`. `--force-with-lease` refuses when the remote moved
    /// since the last fetch, so a teammate's commits cannot be discarded by a
    /// user who has not looked at the remote recently. The confirmation names
    /// the branch, the remote and whether remote history is being rewritten,
    /// because those are the three facts that decide whether this is safe.
    fn force_push(
        &mut self,
        client: &mut Stdio,
        target: &ExecutionTarget,
        root: Option<&str>,
    ) -> Result<(), ClientError> {
        let Some(state) = &self.state else {
            return Ok(());
        };
        let (Some(head), Some(upstream)) = (
            state.status.branch.head.clone(),
            state.status.branch.upstream.clone(),
        ) else {
            client.notify(
                "Cannot force push",
                Some("This branch has no upstream to force-update.".to_owned()),
                NotificationLevel::Warning,
            )?;
            return Ok(());
        };

        let behind = state.status.branch.behind;
        let rewrites = if behind > 0 {
            format!("This rewrites {behind} commit(s) already on {upstream}.")
        } else {
            format!("{upstream} has no commits that {head} does not.")
        };
        let confirmed = client.confirm(
            &format!("Force push {head} to {upstream}?"),
            Some(format!(
                "{rewrites}\nUses --force-with-lease, so it will refuse if {upstream} moved since your last fetch."
            )),
            "Force Push",
            true,
        )?;
        if !confirmed {
            return Ok(());
        }

        let outcome = self.run_action(client, target, root, operations::force_push_with_lease());
        self.report(client, outcome)?;
        self.refresh(client)
    }

    fn switch_branch(
        &mut self,
        client: &mut Stdio,
        target: &ExecutionTarget,
        root: Option<&str>,
    ) -> Result<(), ClientError> {
        let options: Vec<_> = self
            .state
            .iter()
            .flat_map(|state| state.local_branches())
            .filter(|branch| !branch.is_head)
            .map(|branch| extension_protocol::DialogSelectOption {
                id: branch.name.clone(),
                label: branch.name.clone(),
                description: (!branch.subject.is_empty()).then(|| branch.subject.clone()),
            })
            .collect();
        if options.is_empty() {
            client.notify("No other branches", None, NotificationLevel::Info)?;
            return Ok(());
        }

        let Some(branch) = client.select("Switch branch", None, options)? else {
            return Ok(());
        };
        let outcome = self.run_action(client, target, root, operations::switch_branch(&branch));
        self.report(client, outcome)?;
        self.refresh(client)
    }

    fn rebase(
        &mut self,
        client: &mut Stdio,
        target: &ExecutionTarget,
        root: Option<&str>,
    ) -> Result<(), ClientError> {
        let options: Vec<_> = self
            .state
            .iter()
            .flat_map(|state| state.local_branches())
            .filter(|branch| !branch.is_head)
            .map(|branch| extension_protocol::DialogSelectOption {
                id: branch.name.clone(),
                label: branch.name.clone(),
                description: None,
            })
            .collect();
        let Some(base) = client.select("Rebase current branch onto", None, options)? else {
            return Ok(());
        };

        match self.run_action(client, target, root, operations::rebase_onto(&base)) {
            Ok(()) => {
                client.notify(
                    &format!("Rebased onto {base}"),
                    None,
                    NotificationLevel::Info,
                )?;
            }
            Err(error) => {
                // A rebase that stops on a conflict is a normal outcome, not a
                // failure to report as one; the panel now shows the conflicts.
                client.notify(
                    "Rebase paused",
                    Some(format!(
                        "{}\nResolve the conflicts, then continue.",
                        error.summary()
                    )),
                    NotificationLevel::Warning,
                )?;
            }
        }
        self.refresh(client)
    }

    /// Deletes a branch, escalating from a safe delete to a force delete only
    /// after telling the user exactly how many commits it would discard.
    fn delete_branch(
        &mut self,
        client: &mut Stdio,
        target: &ExecutionTarget,
        root: Option<&str>,
        branch: &str,
    ) -> Result<(), ClientError> {
        let confirmed = client.confirm(
            &format!("Delete branch \"{branch}\"?"),
            None,
            "Delete",
            true,
        )?;
        if !confirmed {
            return Ok(());
        }

        let Some(mut repository) = self.repository_for(client, target, root) else {
            return Ok(());
        };
        let result = repository.try_run(&operations::delete_branch(branch))?;
        if result.exit_code == Some(0) {
            return self.refresh(client);
        }

        let base = self
            .state
            .as_ref()
            .and_then(|state| state.default_base_branch().map(str::to_owned));
        let unmerged = base
            .as_ref()
            .and_then(|base| repository.unmerged_commit_count(branch, base));

        let detail = match (&base, unmerged) {
            (Some(base), Some(count)) => format!(
                "\"{branch}\" has {count} commit(s) not merged into {base}. Deleting it may make them hard to recover."
            ),
            _ => format!("\"{branch}\" contains commits that are not merged anywhere else."),
        };
        let forced = client.confirm(
            &format!("Force delete \"{branch}\"?"),
            Some(detail),
            "Force Delete",
            true,
        )?;
        if !forced {
            return Ok(());
        }

        let outcome = self.run_action(
            client,
            target,
            root,
            operations::force_delete_branch(branch),
        );
        self.report(client, outcome)?;
        self.refresh(client)
    }

    fn run_action(
        &self,
        client: &mut Stdio,
        target: &ExecutionTarget,
        root: Option<&str>,
        argv: Vec<String>,
    ) -> Result<(), GitError> {
        let Some(mut repository) = self.repository_for(client, target, root) else {
            return Ok(());
        };
        repository.run(&argv).map(|_| ())
    }

    /// Surfaces a failed Git command, keeping Git's own first line as the
    /// summary so the message matches what the terminal would have said.
    fn report(&self, client: &mut Stdio, outcome: Result<(), GitError>) -> Result<(), ClientError> {
        let Err(error) = outcome else {
            return Ok(());
        };
        eprintln!("warp-git: {error}");
        client.notify(
            "Git command failed",
            Some(error.summary()),
            NotificationLevel::Error,
        )
    }
}

/// A staged row diffs the index against HEAD; anything else diffs the worktree.
///
/// The same path appears in both the staged and unstaged sections when it was
/// staged and then edited again, so the section is what distinguishes the two
/// rows — the path alone cannot.
fn comparison_for_section(section_id: &str) -> DiffComparison {
    match section_id {
        panel::SECTION_STAGED => DiffComparison::HeadToIndex,
        panel::SECTION_UNSTAGED | panel::SECTION_UNTRACKED => DiffComparison::IndexToWorktree,
        _ => DiffComparison::HeadToWorktree,
    }
}
