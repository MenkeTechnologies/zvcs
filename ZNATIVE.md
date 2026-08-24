# znative — the zvcs plugin system

`git znative` installs and manages **plugins**: third-party code that adds git
subcommands to this binary. Two kinds share one content-addressed store:

* a **native** plugin is a Rust `cdylib` compiled against the
  [`znative` C ABI](src/plugin/src/lib.rs) and loaded with `dlopen`;
* a **script** plugin is a repository of `git-<verb>` executables — the shape
  every third-party git subcommand already ships in.

Ported from the zshrs package manager of the same name. Everything is global:
one store under `$ZVCS_HOME/pkg/`, no per-project manifest or lockfile.

```sh
git znative add MenkeTechnologies/zvcs-hello   # install
git hello                                      # the verb it added
```

`git` needs no second VCS to install a plugin: the clone runs through this
binary's own native `clone`. `cargo` is needed only for a native plugin that
ships source rather than a prebuilt library.

## Commands

| Command (aliases)            | Arguments   | What it does |
| ---------------------------- | ----------- | ------------ |
| `add` (`install`, `i`)       | `SOURCE…`   | Resolve, build if needed, install into the store, and record what the plugin provides. Multiple sources allowed. |
| `load` (`source`)            | `[SOURCE…]` | The idempotent form for a bootstrap script. Given a source not yet in the store, install it; given one already there, just re-verify it loads and refresh the derived verb tables — zero network. With no argument, every installed plugin. |
| `remove` (`rm`, `uninstall`) | `NAME…`     | Delete the store copy and drop the index row. |
| `list` (`ls`)                | —           | One line per installed plugin: `name  version  kind  source`. |
| `info` (`show`)              | `NAME`      | Full record: name, version, kind, source, store path, integrity, lib / verbs / overrides / bin. |
| `update` (`upgrade`, `up`)   | `[NAME]`    | Re-resolve and reinstall from the recorded source (one, or all) — pulls the latest upstream and rebuilds. |
| `gc`                         | `[--dry-run]` | Remove `store/<name>@<version>/` directories not pinned by the index (orphans from old versions and upgrades) plus the clone cache. `--dry-run` (`-n`) lists without deleting. |
| `clean`                      | —           | Clear the scratch directories (`git/`, `cache/`, `bin/`); the store and index are untouched. |
| `help` (`-h`, `--help`)      | —           | Usage. |

After an `update` installs a newer version, the previous
`store/<name>@<old>/` directory is left behind; `git znative gc` reclaims it.

Errors print as `znative: <reason>` on stderr and the command exits 1.

## Sources

The `add`/`load`/`update` spec is auto-classified:

| Form                              | Example                                   | Resolves to |
| --------------------------------- | ----------------------------------------- | ----------- |
| `owner/repo`                      | `MenkeTechnologies/zvcs-hello`            | clone `https://github.com/owner/repo` |
| `github:owner/repo`               | `github:owner/repo`                       | GitHub clone (explicit) |
| `git+URL`                         | `git+https://gitlab.com/team/plug.git`    | clone `URL` |
| a URL ending `.git` or with `://` | `https://example.com/x.git`               | clone `URL` |
| `path:DIR`                        | `path:examples/plugin-hello`              | local directory (no network) |
| an absolute / `./` / `../` / `~` path | `~/src/my-plugin`                     | local directory (no network) |

**Install by version** — any remote form may carry an `@ref` suffix (split after
the last `/`) to pin a tag, branch, or commit: `owner/repo@v1.2.0`,
`git+https://host/x.git@main`. The pin is **recorded** in the index
(`source = github:owner/repo@v1.2.0`), so `list` shows it and `update`
re-fetches that exact ref rather than HEAD. Clones are shallow
(`clone --depth 1 [--branch REF]`); a commit id a shallow `--branch` clone
cannot reach falls back to a full clone + `checkout`.

## Plugin kinds

| Kind       | Served by                                | Built with |
| ---------- | ---------------------------------------- | ---------- |
| **native** | `dlopen` + the handler the plugin registered | `cargo build --release` when no prebuilt `lib*.{dylib,so}` is present |
| **script** | `exec` of `<store>/…/git-<verb>`          | nothing — run as shipped |

When there is no explicit `znative.toml`, the kind is auto-detected:

1. a prebuilt `lib*.{dylib,so}` at the repo root, **or** a `Cargo.toml` whose
   `[lib] crate-type` includes `cdylib` → **native**;
2. otherwise any executable `git-<verb>` at the root or in `bin/` → **script**;
3. otherwise `znative` reports that it cannot determine the kind.

## Where a plugin verb resolves

A plugin verb is looked up **after** built-in verbs and aliases and **before**
the `git-<verb>` PATH lookup — the slot git gives dashed externals, and the one
zsh gives an autoloaded module builtin. So a plugin's `git hello` wins over a
`git-hello` script someone happens to have on PATH, and no plugin can shadow a
built-in verb by accident: `add` refuses a plugin whose *added* verb is already
a git command, and refuses one whose verb another installed plugin owns.

