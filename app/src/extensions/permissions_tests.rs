use extension_protocol::{ExtensionManifest, Permission};
use tempfile::TempDir;

use super::*;

const BASE: &str = r#"
id = "dev.warp.example"
name = "Example"
version = "0.1.0"
api_version = 1
command = "./example"
"#;

fn manifest(extra: &str) -> ExtensionManifest {
    let manifest = ExtensionManifest::parse(&format!("{BASE}{extra}")).expect("manifest parses");
    manifest.validate().expect("manifest is valid");
    manifest
}

fn open_store(root: &TempDir) -> GrantStore {
    GrantStore::load_from(root.path().join("extension_grants.json"))
}

#[test]
fn an_unknown_extension_is_always_prompted_for() {
    let root = TempDir::new().expect("temp dir");
    let store = open_store(&root);

    let PermissionDecision::Prompt {
        requested, added, ..
    } = store.decide(&manifest("\n[permissions]\nui_notifications = true\n"))
    else {
        panic!("a never-seen extension must be prompted for");
    };
    assert!(requested.contains(Permission::UiNotifications));
    assert!(
        added.is_empty(),
        "a first install has nothing to call newly added"
    );
}

#[test]
fn a_grant_survives_a_reload_from_disk() {
    let root = TempDir::new().expect("temp dir");
    let manifest = manifest("\n[permissions]\nui_notifications = true\n");

    let mut store = open_store(&root);
    store.grant(&manifest).expect("the grant is written");

    let reloaded = open_store(&root);
    assert_eq!(reloaded.decide(&manifest), PermissionDecision::Granted);
    assert_eq!(
        reloaded
            .granted("dev.warp.example")
            .expect("the grant is recorded")
            .version,
        "0.1.0"
    );
}

#[test]
fn an_upgrade_asking_for_more_is_prompted_for_again() {
    let root = TempDir::new().expect("temp dir");
    let mut store = open_store(&root);
    store
        .grant(&manifest("\n[permissions]\nui_notifications = true\n"))
        .expect("the grant is written");

    let widened = manifest(
        "\n[permissions]\nui_notifications = true\nprocess_execute = true\n\n[execution]\nallowed_executables = [\"git\"]\n",
    );
    let PermissionDecision::Prompt {
        added,
        added_executables,
        ..
    } = store.decide(&widened)
    else {
        panic!("a widened manifest must be prompted for again");
    };
    assert_eq!(
        added,
        [Permission::ProcessExecute],
        "only the newly requested permission is reported as added"
    );
    assert_eq!(added_executables, ["git"]);
}

#[test]
fn an_upgrade_that_only_adds_an_executable_is_prompted_for_again() {
    let root = TempDir::new().expect("temp dir");
    let mut store = open_store(&root);
    let executes = "\n[permissions]\nprocess_execute = true\n\n[execution]\n";
    store
        .grant(&manifest(&format!(
            "{executes}allowed_executables = [\"git\"]\n"
        )))
        .expect("the grant is written");

    let widened = manifest(&format!(
        "{executes}allowed_executables = [\"git\", \"rm\"]\n"
    ));
    let PermissionDecision::Prompt {
        added,
        added_executables,
        ..
    } = store.decide(&widened)
    else {
        panic!(
            "the allowlist is the whole of what limits process.execute, so adding to it is \
             asking for more than the user agreed to"
        );
    };
    assert!(added.is_empty(), "the permission set itself is unchanged");
    assert_eq!(added_executables, ["rm"]);
}

#[test]
fn dropping_an_executable_stays_covered() {
    let root = TempDir::new().expect("temp dir");
    let mut store = open_store(&root);
    let executes = "\n[permissions]\nprocess_execute = true\n\n[execution]\n";
    store
        .grant(&manifest(&format!(
            "{executes}allowed_executables = [\"git\", \"rm\"]\n"
        )))
        .expect("the grant is written");

    let narrowed = manifest(&format!("{executes}allowed_executables = [\"git\"]\n"));
    assert_eq!(store.decide(&narrowed), PermissionDecision::Granted);
}

#[test]
fn an_upgrade_asking_for_less_is_still_covered() {
    let root = TempDir::new().expect("temp dir");
    let mut store = open_store(&root);
    store
        .grant(&manifest(
            "\n[permissions]\nui_notifications = true\nworkspace_read = true\n",
        ))
        .expect("the grant is written");

    let narrowed = manifest("\n[permissions]\nworkspace_read = true\n");
    assert_eq!(store.decide(&narrowed), PermissionDecision::Granted);
}

#[test]
fn declaring_a_panel_makes_the_prompt_mention_it() {
    let root = TempDir::new().expect("temp dir");
    let store = open_store(&root);

    let PermissionDecision::Prompt { requested, .. } = store.decide(&manifest(
        "\n[[panels]]\nid = \"example\"\ntitle = \"Example\"\nlocation = \"left\"\n",
    )) else {
        panic!("a never-seen extension must be prompted for");
    };
    assert!(
        requested.contains(Permission::UiPanel),
        "a declared panel is itself the request for the panel permission, and the \
         user has to see it"
    );
}

#[test]
fn revoking_forgets_the_grant() {
    let root = TempDir::new().expect("temp dir");
    let manifest = manifest("\n[permissions]\nui_notifications = true\n");
    let mut store = open_store(&root);
    store.grant(&manifest).expect("the grant is written");

    store
        .revoke("dev.warp.example")
        .expect("the grant is removed");

    assert!(matches!(
        store.decide(&manifest),
        PermissionDecision::Prompt { .. }
    ));
    assert!(matches!(
        open_store(&root).decide(&manifest),
        PermissionDecision::Prompt { .. }
    ));
}

#[test]
fn an_unreadable_grant_file_denies_rather_than_failing() {
    let root = TempDir::new().expect("temp dir");
    let path = root.path().join("extension_grants.json");
    std::fs::write(&path, "{ this is not json").expect("the file is written");

    let store = GrantStore::load_from(&path);
    assert!(
        matches!(
            store.decide(&manifest("\n[permissions]\nui_notifications = true\n")),
            PermissionDecision::Prompt { .. }
        ),
        "a file that will not parse must cost a prompt, never a silent grant"
    );
}
