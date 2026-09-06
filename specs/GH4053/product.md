# PRODUCT.md — Warp Extension API v0.1

Issue: https://github.com/warpdotdev/warp/issues/4053

## Summary

Warp gains a generic extension layer so trusted, out-of-process plugins can
contribute native-feeling functionality — panels, command-palette entries,
diff and file views, dialogs and notifications — without those features being
compiled into Warp core. The first reference consumer is an external
`warp-git` plugin, but no Git-specific behavior belongs in Warp.

An extension is a directory under `~/.warp/extensions/<id>/` containing a
declarative `extension.toml` manifest and an executable. Warp reads the
manifest, decides whether the extension is eligible for the current workspace,
asks the user to grant the manifest's permissions, starts the executable as a
child process, and speaks a versioned JSON protocol with it over stdio.

Figma: none provided.

## Goals / Non-goals

Goals:

- Discovery, validation, lifecycle and supervision of out-of-process plugins.
- A versioned request/response/event protocol with capability negotiation, so
  an old plugin and a new Warp (or the reverse) fail predictably instead of
  silently misbehaving.
- An explicit, user-granted permission model, enforced per request.
- Extension-scoped logging that never records secrets.
- A development mode that shortens the plugin iteration loop.

Non-goals for v0.1 (explicitly deferred):

- Marketplace, automatic installation, plugin-to-plugin dependencies.
- Arbitrary webviews, HTML/JavaScript, or dynamically loaded native libraries.
- Unrestricted access to Warp internals or arbitrary internal action dispatch.
- Cloud-hosted plugins and deep Agent APIs.
- Plugin-defined settings pages beyond a generic schema.
- Any Git functionality in Warp core.

## Behavior

### Discovery and validation

1. On startup, and whenever the extensions directory changes, Warp enumerates
   immediate subdirectories of the extensions root and treats each directory
   containing an `extension.toml` as one candidate extension. Directories
   without a manifest are ignored silently and are not reported as errors.

2. The extensions root is `~/.warp/extensions`. When `WARP_EXTENSIONS_DIR` is
   set to an absolute path, that path is used instead. A missing root is not an
   error: discovery yields zero extensions.

3. A manifest that cannot be parsed, is missing a required field, or declares a
   malformed field leaves the extension in a `Invalid` state with a
   human-readable reason. The extension is listed in the extensions UI with
   that reason and is never started.

4. A manifest whose `api_version` is not supported by this Warp build leaves the
   extension `Invalid` with a reason naming both the requested and the supported
   version. It is never started.

5. When two discovered manifests declare the same `id`, the first by
   lexicographic directory name is kept and every later one is `Invalid` with a
   duplicate-id reason. Discovery order is otherwise deterministic.

6. An extension whose declared `command` does not resolve to an existing file
   inside its own directory is `Invalid`. A `command` that escapes the
   extension directory (absolute, or containing `..`) is rejected as malformed
   rather than resolved.

### Activation

7. A validated extension is `Inactive` until its activation condition holds.
   With `activation.workspace_contains = [".git"]`, the extension activates once
   the active workspace's working directory, or one of its ancestors up to the
   filesystem root, contains a `.git` entry, and not before.

8. An extension with no `[activation]` table activates as soon as it is enabled.
   An extension the user has disabled never activates, regardless of condition.

9. Warp startup never blocks on extension discovery, validation, permission
   prompts, or process start. A workspace opens with the same latency whether
   zero or many extensions are installed.

10. When the activation condition stops holding — the user switches to a
    workspace with no repository — the extension is stopped and returns to
    `Inactive`. Switching back starts it again.

### Permissions

11. The first time an extension is about to start, Warp shows a permission
    prompt listing, in plain language, every permission its manifest declares
    and the executables it may run. The extension does not start until the user
    allows it. Cancelling leaves the extension `Inactive` and does not re-prompt
    until the user retries explicitly.

12. A granted decision is remembered per extension id and per manifest
    permission set. If a later version of the manifest declares a permission
    that was not previously granted, the user is prompted again before that
    version starts.

13. A request for a method the extension's granted permissions do not cover
    fails with the `permission_denied` error code. The failure is returned to
    the plugin, recorded in the extension log, and does not terminate the
    plugin.

14. `execution.run` accepts only executables named in the manifest's
    `execution.allowed_executables` list. Any other executable fails with
    `permission_denied`. Arguments are always an argv array; there is no shell
    string form and no `sh -c` path.

### Handshake and capability negotiation

15. Immediately after start, the plugin sends `extension.initialize` and Warp
    replies with the protocol version it speaks, its `api_version`, and the list
    of capability tokens it supports (for example `commands.v1`,
    `panel.tree.v1`, `workspace.context.v1`, `execution.v1`, `diff.open.v1`,
    `file.open.v1`, `dialog.confirm.v1`, `notification.v1`).

