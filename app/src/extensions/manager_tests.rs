use std::fs;

use extension_protocol::Activation;
use tempfile::TempDir;

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
            assert_eq!(manager.commands().count(), 0);
            assert!(manager.rejected().is_empty());
        });
    });
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
