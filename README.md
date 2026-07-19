# zvcs

A git-shadowing superset VCS. Pure Rust, built on vendored gitoxide, with no
fork/exec of stock git — a single binary named `git` shadows it on PATH and
serves every subcommand natively.

zvcs targets the failure modes of driving a large meta-repo of submodules under
many concurrent automated agents, which stock git handles poorly:

- **`index.lock` contention** — replaced by a per-repo coordinator with a FIFO
  queue/barrier; a contended writer waits its turn instead of failing.
- **Detached-HEAD by default** — a background reconciler keeps submodules
  *attached* to their tracked mainline (`origin/main`/`origin/master`).
- **Stale / regressed submodule pointers** — forward-only gitlink bumps: a
  pointer is staged only when the new commit is a descendant of the recorded one.

## Layout

| Path             | Contents                                                                 |
|------------------|--------------------------------------------------------------------------|
| `src/ported`     | Vendored gitoxide crates (`gix` + the `gix-*` library crates), in-tree.  |
| `src/extensions` | The zvcs crate: the `git` shadow binary, git-compat porcelain, superset. |

`src/ported` is a self-contained workspace, excluded from the root workspace and
consumed by `src/extensions` as a path dependency. The `gix`/`ein` CLI binaries
and their `gitoxide-core` backend are removed; `git` is the only binary.

## Build

```sh
cargo build
./target/debug/git rev-parse HEAD
```

## Subcommands

Superset verbs (not present in stock git):

| Verb       | Purpose                                                          |
|------------|------------------------------------------------------------------|
| `zdaemon`  | Per-repo coordinator: queue/barrier + background reconcile.      |
| `zsync`    | Reconcile submodules to their mainline, kept attached.           |
| `zbump`    | Forward-only submodule gitlink bumps.                            |

git-compat porcelain is ported incrementally; implemented so far: `rev-parse`
(resolves `HEAD`, `--abbrev-ref`).
