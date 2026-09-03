use super::*;

fn shell_starter(shell_type: ShellType, shell_path: &str) -> DirectShellStarter {
    DirectShellStarter::new_for_test(shell_type, PathBuf::from(shell_path), Vec::new())
}

fn env_value(command: &Command, key: &str) -> Option<Option<String>> {
    command
        .get_envs()
        .find(|(env_key, _)| *env_key == std::ffi::OsStr::new(key))
        .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
}

#[test]
fn host_bash_command_sets_history_size_sentinels() {
    let command = build_host_shell_command(
        shell_starter(ShellType::Bash, "/bin/bash"),
        None,
        HashMap::new(),
        None,
        false,
        false,
        false,
        false,
        false,
        true,
    );

    assert_eq!(
        env_value(&command, "HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
}

#[test]
fn host_non_bash_command_does_not_set_history_size_sentinels() {
    let command = build_host_shell_command(
        shell_starter(ShellType::Zsh, "/bin/zsh"),
        None,
        HashMap::new(),
        None,
        false,
        false,
        false,
        false,
        false,
        true,
    );

    assert_eq!(env_value(&command, "HISTFILESIZE"), None);
    assert_eq!(env_value(&command, "HISTSIZE"), None);
    assert_eq!(env_value(&command, "WARP_INITIAL_HISTFILESIZE"), None);
    assert_eq!(env_value(&command, "WARP_INITIAL_HISTSIZE"), None);
}

#[test]
fn docker_sandbox_command_sets_history_size_sentinels() {
    let docker_starter =
        DockerSandboxShellStarter::new(shell_starter(ShellType::Bash, "sbx"), None);
    let command = build_docker_sandbox_command(
        &docker_starter,
        None,
        HashMap::new(),
        false,
        false,
        false,
        false,
        false,
        true,
    );

    assert_eq!(
        env_value(&command, "HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
}

/// Builds a host-shell command whose only interesting input is
/// `clone_ssh_on_split`, so the ControlPersist tests below cannot be broken by a
/// bool accidentally transposed between the builder's five positional flags.
fn host_command_with_clone_on_split(clone_ssh_on_split: bool) -> Command {
    build_host_shell_command(
        shell_starter(ShellType::Zsh, "/bin/zsh"),
        None,
        HashMap::new(),
        None,
        /* enable_ssh_wrapper */ false,
        /* reuse_ssh_control_master */ false,
        clone_ssh_on_split,
        /* shell_debug_mode */ false,
        /* honor_ps1 */ false,
        /* node_version_chip_enabled */ true,
    )
}

/// `ControlPersist` changes the lifetime of every Warp SSH connection, split or
/// not, so the wrapper only gets it once the user has actually opted in. The env
/// var is the whole contract between `PtyOptions::clone_ssh_on_split` and the
/// bootstrap script.
#[test]
fn host_command_forwards_control_persist_when_clone_on_split_is_enabled() {
    let command = host_command_with_clone_on_split(true);

    assert_eq!(
        env_value(&command, "WARP_SSH_CONTROL_PERSIST"),
        Some(Some("1".to_owned()))
    );
}

/// With the option off the variable must still be set, and set to "0". Leaving it
/// unset would let a stale value inherited from the environment turn persist mode
/// on; "0" is what the wrapper effectively had before this feature existed, since
/// the bootstrap treats every value other than "1" as off.
#[test]
fn host_command_forwards_control_persist_off_when_clone_on_split_is_disabled() {
    let command = host_command_with_clone_on_split(false);

    assert_eq!(
        env_value(&command, "WARP_SSH_CONTROL_PERSIST"),
        Some(Some("0".to_owned()))
    );
}

/// The flag alone must not turn persist on. It reaches this command only through
/// `PtyOptions::clone_ssh_on_split`, which the app sets from the flag AND the
/// `clone_ssh_on_split` setting — so a user who left the setting off pays nothing
/// for the feature even in a build where the flag is enabled.
#[test]
fn host_command_ignores_the_feature_flag_when_clone_on_split_is_disabled() {
    let _guard = FeatureFlag::CloneSshOnSplit.override_enabled(true);

    let command = host_command_with_clone_on_split(false);

    assert_eq!(
        env_value(&command, "WARP_SSH_CONTROL_PERSIST"),
        Some(Some("0".to_owned()))
    );
}
