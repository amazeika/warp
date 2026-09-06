# warp-git

Day-to-day Git inside Warp, as an extension rather than as part of Warp itself.

Warp core gains no Git code. The plugin decides *what* to do; Warp supplies the
environment, runs the command, and draws the result — which is what makes the
same binary work against a local repository and one on the far side of an SSH
session without knowing how Warp's SSH sessions are implemented.

## What it does

**Panel** — repository summary (branch, upstream, ahead/behind, any paused
operation), conflicts, staged, changes, untracked, branches, remote branches,
and recent history.

**Row actions** — open diff, open file, stage, unstage, discard, delete
untracked file, switch/rename/delete branch, check out a remote branch.

**Commands** — refresh, stage all, unstage all, commit, fetch, pull, push,
force push (with lease), create branch, switch branch, rebase, continue rebase,
abort rebase, abort merge.

## Design

**The installed `git` binary does the work.** Reimplementing Git's primitives
would mean reimplementing `.gitconfig`, credential helpers, commit signing and
hooks along with them, and diverging from what the user's own terminal does in
the same repository.

**Machine-readable output only.** `status --porcelain=v2 -z`, `for-each-ref`
with an explicit format, `log` with ASCII record separators. Human-oriented
output is decorated, localised and unstable; `-z` in particular is what makes a
path containing a space, a quote or a newline unambiguous.

**argv, never a shell string.** Every command is an argument vector and every
path-taking command separates its paths with `--`, so a file named like a branch
cannot be reinterpreted as one.

**Warp renders every destructive confirmation.** The plugin marks an action
destructive and supplies the text; it cannot make an irreversible action look
routine. A force update uses `--force-with-lease`, never `--force`, so a
teammate's commits cannot be discarded by someone who has not fetched recently.
Deleting a branch tries the safe delete first and only escalates after telling
the user how many commits would be lost.

**Actions stay where they started.** Every event carries its own origin —
workspace, session, repository, execution target — and the plugin acts on that
rather than on wherever focus has since moved.

## Not yet

- Opening a specific commit's diff needs `diff.openCommit`, which the v0.1 API
  does not have; the row opens the working tree and says so.
- Stash, cherry-pick, revert, tags, worktrees, and line/hunk staging.
- Copying a commit hash needs a clipboard capability the API does not expose.

## Installing it

```bash
cargo build -p extension_git
mkdir -p ~/.warp/extensions/warp-git
cp crates/extension_git/extension.toml ~/.warp/extensions/warp-git/
cp target/debug/warp-git ~/.warp/extensions/warp-git/
```

Nothing in Warp reads that directory yet — the app-side host is a later PR.
Until then `cargo nextest run -p extension_git` is how to exercise it; the
end-to-end tests drive the real binary against real temporary repositories.
