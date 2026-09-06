use std::fs;

use extension_protocol::Activation;
use tempfile::TempDir;
use warp_util::host_id::HostId;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warp_util::remote_path::RemotePath;
use warp_util::standardized_path::StandardizedPath;

use super::*;

/// A plugin that performs the handshake and then waits, written in `sh` so the
/// test does not depend on another crate's build artifacts. Anything that can
/// frame JSON on stdio is a plugin, which is the point of the wire protocol.
const PLUGIN_SCRIPT: &str = r#"#!/bin/sh
printf '{"protocol":1,"request_id":"1","method":"extension.initialize","params":{"extension_id":"dev.warp.test","api_version":1}}\n'
while read -r _line; do
  :
done
"#;

fn manifest_source(extra: &str) -> String {
    format!(
        r#"
id = "dev.warp.test"
name = "Test Extension"
version = "0.1.0"
api_version = 1
command = "./plugin.sh"
{extra}
"#
    )
}

/// Installs one extension under a fresh root and returns the root.
fn install(extra: &str) -> TempDir {
    let root = TempDir::new().expect("temp dir");
    let directory = root.path().join("test-extension");
    fs::create_dir_all(&directory).expect("extension directory");
    fs::write(directory.join("extension.toml"), manifest_source(extra))
        .expect("manifest is written");
    let executable = directory.join("plugin.sh");
    fs::write(&executable, PLUGIN_SCRIPT).expect("plugin is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("plugin is executable");
    }
    root
}

fn open_grants(root: &TempDir) -> GrantStore {
    GrantStore::load_from(root.path().join("grants.json"))
}

const COMMAND: &str = r#"
[[commands]]
id = "do.thing"
title = "Do the thing"
"#;

const PANEL: &str = r#"
[[panels]]
id = "git"
title = "Git"
location = "left"
"#;

fn panel_snapshot() -> PanelViewState {
    PanelViewState {
        panel_id: "git".to_owned(),
        status: extension_protocol::PanelStatus::Ready,
        sections: Vec::new(),
    }
}

fn confirm_params() -> DialogConfirmParams {
    DialogConfirmParams {
        title: "Proceed?".to_owned(),
        body: None,
        confirm_label: "Yes".to_owned(),
        cancel_label: "No".to_owned(),
        destructive: false,
    }
}

#[test]
fn no_activation_table_means_the_extension_is_always_eligible() {
    assert!(should_activate(None, None));
    assert!(should_activate(Some(&Activation::default()), None));
}

#[test]
fn a_workspace_condition_stays_inactive_until_a_directory_is_known() {
    let activation = Activation {
        workspace_contains: vec![".git".to_owned()],
    };
    assert!(
        !should_activate(Some(&activation), None),
        "with no workspace to test, the condition has not been shown to hold"
    );
}

#[test]
fn a_workspace_condition_matches_an_ancestor_directory() {
    let root = TempDir::new().expect("temp dir");
    fs::create_dir_all(root.path().join(".git")).expect(".git");
    let nested = root.path().join("crates").join("thing");
    fs::create_dir_all(&nested).expect("nested directory");

    let activation = Activation {
        workspace_contains: vec![".git".to_owned()],
    };
    assert!(
        should_activate(Some(&activation), Some(&nested)),
        "a repository activates its plugin from any directory inside it"
    );
    assert!(!should_activate(
        Some(&Activation {
            workspace_contains: vec!["Cargo.lock".to_owned()],
        }),
        Some(&nested)
    ));
}

const ACTIVATION: &str = r#"
[activation]
workspace_contains = [".git"]
"#;

fn context_in(cwd: LocalOrRemotePath) -> ActiveContext {
    ActiveContext {
        workspace_id: "1".to_owned(),
        active_pane_id: Some("2".to_owned()),
        cwd: Some(cwd),
        repository_root: None,
        session_id: Some(SessionId::from(3)),
        execution_target: ExecutionTarget::Local,
    }
}

#[test]
fn a_workspace_condition_is_evaluated_once_the_context_says_where_the_user_is() {
    let root = install(ACTIVATION);
    let grant_root = TempDir::new().expect("temp dir");
    let repository = TempDir::new().expect("temp dir");
    fs::create_dir_all(repository.path().join(".git")).expect(".git");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            assert!(
                manager.asking.is_none() && manager.questions.is_empty(),
                "with nowhere known to look, the condition has not been shown to hold, so nothing is even asked about"
            );

            manager.context_moved(
                Some(context_in(LocalOrRemotePath::Local(
                    repository.path().to_path_buf(),
                ))),
                ctx,
            );
            assert_eq!(
                manager.asking,
                Some(Outcome::Permission {
                    extension_id: "dev.warp.test".to_owned()
                }),
                "the context is what makes the extension eligible, and the prompt is still the gate"
            );
        });
    });
}

