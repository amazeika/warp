use std::path::Path;

use tempfile::TempDir;

use super::*;

const VALID_MANIFEST: &str = r#"
id = "dev.warp.example"
name = "Warp Example"
version = "0.1.0"
api_version = 1
command = "./plugin"

[permissions]
workspace_read = true
"#;

fn install(root: &Path, directory: &str, manifest: &str, with_executable: bool) {
    let path = root.join(directory);
    std::fs::create_dir_all(&path).expect("extension directory is created");
    std::fs::write(path.join("extension.toml"), manifest).expect("manifest is written");
    if with_executable {
        std::fs::write(path.join("plugin"), "#!/bin/sh\n").expect("executable is written");
    }
}

fn invalid_reason(discovered: &DiscoveredExtension) -> &ValidationError {
    match &discovered.record {
        ExtensionRecord::Invalid { reason } => reason,
        ExtensionRecord::Valid { .. } => panic!("expected an invalid extension"),
    }
}

#[test]
fn a_missing_root_yields_no_extensions() {
    let root = TempDir::new().expect("temp dir");
    let missing = root.path().join("nothing-here");
    assert!(discover_in(&missing).is_empty());
}

#[test]
fn a_valid_extension_is_discovered_with_its_resolved_executable() {
    let root = TempDir::new().expect("temp dir");
    install(root.path(), "example", VALID_MANIFEST, true);

    let discovered = discover_in(root.path());
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].id(), Some("dev.warp.example"));
    let ExtensionRecord::Valid { executable, .. } = &discovered[0].record else {
        panic!("expected a valid extension");
    };
    assert_eq!(executable, &root.path().join("example").join("plugin"));
}

#[test]
fn directories_without_a_manifest_are_ignored_silently() {
    let root = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(root.path().join("not-an-extension")).expect("dir");
    std::fs::write(root.path().join("stray-file"), "").expect("file");
    install(root.path(), "example", VALID_MANIFEST, true);

    let discovered = discover_in(root.path());
    assert_eq!(
        discovered.len(),
        1,
        "only the manifest-bearing directory counts"
    );
}

#[test]
fn a_malformed_manifest_is_listed_with_a_reason_rather_than_dropped() {
    let root = TempDir::new().expect("temp dir");
    install(root.path(), "broken", "this is not toml = [", true);

    let discovered = discover_in(root.path());
    assert_eq!(discovered.len(), 1);
    assert!(matches!(
        invalid_reason(&discovered[0]),
        ValidationError::Manifest(extension_protocol::ManifestError::Malformed(_))
    ));
}

#[test]
fn an_unsupported_api_version_is_invalid_and_names_both_versions() {
    let root = TempDir::new().expect("temp dir");
    install(
        root.path(),
        "future",
        &VALID_MANIFEST.replace("api_version = 1", "api_version = 99"),
        true,
    );

    let discovered = discover_in(root.path());
    let reason = invalid_reason(&discovered[0]).to_string();
    assert!(reason.contains("99"), "{reason}");
    assert!(reason.contains('1'), "{reason}");
}

#[test]
fn a_missing_executable_is_invalid() {
    let root = TempDir::new().expect("temp dir");
    install(root.path(), "example", VALID_MANIFEST, false);

    let discovered = discover_in(root.path());
    assert_eq!(
        invalid_reason(&discovered[0]),
        &ValidationError::MissingExecutable("./plugin".to_owned())
    );
}

#[test]
fn a_command_escaping_the_extension_directory_is_invalid() {
    let root = TempDir::new().expect("temp dir");
    std::fs::write(root.path().join("plugin"), "").expect("sibling file exists");
    install(
        root.path(),
        "escaping",
        &VALID_MANIFEST.replace("command = \"./plugin\"", "command = \"../plugin\""),
        true,
    );

    let discovered = discover_in(root.path());
    assert!(matches!(
        invalid_reason(&discovered[0]),
        ValidationError::Manifest(extension_protocol::ManifestError::InvalidCommandPath(_))
    ));
}

#[test]
fn a_duplicate_id_keeps_the_first_directory_and_invalidates_the_rest() {
    let root = TempDir::new().expect("temp dir");
    install(root.path(), "b-second", VALID_MANIFEST, true);
    install(root.path(), "a-first", VALID_MANIFEST, true);
    install(root.path(), "c-third", VALID_MANIFEST, true);

    let discovered = discover_in(root.path());
    assert_eq!(discovered.len(), 3);
    assert_eq!(
        discovered[0].directory.file_name().and_then(|n| n.to_str()),
        Some("a-first"),
        "discovery is sorted so the winner is deterministic"
    );
    assert!(discovered[0].manifest().is_some());

    for later in &discovered[1..] {
        assert_eq!(
            invalid_reason(later),
            &ValidationError::DuplicateId {
                id: "dev.warp.example".to_owned(),
                first: "a-first".to_owned(),
            }
        );
    }
}

#[test]
fn discovery_order_is_stable_across_runs() {
    let root = TempDir::new().expect("temp dir");
    for (index, directory) in ["zebra", "alpha", "mango"].into_iter().enumerate() {
        install(
            root.path(),
            directory,
            &VALID_MANIFEST.replace("dev.warp.example", &format!("dev.warp.example{index}")),
            true,
        );
    }

    let first: Vec<_> = discover_in(root.path())
        .into_iter()
        .filter_map(|extension| extension.id().map(str::to_owned))
        .collect();
    let second: Vec<_> = discover_in(root.path())
        .into_iter()
        .filter_map(|extension| extension.id().map(str::to_owned))
        .collect();
    assert_eq!(first, second);
    assert_eq!(
        first,
        [
            "dev.warp.example1",
            "dev.warp.example2",
            "dev.warp.example0"
        ],
        "alpha, mango, zebra"
    );
}

#[test]
fn candidacy_requires_a_directory_holding_a_manifest() {
    let root = TempDir::new().expect("temp dir");
    install(root.path(), "example", VALID_MANIFEST, true);
    assert!(extension_directory_is_candidate(
        &root.path().join("example")
    ));
    assert!(!extension_directory_is_candidate(
        &root.path().join("absent")
    ));
    assert!(!extension_directory_is_candidate(
        &root.path().join("example").join("extension.toml")
    ));
}