A native plugin may instead **override** an existing verb. The override runs in
place of the built-in implementation and calls the host back through
`dispatch_verb` when it wants the original — so a plugin can wrap `git blame`
without reimplementing it. `git znative` itself can never be overridden, so a
misbehaving plugin cannot lock you out of removing it.

## The store

Everything lives under `$ZVCS_HOME/pkg/` (default `~/.zvcs/pkg/`):

```
$ZVCS_HOME/pkg/
  store/<name>@<version>/   # the installed plugin (content-addressed)
  installed.toml            # the global index — the source of truth
  verbs.tsv                 # derived: verb -> plugin
  overrides.tsv             # derived: overridden verb -> plugin
  git/                      # scratch: clones land here, then copy to store/
  cache/  bin/              # internal scratch
```

The copy into `store/` excludes `.git/` and `target/`, so the store holds only
loadable content, and file modes are preserved (a script plugin's verb must stay
executable). Each install is SHA-256 pinned as `sha256-<hex>`. A record:

```toml
[[package]]
name = "hello"
version = "0.1.0"
source = "github:MenkeTechnologies/zvcs-hello"
kind = "native"
integrity = "sha256-…"
lib = "libzvcs_plugin_hello.dylib"   # native: the cdylib to dlopen
verbs = ["hello"]                    # what it added
overrides = ["version"]              # what it replaced
# script plugins record instead:
# bin = ["bin"]
```

### Why the two `.tsv` tables exist

A shell loads its plugins once into a process that then lives for hours. `git`
is a fresh process per command, so **nothing is loaded until a verb proves to
belong to a plugin**. The verbs a native plugin registers are discovered by
loading it once at install time — never declared, so the record cannot lie — and
written to `verbs.tsv` / `overrides.tsv`. Dispatch reads those, then `dlopen`s
exactly the one library that owns the verb.

Both tables are **deleted rather than written empty** when they have no rows.
That is the point: a machine with no plugins installed pays two failed `stat`s
per command and never opens a file.

They are pure projections of `installed.toml` — `git znative load` rebuilds them
if they are ever lost.

## `znative.toml` (optional manifest)

A plugin may ship a `znative.toml` at its root to declare metadata and the load
recipe explicitly (it overrides auto-detection):

```toml
[plugin]
name = "hello"
version = "0.1.0"
description = "the example zvcs plugin"

# Native (Rust cdylib) plugin:
[native]
lib = "zvcs_plugin_hello"   # produces lib<lib>.{dylib,so}
build = true                # default: true when a Cargo.toml is present

# …OR a script plugin:
[script]
bin = ["bin"]               # dirs holding the git-<verb> executables (default ".")
verbs = ["hello"]           # default: every git-<verb> found in bin
```

Without a manifest the name and version come from the source basename and
`0.0.0`.

## Writing a native plugin

The full working example is [`examples/plugin-hello`](examples/plugin-hello).

```rust
use std::os::raw::c_int;
use znative::{declare_plugin, Args, Host};

fn hello(host: &Host, args: &Args) -> c_int {
    let head = host.repo_info("head").unwrap_or_else(|| "(unborn)".into());
    host.print(&format!("hello from {} at {head}\n", args.name()));
    0
}

declare_plugin! {
    name: "hello",
    version: "0.1.0",
    verbs: { "hello" => hello },
}
```

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
znative = { git = "https://github.com/MenkeTechnologies/zvcs" }
```

Then `git znative add path:.` builds it, installs it, and `git hello` runs.

### The host API

Inside a handler, `Host` is the shadow binary's callback table:

| Method | Purpose |
| --- | --- |
| `print` / `eprint` | write to stdout / stderr |
| `run(verb, args)` | run a `git` subcommand **in-process** and get its status — no fork |
| `dispatch_verb(verb, args)` | run a verb's *original* implementation; how an override delegates |
| `config_get` / `config_set` | read / write a git config value through the porcelain's own resolution |
| `repo_info(field)` | `gitdir`, `workdir`, `head`, `branch` |
| `resolve_rev(spec)` | a revision spec → object id |
| `object_read` / `object_write` | read / write an object in the repository |
| `register_verb` / `register_override` | add a subcommand / replace an existing one (usually via `declare_plugin!`) |

### ABI stability

The boundary is `#[repr(C)]` structs and `extern "C"` function pointers, and
both sides compile against the same `znative` crate so they agree on the exact
layout. Nothing about Rust's unstable `repr(Rust)` layout, allocator, or panic
ABI crosses it.

Two gates make a mismatch a refusal rather than undefined behaviour: every table
carries a magic word (`ZVCSPLUG`), and `ABI_VERSION` must match the host's
exactly. The init symbol is `zvcs_native_init`, which the zshrs SDK of the same
crate name does not export — a shell plugin handed to this host is rejected
before any struct is read.

`ABI_VERSION` is bumped on **any** change to the table layout or semantics.
