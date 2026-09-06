use extension_protocol::{ExtensionManifest, PanelLocation};

use super::*;

fn manifest(id: &str, extra: &str) -> ExtensionManifest {
    let source = format!(
        r#"
id = "{id}"
name = "Name of {id}"
version = "0.1.0"
api_version = 1
command = "./plugin"
{extra}
"#
    );
    let manifest = ExtensionManifest::parse(&source).expect("manifest parses");
    manifest.validate().expect("manifest is valid");
    manifest
}

const ONE_COMMAND: &str = r#"
[[commands]]
id = "do.thing"
title = "Do the thing"
"#;

#[test]
fn a_registered_command_can_be_resolved_back_to_its_extension() {
    let mut contributions = Contributions::default();
    contributions.register(&manifest("dev.warp.a", ONE_COMMAND));

    let command = contributions
        .command("dev.warp.a", "do.thing")
        .expect("the command is registered");
    assert_eq!(command.title, "Do the thing");
    assert_eq!(command.extension_name, "Name of dev.warp.a");
    assert!(
        contributions.command("dev.warp.b", "do.thing").is_none(),
        "two extensions may use the same command id, so the extension is part of the key"
    );
}

#[test]
fn registering_again_replaces_rather_than_duplicates() {
    let mut contributions = Contributions::default();
    contributions.register(&manifest("dev.warp.a", ONE_COMMAND));
    contributions.register(&manifest(
        "dev.warp.a",
        "\n[[commands]]\nid = \"other.thing\"\ntitle = \"Other\"\n",
    ));

    let ids: Vec<&str> = contributions
        .commands()
        .map(|command| command.command_id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["other.thing"],
        "a restart after an edit must not leave the old command behind"
    );
}

#[test]
fn unregistering_removes_every_surface_the_extension_offered() {
    let mut contributions = Contributions::default();
    contributions.register(&manifest(
        "dev.warp.a",
        "\n[[commands]]\nid = \"c\"\ntitle = \"C\"\n\n[[panels]]\nid = \"p\"\ntitle = \"P\"\nlocation = \"left\"\n",
    ));
    contributions.register(&manifest("dev.warp.b", ONE_COMMAND));

    contributions.unregister("dev.warp.a");

    assert_eq!(contributions.commands().count(), 1);
    assert_eq!(contributions.panels().count(), 0);
    assert!(!contributions.is_empty());
}

#[test]
fn panels_keep_the_location_the_manifest_asked_for() {
    let mut contributions = Contributions::default();
    contributions.register(&manifest(
        "dev.warp.a",
        "\n[[panels]]\nid = \"p\"\ntitle = \"P\"\nlocation = \"right\"\n",
    ));

    let panel = contributions.panels().next().expect("the panel registers");
    assert_eq!(panel.location, PanelLocation::Right);
    assert_eq!(panel.panel_id, "p");
}

#[test]
fn commands_are_ordered_by_extension_so_the_palette_is_stable() {
    let mut contributions = Contributions::default();
    contributions.register(&manifest("dev.warp.z", ONE_COMMAND));
    contributions.register(&manifest("dev.warp.a", ONE_COMMAND));

    let owners: Vec<&str> = contributions
        .commands()
        .map(|command| command.extension_id.as_str())
        .collect();
    assert_eq!(owners, ["dev.warp.a", "dev.warp.z"]);
}
