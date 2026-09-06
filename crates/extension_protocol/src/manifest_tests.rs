use super::*;
use crate::permissions::Permission;

/// The manifest from the design document, used as a compatibility fixture: if
/// this stops parsing, published plugins stop loading.
const REFERENCE_MANIFEST: &str = r#"
id = "dev.warp.git"
name = "Warp Git"
version = "0.1.0"
api_version = "1"

command = "./warp-git"

[activation]
workspace_contains = [".git"]

[permissions]
workspace_read = true
workspace_write = true
process_execute = true
git_mutate = true

[execution]
allowed_executables = ["git"]

[[commands]]
id = "git.open"
title = "Git: Open"

[[commands]]
id = "git.fetch"
title = "Git: Fetch"

[[panels]]
id = "git"
title = "Git"
location = "left"
"#;

fn reference() -> ExtensionManifest {
    ExtensionManifest::parse(REFERENCE_MANIFEST).expect("reference manifest parses")
}

#[test]
fn the_reference_manifest_parses_and_validates() {
    let manifest = reference();
    manifest.validate().expect("reference manifest is valid");

    assert_eq!(manifest.id, "dev.warp.git");
    assert_eq!(manifest.api_version, 1);
    assert_eq!(manifest.command, "./warp-git");
    assert_eq!(
        manifest
            .activation
            .as_ref()
            .map(|activation| activation.workspace_contains.as_slice()),
        Some([".git".to_owned()].as_slice())
    );
    assert_eq!(manifest.commands.len(), 2);
    assert_eq!(manifest.panels[0].location, PanelLocation::Left);
}

#[test]
fn api_version_is_accepted_as_a_number_or_a_string() {
    let numeric = REFERENCE_MANIFEST.replace("api_version = \"1\"", "api_version = 1");
    let manifest = ExtensionManifest::parse(&numeric).expect("numeric api_version parses");
    assert_eq!(manifest.api_version, 1);
}

#[test]
fn an_unsupported_api_version_is_rejected_with_both_versions_named() {
    let source = REFERENCE_MANIFEST.replace("api_version = \"1\"", "api_version = 99");
    let manifest = ExtensionManifest::parse(&source).expect("manifest parses");
    let error = manifest
        .validate()
        .expect_err("api_version 99 is unsupported");
    assert_eq!(
        error,
        ManifestError::UnsupportedApiVersion {
            requested: 99,
            supported: crate::protocol::API_VERSION,
        }
    );
}

#[test]
fn required_fields_are_required() {
    for field in ["id", "name", "version", "api_version", "command"] {
        let source: String = REFERENCE_MANIFEST
            .lines()
            .filter(|line| !line.starts_with(&format!("{field} =")))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            ExtensionManifest::parse(&source).is_err(),
            "a manifest without `{field}` must not parse"
        );
    }
}

#[test]
fn unknown_manifest_keys_are_rejected() {
    let source = format!("{REFERENCE_MANIFEST}\nunknown_key = true\n");
    let error = ExtensionManifest::parse(&source).expect_err("unknown keys are malformed");
    assert!(matches!(error, ManifestError::Malformed(_)));
}

#[test]
fn a_command_that_escapes_the_extension_directory_is_rejected() {
    for command in ["/usr/bin/git", "../../usr/bin/git", "nested/../../escape"] {
        let source = REFERENCE_MANIFEST.replace(
            "command = \"./warp-git\"",
            &format!("command = \"{command}\""),
        );
        let manifest = ExtensionManifest::parse(&source).expect("manifest parses");
        let error = manifest
            .validate()
            .unwrap_err_or_panic(&format!("`{command}` must be rejected"));
        assert!(matches!(error, ManifestError::InvalidCommandPath(_)));
    }
}

#[test]
fn a_nested_relative_command_is_accepted() {
    let source =
        REFERENCE_MANIFEST.replace("command = \"./warp-git\"", "command = \"bin/warp-git\"");
    let manifest = ExtensionManifest::parse(&source).expect("manifest parses");
    manifest.validate().expect("a nested relative path is fine");
}

#[test]
fn malformed_ids_are_rejected() {
    for id in ["", "Dev.Warp.Git", "dev..git", "dev warp git", "dev/git"] {
        let source = REFERENCE_MANIFEST.replace("id = \"dev.warp.git\"", &format!("id = \"{id}\""));
        let manifest = ExtensionManifest::parse(&source).expect("manifest parses");
        assert!(manifest.validate().is_err(), "id `{id}` must be rejected");
    }
}

#[test]
fn duplicate_contribution_ids_are_rejected() {
    let source = REFERENCE_MANIFEST.replace("id = \"git.fetch\"", "id = \"git.open\"");
    let manifest = ExtensionManifest::parse(&source).expect("manifest parses");
    let error = manifest.validate().expect_err("duplicate command ids fail");
    assert_eq!(
        error,
        ManifestError::DuplicateCommandId("git.open".to_owned())
    );
}

#[test]
fn process_execute_without_an_allowlist_fails_closed() {
    let source = REFERENCE_MANIFEST.replace(
        "allowed_executables = [\"git\"]",
        "allowed_executables = []",
    );
    let manifest = ExtensionManifest::parse(&source).expect("manifest parses");
    assert_eq!(
        manifest
            .validate()
            .expect_err("an empty allowlist is rejected"),
        ManifestError::MissingExecutableAllowlist
    );
}

#[test]
fn an_allowlisted_executable_must_be_a_bare_name() {
    for executable in ["./git", "/usr/bin/git", "..", "sub/git"] {
        let source = REFERENCE_MANIFEST.replace(
            "allowed_executables = [\"git\"]",
            &format!("allowed_executables = [\"{executable}\"]"),
        );
        let manifest = ExtensionManifest::parse(&source).expect("manifest parses");
        let error = manifest
            .validate()
            .unwrap_err_or_panic(&format!("`{executable}` must be rejected"));
        assert!(matches!(error, ManifestError::InvalidAllowedExecutable(_)));
    }
}

#[test]
fn allows_executable_matches_only_the_declared_names() {
    let manifest = reference();
    assert!(manifest.allows_executable("git"));
    assert!(!manifest.allows_executable("sh"));
    assert!(!manifest.allows_executable("/usr/bin/git"));
}

#[test]
fn declaring_a_contribution_grants_the_matching_ui_permission() {
    let manifest = reference();
    let granted = manifest.effective_permissions();

    assert!(granted.contains(Permission::UiCommands));
    assert!(granted.contains(Permission::UiPanel));
    assert!(granted.contains(Permission::WorkspaceRead));
    assert!(granted.contains(Permission::ProcessExecute));
    assert!(
        !granted.contains(Permission::UiNotifications),
        "a permission that was never declared must not be granted"
    );
}

#[test]
fn a_plugin_may_only_drive_surfaces_it_declared() {
    let manifest = reference();
    assert!(manifest.declares_panel("git"));
    assert!(!manifest.declares_panel("other"));
    assert!(manifest.declares_command("git.fetch"));
    assert!(!manifest.declares_command("git.push"));
}

/// `Result::expect_err` needs `T: Debug`; this keeps the failure message
/// specific in loops where the success type is only `()`.
trait UnwrapErrOrPanic<E> {
    fn unwrap_err_or_panic(self, message: &str) -> E;
}

impl<E> UnwrapErrOrPanic<E> for Result<(), E> {
    fn unwrap_err_or_panic(self, message: &str) -> E {
        match self {
            Ok(()) => panic!("{message}"),
            Err(error) => error,
        }
    }
}