#[test]
fn a_remote_directory_is_not_evaluated_against_this_machine() {
    let root = install(ACTIVATION);
    let grant_root = TempDir::new().expect("temp dir");
    let repository = TempDir::new().expect("temp dir");
    fs::create_dir_all(repository.path().join(".git")).expect(".git");
    let remote = LocalOrRemotePath::Remote(RemotePath::new(
        HostId::new("host-1".to_owned()),
        StandardizedPath::try_new(&repository.path().to_string_lossy()).expect("a remote path"),
    ));

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            manager.context_moved(Some(context_in(remote)), ctx);

            assert!(
                manager.workspace_directory.is_none(),
                "`workspace_contains` is answered by looking at this file system, which a remote checkout is not on"
            );
            assert!(
                manager.asking.is_none(),
                "a same-named path on this machine must not activate an extension for a repository on another one"
            );
        });
    });
}

#[test]
fn an_extension_without_a_grant_is_discovered_but_not_started() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, _| {
            assert_eq!(
                manager.state("dev.warp.test"),
                Some(&ExtensionState::Inactive),
                "an extension the user has not answered for must not be running"
            );
            assert_eq!(
                manager.asking,
                Some(Outcome::Permission {
                    extension_id: "dev.warp.test".to_owned()
                }),
                "the prompt is the gate, so it is up before anything has started"
            );
            assert_eq!(manager.commands().count(), 0);
            assert!(manager.rejected().is_empty());
        });
    });
}

#[test]
fn declining_leaves_the_extension_inactive_and_is_not_asked_again() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");
    let grants_file = grant_root.path().join("grants.json");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(
                root.path().to_path_buf(),
                GrantStore::load_from(&grants_file),
                ctx,
            )
        });

        manager.update(&mut app, |manager, ctx| {
            manager.answered(false, ctx);

            assert_eq!(
                manager.state("dev.warp.test"),
                Some(&ExtensionState::Inactive)
            );
            assert!(manager.asking.is_none());

            manager.refresh(ctx);
            assert!(
                manager.asking.is_none() && manager.questions.is_empty(),
                "a refusal is an answer; rescanning the directory must not ask again"
            );
        });

        assert!(
            GrantStore::load_from(&grants_file)
                .granted("dev.warp.test")
                .is_none(),
            "declining records nothing"
        );
    });
}

#[test]
fn allowing_the_prompt_starts_the_extension() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");
    let grants_file = grant_root.path().join("grants.json");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(
                root.path().to_path_buf(),
                GrantStore::load_from(&grants_file),
                ctx,
            )
        });

        manager.update(&mut app, |manager, ctx| {
            manager.answered(true, ctx);
            assert_eq!(
                manager.state("dev.warp.test"),
                Some(&ExtensionState::Starting)
            );
            assert!(manager.asking.is_none());
        });

        assert!(
            GrantStore::load_from(&grants_file)
                .granted("dev.warp.test")
                .is_some()
        );

        manager.update(&mut app, |manager, ctx| manager.stop_all(ctx));
    });
}

#[test]
fn retrying_asks_again_after_a_decline() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            manager.answered(false, ctx);
            manager.retry("dev.warp.test", ctx);
            assert_eq!(
                manager.asking,
                Some(Outcome::Permission {
                    extension_id: "dev.warp.test".to_owned()
                }),
                "coming back to the extension deliberately is the way past a refusal"
            );
        });
    });
}

#[test]
fn a_rescan_while_the_prompt_is_up_does_not_stack_a_second_one() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            manager.refresh(ctx);
            manager.refresh(ctx);
            assert!(
                manager.questions.is_empty(),
                "the same extension must not queue a second copy of the question already on screen"
            );
        });
    });
}

#[test]
fn a_dialog_is_answered_back_to_the_plugin_and_lets_the_next_question_through() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            manager.answered(true, ctx);

            manager.confirm("dev.warp.test", "7".to_owned(), confirm_params(), ctx);
            manager.confirm("dev.warp.test", "8".to_owned(), confirm_params(), ctx);
            assert_eq!(
                manager.asking,
                Some(Outcome::Dialog {
                    extension_id: "dev.warp.test".to_owned(),
                    request_id: "7".to_owned()
                })
            );
            assert_eq!(
                manager.questions.len(),
                1,
                "one modal at a time, or the second replaces the first and its answer never arrives"
            );

            manager.answered(true, ctx);
            assert_eq!(
                manager.asking,
                Some(Outcome::Dialog {
                    extension_id: "dev.warp.test".to_owned(),
                    request_id: "8".to_owned()
                })
            );

            manager.answered(false, ctx);
            assert!(manager.asking.is_none());
        });

        manager.update(&mut app, |manager, ctx| manager.stop_all(ctx));
    });
}

