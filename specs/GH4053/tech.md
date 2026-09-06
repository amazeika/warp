# TECH.md — Warp Extension API v0.1

Issue: https://github.com/warpdotdev/warp/issues/4053
Product spec: [`product.md`](product.md)

## Context

### What exists today

Warp already owns every capability a Git plugin would otherwise reimplement.
The work is to expose stable wrappers, not to build new subsystems.

- **Local control (`warpctrl`)** — `crates/local_control/` is a UI-agnostic
  crate holding the wire envelopes, error codes, target selectors and
  credential types (`crates/local_control/src/protocol.rs:1`,
  `crates/local_control/src/catalog.rs`,
  `crates/local_control/src/selectors.rs`). `app/src/local_control/mod.rs:1`
  owns the app-side listener, discovery record and credential broker, and
  `app/src/local_control/bridge.rs:21` transfers work from the Tokio thread onto
  the WarpUI main thread through `ModelSpawner<LocalControlBridge>`. Permission
  and version gating lives in `app/src/local_control/permissions.rs:13`.
  The normative security model is `specs/warp-control-cli/SECURITY.md`.

  This split — a UI-agnostic protocol crate plus an app-side bridge — is the
  shape this feature copies. It does **not** reuse `warpctrl`'s action catalog:
  that catalog deliberately excludes arbitrary execution and file mutation, and
  extensions need a different, permission-gated surface.

- **Code Review / diff** — `app/src/code_review/mod.rs:43` defines
  `CodeReviewPanelArg`, the only way to open the diff pane today. It carries
  `repo_path: Option<LocalOrRemotePath>` **and a `WeakViewHandle<TerminalView>`**,
  and is dispatched as `pane_group::Event::OpenCodeReviewPane`
  (`app/src/pane_group/mod.rs:583`). Diff content is produced by
  `LocalDiffStateModel` (`app/src/code_review/diff_state/local.rs:197`), whose
  entry points are repo- and mode-scoped
  (`load_diffs_for_current_repo`, `set_diff_mode`), not
  "compare these two arbitrary revisions".

- **Repository state** — `app/src/util/git.rs:186` (`RepoGitSummary`), `:251`
  (`Commit`), `:262` (`FileChangeEntry`), `:1095` (`BranchEntry`) and
  `:511` (`git_operation_in_progress`). Repository identity and file-tree
  metadata live in `crates/repo_metadata/src/local_model.rs`.

- **Remote execution** — `crates/remote_server/src/client/mod.rs:791`
  (`run_command`) already runs a command inside an established remote session
  and returns a `RunCommandResponse`. It takes `command: String` — a shell
  string — plus a working directory and environment map.

- **Remote-aware paths** — `crates/warp_util/src/local_or_remote_path.rs:12`
  (`LocalOrRemotePath`) is the canonical file identity across the buffer model
  and the view layer, so file and diff APIs can be target-aware without a new
  path type.

- **Panels** — `app/src/workspace/view/left_panel.rs:148` defines
  `ToolPanelView` as a closed enum (`ProjectExplorer`, `GlobalSearch`,
  `WarpDrive`, `ConversationListView`), with availability and view state keyed
  off it throughout the file.

- **Feature gating** — `app/src/features.rs:465` shows the
  `#[cfg(feature = "...")] FeatureFlag::X` pattern used to gate `warpctrl`.

### Answers to the design questions

| Question | Answer from this checkout | Consequence |
| --- | --- | --- |
| A. Can the Code Review renderer accept an explicit repo/file/comparison? | Partly. `CodeReviewPanelArg` takes a repo path but also requires a `TerminalView` handle, and `LocalDiffStateModel` is scoped to "current repo + diff mode". | A `DiffService` refactor is required before `diff.openCommit` / `diff.openComparison`. `diff.openWorkingTree` / `diff.openFile` can wrap what exists. |
| B. Is there a reusable remote execution request? | Yes — `remote_server` `run_command`. | Wrap it. But it takes a shell string while the product spec requires argv, so the host must quote argv for the remote path (below). |
| C. Are panels statically wired? | Yes, `ToolPanelView` is a closed enum. | A dynamic panel registry is needed; do not special-case Git. |
| D. Can palette entries be registered dynamically? | Bindings are registered at `init` time (`app/src/code_review/mod.rs:64`). | Needs a generic contribution registry consulted by the palette. |
| E. How is repository identity represented? | `repo_metadata` plus `LocalOrRemotePath`. | Reuse; do not invent plugin-side repo identity. |
| F. Can remote paths reach the editor/diff pipeline? | Yes — `LocalOrRemotePath::Remote`. | File and diff APIs take target-aware paths. |

