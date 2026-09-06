# warp-extension-example

The reference Warp extension. It is deliberately not Git: its job is to prove
the extension API end to end so that a real plugin's complexity cannot hide a
problem in the host.

On start it:

1. completes the `extension.initialize` handshake and logs the negotiated
   protocol version and capabilities,
2. asks for the workspace context and reports whether it is local or SSH,
3. runs the one allow-listed executable its manifest declares (`echo`),
4. opens a file in Warp's native viewer,
5. opens Warp's native working-tree diff,
6. shows a notification,
7. publishes a panel showing the working directory, execution target and
   repository, with an `open_diff` row action.

It then serves events: a `panel.action` opens the corresponding file diff
against the target the action originated in, a `command.invoked` shows a
notification, a context change republishes panel state, and
`extension.shutdown` exits cleanly.

## Installing it

```bash
cargo build -p extension_example
mkdir -p ~/.warp/extensions/warp-extension-example
cp crates/extension_example/extension.toml ~/.warp/extensions/warp-extension-example/
cp target/debug/warp-extension-example ~/.warp/extensions/warp-extension-example/
```

`WARP_EXTENSIONS_DIR` overrides the extensions root if you would rather not
install into your home directory.

## Notes for plugin authors

- **stdout is the transport.** Anything printed there is read as a protocol
  frame. Log to stderr; Warp captures it into
  `~/.warp/logs/extensions/<id>.log`.
- The plugin depends only on `extension_protocol`, not on any SDK. The wire
  format is the compatibility layer; an SDK is a convenience.
- Every request that acts on the world names its execution target explicitly.
  Copy that habit: it is what keeps a remote operation off a same-named local
  path.
