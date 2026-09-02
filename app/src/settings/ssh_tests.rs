//! The user-facing contract of the SSH settings: where they live in
//! `~/.warp/settings.toml`, and that a value written there survives a read back.

use settings::Setting;
use warpui_extras::user_preferences::toml_backed::TomlBackedUserPreferences;

use super::CloneSshOnSplit;

fn settings_file(contents: &str) -> (tempfile::TempDir, TomlBackedUserPreferences) {
    let dir = tempfile::tempdir().expect("a temp dir for the settings file");
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, contents).expect("writing the settings file");
    let (prefs, error) = TomlBackedUserPreferences::new(path);
    assert!(error.is_none(), "settings file did not parse: {error:?}");
    (dir, prefs)
}

/// The path is the user-facing name of this setting: it appears verbatim in
/// `~/.warp/settings.toml` and in the generated settings schema, so changing it
/// silently orphans every user who has already set it.
#[test]
fn clone_on_split_lives_under_warpify_ssh() {
    assert_eq!(
        CloneSshOnSplit::toml_path(),
        Some("warpify.ssh.clone_ssh_on_split")
    );
    assert_eq!(CloneSshOnSplit::hierarchy(), Some("warpify.ssh"));
    assert_eq!(CloneSshOnSplit::toml_key(), "clone_ssh_on_split");
}

/// It ships disabled: the feature soaks behind both the flag and an opt-in.
#[test]
fn clone_on_split_defaults_to_off() {
    assert!(!CloneSshOnSplit::default_value());
}

#[test]
fn clone_on_split_reads_a_hand_written_settings_file() {
    let (_dir, prefs) = settings_file("[warpify.ssh]\nclone_ssh_on_split = true\n");

    assert_eq!(CloneSshOnSplit::read_from_preferences(&prefs), Some(true));
}

#[test]
fn clone_on_split_round_trips_through_the_settings_file() {
    let (_dir, prefs) = settings_file("");

    CloneSshOnSplit::write_to_preferences(&true, &prefs).expect("writing the setting");

    assert_eq!(CloneSshOnSplit::read_from_preferences(&prefs), Some(true));
}

/// An absent key is absence, not `false`: the caller falls back to the declared
/// default rather than to whatever the last writer happened to leave behind.
#[test]
fn clone_on_split_is_absent_from_an_empty_settings_file() {
    let (_dir, prefs) = settings_file("[warpify.ssh]\nreuse_existing_control_master = true\n");

    assert_eq!(CloneSshOnSplit::read_from_preferences(&prefs), None);
}