### Scope of this document

The product spec covers the whole v0.1 API. Implementation is split into the PR
sequence in [Parallelization](#parallelization). **This tech spec's "Proposed
changes" describes the whole design; the sections marked _(PR 1)_ are what the
accompanying code change implements.** Later sections are specified here so the
protocol does not have to change to accommodate them.

## Proposed changes

### 1. `crates/extension_protocol` — the wire contract _(PR 1)_

A new UI-agnostic crate, sibling to `crates/local_control`, depending only on
`serde`, `serde_json`, `thiserror`, `uuid` and `toml`. It is the single source
of truth for anything that crosses the process boundary, and it is the
compatibility layer — the SDK in §52 of the plan is convenience over this.

Modules:

- `protocol` — `PROTOCOL_VERSION`, `RequestEnvelope`, `ResponseEnvelope`,
  `EventEnvelope`, `Message` (the untagged union actually read off the wire),
  `ExtensionError`, `ErrorCode`.

  ```json
  { "protocol": 1, "request_id": "42", "method": "workspace.getContext", "params": {} }
  { "protocol": 1, "request_id": "42", "result": { "workspace_id": "abc" } }
  { "protocol": 1, "event": "workspace.contextChanged", "params": { "workspace_id": "abc" } }
  ```

  `ErrorCode` is the closed set from the product spec: `plugin_not_found`,
  `plugin_failed`, `permission_denied`, `unsupported_capability`,
  `workspace_missing`, `repository_missing`, `session_disconnected`,
  `execution_failed`, `target_stale`, `operation_conflict`, `invalid_request`,
  `protocol_mismatch`. Codes serialize as `snake_case` and are stable API.

- `manifest` — `ExtensionManifest` and its `toml` deserialization, plus
  `validate()` returning `ManifestError`. Required: `id`, `name`, `version`,
  `api_version`, `command`. Optional: `[activation]`, `[permissions]`,
  `[execution]`, `[[commands]]`, `[[panels]]`.

  `command` is validated as a directory-relative path with no absolute prefix
  and no `..` component, so a manifest cannot point at an executable outside its
  own directory. This is validation, not resolution: the host resolves it
  against the extension directory separately.

- `permissions` — `Permission` and `PermissionSet`. The categories are
  `workspace.read`, `workspace.write`, `process.execute`, `network.access`,
  `ui.panel`, `ui.commands`, `ui.notifications`, `ui.dialogs`, `git.read`,
  `git.mutate`, `session.read`, `session.execute`. `ui.dialogs` is separate from
  `ui.notifications` because a dialog interrupts and demands an answer while a
  notification does not, and a plugin that only reports progress should not
  inherit the ability to block the user.

  `Method::permission()` maps every host method to the permission that gates it,
  so the gate is a total function over methods rather than a scattered set of
  `if` checks, and a test asserts the map covers `Method::ALL`.

  `ExtensionManifest::effective_permissions()` adds `ui.commands` and `ui.panel`
  when the manifest declares commands or panels. A contribution *is* the request
  for its surface, so the manifest does not have to say the same thing twice —
  and a plugin still cannot drive a panel it never declared, which is checked
  per request against the panel id.

- `capabilities` — `Capability` tokens (`commands.v1`, `panel.tree.v1`,
  `workspace.context.v1`, `execution.v1`, `diff.open.v1`, `file.open.v1`,
  `dialog.confirm.v1`, `notification.v1`) and the `HostCapabilities` payload
  returned from `extension.initialize`.

- `methods` — the `Method` enum plus one typed params/result pair per method:
  `WorkspaceContext`, `ExecutionRunParams`/`ExecutionRunResult`,
  `FileOpenParams`, `DiffOpenParams`, `NotificationParams`, `DialogConfirm*`,
  `PanelStateParams`, and the panel data model (`PanelSection`, `PanelItem`,
  `PanelItemAction`, `PanelViewState`).

  `ExecutionTarget` is `Local` or `Ssh { session_id }`. `ExecutionRunParams`
  carries `executable: String` and `args: Vec<String>` — never a shell string.

- `events` — `EventKind` (`workspace.changed`, `cwd.changed`,
  `repository.changed`, `session.changed`, `ssh.connected`, `ssh.disconnected`,
  `command.invoked`, `panel.action`, `extension.shutdown`) and their payloads.
  `command.invoked` and `panel.action` carry an `ActionOrigin` — workspace,
  session, repository and execution target — so an action a plugin is slow to
  answer cannot be retargeted by a focus change it never saw.
  `extension.shutdown` is what makes a clean stop possible before the host
  falls back to killing the process.

### 2. `crates/extension_host` — discovery, framing, supervision _(PR 1)_

A second UI-agnostic crate. It knows how to find, validate, start, talk to and
stop a plugin process; it knows nothing about WarpUI. Keeping it free of
`warpui` is what makes the lifecycle and permission logic unit-testable without
an app harness, exactly as `crates/local_control` is testable without one.

- `discovery` — `extensions_root()` (`$WARP_EXTENSIONS_DIR`, else
  `~/.warp/extensions`), `discover(root)` returning `Vec<DiscoveredExtension>`
  sorted by directory name, each either validated or carrying a
  `ValidationError`. Duplicate ids, unsupported `api_version`, missing
  executable and manifest parse failures all resolve to a validated-or-invalid
  record rather than an aborted scan.

- `codec` — newline-delimited JSON framing over `impl BufRead` / `impl Write`,
  with `MAX_MESSAGE_BYTES`. Framing is deliberately separated from process I/O
  so protocol behavior (oversized message, truncated line, malformed JSON,
  version mismatch) is tested against in-memory buffers.

- `lifecycle` — the `ExtensionState` machine from the product spec and
  `RestartPolicy` (three failures in five minutes → `Failed`). Pure logic over
  an injected clock, so the rate limiter is tested without sleeping.

- `process` — spawns the child with piped stdio, owns a reader thread that
  decodes messages onto a channel and a stderr drain that forwards to the
  extension log, and terminates the child on drop. `stdin` is inherited by
  nothing: the plugin's stdio is the transport, so plugins must log to stderr,
  not stdout.

- `session` — request/response correlation by `request_id`, the
  `extension.initialize` handshake with a start timeout, capability
  advertisement, and the permission gate applied to every inbound method before
  it is dispatched to the host adapter.

- `host` — the `ExtensionHost` trait the app implements (one method per host
  capability). The host crate calls it; it never reaches into app types.

- `logging` — `~/.warp/logs/extensions/<id>.log` path resolution and a redacting
  writer. The extension id is sanitised on the way into a filename, because it
  arrives from a manifest and a manifest is not trusted to stay inside the log
  directory. A line naming a secret-shaped field is dropped whole rather than
  partially masked: plugin output has no schema, so there is no reliable way to
  keep the safe half.

Two repository lint rules shape this crate: `std::process::Command` and
`std::time::Instant` are disallowed (`.clippy.toml`), so process spawning goes
through `command::blocking::Command` — which also gives Windows
`kill_on_parent_process_close`, keeping a plugin from outliving Warp — and all
timing uses `instant::Instant`.

Transport choice: **stdio with newline-delimited JSON**. Unlike `warpctrl`,
whose caller is an unrelated same-user process needing discovery and a
credential broker, an extension is Warp's own child; parentage is the
authentication. `codec` and `process` are separated so a socket transport can be
added later without touching `session`.

### 3. `crates/extension_sdk` — optional client conveniences _(PR 1)_

Request correlation and typed call wrappers, so a Rust plugin does not rewrite
them. Optional by design: the wire protocol is the compatibility layer, and a
plugin in any language that can frame JSON on stdio is a first-class plugin.
`ClientError` keeps a refusal (`permission_denied`, `unsupported_capability`)
distinct from a transport failure, because the first is a normal answer a plugin
should handle and the second is not.

### 4. `crates/extension_example` — the proof-of-concept plugin _(PR 1)_

`warp-extension-example`: a plugin that performs the handshake, prints the
workspace context, runs one allow-listed harmless command, opens one file, opens
one diff, shows one notification and publishes one panel. It exists so the API
is proven without Git complexity, and it is the fixture the host's end-to-end
test drives, so the example cannot rot.

### 5. `crates/extension_git` — the reference Git plugin _(PR 1)_

`warp-git`: the Git MVP as a plugin, with no Git code in Warp core.

This bundles a reference plugin with the core API, which §44 of the originating
plan advised against. It is included deliberately: an API justified only by a
toy plugin is an API nobody has load-tested, and building this one immediately
surfaced a protocol gap — `panel.action` did not identify the section, so a
plugin could not distinguish a staged row from an unstaged row with the same
path. That is exactly the kind of defect a reference consumer is for. It stays
a separate crate and a separate process, so removing it from a core PR is
deleting a directory.

Structure:

- `git/status.rs` — `status --porcelain=v2 -z --branch`. NUL termination is
  what makes a path containing a space, a quote or a newline unambiguous, and
  the rename record's trailing original-path field must be consumed or every
  later record is misread.
- `git/branches.rs`, `git/commits.rs` — `for-each-ref` and `log` with explicit
  formats and ASCII unit/record separators, never Git's human output.
- `git/operations.rs` — pure argv builders, so the exact command a destructive
  action would run is assertable without executing it.
- `state.rs`, `panel.rs` — the repository model and its rendering into
  `PanelViewState`.
- `repository.rs` — runs Git through `execution.run`, bound to the target and
  root it was constructed with.

### 6. App-side bridge — `app/src/extensions/` _(PR 2–5)_

Mirrors `app/src/local_control/`:

- `manager.rs` — an `ExtensionManager` singleton entity owning discovered
  extensions, their processes, and activation evaluation against workspace
  events.
- `bridge.rs` — `ModelSpawner<ExtensionBridge>` to move inbound requests onto
  the main thread, following `app/src/local_control/bridge.rs:21`.
- `permissions.rs` — the grant store and the permission prompt, gated behind a
  new `FeatureFlag::Extensions` in the style of
  `app/src/features.rs:465`.
- `contributions.rs` — the registry of commands and panels contributed by
  running extensions, consulted by the command palette and the panel switcher.
- `adapters/` — the `ExtensionHost` implementation, translating protocol types
  into existing app services.

### 7. Internal services the adapters need _(PR 4–5)_

- **`ExecutionService`** — resolves `ExecutionTarget::Local` to a spawned
  process and `ExecutionTarget::Ssh { session_id }` to
  `remote_server::client::run_command`
  (`crates/remote_server/src/client/mod.rs:791`).

  **Tradeoff — argv vs. shell string.** The product spec requires argv; the
  remote proto takes a shell string. v0.1 shell-quotes the argv vector with a
  POSIX single-quote escape for the remote path only, and the local path spawns
  argv directly. This is a real, documented asymmetry: quoting is a correctness
  risk that argv avoids. The follow-up is to add a repeated `args` field to
  `RunCommandRequest` and drop the quoting; the extension-facing API does not
  change when that happens, which is the point of putting the wrapper here.

- **`DiffService`** — extracts the "what to compare" decision out of
  `CodeReviewPanelArg`'s dependence on a `TerminalView`
  (`app/src/code_review/mod.rs:43`) so a diff can be opened from a non-terminal
  origin. `diff.openWorkingTree` / `diff.openFile` map onto today's repo+mode
  behavior; `diff.openCommit` / `diff.openComparison` require
  `LocalDiffStateModel` (`app/src/code_review/diff_state/local.rs:197`) to
  accept an explicit revision pair and are therefore v0.2 of the diff
  capability, advertised as a separate token.

- **`WorkspaceContextService`** — assembles `WorkspaceContext` from the
  workspace, the active pane's `WorkingDirectory`, `repo_metadata`, and the
  session's local/remote identity, and emits the context events.

- **Panel registry** — replaces the closed `ToolPanelView`
  (`app/src/workspace/view/left_panel.rs:148`) with a registry of built-in
  variants plus extension-contributed panels, rendering the
  `PanelViewState` model with existing WarpUI tree/list components. This is the
  largest core refactor in the sequence and is why panels are their own PR.

## Testing and validation

Unit tests live beside the source as `<module>_tests.rs` included with
`#[cfg(test)] #[path = "..."] mod tests;`, matching
`crates/local_control/src/protocol.rs:488`.

| Invariants | Test |
| --- | --- |
| 1, 2, 5, 6 | `discovery` tests over `tempfile` roots: manifest-less directory ignored, `WARP_EXTENSIONS_DIR` honoured, missing root yields empty, duplicate id keeps the first, escaping `command` rejected. |
| 3, 4 | `manifest` tests: each required field missing, malformed types, unsupported `api_version`, and a golden round-trip of the §8 manifest from the plan. |
| 11–14 | `permissions` tests: `Method::permission()` is total over `Method`; a method outside the granted set yields `permission_denied`; `execution.run` with an executable outside `allowed_executables` is denied. |
| 15–17, 22 | `codec` and `session` tests: handshake success, wrong `protocol` yields `protocol_mismatch`, un-advertised capability yields `unsupported_capability`, oversized frame yields `invalid_request`, truncated and malformed frames. |
| 18–21 | `lifecycle` tests over an injected clock: full state sequence, unexpected exit surfaces a restartable failure, three failures in five minutes reaches `Failed`, a fourth is not auto-restarted. |
| 23–26 | `crates/extension_example/tests/end_to_end.rs` drives the real plugin binary through the real host with a fake `ExtensionHost`: the documented startup sequence, panel state, a `panel.action` event round-tripping back into `diff.openFile`, a denied permission stopping the plugin without disturbing the host, and a handshake that claims a different extension being refused. |
| Git parsers | `crates/extension_git/src/git/*_tests.rs` run against status output captured verbatim from Git, covering a path staged and then edited again, a rename with its original-path record, a merge conflict, an empty repository, a detached HEAD, divergence, and truncated records. |
| Git safety | `operations_tests.rs` asserts the argv itself: `--` before every path, `restore --staged` rather than `reset`, `--force-with-lease` and never `--force`, safe delete distinct from force delete, no bulk `clean`, and no argument that could introduce a shell. |
| Git end to end | `crates/extension_git/tests/end_to_end.rs` runs the real `warp-git` binary against real temporary repositories with a hermetic Git environment: staging changes the actual index, a commit message with quotes crosses as one argv entry, a dismissed dialog leaves the repository untouched, a discard runs nothing before the user agrees, and an unmerged branch delete escalates to a second confirmation before any `--force`. |
| 27–32 | Adapter tests with a fake `ExecutionService`: context assembly local and SSH, a stale session yields `session_disconnected`, target retained across a focus change, output truncation flagged. |
| 33–35 | Adapter tests asserting the app service each method calls, and that a remote path routes through the remote transport. |
| 36 | `logging` tests: the redacting writer drops token-shaped values and environment blocks. |
| 15, 30 | Security tests: a plugin calling an undeclared method, a malformed argv, and a request naming another extension's panel are all rejected. |

Manual validation for the PRs that touch UI: the §47 end-to-end workflow run
once against a local repository and once over an SSH session, with a screen
recording, since the local/remote asymmetry is exactly what unit tests cannot
prove.

## Parallelization

Per §45 of the plan, and matching the repo's preference for focused PRs:

1. **PR 1 — foundation.** `extension_protocol`, `extension_host`,
   `extension_sdk`, `extension_example`, `extension_git`. No app changes and no
   UI; the Git plugin is a separate process that Warp core knows nothing about.
2. **PR 2 — commands, notifications, dialogs.** First app wiring:
   `ExtensionManager`, the bridge, permission prompt, contribution registry.
3. **PR 3 — panel API.** The `ToolPanelView` registry refactor.
4. **PR 4 — workspace context and execution targets.** `WorkspaceContextService`,
   `ExecutionService`, session-staleness handling.
5. **PR 5 — diff and file wrappers.** `DiffService`, `file.open`.

PRs 2–5 depend on PR 1 only through `extension_protocol`, so the external
`warp-git` plugin can be developed against the protocol crate in parallel from
the moment PR 1 lands.

## Risks

- **The API grows past what the reference plugin needs.** Mitigated by deriving
  every method in `methods` from a concrete `warp-git` MVP requirement and
  advertising each as a separately negotiable capability token.
- **Remote/local execution mismatch.** The highest-consequence failure is
  running a mutating Git command against a same-named local path. Mitigated by
  making `ExecutionTarget` a required field on every acting request, never
  defaulting it, and failing closed on a stale session.
- **Shell quoting on the remote path.** Documented above; contained to one
  function in `ExecutionService` and removed by the proto follow-up.
- **The panel registry refactor is large.** Mitigated by isolating it in PR 3
  and keeping built-in panels on the same registry, so extensions are not a
  special case.
