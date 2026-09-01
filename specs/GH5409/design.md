---
status: in-progress
issue: 5409
tracking: amazeika/warp#1
pr: null
completed: [1, 2]
---

# Reuse the SSH connection when splitting a pane — Design Document

**Derived from:** `.ckit/scratch/warp-clone-ssh-on-pane-split.md`

Splitting a pane that holds a warpified SSH session currently opens a local shell on the user's
laptop. This design makes the new pane attach to the *same* SSH connection as the source pane, via
the ControlMaster socket Warp already creates for every warpified SSH session, and land in the same
remote working directory. Because the new pane multiplexes onto an already-authenticated
connection, it never re-authenticates — the split works identically for password, keyboard-
interactive, and 2FA destinations, not only for key-based ones.

## 1. Motivation

### Current state

A Warp split is a unique terminal session. `ssh` is a command running inside a local PTY, so
splitting a pane creates a new *local* shell: `add_session_with_default_session_mode_behavior`
resolves a startup directory through `WorkingDirectoryConfig`
([working_directory_config.rs](../../app/src/terminal/session_settings/working_directory_config.rs))
and spawns a local session ([pane_group/mod.rs:6636-6682](../../app/src/pane_group/mod.rs#L6636-L6682)).
`WorkingDirectoryMode::PreviousDir` copies an `Option<PathBuf>`, which is local by construction.

Warp already knows the session is remote, and already knows the remote working directory — it drives
the file tree — but none of that reaches pane creation.

Kitty and WezTerm treat the remote host as the session, so a split already knows the host and cwd.
The gap is tracked as [warpdotdev/warp#5409](https://github.com/warpdotdev/warp/issues/5409), which
asks specifically for the new pane to reuse the existing connection rather than dial a new one.

The machinery to do that is already shipped, and is the reason this design is small:

| Capability | Location |
| --- | --- |
| A ControlMaster is created for **every** warpified SSH session, at `$SSH_SOCKET_DIR/$WARP_SESSION_ID` | [zsh_body.sh:1046-1084](../../app/assets/bundled/bootstrap/zsh_body.sh#L1046-L1084), [bash_body.sh:1207](../../app/assets/bundled/bootstrap/bash_body.sh#L1207), [fish.sh:711](../../app/assets/bundled/bootstrap/fish.sh#L711) |
| `SSH_SOCKET_DIR` is injected into every local PTY | [local_tty/unix.rs:377](../../crates/warp_terminal/src/local_tty/unix.rs#L377), [:890](../../crates/warp_terminal/src/local_tty/unix.rs#L890) |
| The socket path is reported to the app and stored on the session | `IsSSHWrapperSession::Yes { socket_path, external_control_master }` — [session.rs:579-592](../../app/src/terminal/model/session.rs#L579-L592), populated at [:671-673](../../app/src/terminal/model/session.rs#L671-L673) |
| Warp already opens additional multiplexed channels on that master | `RemoteCommandExecutor` — [remote_command_executor.rs:16-19](../../app/src/terminal/model/session/command_executor/remote_command_executor.rs#L16-L19); the remote-server proxy — [zsh_body.sh:96-102](../../app/assets/bundled/bootstrap/zsh_body.sh#L96-L102) |
| Master ownership is tracked so teardown does not kill user-owned masters | `ControlPath::{WarpManaged, UserOwned}` — [ssh_transport.rs:268-279](../../app/src/remote_server/ssh_transport.rs#L268-L279) |
| New panes already accept environment variables | `create_terminal_pane_data(… env_vars …)` — [pane_group/mod.rs:6684-6700](../../app/src/pane_group/mod.rs#L6684-L6700) |
| New panes already accept startup commands | `set_pending_command_queue` — [view.rs:9550-9557](../../app/src/terminal/view.rs#L9550-L9557), used by tab configs at [mod.rs:1442-1445](../../app/src/pane_group/mod.rs#L1442-L1445) |

`RemoteCommandExecutor` passes `-o PasswordAuthentication=no` on the channels it opens today. That
is the load-bearing proof for this design: a client attaching to a live master never authenticates.

### Goal

Splitting a pane whose active session is a warpified SSH session established by the Warp SSH
wrapper opens a new pane on the *same* SSH connection, in the same remote directory, with no
authentication prompt.

Non-goals:

- Cloning `docker exec`, `mosh`, `kitten ssh`, or nested `sudo su`.
- SSH-like CLIs (`gcloud compute ssh`, `eb ssh`, `doctl compute ssh`) — they carry no wrapper
  socket.
- Splitting inside tmux as a substitute for a Warp pane.
- The same behavior for new tabs or new windows (the helper is written so this is a later
  follow-up).
- Changing Warpify triggering, the SSH extension install prompt, or OSC 7 handling.
- A user-facing plugin or URI action.

### Use cases

- A user SSHed into a build host at `/srv/app` splits the pane and immediately has a second shell
  at `/srv/app` on that host — no password re-entry, no second 2FA challenge.
- A user on a jump-hosted, password-authenticated database host splits three times to watch logs,
  run queries, and edit config, authenticating once.
- A user working locally splits a pane and gets today's behavior, unchanged.

## 2. Design

### Data flow

```
source pane (warpified SSH session)
  session.is_ssh_wrapper_session -> Yes { socket_path, external_control_master }
  active block metadata          -> remote cwd (raw string)
  bound warpify record           -> original ssh command
                     |
                     v  split
new pane
  env      WARP_SSH_ATTACH_CONTROL_PATH = socket_path   (one-shot)
  command  <original ssh command, verbatim>
                     |
                     v  wrapper sees the env var, ssh -O check passes
  ssh -o ControlMaster=no -o ControlPath=<socket_path> \
      -o ProxyCommand=false -o ProxyJump=none -t <dest> <bootstrap>
                     |
                     v  bootstrap completes, session becomes WarpifiedRemote
  submit  cd '<remote cwd>'
```

### Why the command is replayed verbatim

The wrapper only warpifies an `ssh` invocation with **exactly one positional argument**
([zsh_body.sh:1012-1014](../../app/assets/bundled/bootstrap/zsh_body.sh#L1012-L1014),
[bash_body.sh:1169](../../app/assets/bundled/bootstrap/bash_body.sh#L1169),
[fish.sh:673](../../app/assets/bundled/bootstrap/fish.sh#L673)), and the app-side parser agrees
([util.rs:126-133](../../app/src/terminal/ssh/util.rs#L126-L133)). Appending a remote command such
as `cd X && exec $SHELL -l` would push the invocation to two positionals, fall through to
`command ssh "$@"`, and produce a plain non-warpified pane with no remote cwd — which could not
itself be split. So the new pane's command must be the original command, unchanged.

Because the stored command already passed `parse_interactive_ssh_command`, it is guaranteed to have
one positional and no `-T`/`-W`. There is nothing to reconstruct: no flag walking, no `-t`
insertion, no argv quoting. The stored string is the alias-expanded form
([view.rs:12042-12049](../../app/src/terminal/view.rs#L12042-L12049)), so
`alias m='ssh -J bastion mini'` replays as the real command.

### Wrapper attach mode

Each shell's `warp_ssh_helper` currently computes:

```sh
local control_path="$SSH_SOCKET_DIR/$WARP_SESSION_ID"
local control_master_mode="yes"
local external_control_master="false"
```

Attach mode adds one branch before the existing `WARP_SSH_REUSE_CONTROL_MASTER` branch. When
`WARP_SSH_ATTACH_CONTROL_PATH` is set, passes the same character-safety filter the existing branch
applies to user-configured paths, and `ssh -O check` confirms a live master, the wrapper uses that
path with `control_master_mode="no"`.

**The request is consumed before anything else can return.** `warp_ssh_helper` has two fallbacks
that run plain `ssh` and return: one when it cannot mint a session id, and one when the destination
configures its own `RemoteCommand` (OpenSSH refuses to run Warp's bootstrap alongside it). Reading
the request later than those would break both guarantees at once. The pending attach would dial a
fresh connection, prompting for the credentials the split exists to avoid, and the variable would
survive into the next `ssh` in that pane. So the variable is read and unset on the first lines of
the function, and both fallbacks refuse rather than dial when a request is pending.

Reading it must also tolerate its absence. The variable is set only for split panes, so an
unguarded expansion aborts every wrapped `ssh` under `set -u` / `setopt nounset`, which this repo
supports (`test_zsh_bootstraps_with_nounset_option`). The neighbouring `WARP_SSH_REUSE_CONTROL_MASTER`
needs no such guard only because Warp always sets it to `"1"` or `"0"`
([unix.rs:368-371](../../crates/warp_terminal/src/local_tty/unix.rs#L368-L371)).

**An attaching session never owns the master.** It joined a connection someone else created, and
another pane — including the one it was split from — is still using it. So attach mode reports
`external_control_master=true` unconditionally. Reporting `false` would tag the shared socket
`ControlPath::WarpManaged` ([ssh_transport.rs:274-278](../../app/src/remote_server/ssh_transport.rs#L274-L278)),
and that pane's exit would run `ssh -O exit` and kill the connection for every other pane on it. The
session that created the master still reports `false` and keeps teardown responsibility, so
ownership stays with exactly one session.

**Attach mode is one-shot.** The variable lives in the pane's shell environment, so it would
otherwise outlive the attached session. After logging out the user is back at a local prompt in that
pane with the variable still set, and a subsequent `ssh other-host` would attach to the *original*
host's master and land them on the wrong machine. `ssh -O check` does not protect against this: it
proves the socket is live, not that it belongs to the requested destination.

A destination-match guard was considered as defense in depth and **rejected**. It would only close
the window in which a user types their own `ssh` in the microseconds between the pane being created
and the submitted command landing — a race against their own split. Implementing it portably costs
more than it is worth. zsh and bash expose the parsed positional through an implicitly global `ARGS`
array set by `is_interactive_ssh_session`
([zsh_body.sh:984](../../app/assets/bundled/bootstrap/zsh_body.sh#L984),
[bash_body.sh:1141](../../app/assets/bundled/bootstrap/bash_body.sh#L1141)), which is fragile
cross-function coupling to depend on. fish uses `argparse` and exposes no equivalent
([fish.sh:654-676](../../app/assets/bundled/bootstrap/fish.sh#L654-L676)), so the guard would mean
duplicating ssh option parsing into a third implementation. The one-shot unset closes the real hole.

One caveat follows from consuming the variable inside `warp_ssh_helper`: when the SSH wrapper is
disabled (`WARP_USE_SSH_WRAPPER != 1`) the helper never runs, so the variable is never consumed. That
is harmless, since nothing else reads it and attach cannot happen without the wrapper. It does mean
the variable's lifetime is "until the first wrapped `ssh`", not "until the first `ssh`".

**Attach mode fails closed.** When the variable is set but the master is not usable, the helper must
*not* fall through to creating a fresh master. By the time the wrapper runs, the split pane already
exists and the command has been submitted, so there is no way back to a local split. Falling through
would surface exactly the surprise password or 2FA prompt this design exists to remove, and would
violate the governing rule of preferring no clone over a wrong one. Instead the helper prints a
one-line explanation and exits non-zero, leaving the user at a local prompt with the `ssh` command in
their history, which they can run themselves if they want a fresh connection. The fall-through to a
Warp-owned master remains the behavior when the variable is *unset*, which is every session that is
not a split.

**`ssh -O check` alone does not make it fail closed.** `ControlMaster=no` only *prefers* the socket:
if the master dies between the probe and the connection, OpenSSH dials the destination directly and
prompts. Attach mode therefore also passes `-o ProxyCommand=false -o ProxyJump=none`. Neither option
is consulted when the socket is live, so they cost nothing on the success path, but they make a
direct connection impossible — the connection either rides the master or fails. Verified locally:
with the guard, `ssh` to a reachable host fails at `Connection closed by UNKNOWN` before any
authentication, where without it the same command reaches the host and prompts.

Failing closed in the wrapper covers the race between the app's preflight (below) and the dial. The
app also checks the master before spawning anything, so the common failure — a master that died
earlier — produces an ordinary local split with no pane churn at all.

The character filter is not optional. The control path is interpolated into the SSH hook JSON that
the remote side prints back ([zsh_body.sh:1093](../../app/assets/bundled/bootstrap/zsh_body.sh#L1093)),
which is why the existing branch rejects anything outside `[[:alnum:]._/~@:+,-]`.

### Connection lifetime

Warp sets no `ControlPersist` today, so the master is the source pane's foreground `ssh` and every
attached pane dies with it. This design adds `ControlPersist` to Warp-owned masters: the master
detaches at connect time, the connection is owned above any single pane, and panes may be closed in
any order.

This is a change to the lifetime of *every* Warp-owned SSH connection, including sessions that are
never split — a wide blast radius for a split feature, and one that would contradict this feature's
own reversibility story if it shipped unconditionally. It is therefore gated on the same feature
flag as the rest of this work: the flag's value is forwarded to the shell as an environment
variable, the way `WARP_SSH_REUSE_CONTROL_MASTER` already is, so persist mode is only active where
the feature is. With the flag off, no `ControlPersist` is added, `persist` is false, the forced
`ssh -O exit` still runs, and master lifetime is byte-for-byte today's behavior.

Teardown must change to match, and the reason it is *safe* to change is specific. `ssh -O exit`
exists because "the master is the user's interactive ssh process and, without the explicit `-O exit`,
it hangs waiting for remote-side cleanup of multiplexed channels"
([ssh.rs:53-62](../../crates/remote_server/src/ssh.rs#L53-L62),
[manager.rs:2359-2371](../../crates/remote_server/src/manager.rs#L2359-L2371)). Under `ControlPersist`
that premise no longer holds: the master detaches from the interactive `ssh` at connect time, so
there is no foreground process left to hang. Skipping the forced exit is therefore safe in persist
mode and **unsafe outside it** — it would reintroduce the hang.

Teardown must consequently be able to tell the two apart, and nothing in `ControlPath` or the SSH
hook carries that today. The wrapper therefore echoes a `persist` field in the SSH hook JSON beside
`external_control_master`, it is stored on the session the same way, and both the `ControlPersist`
flag and the `-O exit` skip key off it. `UserOwned` masters are untouched, as today.

The visible change for a single-pane session is that the connection lingers for the timeout after
the pane exits. Rejected alternatives: leaving the source pane load-bearing (closing it kills every
attached pane, with no signal to the user), and adding an attached-client check before teardown
(deterministic, but reintroduces refcount bookkeeping).

### Gating

The split clones only when all of the following hold, and otherwise falls back to today's local
split. Preferring no clone over a wrong clone is the governing rule.

| Condition | Source |
| --- | --- |
| Setting and feature flag enabled | `SshSettings`, `FeatureFlag` |
| Active session is remote | `SessionType::WarpifiedRemote { .. }` — the predicate used by `is_subshell_or_ssh` ([session.rs:1060-1064](../../app/src/terminal/model/session.rs#L1060-L1064)) |
| Session was established by the wrapper | `IsSSHWrapperSession::Yes { socket_path, .. }` ([session.rs:579-592](../../app/src/terminal/model/session.rs#L579-L592)) |
| A stored `ssh` command bound to this session id | new binding, see below |
| The master is live *now* | `ssh -O check -o ControlPath=<socket>` preflight before the pane is created |

The preflight matters because attach mode fails closed: once the pane exists there is no way back to
a local split, so a dead master must be detected before anything is spawned. The check is a
local-only probe, the same one the wrapper already uses for user-owned masters
([zsh_body.sh:1064](../../app/assets/bundled/bootstrap/zsh_body.sh#L1064)).

Two rejected gates, both from the source scratch, are recorded because they look plausible:

- `pwd_as_local_or_remote` returning `Remote` ([view.rs:23761-23788](../../app/src/terminal/view.rs#L23761-L23788))
  requires `host_id` to be `Some`, which it is not until the remote-server handshake completes and
  is never when that feature is off ([session.rs:911-918](../../app/src/terminal/model/session.rs#L911-L918)).
  Gating on it would silently require the SSH extension and fail across reconnects.
- `WarpifyState::should_prevent_input` ([trigger_state.rs:285-290](../../app/src/terminal/warpify/trigger_state.rs#L285-L290))
  is dead code, and `SshBlockState::should_prevent_input` returns `true` for the whole life of the
  warpified session ([trigger_state.rs:49-51](../../app/src/terminal/warpify/trigger_state.rs#L49-L51)),
  so requiring it to be false would disable the feature entirely.

Sessions warpified by the RC-file snippet rather than the wrapper carry no socket
([session.rs:575-578](../../app/src/terminal/model/session.rs#L575-L578)) and do not clone. Mid-login
sessions are not yet `WarpifiedRemote`, so invariant 6 holds without a separate check.

### Binding the ssh command to its session

`pending_command` is set at preexec ([view.rs:12111-12113](../../app/src/terminal/view.rs#L12111-L12113))
and already survives the banner — `clear_pending_ssh_host` nulls only the host
([trigger_state.rs:249-253](../../app/src/terminal/warpify/trigger_state.rs#L249-L253)), and the
whole `pending_state` is taken only on session completion
([trigger_state.rs:313-325](../../app/src/terminal/warpify/trigger_state.rs#L313-L325)).

The defect is that it is *unbound*. An inner `ssh other` run inside a remote session still
overwrites it at preexec even though it can never warpify (the wrapper is gated on
`WARP_IS_LOCAL_SHELL_SESSION == 1`, [zsh_body.sh:979](../../app/assets/bundled/bootstrap/zsh_body.sh#L979)),
so an unbound read could point the new pane at a destination reachable only through the bastion.

The fix is to bind `(SessionId, original_command)` when warpification starts —
`on_warpify_start` already receives the session id
([trigger_state.rs:307-310](../../app/src/terminal/warpify/trigger_state.rs#L307-L310)) — and to read
it only when it matches `active_block_session_id()`. The binding is made only for
`WarpifiedRemote` sessions, because `add_bootstrap_success_block` also fires for local subshell
warpification ([view.rs:9922-9924](../../app/src/terminal/view.rs#L9922-L9924)), and is cleared
alongside `pending_state` on session completion.

### Delivering the remote `cd`

The remote cwd comes from the active block's metadata
([block.rs:667-669](../../app/src/terminal/model/block.rs#L667-L669)) as a raw string — no
`RemotePath`, `StandardizedPath`, or `host_id` involved, so the SSH extension is not required and
the `local_fs`-gated `WorkingDirectoriesModel`
([working_directories.rs:258-308](../../app/src/pane_group/working_directories.rs#L258-L308)) stays
off the path.

It cannot be delivered through the command queue. `set_pending_command_queue` advances only when the
previous command's block completes ([view.rs:12341-12351](../../app/src/terminal/view.rs#L12341-L12351)),
and an interactive `ssh` block does not complete until logout — a queued `cd` would run on the
laptop after the user disconnects. Instead the new pane carries a pending remote cwd that is
submitted when its own session bootstraps
([view.rs:9902-9958](../../app/src/terminal/view.rs#L9902-L9958)), and dropped if the session ends
first or the SSH warpify timeout fires.

### Security

The remote cwd is remote-reported text submitted through the terminal input path. POSIX
single-quoting handles shell metacharacters but not newline or carriage return, which would split
the submitted line into a second command. The cwd is rejected outright — no `cd` is sent — if it
contains any control character. Sanitizing is deliberately not attempted.

The `WARP_SSH_ATTACH_CONTROL_PATH` value is generated by Warp, never by the remote host, and is
filtered against the same character set the existing ControlPath branch enforces before it reaches
the SSH hook JSON.

### Settings

A new `clone_ssh_on_split` bool joins `SshSettings`
([settings/ssh.rs](../../app/src/settings/ssh.rs)) under `warpify.ssh.clone_ssh_on_split`, surfaced
as a checkbox in `SSHWidget` ([warpify_page.rs:611](../../app/src/settings_view/warpify_page.rs#L611)),
mirroring the `reuse_existing_control_master` row at
[:692-713](../../app/src/settings_view/warpify_page.rs#L692-L713). The product default is on; it
ships behind a feature flag defaulting off until it has soaked.

## 3. Implementation

### Phase 1: Attach to a supplied ControlMaster in the shell wrapper

**ID:** `1`
**Goal:** an `ssh` run with `WARP_SSH_ATTACH_CONTROL_PATH` set to a live Warp master multiplexes
onto it instead of creating a new master, in all three bundled shells
**Tests:** `crates/integration/src/test/ssh_control_master_attach.rs`

**Acceptance criteria:**

- [ ] With the variable set to a live master's socket, `warp_ssh_helper` invokes `ssh` with
      `-o ControlMaster=no -o ControlPath=<that path>`.
- [ ] The second connection completes with no authentication prompt against a password-only test
      host.
- [ ] With the variable unset, the emitted `ssh` invocation is byte-identical to today's, including
      the existing fall-through to a Warp-owned master.
- [ ] With the variable set to a nonexistent, stale, or dead socket, the helper exits non-zero with
      a one-line explanation and **does not** create a new master or prompt for credentials.
- [ ] A value containing characters outside `[[:alnum:]._/~@:+,-]` is rejected and the helper fails
      closed the same way, so nothing unsafe reaches the SSH hook JSON.
- [ ] Attach mode reports `external_control_master=true` unconditionally, so an attaching session
      never becomes responsible for tearing down a master it joined.
- [ ] The request is read and unset on the first lines of `warp_ssh_helper`, before the
      session-id and `RemoteCommand` fallbacks, and both fallbacks refuse instead of dialing when a
      request is pending.
- [ ] With the variable absent, a wrapped `ssh` still works under `set -u` / `setopt nounset`.
- [ ] Attach mode passes `-o ProxyCommand=false -o ProxyJump=none`, so a master that dies after the
      `ssh -O check` probe cannot fall back to a direct connection.
- [ ] The wrapper unsets `WARP_SSH_ATTACH_CONTROL_PATH` before dialing, so a second `ssh` run in the
      same pane after logout creates its own connection and never attaches to the first host's
      master.
- [ ] zsh, bash, and fish behave identically on every criterion above.

**Steps:**

1. Add the attach branch in `warp_ssh_helper` in
   [zsh_body.sh](../../app/assets/bundled/bootstrap/zsh_body.sh),
   [bash_body.sh](../../app/assets/bundled/bootstrap/bash_body.sh), and
   [fish.sh](../../app/assets/bundled/bootstrap/fish.sh), ahead of the existing
   `WARP_SSH_REUSE_CONTROL_MASTER` branch, reusing its character filter and `ssh -O check` probe.
2. Read and unset the variable on the first lines of `warp_ssh_helper`, ahead of both plain-`ssh`
   fallbacks, and make those fallbacks refuse when a request is pending.
3. Add the variable to the Windows environment map if the wrapper is reachable there
   ([windows/environment.rs:30](../../crates/warp_terminal/src/local_tty/windows/environment.rs#L30)).

**Delivered.** Attach mode is implemented in all three bundled wrappers, with the request read and
unset on the first lines of `warp_ssh_helper` so both plain-`ssh` fallbacks refuse rather than dial.
Reads are nounset-safe, and the guard array is expanded with `${a[@]+"${a[@]}"}` because bash 3.2 --
still `/bin/bash` on macOS -- aborts on a quoted empty array under `set -u`. Attach mode reports
`external_control_master=true` unconditionally, which removed the planned `WARP_SSH_ATTACH_EXTERNAL`
channel entirely: an attaching session never created the master and must never tear it down.

Two deviations from the plan as written. `WARP_SSH_ATTACH_DESTINATION` was dropped: it only closed a
race against the user's own split, and implementing it portably would have meant duplicating ssh
option parsing into fish. The Windows environment-map entry (step 3) is not done and moves to Phase 4,
where the app side that sets the variable is built.

Known limitation carried into Phase 2: closing the pane that *created* the master still tears down
panes attached to it, because only the joining session reports external ownership. Phase 2's
`ControlPersist` work is what resolves this, and it lands before Phase 4 activates attach.

### Phase 2: Persist Warp-owned masters beyond their foreground client

**ID:** `2`
**Goal:** the SSH connection outlives the pane that created it, so panes can be closed in any order
**Tests:** `crates/integration/src/test/ssh_control_persist.rs`, `crates/remote_server/src/ssh_tests.rs`, `crates/warp_terminal/src/local_tty/unix_tests.rs`, `crates/warp_terminal/src/model/ansi/mod_tests.rs`, `crates/warp_terminal/src/model/ansi/dcs_hooks_tests.rs`

**Acceptance criteria:**

- [ ] With the feature flag on, Warp-owned masters are created with `ControlPersist` and the SSH
      hook reports `persist: true`; user-owned masters are unchanged.
- [ ] With the feature flag off, no `ControlPersist` is added, `persist` is false, the forced
      `ssh -O exit` still runs, and lifetime and teardown are byte-for-byte today's behavior.
- [ ] Closing the pane that created the connection leaves the master alive and `ssh -O check`
      still succeeds.
- [ ] Teardown skips `ssh -O exit` **only** for masters whose session reports `persist: true`; a
      non-persist Warp-managed master still gets the forced exit and still does not hang on exit.
- [ ] `UserOwned` masters are still never torn down by Warp.
- [ ] After the last client exits, the master is gone once the idle timeout elapses.
- [ ] A master orphaned by an app crash does not survive indefinitely, and its stale socket does not
      cause a later session to attach to a dead connection.

**Steps:**

1. Add the feature flag per the `add-feature-flag` skill — it is needed here, not in Phase 6, because
   it gates this lifetime change.
2. Forward the flag to the shell as an environment variable, following the
   `WARP_SSH_REUSE_CONTROL_MASTER` pattern.
3. Add `ControlPersist` to the `command ssh -o ControlMaster=…` invocation in all three shells,
   gated on that variable, and echo a `persist` field in the SSH hook JSON beside
   `external_control_master`.
4. Store `persist` on the session alongside `external_control_master`
   ([session.rs:669-675](../../app/src/terminal/model/session.rs#L669-L675)) and key the teardown
   skip on it ([ssh_transport.rs:268-279](../../app/src/remote_server/ssh_transport.rs#L268-L279)).
5. Confirm `RemoteCommandExecutor` and the remote-server proxy still attach correctly to a
   persisted master.

**Delivered.** `ControlPersist=60` is added only where Warp creates the master — the attach branch
and the user-master branch both set `control_master_mode=no` before the persist block, so a master
this session joined never has its lifetime extended. The value travels as a `persist` field in the
SSH hook JSON, through `SSHValue` and `IsSSHWrapperSession::Yes`, to
`ControlPath::WarpManaged { socket_path, persist }`, where teardown skips the forced `ssh -O exit`.
Everything is gated on the new `FeatureFlag::CloneSshOnSplit`, which is in no flag array: with it
off the `ssh` options, and so master lifetime and teardown, are exactly what they were before.

Three decisions worth recording. `ControlPath::WarpManaged` became a struct variant rather than a
fourth enum variant, so `UserOwned` and `None` are untouched and no catch-all arm appears over the
enum. The teardown decision was extracted into a pure `socket_to_force_exit`, so ownership and
persistence rules are unit-tested without spawning `ssh`. And `WARP_SSH_CONTROL_PERSIST` is read
nounset-safe, matching the idiom Phase 1 established in this function rather than the bare read its
neighbour uses.

One deviation from the phase as planned: `fish.sh` gained `export WARP_IS_SSH='1'`, which zsh and
bash have sent since the initial public release. That variable installs the remote `ExitShell` hook,
and `ExitShell` is what releases Warp's proxy child so the idle timeout can start — dormant before
this phase, load-bearing after it. Making a phase's own precondition true is not scope creep, but it
does touch a Phase 1 file. `.ckit/conventions.yaml` also gained four unit-test rows, since `nextest`
selects by name substring and the phase's declared test files had no gate.

Adversarial review of this phase raised, and this phase does not fix, whether skipping `-O exit` is
safe when Warp cannot guarantee the idle timeout ever starts. That is recorded in Open Questions
with a `TODO(doubt)` breadcrumb at the code site, and gates enabling the flag rather than shipping
the phase.

### Phase 3: Bind the originating ssh command to its warpified session

**ID:** `3`
**Goal:** the app can ask a terminal view for the `ssh` command that created its *current* remote
session, with no risk of returning a stale or inner-session command
**Tests:** pending

**Acceptance criteria:**

- [ ] The `(SessionId, command)` pair is recorded when a `WarpifiedRemote` session starts.
- [ ] It is not recorded for local subshell warpification.
- [ ] The accessor returns `None` when the bound session id does not match
      `active_block_session_id()`.
- [ ] Running a non-warpifying `ssh` inside a remote session does not change what the accessor
      returns for that session.
- [ ] The binding is cleared when the warpified session completes.

**Steps:**

1. Store the bound pair in `WarpifyState` ([trigger_state.rs](../../app/src/terminal/warpify/trigger_state.rs)),
   set from `on_warpify_start` and guarded on session type.
2. Clear it where `pending_state` is taken in `get_completed_warpify_session_id`.
3. Expose a validating accessor on `TerminalView`.

### Phase 4: Open the split pane on the source pane's connection

**ID:** `4`
**Goal:** splitting a warpified, wrapper-established SSH pane produces a second pane on the same
connection, at the remote default directory, with no authentication prompt
**Tests:** pending

**Acceptance criteria:**

- [ ] Splitting such a pane creates a pane whose env carries `WARP_SSH_ATTACH_CONTROL_PATH` and
      the source session's socket, and whose first command is the bound `ssh` command verbatim.
- [ ] The new pane reaches a warpified remote session on the same host with no auth prompt.
- [ ] The new pane is itself splittable, producing a third pane on the same connection.
- [ ] Splitting a local pane is unchanged, including `WorkingDirectoryConfig` handling.
- [ ] No clone when: the session is not `WarpifiedRemote`; it is `IsSSHWrapperSession::No`; no bound
      command matches; SSH is still authenticating; or the `ssh -O check` preflight fails.
- [ ] When the preflight fails, an ordinary local split happens and no pane is created and
      discarded.
- [ ] Splitting from a non-terminal pane never attempts a clone.
- [ ] Closing either pane leaves the other alive and usable.
- [ ] After logging out of the attached session, running `ssh <a different host>` in that same pane
      connects to that host, not to the original one.
- [ ] Splitting a session that attached to a *user-owned* master works, and the new session reports
      `external_control_master: true` so Warp never tears that master down.
- [ ] On a host whose `sshd` refuses a further multiplexed session (`MaxSessions` exhausted), the
      user gets a clear failure rather than a silently dead pane.
- [ ] On Windows/WSL the env vars are set only when the new pane spawns in the same distro as the
      source session, since the socket lives inside the distro.

**Steps:**

1. Add a helper that inspects the focused terminal pane, runs the `ssh -O check` preflight, and
   returns an optional clone request (socket path, command, local cwd).
2. Branch in `add_session_with_default_session_mode_behavior`
   ([mod.rs:6636-6682](../../app/src/pane_group/mod.rs#L6636-L6682)), keeping the branch small and
   delegating to the helper.
3. Pass `WARP_SSH_ATTACH_CONTROL_PATH` through `create_terminal_pane_data`, and the command through
   `set_pending_command_queue`.

### Phase 5: Land the split pane in the source pane's remote directory

**ID:** `5`
**Goal:** the new pane's remote shell starts in the same remote directory as the source pane
**Tests:** pending

**Acceptance criteria:**

- [ ] After bootstrap, the new pane's remote cwd equals the source pane's remote cwd.
- [ ] A remote cwd containing spaces, single quotes, `$`, backticks, or non-ASCII characters is
      entered correctly.
- [ ] A remote cwd containing any control character is rejected and no `cd` is submitted.
- [ ] When the remote cwd is unknown, the pane lands in the remote default and no `cd` is
      submitted.
- [ ] The pending `cd` is dropped if the session ends before bootstrap or the warpify timeout
      fires.
- [ ] The `cd` is never submitted into a local session.
- [ ] The `cd` is not routed through `set_pending_command_queue`.

**Steps:**

1. Add `posix_single_quote` and a control-character validator to
   [ssh/util.rs](../../app/src/terminal/ssh/util.rs).
2. Carry a pending remote cwd on the new `TerminalView`; submit on the bootstrap-success path
   ([view.rs:9902-9958](../../app/src/terminal/view.rs#L9902-L9958)); clear on session end and on
   timeout.

### Phase 6: Expose the setting and gate the feature

**ID:** `6`
**Goal:** users can turn the behavior off, and it ships disabled until it has soaked
**Tests:** pending

**Acceptance criteria:**

- [ ] `warpify.ssh.clone_ssh_on_split` exists in `SshSettings` and round-trips through
      `~/.warp/settings.toml`.
- [ ] The Warpify settings page shows the checkbox in the SSH section and it is discoverable via
      settings search.
- [ ] With the setting off, splitting an SSH pane produces a local pane.
- [ ] With the feature flag off, the behavior is entirely absent regardless of the setting,
      including the Phase 2 lifetime change.
- [ ] Telemetry records clone attempted, succeeded, and fell-back-to-local.

**Steps:**

1. Add the setting to [settings/ssh.rs](../../app/src/settings/ssh.rs) and the widget row to
   [warpify_page.rs](../../app/src/settings_view/warpify_page.rs), following the
   `reuse_existing_control_master` pattern.
2. Add telemetry per `add-telemetry` (the feature flag already exists from Phase 2).
3. Add the changelog entry.

### Phase 7: Full test sweep

**ID:** `7`
**Goal:** every test declared by this spec is green together
**Tests:** all

**Acceptance criteria:**

- [ ] The union of test paths declared by completed phases passes through the scoped resolver.
- [ ] Failures surfaced by the sweep are remediated in this phase.

### Phase 8: Outcome

**ID:** `8`

Reconcile delivered behavior, deviations, decisions, deferred work, and the full-sweep result into
`Outcome`.

### Phase 9: Documentation

**ID:** `9`

Invoke `/ckit:docs` for the shipped behavior and verification instructions.

## 4. Verification

Run against a key-based host and a **password-only** host; the second is the case this design
exists for.

- [ ] Local shell → split → new local pane in the same directory (today's behavior).
- [ ] `ssh host` in `$HOME` → split → second pane on that host, no prompt.
- [ ] `ssh host`, `cd /tmp` → split → new pane's `pwd` is `/tmp`, `hostname` matches.
- [ ] `cd "/tmp/dir with spaces"` → split → lands in that directory.
- [ ] Password-authenticated host → split → **no second password prompt**.
- [ ] 2FA host → split → no second challenge.
- [ ] `ssh -J bastion host` → split → second pane reaches the host without re-traversing the jump.
- [ ] Split the split → third pane on the same connection.
- [ ] In a split pane, log out, then `ssh` a *different* host → lands on that host, not the first.
- [ ] Close the origin pane → the other panes stay alive and usable.
- [ ] Close every pane → the master is reaped after the idle timeout.
- [ ] At a password prompt → split → local pane, no prompt storm.
- [ ] `ssh host ls` (exits) → split → local pane.
- [ ] Session warpified by the RC snippet rather than the wrapper → split → local pane.
- [ ] Source session attached to a user-owned master → split works, and Warp never runs `ssh -O exit`
      against that master.
- [ ] Flag off → an ordinary SSH session exits without hanging (the `-O exit` path still runs).
- [ ] Host with a low `sshd` `MaxSessions` → split beyond the limit fails visibly, not silently.
- [ ] Setting off, and flag off → split → local pane.
- [ ] Full-screen TUI (`vim`, `htop`) running over SSH → split → still works.
- [ ] Split from an editor pane beside an SSH pane → no clone.
- [ ] `./script/presubmit` passes; a narrated screen recording is captured for the upstream PR.

## 5. Open Questions

- **Default on or off at GA?** The product intent is on; Warp will likely want the flag off through
  at least one release. Settle on the spec PR.
- **`ControlPersist` value.** *Resolved in Phase 2: `ControlPersist=60`.* Long enough that any
  split-then-close ordering keeps the connection, short enough to bound how long an authenticated
  session lingers past visible use.
- **Should the source pane's local launch directory be snapshotted for the new pane's local PTY?**
  Attaching needs no credentials, so relative `-i`/`-F` no longer matter and this is now cosmetic —
  the local PTY is a thin shell that immediately enters SSH. Deferred unless verification shows it
  matters.
- **New tab and new window from an SSH pane.** Out of scope here; the Phase 4 helper is written so
  this is a small follow-up.
- **Windows/WSL.** Whether the wrapper attach path is reachable and correct there is unexamined
  beyond the same-distro constraint recorded in Phase 4.
- **Does `ControlPersist` need a last-client check after all?** Section 2 rejected an
  attached-client check before teardown as "reintroduces refcount bookkeeping", on the premise that
  the idle timeout reaps a master Warp stops force-exiting. Two independent reviews of Phase 2
  challenged that premise on evidence, and it is unresolved:
  - *The timeout may never start.* `ControlPersist` counts idle from when the last multiplexed
    client is gone. Warp's own `ssh … remote-server-proxy` child is dropped only by
    `deregister_session`, which fires on `ExitShell`. Any session that never emits `ExitShell`
    holds the master open indefinitely.

    The reach is wide. `SshRemoteServer` is in both `DOGFOOD_FLAGS`
    ([lib.rs:1055](../../crates/warp_features/src/lib.rs#L1055)) and `RELEASE_FLAGS`
    ([:1088](../../crates/warp_features/src/lib.rs#L1088)), so that proxy exists for essentially
    every SSH session, not only splits. And `ExitShell` is a *remote* DCS hook: on an abrupt pane
    or tab close the local client dies first, so the hook never arrives. That is a likelier path
    into this state than the clean-logout case.

    One reproducible instance is closed. `fish.sh`'s remote command never exported
    `WARP_IS_SSH='1'`, which zsh and bash have sent since the initial public release, and that
    variable is what installs the remote `ExitShell` hook. Phase 2 adds it, because Phase 2 is what
    makes `ExitShell` load-bearing. Remote *fish* needs nothing here: all three wrappers warpify
    only `bash` and `zsh` remotely, so a remote fish login shell is never warpified and never gets
    a proxy to release.
  - *A flag flip can sever a split.* `WARP_SSH_CONTROL_PERSIST` is captured at pane spawn
    ([unix.rs:376](../../crates/warp_terminal/src/local_tty/unix.rs#L376)), not read live. Just
    after the flag turns on, a pane spawned before it still holds `0` and creates a non-persistent
    master. A split of that pane attaches to a master whose teardown still runs the forced
    `ssh -O exit`, so closing the source pane severs the split. It degrades to Phase 1 behaviour
    rather than breaking anything new, but Phase 4 should gate the attach request on the source
    session's hook reporting `persist: true`, rather than on the flag.
  - *Port forwarding outlives the visible session.* `-L`/`-R`/`-D` pass the warpify gate
    (`is_interactive_ssh_session`'s option list accepts them), so a forwarding session gets a
    Warp-owned master. Under `ControlPersist` its listeners survive pane close for the timeout —
    a revocation boundary the design did not weigh.
  Provisional choice: ship the phase as specified, flag off, and settle this before the flag is
  enabled anywhere. Options are a last-client force-exit, excluding forwarding invocations from
  persistence, or accepting a bounded window and documenting it.
- **`sshd` `MaxSessions` budget.** Each split consumes an interactive channel plus a
  remote-server-proxy channel, so the default budget of 10 allows fewer splits than it appears.
  Whether to surface a specific message at the limit, or simply fail visibly, is unsettled.

## Outcome

<!-- Build fills this after implementation, including the full-sweep result. Keep it brief. -->
