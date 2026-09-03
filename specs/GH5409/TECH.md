# Reuse the SSH connection when splitting a pane — Tech Spec

Tracking issue: [warpdotdev/warp#5409](https://github.com/warpdotdev/warp/issues/5409)

## Context

A Warp split is a new terminal session. `ssh` is a command running inside a local PTY, so splitting
a pane that holds an SSH session opens a new *local* shell:
`add_session_with_default_session_mode_behavior` resolves a startup directory through
`WorkingDirectoryConfig` and spawns a local session. Kitty and WezTerm treat the remote host as the
session, so a split there already knows the host. Issue #5409 asks specifically for the new pane to
reuse the existing connection rather than dial a new one — which also means no second
authentication, including on password, keyboard-interactive and 2FA destinations.

The machinery for that is already shipped, which is why this change is small:

| Capability | Location |
| --- | --- |
| A ControlMaster is created for every warpified SSH session, at `$SSH_SOCKET_DIR/$WARP_SESSION_ID` | `zsh_body.sh`, `bash_body.sh`, `fish.sh` |
| `SSH_SOCKET_DIR` is injected into every local PTY | `local_tty/unix.rs` |
| The socket path is reported to the app and stored on the session | `IsSSHWrapperSession::Yes` — `terminal/model/session.rs` |
| Warp already opens additional multiplexed channels on that master | `RemoteCommandExecutor` — `terminal/model/session/command_executor/`, and the remote-server proxy |
| Master ownership is tracked so teardown does not kill user-owned masters | `ControlPath` — `crates/remote_server/src/transport.rs` |
| New panes already accept environment variables and startup commands | `create_terminal_pane_data`, `set_pending_command_queue` |

`RemoteCommandExecutor` passes `-o PasswordAuthentication=no` on the channels it opens today. That
is the proof the approach works: a client attaching to a live master never authenticates.

## Approach

```
source pane (warpified SSH session)
  session.is_ssh_wrapper_session -> Yes { socket_path, external_control_master, persist }
  bound warpify record           -> original ssh command
                     |
                     v  split
new pane
  env      WARP_SSH_ATTACH_CONTROL_PATH = socket_path   (one-shot)
  command  <original ssh command, verbatim>
                     |
                     v  wrapper sees the env var, `ssh -O check` passes
  ssh -o ControlMaster=no -o ControlPath=<socket_path> \
      -o ProxyCommand=false -o ProxyJump=none -t <dest> <bootstrap>
```

### Why the command is replayed verbatim

The wrapper only warpifies an `ssh` invocation with exactly one positional argument, and the
app-side parser agrees (`terminal/ssh/util.rs`). Appending a remote command would push the
invocation to two positionals, fall through to `command ssh "$@"`, and produce a plain
non-warpified pane — which could not itself be split. Because the stored command already passed
`parse_interactive_ssh_command`, there is nothing to reconstruct: no flag walking, no `-t`
insertion, no argv quoting. The stored string is the alias-expanded form, so
`alias m='ssh -J bastion mini'` replays as the real command.

### Attach mode in the wrapper

`WARP_SSH_ATTACH_CONTROL_PATH` is read and unset at the top of `warp_ssh_helper`, so it applies to
exactly one invocation. Attach mode then fails closed: the path is character-checked, `ssh -O check`
must succeed, and `ProxyCommand=false`/`ProxyJump=none` are set so that if the master dies between
the check and the connection, `ssh` fails rather than silently dialling the destination and
prompting for credentials. Every refusal prints a reason and returns non-zero rather than falling
back to a plain `ssh`.

An attaching session reports `external_control_master: true` and `persist: false` — it joined a
master it did not create, so its lifetime is not the attacher's to extend or end.

### Connection lifetime

A master must outlive the foreground `ssh` that created it, or closing the source pane would sever
every split made from it. Warp-owned masters are therefore created with `ControlPersist=60`, gated
on the feature flag and the user's setting so that a build with either off behaves exactly as
before.

`ControlPersist` only starts its idle timer once every multiplexed client is gone, and Warp's own
remote-server proxy is a client. Releasing that proxy when a pane goes away is what lets the master
expire; `deregister_session`'s only non-failure caller is the remote `ExitShell` hook, which an
abruptly closed pane never sends, so the local-shell-exit path releases it as a backstop.

Persistence also changes how the socket must be named. `$SSH_SOCKET_DIR/$WARP_SESSION_ID` is unique
only while the master dies with its `ssh`; once it persists, a second `ssh` in the same pane finds
the socket still present, and OpenSSH warns and disables multiplexing. Worse, if that second
session reached a different host, a live master for the *previous* host sits at the path a split
would attach to, and OpenSSH does not verify that a destination matches the master behind a
`ControlPath`. Persistent masters are therefore named per connection.

## Gating

A split attaches only when all of these hold. Every check fails closed to an ordinary local split:

| Condition | Why |
| --- | --- |
| `FeatureFlag::CloneSshOnSplit` | Rollout gate; also keeps the setting off the settings page |
| `warpify.ssh.clone_ssh_on_split` | The user's opt-in, default off |
| SSH warpification enabled | A pane spawned with the wrapper off never reaches `warp_ssh_helper`, so the replayed `ssh` would dial the host itself |
| Source session is `WarpifiedRemote` with a wrapper socket | A session warpified by the RC-file snippet carries no socket |
| `persist \|\| external_control_master` | A master that dies with the source pane would sever the split |
| Target shell is bash, zsh or fish | `warp_ssh_helper` is defined only in those bootstraps |
| Target WSL distro matches the source's | The socket lives inside the source's distro |

## Testing

- `terminal/ssh/clone_on_split_tests.rs` — the gate and every decline reason.
- `terminal/ssh/util_tests.rs` — `ssh` command parsing.
- `terminal/view_tests.rs` — the attach outcome's lifetime; the in-band precmd CWD regression.
- `pane_group/mod_tests.rs`, `workspace/view_tests.rs`, `undo_close/stack_tests.rs` — pane, tab and
  window teardown releasing the sessions that hold masters open.
- `crates/integration` — the wrapper itself against a stub `ssh`: attach mode, its one-shot
  consumption, fail-closed paths, `ControlPersist`, and per-connection socket naming.

The integration tests fail closed inside the wrapper before dialling, so they run offline. No test
here completes a real SSH handshake.

## Known limitations

- **The new pane starts in the remote default directory**, not the source pane's. Landing it in the
  source's directory was implemented and withdrawn: the pending directory was discarded before the
  remote session could enter it, and repairing it was out of scope for what #5409 asks. It is a
  clean follow-up.
- **New tab and new window** from an SSH pane are unchanged; only splits attach.
- **Windows and WSL** are reachable in the plumbing but were not exercised on hardware.
- **Outcome telemetry is a floor, not a partition.** A pane closed outright delivers no `Exit`, so
  an attach that failed and was then closed reports nothing.
- **`add_to_undo_stack` is not a moved-or-closed signal.** A tab removed without an undo entry may
  have been moved to another window, so its detach stays reversible; the cost is that a tab closed
  without an undo entry does not release its SSH sessions.
- Not covered: `docker exec`, `mosh`, `kitten ssh`, nested `sudo su`, SSH-like CLIs
  (`gcloud compute ssh`, `eb ssh`, `doctl compute ssh`) — none carry a wrapper socket.