16. A plugin that calls a method whose capability Warp did not advertise
    receives `unsupported_capability` rather than a generic failure, so a plugin
    can degrade instead of breaking.

17. A message whose `protocol` field does not match the version Warp speaks is
    rejected with `protocol_mismatch`. A plugin that fails to complete the
    handshake within the start timeout is treated as a failed start.

### Lifecycle and failure

18. Extension states are `Discovered`, `Invalid`, `Inactive`, `Starting`,
    `Running`, `Stopping`, and `Failed`. Every transition is written to that
    extension's log with a timestamp.

19. When a running plugin exits unexpectedly, Warp remains fully usable. The
    user gets one notification naming the extension and offering **Restart**.
    Contributions from that extension (panels, commands) disappear until it runs
    again.

20. Repeated crashes are rate limited: after three failed starts inside five
    minutes the extension enters `Failed` and is not restarted automatically.
    The notification then offers a manual restart only.

21. On Warp shutdown, running plugins are asked to stop and are terminated if
    they do not exit within the stop timeout. Warp quitting is never blocked
    beyond that timeout.

22. A plugin that floods the host, or sends a single message above the size
    limit, has that message rejected with `invalid_request`; sustained flooding
    stops the plugin with a logged reason rather than degrading the UI.

### Contributions

23. Commands declared in the manifest appear in the command palette with their
    declared titles once the extension is running, and disappear when it stops.
    Invoking one delivers a `command.invoked` event to the plugin, carrying the
    workspace, session and repository identity that was current at invocation.

24. A declared panel appears in the panel switcher once the extension is
    running. Its content is a data model the plugin supplies — sections, trees,
    lists, rows, icons, badges — rendered by WarpUI. A plugin cannot supply
    markup or executable UI code.

25. A panel with no state yet renders Warp's standard loading state; a panel
    whose plugin reported an error renders Warp's standard error state with the
    plugin's message; an empty model renders Warp's standard empty state.

26. Activating a row or a row action sends a `panel.action` event naming the
    panel, the section, the item id, and the action id, again carrying the
    originating workspace, session and repository identity. The section is part
    of the identity because item ids are only unique within a section — the same
    file legitimately appears in more than one.

### Workspace context and execution targeting

27. `workspace.getContext` returns the active workspace id, working directory,
    repository root when there is one, active pane and session ids, and an
    execution target that is either local or an SSH session with its session id.

28. Warp emits `workspace.changed`, `cwd.changed`, `repository.changed`,
    `session.changed`, `ssh.connected` and `ssh.disconnected` events so a plugin
    can follow the user without polling.

29. Every request that acts on the world — `execution.run`, `diff.*`,
    `file.open` — names its execution target explicitly. Warp never resolves
    "whatever is focused now" on the plugin's behalf.

30. An operation started against a session that has since ended fails with
    `session_disconnected`; one naming a target that no longer exists fails with
    `target_stale`. Neither is silently retargeted, and in particular a request
    for a remote path is never executed against a same-named local path.

31. If the user changes focus while a plugin operation is running, the operation
    stays attached to the workspace, session and repository it started in.

32. `execution.run` returns exit code, stdout, stderr and duration. Output above
    the configured limit is truncated, and the response says it was truncated
    rather than failing.

### Native surfaces

33. `file.open` opens a path in Warp's existing file viewer/editor, honouring an
    optional line and column, and resolves the path against the request's
    execution target so a remote path opens over the SSH transport.

34. `diff.openWorkingTree` and `diff.openFile` open Warp's existing Code Review
    diff UI for the named repository, and for `diff.openFile` scrolled to the
    named file. Plugins do not render diffs.

35. `notification.show` surfaces a Warp notification. `dialog.confirm`,
    `dialog.input` and `dialog.select` render native Warp dialogs and return the
    user's choice, including an explicit cancellation result. A destructive
    confirmation is always rendered by Warp, never drawn by the plugin.

### Logging and development mode

36. Each extension writes to `~/.warp/logs/extensions/<id>.log`, recording start
    and stop, negotiated protocol version, request ids, method names, durations
    and exit codes. Credentials, tokens, passphrases and full environment
    variables are never written.

37. `warp --extension-dev-path <dir>` loads that directory as an extension
    regardless of the installed set, restarts it when its executable changes,
    and surfaces manifest validation errors directly in the UI.

### Accessibility

38. Panel trees, rows and row actions are keyboard reachable and expose the same
    labels to assistive technology as Warp's built-in panels, because they are
    rendered by the same WarpUI components.

## Open questions

- Whether the permission prompt should offer per-permission granularity in v0.1
  or remain all-or-nothing per extension.
- Whether panel state should be a full snapshot per update, or gain a delta form
  before the first large repository ships.
