use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};

define_settings_group!(SshSettings,
    settings: [
        reuse_existing_control_master: ReuseExistingSshControlMaster {
            type: bool,
            default: false,
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
            surface: settings::SettingSurfaces::GUI,
            private: false,
            storage_key: "ReuseExistingSshControlMaster",
            toml_path: "warpify.ssh.reuse_existing_control_master",
            description: "Whether the legacy SSH wrapper attaches to an existing SSH ControlMaster for the destination host instead of always creating its own.",
        },
        clone_ssh_on_split: CloneSshOnSplit {
            type: bool,
            default: false,
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
            surface: settings::SettingSurfaces::GUI,
            private: false,
            storage_key: "CloneSshOnSplit",
            toml_path: "warpify.ssh.clone_ssh_on_split",
            description: "Whether splitting a pane that holds a warpified SSH session opens the new pane on that same SSH connection instead of a local shell.",
        },
    ]
);

#[cfg(test)]
#[path = "ssh_tests.rs"]
mod tests;
