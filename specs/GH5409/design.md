---
status: in-progress
issue: 5409
tracking: amazeika/warp#1
pr: null
completed: [1, 2, 3, 4, 5, 6, 7]
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

Failing closed in the wrapper is the *only* liveness gate. An earlier draft of this design also ran
`ssh -O check` app-side, before the split; that check was removed. The wrapper re-runs the probe
itself immediately before attaching, so an app-side copy can never be authoritative: a master may
die in the window between the two. Every outcome it changed was cosmetic, and its cost was not.
Because the decision has to precede pane creation, the probe would have to finish before the split
gesture put anything on screen. A wedged socket would then freeze the window for the whole timeout
— exactly the case the probe existed for. Without it the pane appears immediately and the wrapper
reports the failure inside it.

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
| The source master outlives the source pane | `persist` or `external_control_master` on `IsSSHWrapperSession::Yes` |

Every gate is a synchronous field read, so the decision costs nothing and the split stays instant.
Master liveness is deliberately *not* among them — see "Attach mode fails closed" above.

The master-survival gate is what keeps a split alive after its source pane closes. Teardown force-
exits a master only when Warp owns it *and* it is non-persistent
([ssh.rs:73-88](../../crates/remote_server/src/ssh.rs#L73-L88)), so both a persistent Warp master
and a user-owned one are safe to attach to. The gate is on the source session's *reported* `persist`
rather than on the feature flag, because `WARP_SSH_CONTROL_PERSIST` is captured at pane spawn
([unix.rs:376](../../crates/warp_terminal/src/local_tty/unix.rs#L376)): a pane spawned before the
flag turned on still holds a non-persistent master.

The split decision is made where the user actually asks for a split. Both entry points that carry
that intent — `PaneGroupAction::Add` from the pane-group bindings, and the terminal's own
`PaneEvent::Split*` from its context menu and split actions — route through
`PaneGroup::split_terminal_pane`, which takes the pane being split and passes the resulting request
down explicitly. Resolving from the *pane being split* rather than the active session is
load-bearing: `active_session_id` is only updated when focus moves to a terminal pane, so with an
editor or notebook pane focused it still names the last terminal, and a split there would clone a
connection belonging to a pane the user did not split. A non-terminal source yields no terminal
view and so no request.

The decision is deliberately *not* made inside `add_session_with_default_session_mode_behavior`,
which is generic pane creation shared by six non-split callers that each install their own command
into the new pane: the LSP log viewer ([view.rs:17821](../../app/src/workspace/view.rs#L17821)), the
`CopyFileToRemote` uploader ([terminal_pane.rs:1221](../../app/src/pane_group/pane/terminal_pane.rs#L1221)),
the workflow runner ([view.rs:17755](../../app/src/workspace/view.rs#L17755)), the plugin
instructions pane ([view.rs:19345](../../app/src/workspace/view.rs#L19345)), the editor fallback
([view.rs:6481](../../app/src/workspace/view.rs#L6481)), and local continuation of a third-party
conversation ([view.rs:13666](../../app/src/workspace/view.rs#L13666)), which explicitly wants a
local session. No parameter of that function distinguishes them: the agent-mode caller passes
`base_pane_id_for_split: None` with a context pane set, while `add_terminal_pane` — documented "not
splitting panes" — passes `Some(focused_pane_id)`.

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
([view.rs:9902-9958](../../app/src/terminal/view.rs#L9902-L9958)).

**Its lifetime is the one attach attempt the split was made for**, and that boundary is what keeps a
directory named by one host from being entered on another. The pending cwd is dropped when the
replayed `ssh` completes as a user block, and as a backstop when the pane's local shell exits. The
`ssh` block completing is the load-bearing one: on the success path that block stays open until
logout, long after bootstrap consumed the cwd, so reaching it with one still pending means the
attempt ended without warpifying — which is exactly what the wrapper's fail-closed path produces,
leaving the user at a local prompt in that pane. Without this the next `ssh` there, to any host,
would inherit the first host's directory.

Two earlier candidates for that boundary were **rejected on review evidence**:

- *The bootstrap timeout.* `BOOTSTRAP_FAILED_DURATION` is a slow-bootstrap *warning*: the handler
  emits `BootstrappingSlow` telemetry and opens an auto-dismissing banner, and never aborts
  warpification ([view.rs:16267-16356](../../app/src/terminal/view.rs#L16267-L16356)). A session
  that bootstraps after 7s still succeeds, so dropping the cwd there silently sends slow hosts to
  the remote default. Its name reads like a failure signal and is not one.
- *The local shell exiting, alone.* It is a real backstop but far too late. The wrapper failing
  closed does not end the local shell — it hands the pane back to the user with the cwd still
  armed, and if it fails before dialing no `ModelEvent::SSH` fires either, so no timer runs.

### Quoting the `cd` for the shell that receives it

The cwd travels to the new pane unquoted and is quoted only once that pane's own session reports
its shell, because the correct quoting differs by shell and the local pane's shell is not the one
that will parse it. Fish honours backslash escapes inside single quotes, so the POSIX
`'\''` idiom does not survive there: a cwd of `a\'; echo INJECTED #` POSIX-quotes to a `cd` that
fish executes as two commands, where bash and zsh read it as one literal word. Quoting therefore
goes through `shell_quote_arg`
([shell/mod.rs:1019-1058](../../crates/warp_terminal/src/shell/mod.rs#L1019-L1058)) with the shell
from the `SessionBootstrappedEvent`.

That helper's own Fish arm escaped the quote but not the backslash, so it fell to the identical
payload; it is fixed here rather than worked around, since roughly ten other call sites depend on
it.

### Security

The remote cwd is remote-reported text submitted through the terminal input path. Quoting handles
shell metacharacters but no quoting survives a newline or carriage return, which would end the
submitted line and make the remainder a second command. The cwd is rejected outright — no `cd` is
sent — if it contains any control character. Sanitizing is deliberately not attempted. Quoting
itself is per-receiving-shell, for the reason given under "Quoting the `cd` for the shell that
receives it".

The `WARP_SSH_ATTACH_CONTROL_PATH` value is generated by Warp, never by the remote host, and is
filtered against the same character set the existing ControlPath branch enforces before it reaches
the SSH hook JSON.

### Settings

A new `clone_ssh_on_split` bool joins `SshSettings`
([settings/ssh.rs](../../app/src/settings/ssh.rs)) under `warpify.ssh.clone_ssh_on_split`, surfaced
as a checkbox in its own `SshCloneOnSplitWidget` in
[warpify_page.rs](../../app/src/settings_view/warpify_page.rs), built into the SSH category only
when the feature flag is on and modelled on the `reuse_existing_control_master` row beside it. It
gets its own widget rather than a row inside `SSHWidget` because a widget is the smallest unit
settings search can filter, so a row hidden under advertised search terms would make the page match
a query and then show nothing for it.

The setting defaults **off**, and the feature flag defaults off as well: the behavior has to soak
before either becomes the default. Splitting is gated on the flag, this setting, and
`enable_ssh_warpification` together — without the wrapper the new pane's shell never runs
`warp_ssh_helper`, so a replayed `ssh` would dial the host itself and prompt for credentials.

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
**Tests:** `app/src/terminal/warpify/trigger_state_tests.rs`

**Acceptance criteria:**

- [ ] The `(SessionId, command)` pair is recorded when a `WarpifiedRemote` session established by
      the SSH wrapper starts. The command is the pending, alias-expanded one that already passed
      `parse_interactive_ssh_command`, per §2's verbatim-replay reasoning — not the block text as
      typed.
- [ ] Nothing is recorded when that validated command is absent.
- [ ] It is not recorded for a session the wrapper did not establish — a local subshell, or a
      subshell warpified on the remote host or in a container, all of which reach the same
      bootstrap path.
- [ ] The accessor returns `None` when the bound session id does not match
      `active_block_session_id()`.
- [ ] Running a non-warpifying `ssh` inside a remote session does not change what the accessor
      returns for that session.
- [ ] The binding is cleared when the warpified session completes.

**Steps:**

1. Store the bound pair in `WarpifyState` ([trigger_state.rs](../../app/src/terminal/warpify/trigger_state.rs)),
   sourced from its own pending command and guarded on session type and wrapper origin.
2. Release it when the session ends, and again when the pane's local shell exits.
3. Expose a validating accessor on `TerminalView`.

**Delivered.** Three deviations from the phase as first written, each forced by evidence:

- **The write site moved.** The plan bound from `add_bootstrap_success_block`, which runs only for
  sessions carrying `subshell_info`. A wrapper-established SSH session has none — its remote
  `InitShell` payload sends no `is_subshell` — so that site never runs on the path Phase 4 needs.
  The binding is made in `handle_session_bootstrapped`, keyed on the bootstrap event's own
  `session_id` rather than `active_block_session_id()`.
- **Binding requires wrapper origin, not just a remote session type.** `determine_session_type`
  decides the type by comparing hostnames, so a subshell warpified on the remote host or in a
  container is typed `WarpifiedRemote` too. Binding on type alone let such a subshell overwrite the
  outer session's command and hand out a non-`ssh` string. The guard is
  `IsSSHWrapperSession::Yes`, which is what Phase 4 requires anyway.
- **Release is not on the local completion path.** Step 2 originally cleared the binding in
  `get_completed_warpify_session_id`, which only fires for subshell-warpified sessions and so never
  for wrapper SSH. Release is on `ExitShell`, with the pane's local shell exiting as the backstop
  for the abrupt-close case where that remote hook never arrives.

An Open Question added mid-phase — which form of the `ssh` command to bind — was withdrawn rather
than carried: §2's verbatim-replay reasoning had already settled it, and the implementation now
follows it.

### Phase 4: Open the split pane on the source pane's connection

**ID:** `4`
**Goal:** splitting a warpified, wrapper-established SSH pane produces a second pane on the same
connection, at the remote default directory, with no authentication prompt
**Tests:** `app/src/terminal/ssh/clone_on_split_tests.rs`, `app/src/terminal/model/session_tests.rs`

**Acceptance criteria:**

- [x] Splitting such a pane creates a pane whose env carries `WARP_SSH_ATTACH_CONTROL_PATH` and
      the source session's socket, and whose first command is the bound `ssh` command verbatim.
- [x] The new pane reaches a warpified remote session on the same host with no auth prompt.
- [x] The new pane is itself splittable, producing a third pane on the same connection.
- [x] Splitting a local pane is unchanged, including `WorkingDirectoryConfig` handling.
- [x] No clone when: the session is not `WarpifiedRemote`; it is `IsSSHWrapperSession::No`; no bound
      command matches; or SSH is still authenticating.
- [x] The clone decision is made only for a user-initiated split. A pane created by the LSP log
      viewer, the `CopyFileToRemote` uploader, the workflow runner, the plugin instructions pane,
      the editor fallback, or local continuation of a third-party conversation is never attached
      and never has a command submitted into it, even when the source pane holds a clonable SSH
      session.
- [x] When the master is gone, the split pane shows the wrapper's one-line explanation and stays at
      a live local prompt with the `ssh` command in its history. No app-side liveness probe runs,
      and the pane appears as fast as an ordinary split.
- [x] Splitting from a non-terminal pane never attempts a clone.
- [x] Closing either pane leaves the other alive and usable.
- [x] After logging out of the attached session, running `ssh <a different host>` in that same pane
      connects to that host, not to the original one.
- [x] No clone unless the source session's master will outlive the source pane — the wrapper
      reported `persist: true`, or `external_control_master: true`. A pane spawned before the flag
      flipped still holds `WARP_SSH_CONTROL_PERSIST=0`, so its master is force-exited on teardown
      and a split attached to it would die with the source pane.
- [x] Splitting a session that attached to a *user-owned* master works, and the new session reports
      `external_control_master: true` so Warp never tears that master down.
- [x] On a host whose `sshd` refuses a further multiplexed session (`MaxSessions` exhausted), the
      user gets a clear failure rather than a silently dead pane.
- [x] On Windows/WSL the env vars are set only when the new pane spawns in the same distro as the
      source session, since the socket lives inside the distro.

**Delivered.** Criteria with executable evidence in this checkout: the gating matrix, master
survival, the user-owned master, the WSL distro rule, and the absence of any app-side liveness
probe — `app/src/terminal/ssh/clone_on_split_tests.rs` (12 tests) and
`app/src/terminal/model/session_tests.rs` (10). The rest are runtime behaviours needing a live
warpified host and are tracked in section 4's manual pass, which is where this feature's runtime
confirmation belongs; nothing here has been confirmed against a real connection yet.

Deviations from the phase as planned, all found by review or doubt and all narrowing:

- The split boundary is `PaneGroup::split_terminal_pane`, reached from both `PaneGroupAction::Add`
  and the terminal's own `PaneEvent::Split*`. Placing it in
  `add_session_with_default_session_mode_behavior` — the original plan — would have injected an
  `ssh` command into the LSP log viewer, the `CopyFileToRemote` uploader, workflows, plugin
  instructions, the editor fallback, the notebook pane and local continuation of a third-party
  conversation. Wiring only `PaneGroupAction::Add` left the terminal's own context-menu split
  silently doing nothing.
- The clone source is the pane being split, never the active session. `active_session_id` only
  moves when focus lands on a terminal pane, so reading it would clone an unrelated host whenever a
  non-terminal pane held focus — the case section 4 already anticipated.
- The app-side `ssh -O check` preflight was dropped rather than implemented; see "Attach mode fails
  closed" in section 2.
- The WSL distro comes from the local pane's shell. Sourcing it from the session made the gate
  unpassable, because a warpified remote session reports no distro at all.
- A cloned split defers agent entry instead of entering agent view over a live `ssh` submission.
  The deferral fires when the `ssh` block completes, which for an interactive session means at
  logout — so an agent-default user gets a terminal until they log out.
- The app now re-applies the wrapper's own character filter to the hook-reported ControlMaster
  path, and rejects a path that is neither rooted nor tilde-prefixed. The tilde form is the one
  that actually arrives: `SSH_SOCKET_DIR` is the literal `~/.ssh` and the wrapper interpolates it
  inside double quotes.

Scope added during the phase: three acceptance criteria (master survival, non-split callers, the
dead-master path) and a second declared test gate over `session_tests.rs`.

**Steps:**

1. Add a helper that inspects the source terminal pane and returns an optional clone request
   (socket path, command). Every gate is a synchronous field read, including
   `persist || external_control_master`; no subprocess runs.
2. Call it from the `PaneGroupAction::Add` arm of `PaneGroup::handle_action`
   ([mod.rs:8225](../../app/src/pane_group/mod.rs#L8225)) — the only boundary that means the user
   asked for a split — and thread the resulting `Option<SshCloneRequest>` down through
   `add_terminal_pane` → `add_session` → `add_session_with_default_session_mode_behavior` →
   `add_session_in_directory`. Every other caller passes `None`.
3. Pass `WARP_SSH_ATTACH_CONTROL_PATH` through `create_terminal_pane_data`, and the command through
   `set_pending_command_queue`.

### Phase 5: Land the split pane in the source pane's remote directory

**ID:** `5`
**Goal:** the new pane's remote shell starts in the same remote directory as the source pane
**Tests:** `app/src/terminal/ssh/util_tests.rs`, `app/src/terminal/ssh/clone_on_split_tests.rs`, `app/src/terminal/view_tests.rs`, `crates/warp_terminal/src/shell/mod_tests.rs`

**Acceptance criteria:**

- [x] After bootstrap, the new pane's remote cwd equals the source pane's remote cwd.
- [x] A remote cwd containing spaces, single quotes, backslashes, `$`, backticks, or non-ASCII
      characters is entered correctly.
- [x] The `cd` is quoted for the shell the bootstrapped remote session reported, not for the
      pane's local shell.
- [x] A remote cwd containing any control character is rejected and no `cd` is submitted.
- [x] When the remote cwd is unknown, the pane lands in the remote default and no `cd` is
      submitted.
- [x] The pending cwd is dropped when the replayed `ssh` ends without warpifying, so no later
      session in that pane can inherit it, and as a backstop when the pane's shell exits.
- [x] A slow bootstrap is not treated as an abandoned one: a session that warpifies after the
      slow-bootstrap timer has fired still enters the directory.
- [x] The `cd` is never submitted into a local session.
- [x] The `cd` is not routed through `set_pending_command_queue`, and does not merge with text
      already in the pane's input.

**Delivered.** All nine criteria have executable evidence in this checkout:
`app/src/terminal/ssh/util_tests.rs` (3 tests over the validator),
`app/src/terminal/ssh/clone_on_split_tests.rs` (3 over the carried cwd),
`app/src/terminal/view_tests.rs` (9 over submission, quoting and lifetime) and
`crates/warp_terminal/src/shell/mod_tests.rs` (5 over the Fish escape). Landing in the right
directory over a real connection still belongs to section 4's manual pass, like Phase 4.

Deviations from the phase as planned, all found by review and all widening:

- **The bootstrap timeout is not a drop condition.** The phase as planned dropped the pending cwd
  when the warpify timeout fired. `BOOTSTRAP_FAILED_DURATION` names a *slow* bootstrap, not a
  failed one: the handler emits telemetry and opens an auto-dismissing banner and never aborts
  warpification, so a host slower than 7s would have silently landed in the remote default. The
  criterion was wrong, not just the code.
- **The lifetime is the attach attempt, not the session.** Clearing only on session end left the
  cwd armed on exactly the path the design already documents as the failure mode — the wrapper
  failing closed hands the pane back at a local prompt without ending the shell, and when it fails
  before dialing no `ModelEvent::SSH` fires either. The next `ssh` in that pane, to any host,
  would have inherited the first host's directory. The drop now happens when the replayed `ssh`
  completes as a user block, with session end kept as a backstop.
- **Quoting is per receiving shell.** The phase as planned added a `posix_single_quote` helper.
  POSIX quoting is injectable when the receiving warpified shell is fish, which honours backslash
  escapes inside single quotes: a cwd of `a\'; echo INJECTED #` becomes two commands there and one
  literal word in bash and zsh. The cwd now travels unquoted and is quoted at submission through
  `shell_quote_arg` with the shell the session reported.
- **A pre-existing bug in that helper was fixed rather than avoided.** `shell_escape_single_quotes`
  escaped the single quote but not the backslash for fish, so it fell to the identical payload.
  It is repaired in `crates/warp_terminal/src/shell/mod.rs`, which roughly ten other call sites
  share.
- **The submitted line is exactly the `cd`.** `set_pending_command` inserts at the cursor rather
  than replacing, so text already in the pane's input would have been submitted along with it.

Scope added during the phase: two acceptance criteria (per-shell quoting, slow-bootstrap
retention), one criterion widened to cover input merging, a fourth declared test gate over
`crates/warp_terminal/src/shell/mod_tests.rs`, and the `warp_terminal` crate entering this phase's
change set.

A note on the tests themselves: the view tests now register the session with the `History` model
and bind it to the active block, because `can_execute_command` refuses a session whose active
block carries no session id. Without that the `cd` was inserted but never executed, and assertions
on the input buffer passed while nothing had been submitted.

**Steps:**

1. Add a control-character validator to [ssh/util.rs](../../app/src/terminal/ssh/util.rs) and
   carry the cwd unquoted on the clone request.
2. Carry a pending remote cwd on the new `TerminalView`; quote and submit it for the bootstrapped
   session's shell on the bootstrap-success path
   ([view.rs:9902-9958](../../app/src/terminal/view.rs#L9902-L9958)); drop it when the replayed
   `ssh` completes as a user block and when the pane's shell exits.
3. Fix the Fish arm of `shell_escape_single_quotes`
   ([shell/mod.rs:1019](../../crates/warp_terminal/src/shell/mod.rs#L1019)), which escaped the
   single quote but not the backslash.

### Phase 6: Expose the setting and gate the feature

**ID:** `6`
**Goal:** users can turn the behavior off, and it ships disabled until it has soaked
**Tests:** `app/src/settings/ssh_tests.rs`, `app/src/settings_view/warpify_page_tests.rs`, `app/src/terminal/ssh/clone_on_split_tests.rs`, `app/src/terminal/view_tests.rs`, `app/src/pane_group/mod_tests.rs`

**Acceptance criteria:**

- [x] `warpify.ssh.clone_ssh_on_split` exists in `SshSettings` and round-trips through
      `~/.warp/settings.toml`.
- [x] The Warpify settings page shows the checkbox in the SSH section and it is discoverable via
      settings search.
- [x] With the setting off, splitting an SSH pane produces a local pane.
- [x] With the feature flag off, the behavior is entirely absent regardless of the setting,
      including the Phase 2 lifetime change.
- [x] Telemetry records clone attempted, succeeded, and fell-back-to-local.
- [x] Telemetry reports success only from the new pane's bootstrap outcome, never from the
      decision to attach. *(Added during the phase — see below.)*

**Steps:**

1. Add the setting to [settings/ssh.rs](../../app/src/settings/ssh.rs) and the widget row to
   [warpify_page.rs](../../app/src/settings_view/warpify_page.rs), following the
   `reuse_existing_control_master` pattern.
2. Add telemetry per `add-telemetry` (the feature flag already exists from Phase 2).
3. Add the changelog entry.

**Delivered.** The setting is `warpify.ssh.clone_ssh_on_split`, defaulting off, in its own
`SshCloneOnSplitWidget` built only when the flag is on. A split may attach only when the feature
flag, `enable_ssh_warpification`, and this setting all hold, expressed as `CloneGate` in
[clone_on_split.rs](../../app/src/terminal/ssh/clone_on_split.rs) and sourced by
`PaneGroup::ssh_clone_gate`.

Three changes came from review rather than the plan, and the first is a defect the phase would
otherwise have shipped:

- **Warpification is part of the gate.** Gating on the flag and the setting alone let a pane
  spawned with the SSH wrapper off still receive a clone request. That pane carries
  `WARP_USE_SSH_WRAPPER=0`, so its bootstrap never calls `warp_ssh_helper` and never reads
  `WARP_SSH_ATTACH_CONTROL_PATH`; the replayed `ssh` would run as a plain command and prompt for
  the credentials this feature exists to avoid — worse than the local pane it replaced. Reachable
  because turning warpification off leaves this setting set: the page only disables its switch
  rather than clearing the value.
  `reuse_existing_control_master` already guards itself the same way at
  [terminal_manager.rs:852](../../app/src/terminal/local_tty/terminal_manager.rs#L852).
- **The gate is a named type, not an inline conjunction.** Three same-typed bools at a call site
  are swappable, and dropping one would compile and break no test. `CloneGate` makes removing a
  condition a compile error, and `ssh_clone_gate` is split out so a test can pin that each
  condition comes from its own real source — the rule and its wiring are separately provable.
- **A pane closed mid-connect reports `Abandoned`.** The Exit path originally cleared the pending
  attach silently, which orphaned its `Requested` event: any success rate would then have been
  depressed by ordinary pane closes, in telemetry whose whole purpose is judging the soak.
  `Requested` now resolves into exactly one of `Succeeded`, `FellBackToLocal`, or `Abandoned`.

Deviations from the phase as planned, all traced to the pre-implementation challenge on the
`clone_request` contract:

- **`Ok` is not success, so success is not this module's to report.** The phase as planned put all
  three telemetry outcomes at the split site. `clone_on_split.rs` deliberately does not gate on
  master liveness — the wrapper re-runs `ssh -O check` after the app has decided, and fails closed
  on a master that has gone away. Reporting success from `clone_request`'s return would therefore
  have claimed it in exactly the dead-master case this feature is judged on. `Requested` and
  `Declined { reason }` now fire at the split; `Succeeded` and `FellBackToLocal` fire from the new
  pane's own bootstrap outcome, which is the first moment anything in the app knows.
- **`clone_request` returns `Result<SshCloneRequest, CloneDeclined>`.** A bare `Option` cannot name
  which gate refused, and the decline reason is what makes the fallback rate readable. The 15
  pre-existing tests now assert *which* gate fired rather than only that none passed.
- **The fallback line is drawn at "the user split a warpified SSH pane".** `CloneDeclined::is_fallback`
  excludes `Disabled` and `NotWarpifiedRemote` — ordinary local splits and deliberate opt-outs would
  otherwise drown the rate. It deliberately *includes* `NoWrapperSocket` and
  `MasterWouldNotOutliveSource`: the feature could never have served those panes, but the user
  experienced the same fallback, and both stay separable by reason.
- **A clone needs its own marker.** `pending_remote_cwd` is legitimately `None` for a clone whose
  source pane reported no directory, so it cannot say "an attach is outstanding".
  `TerminalView::awaiting_ssh_clone` carries that, with the same lifetime, and
  `set_pending_remote_cwd` became `set_pending_ssh_clone`. This reaches into Phase 5's code.
- **The setting deliberately does not reach `WARP_SSH_CONTROL_PERSIST`.** The fourth criterion's
  ControlPersist clause was already satisfied at pane spawn by Phase 2's flag gate
  ([unix.rs:379](../../crates/warp_terminal/src/local_tty/unix.rs#L379)). The setting is read at
  split time, so it could never retroactively change the lifetime of a master a pane already holds.
  Nothing in this phase was needed for that clause.

Scope added during the phase: one acceptance criterion (honest success reporting), three declared
test gates (`app/src/settings/ssh_tests.rs`, `app/src/settings_view/warpify_page_tests.rs`,
`app/src/pane_group/mod_tests.rs`), a fourth telemetry event plus `Abandoned`, and Phase 5's
`view.rs` entering this phase's change set.

Not in this commit: the changelog entry is a `CHANGELOG-NEW-FEATURE:` marker in the PR description
per [pull_request_template.md](../../.github/pull_request_template.md), not a file in the tree, so
it lands when the PR is opened.

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

- **Default on or off at GA?** *Resolved in Phase 6: both the flag and the setting ship off.* The
  product intent remains on at GA; flipping the setting's default is a separate, deliberate step
  once the flag has soaked.
- **`ControlPersist` value.** *Resolved in Phase 2: `ControlPersist=60`.* Long enough that any
  split-then-close ordering keeps the connection, short enough to bound how long an authenticated
  session lingers past visible use.
- **Should the source pane's local launch directory be snapshotted for the new pane's local PTY?**
  Attaching needs no credentials, so relative `-i`/`-F` no longer matter and this is now cosmetic —
  the local PTY is a thin shell that immediately enters SSH. Deferred unless verification shows it
  matters.
- **New tab and new window from an SSH pane.** Out of scope here; the Phase 4 helper is written so
  this is a small follow-up.
- **Should opting out of the split also stop `ControlPersist`?** Raised by review in Phase 6. With
  the flag on and `clone_ssh_on_split` off, every Warp-owned master still gets `ControlPersist=60`,
  because `WARP_SSH_CONTROL_PERSIST` is read at pane spawn from the flag alone
  ([unix.rs:379](../../crates/warp_terminal/src/local_tty/unix.rs#L379)). Users who explicitly
  declined the only feature that consumes those lingering masters still pay for them. This meets
  Phase 6's fourth criterion, which deliberately ties the lifetime change to the flag, and the
  spawn-time read is the same shape `reuse_existing_control_master` already uses, so ANDing the
  setting in is feasible. **Settle before the flag promotes to Stable**, not in this spec.
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

    *Addressed in Phase 4:* the attach is gated on the source master outliving the source pane, not
    on the flag. The condition is `persist || external_control_master`, not `persist` alone, because
    teardown force-exits a master only when Warp owns it *and* it is non-persistent — so a
    user-owned master is safe to attach to as well. This settles the split-severing symptom; the
    two other bullets of this question remain open.
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