#[test]
fn stopping_an_extension_abandons_the_questions_it_was_waiting_on() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            manager.answered(true, ctx);
            manager.confirm("dev.warp.test", "7".to_owned(), confirm_params(), ctx);
            manager.confirm("dev.warp.test", "8".to_owned(), confirm_params(), ctx);

            manager.stop("dev.warp.test", StopReason::Requested, ctx);
            assert!(
                manager.questions.is_empty(),
                "a question for an extension that is gone has nothing left to ask about"
            );
            assert_eq!(
                manager.asking,
                Some(Outcome::Abandoned),
                "the modal already on screen cannot be withdrawn, so its answer is neutralised"
            );

            manager.answered(true, ctx);
            assert!(manager.asking.is_none());
        });
    });
}

#[test]
fn a_command_is_only_delivered_while_its_extension_is_running() {
    let root = install(COMMAND);
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            // Nothing is registered until the handshake completes, so a palette
            // entry cannot exist yet — and invoking one is a no-op rather than a
            // message written to a plugin that has not said hello.
            assert_eq!(manager.commands().count(), 0);
            manager.invoke_command("dev.warp.test", "do.thing", ctx);
            manager.invoke_command("dev.warp.test", "not.declared", ctx);
        });
    });
}

#[test]
fn a_first_prompt_lists_everything_and_an_upgrade_lists_only_what_is_new() {
    let requested: PermissionSet = [Permission::ProcessExecute, Permission::UiDialogs]
        .into_iter()
        .collect();
    let executables = vec!["git".to_owned()];

    let first = permission_question("Warp Git", &requested, &executables, &[], &[]);
    assert_eq!(first.title, "Allow Warp Git to run?");
    assert!(
        first
            .body
            .contains(Permission::ProcessExecute.description())
    );
    assert!(first.body.contains(Permission::UiDialogs.description()));
    assert!(first.body.contains("Run only these programs: git"));

    let upgrade = permission_question(
        "Warp Git",
        &requested,
        &executables,
        &[Permission::UiDialogs],
        &[],
    );
    assert_eq!(upgrade.title, "Warp Git is asking for more access");
    assert!(upgrade.body.contains(Permission::UiDialogs.description()));
    assert!(
        !upgrade
            .body
            .contains(Permission::ProcessExecute.description()),
        "asking a second time is about the difference; repeating the rest is how a prompt \
         stops being read"
    );
}

#[test]
fn a_destructive_confirmation_says_so_and_names_the_extension() {
    let question = confirm_question(
        "Warp Git",
        DialogConfirmParams {
            title: "Discard changes to main.rs?".to_owned(),
            body: Some("3 lines will be lost.".to_owned()),
            confirm_label: "Discard".to_owned(),
            cancel_label: "Keep".to_owned(),
            destructive: true,
        },
    );
    assert_eq!(question.title, "Warp Git: Discard changes to main.rs?");
    assert!(question.body.contains("3 lines will be lost."));
    assert!(question.body.contains("This cannot be undone."));
    assert_eq!(question.confirm_label, "Discard");
    assert_eq!(question.cancel_label, "Keep");
}

#[test]
fn a_granted_extension_is_started() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");
    let mut store = open_grants(&grant_root);
    let manifest =
        extension_protocol::ExtensionManifest::parse(&manifest_source("")).expect("manifest");
    store.grant(&manifest).expect("the grant is written");

    warpui::App::test((), |mut app| async move {
        let manager = app
            .add_model(|ctx| ExtensionManager::with_paths(root.path().to_path_buf(), store, ctx));

        manager.update(&mut app, |manager, _| {
            assert_eq!(
                manager.state("dev.warp.test"),
                Some(&ExtensionState::Starting),
                "a granted extension starts, and stays in Starting until it answers the handshake"
            );
        });
        manager.update(&mut app, |manager, ctx| manager.stop_all(ctx));
    });
}

#[test]
fn a_directory_without_a_manifest_is_ignored_rather_than_rejected() {
    let root = install("");
    fs::create_dir_all(root.path().join("not-an-extension")).expect("directory");
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, _| {
            assert!(
                manager.rejected().is_empty(),
                "a directory that never claimed to be an extension is not a failure to report"
            );
            assert!(manager.state("dev.warp.test").is_some());
        });
    });
}

#[test]
fn a_malformed_manifest_is_reported_with_its_reason() {
    let root = TempDir::new().expect("temp dir");
    let directory = root.path().join("broken");
    fs::create_dir_all(&directory).expect("directory");
    fs::write(directory.join("extension.toml"), "id = \"broken\"\n").expect("manifest");
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, _| {
            let rejected = manager.rejected();
            assert_eq!(rejected.len(), 1);
            assert!(
                !rejected[0].1.is_empty(),
                "an extension the user installed and Warp will not run has to say why"
            );
        });
    });
}

