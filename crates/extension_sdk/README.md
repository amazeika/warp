# extension_sdk

Client-side conveniences for writing a Warp extension in Rust.

The SDK is optional. `extension_protocol` is the compatibility layer, and a
plugin in any language that can frame JSON on stdio is a first-class plugin.
This crate only saves a Rust plugin from rewriting request correlation and the
typed call wrappers.

```rust,ignore
let mut client = Client::from_stdio();
let handshake = client.initialize("dev.warp.git")?;

let context = client.workspace_context()?;
let status = client.run(
    &context.execution_target,
    &context.cwd,
    "git",
    &["status".to_owned(), "--porcelain=v2".to_owned(), "-z".to_owned()],
)?;

if client.supports(Capability::DiffOpenV1) {
    client.open_file_diff(&context.execution_target, root, path, DiffComparison::HeadToWorktree)?;
}
```

Two things worth knowing:

- **`ClientError::Extension` is not a crash.** A refusal — a denied permission,
  an unadvertised capability — is a normal answer a plugin should handle, and it
  is kept distinct from a transport failure for that reason.
- **Check `supports` before using an optional surface.** A plugin built against
  a newer Warp must degrade on an older one rather than fail.