#[test]
fn allowing_an_extension_records_the_grant_and_starts_it() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");
    let grants_file = grant_root.path().join("grants.json");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(
                root.path().to_path_buf(),
                GrantStore::load_from(&grants_file),
                ctx,
            )
        });

        manager.update(&mut app, |manager, ctx| {
            assert_eq!(
                manager.state("dev.warp.test"),
                Some(&ExtensionState::Inactive)
            );
            manager.allow("dev.warp.test", ctx);
            assert_eq!(
                manager.state("dev.warp.test"),
                Some(&ExtensionState::Starting)
            );
        });

        assert!(
            GrantStore::load_from(&grants_file)
                .granted("dev.warp.test")
                .is_some(),
            "the answer has to outlive the session that asked the question"
        );

        manager.update(&mut app, |manager, ctx| manager.stop_all(ctx));
    });
}

#[test]
fn revoking_stops_the_extension_and_forgets_the_answer() {
    let root = install("");
    let grant_root = TempDir::new().expect("temp dir");
    let grants_file = grant_root.path().join("grants.json");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(
                root.path().to_path_buf(),
                GrantStore::load_from(&grants_file),
                ctx,
            )
        });

        manager.update(&mut app, |manager, ctx| {
            manager.allow("dev.warp.test", ctx);
            manager.revoke("dev.warp.test", ctx);

            assert_eq!(
                manager.state("dev.warp.test"),
                Some(&ExtensionState::Inactive),
                "revoking is a deliberate stop, so it must not count against the restart budget"
            );
            assert_eq!(manager.commands().count(), 0);
            assert_eq!(manager.panels().count(), 0);
        });

        assert!(
            GrantStore::load_from(&grants_file)
                .granted("dev.warp.test")
                .is_none()
        );
    });
}

#[test]
fn a_manifest_that_cannot_be_started_is_not_left_running() {
    let root = TempDir::new().expect("temp dir");
    let directory = root.path().join("missing-executable");
    fs::create_dir_all(&directory).expect("directory");
    fs::write(directory.join("extension.toml"), manifest_source("")).expect("manifest");
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, _| {
            assert!(
                manager.state("dev.warp.test").is_none(),
                "a manifest whose executable is missing never becomes a managed extension"
            );
            assert_eq!(manager.rejected().len(), 1);
        });
    });
}

#[test]
fn a_published_snapshot_is_readable_back_for_the_panel_it_names() {
    let root = install(PANEL);
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            manager.set_panel_state("dev.warp.test", panel_snapshot(), ctx);

            assert!(
                manager.panel_state("dev.warp.test", "git").is_some(),
                "the panel reads the manager's record rather than keeping its own"
            );
            assert!(
                manager.panel_state("dev.warp.test", "other").is_none(),
                "a snapshot answers only for the panel it names"
            );
            assert!(
                manager.panel_state("dev.warp.other", "git").is_none(),
                "two extensions may use the same panel id, so the extension is part of the key"
            );
        });
    });
}

#[test]
fn a_stopped_extension_leaves_no_snapshot_behind() {
    let root = install(PANEL);
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            manager.answered(true, ctx);
            manager.set_panel_state("dev.warp.test", panel_snapshot(), ctx);
            assert!(manager.panel_state("dev.warp.test", "git").is_some());

            manager.stop("dev.warp.test", StopReason::Requested, ctx);
            assert!(
                manager.panel_state("dev.warp.test", "git").is_none(),
                "a panel left showing a state nothing is maintaining reads as \
                 current and is not"
            );
        });
    });
}

#[test]
fn a_panel_action_is_only_delivered_for_a_declared_panel_of_a_running_extension() {
    let root = install(PANEL);
    let grant_root = TempDir::new().expect("temp dir");

    warpui::App::test((), |mut app| async move {
        let manager = app.add_model(|ctx| {
            ExtensionManager::with_paths(root.path().to_path_buf(), open_grants(&grant_root), ctx)
        });

        manager.update(&mut app, |manager, ctx| {
            // Nothing is registered until the handshake completes, so a row
            // cannot have been drawn yet — and acting on one is a no-op rather
            // than a message written to a plugin that has not said hello.
            assert_eq!(manager.panels().count(), 0);
            manager.invoke_panel_action("dev.warp.test", "git", "git.staged", "a.rs", "stage", ctx);
            // A panel id the manifest never declared is refused by the same
            // check, which is what keeps one extension out of another's panel.
            manager.invoke_panel_action(
                "dev.warp.test",
                "not.declared",
                "git.staged",
                "a.rs",
                "stage",
                ctx,
            );
        });
    });
}
