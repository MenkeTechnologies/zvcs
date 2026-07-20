```text
███████╗██╗   ██╗ ██████╗███████╗
╚══███╔╝██║   ██║██╔════╝██╔════╝
  ███╔╝ ██║   ██║██║     ███████╗
 ███╔╝  ╚██╗ ██╔╝██║     ╚════██║
███████╗ ╚████╔╝ ╚██████╗███████║
╚══════╝  ╚═══╝   ╚═════╝╚══════╝
```

![Rust](https://img.shields.io/badge/Rust-2021-05d9e8?style=flat-square)
[![Docs](https://img.shields.io/badge/docs-online-blue.svg)](https://menketechnologies.github.io/zvcs/)
[![Built on](https://img.shields.io/badge/built%20on-gitoxide-8a2be2.svg)](https://github.com/GitoxideLabs/gitoxide)
![status](https://img.shields.io/badge/status-early%20%C2%B7%20in%20development-9b5de5?style=flat-square)
![license](https://img.shields.io/badge/license-MIT-ff2a6d?style=flat-square)

### `[GIT, SHADOWED AND SUPERSET — ONE RUST BINARY NAMED git]`

> *"gitoxide already ported git to Rust. zvcs does the thing git structurally
> cannot: fair mutual exclusion, self-attaching submodules, forward-only
> pointer bumps — a VCS built for a meta-repo under many concurrent agents."*

**zvcs** is a git-shadowing superset VCS. It ships a single Rust binary named
`git` that shadows stock git on `PATH` and serves subcommands natively via
vendored [gitoxide](https://github.com/GitoxideLabs/gitoxide) crates — there is
**no fork/exec of stock git**. On top of git compatibility it adds the zvcs
**superset**: coordination verbs stock git cannot have, aimed at the exact
failure modes of driving a large meta-repo of submodules under many concurrent
automated agents.

The world's-first leg is not "git in Rust" — gitoxide is already that. It is the
superset coordination layer: a fair FIFO index-lock daemon, a
reconcile-to-mainline attacher, and forward-only gitlink bumps, served from the
same binary that answers `rev-parse`.

### [`Read the Docs`](https://menketechnologies.github.io/zvcs/) &middot; [`Engineering Report`](https://menketechnologies.github.io/zvcs/report.html) &middot; [`gitoxide`](https://github.com/GitoxideLabs/gitoxide)

---

## Table of Contents

- [\[0x00\] Overview](#0x00-overview)
- [\[0x01\] The Problem It Solves](#0x01-the-problem-it-solves)
- [\[0x02\] Build](#0x02-build)
- [\[0x03\] Subcommands](#0x03-subcommands)
- [\[0x04\] The Superset Verbs](#0x04-the-superset-verbs)
- [\[0x05\] The zdaemon Coordinator](#0x05-the-zdaemon-coordinator)
- [\[0x06\] Layout](#0x06-layout)
- [\[0x07\] Status & Roadmap](#0x07-status--roadmap)
- [\[0x08\] Documentation](#0x08-documentation)
- [\[0xFF\] License](#0xff-license)

---

## [0x00] OVERVIEW

zvcs is a from-source VCS, not a wrapper. The `git` binary discovers and reads
the same on-disk `.git` directory stock git does, so tools already on `PATH`
(RustRover, `gh`, `cargo`) see identical behavior. Git-compat porcelain is
ported incrementally on top of the vendored gitoxide (`gix`) library; when a
subcommand is not yet ported the binary errors terse rather than falling through
to stock git — this is a from-scratch engine, not a shim.

The reason zvcs exists is the superset. The meta-repo it targets is a shell of
git submodules driven by up to 16 concurrent automated agents. Stock git handles
that topology poorly in three specific, reproducible ways, and each superset verb
closes one of them.

## [0x01] THE PROBLEM IT SOLVES

| Stock-git failure mode | zvcs answer |
|------------------------|-------------|
| **`index.lock` contention.** Git guards index writes with an `O_EXCL` lockfile; a contended writer does not wait, it *fails* (`Unable to create '.git/index.lock'`). Under N agents that is a thundering herd of retries with no fairness. | **`zdaemon`** — one machine-wide daemon with a **per-repo FIFO lane** replaces the flock. A contended writer blocks in arrival order and is answered `GRANTED` only at the head of its repo's lane; first-come-first-served, no starvation, unrelated repos fully parallel. |
| **Detached HEAD by default.** `git submodule update` leaves every submodule on a detached HEAD, so committed work is orphaned unless the caller re-attaches by hand. | **`zsync`** + the daemon's **attach-scan** reconcile each submodule to its tracked mainline (`origin/main`, else `origin/master`) and keep `HEAD` *attached* — even a dirty detached HEAD is rescued in place (no-clobber ref op). Fast-forward only. |
| **Constant `modified: <sub> (new commits)` markers + stale pointers.** Every submodule commit dirties the parent's gitlink; a blanket `git add` can also move it *backwards*. | **`zbump`** + **autobump** — forward-only gitlink bumps, **committed** (clears the marker), coalesced, done by the daemon on a file-watch so agents never touch the root. Never regresses a pointer. |
| **Agents colliding on one shared tree.** N agents editing one meta tree collide on files, index, and HEAD. | **`zworktree`** — one command gives each agent a private, object-sharing worktree of the whole submodule tree; complete isolation, no re-clone. |

## [0x02] BUILD

```sh
git clone https://github.com/MenkeTechnologies/zvcs
cd zvcs
cargo build
./target/debug/git rev-parse HEAD
```

The workspace has two members-by-convention: `src/ported` (the vendored gitoxide
crates, a self-contained workspace excluded from the root) is consumed by
`src/extensions` (the zvcs crate) as a path dependency. `gix` is built with the
`blocking-http-transport-reqwest-rust-tls` feature so `zsync`'s reconcile fetch
runs over HTTPS on a pure-Rust TLS stack — no curl/openssl C toolchain.

## [0x03] SUBCOMMANDS

Two namespaces share one dispatch table (`src/extensions/src/dispatch.rs`):

- **superset** verbs (`z*`) — the novel coordination layer.
- **git-compat** porcelain — stock git subcommands served via gitoxide, ported
  incrementally.

| Verb group | Verbs | What it does |
|---|---|---|
| Coordination | `zdaemon` `zsync` `zbump` | singleton daemon; reconcile-to-mainline; forward-only pointer bumps |
| Repo index | `zrepos` `zreindex` | machine-wide index of every git repo (retires a shell repo-list) |
| Async queue | `zcommit` `zpush` `zjobs` `zjob` | fire-and-forget commit/push jobs + ledger (`zjob stop`/`restart`) |
| Multi-agent | `zclaim` `zunclaim` `zwho` | advisory per-repo leases so agents don't collide |
| Observability | `zstatus [--all]` `zlog` `zundo` | instant machine-wide status; cross-repo timeline; one-step rewind |
| Snapshots | `zsnapshot` `zrestore` `zsnapshots` | tree-wide restore points across all submodules |
| Worktrees | `zworktree add/list/remove` | per-agent isolated, object-sharing worktree of the whole tree |
| Console | `zrepl` | interactive line console over the verbs |
| git-compat | every stock subcommand | dispatched natively; depth varies — see the parity report |

Every subcommand stock git ships has a dispatch arm, so nothing reaches the
`not yet ported` path; there is no fallthrough to stock git. Dispatching is not
the same as agreeing with git, and the two are measured separately — an
unimplemented flag errors terse rather than guessing, and the parity harness
scores that as a failure.

Run the harness to see current depth per subcommand:

```sh
cargo run -p zvcs-parity                 # curated corpus
cargo run -p zvcs-parity -- --fuzz 12    # plus generated flag combinations
```

It builds fixture repositories with stock git, runs each invocation against both
binaries, and compares stdout, exit code, and the resulting repository state.

## [0x04] THE SUPERSET VERBS

**Coordination.** `git zsync [<path>...]` reconciles submodules to their tracked
mainline (`origin/main`, else `origin/master`), fast-forward only, leaving `HEAD`
attached; a dirty worktree is skipped. `git zbump [<path>...]` advances the
parent's gitlink to each submodule's HEAD **only** on a fast-forward, then
**commits** the coalesced bumps (clearing the `(new commits)` marker). `git
zdaemon <start|stop|status>` controls the singleton coordinator (below).

**Repo index.** `git zreindex [<path>...]` crawls for git repositories and
records them in the ledger, pruning ones deleted from disk; `git zrepos` lists
them (pipe-clean, one path per line) — a drop-in for a shell git-repo index.

**Async queue.** `git zcommit <paths> -m <msg> [--push]` and `git zpush` submit
fire-and-forget jobs to the daemon (with a network-free / live `ls-refs` push
pre-flight that refuses a non-fast-forward before enqueue) and return a job
number; `git zjobs` / `git zjob <id>` show the ledger, and `git zjob stop|restart
<id>` control a running/queued job. Falls back to synchronous execution when no
daemon is running.

**Multi-agent.** `git zclaim [<path>]` takes an advisory per-repo lease for the
session (`ZVCS_SESSION`), refusing if another agent holds it; `git zunclaim`
releases and `git zwho` lists who holds what.

**Observability.** `git zstatus` reports the current repo's status live; `git
zstatus --all` reads every indexed repo's status from the daemon-maintained
cache (zero-walk). `git zlog` merges every repo's reflog into one machine-wide
timeline; `git zundo [<path>]` rewinds a repo one step (`reset --hard` to the
previous HEAD, refuses on dirty).

**Snapshots.** `git zsnapshot <name>` records the HEAD of the repo + every nested
submodule as one restore point; `git zrestore <name>` resets the whole tree back
to it; `git zsnapshots` lists them.

**Worktrees.** `git zworktree add <name>` provisions a complete, object-sharing,
isolated worktree of the repo + all nested submodules (each on a `zwt/<name>`
branch) at `~/.zvcs/worktrees/<name>/`, so each agent gets a private tree that
can't collide with any other — no re-clone. `list` / `remove <name>` manage them.

**Console.** `git zrepl` opens an interactive line console over all the verbs.

## [0x05] THE zdaemon COORDINATOR

`zdaemon` is one machine-wide daemon (state under `~/.zvcs/`, socket
`~/.zvcs/zvcs.sock`) — the fair replacement for `index.lock` plus the host for
autonomy, the SQLite ledger, and the async job queue. It is reactive: **no
timers, no polling**; a `git pull`/commit updates local refs, a `notify`
file-watch fires, and the daemon reacts. It never contacts a remote itself.

The lock is **per-repo**: unrelated repos run fully in parallel; only writers to
the same repo serialize, first-come-first-served. Clients reach it through
`RepoLock::acquire` (`src/extensions/src/lock.rs`), an RAII guard; release is
automatic on drop and on socket EOF, so a crashed holder can't wedge a repo. With
no daemon the lock degrades to a no-op guard (the op still runs). Index writes
also go through `index.lock` via `gix-lock` for interop with stock git.

Wire protocol — line-based over the unix socket:

| Line | Direction | Meaning |
|------|-----------|---------|
| `ACQUIRE <id> <git-dir>` | client → daemon | Enqueue on that repo's lane; answered `GRANTED` at its head. |
| `RELEASE <id>` | client → daemon | Current holder releases; next waiter granted. |
| `SUBMIT <json>` | client → daemon | Queue an async job; answered `JOB <id>`. |
| `JOBSTOP <id>` / `JOBRESTART <id>` | client → daemon | Cancel / re-enqueue a job. |
| `STATUS` / `STOP` | client → daemon | Snapshot / shut down. |

### Autonomous mode + configuration

All autonomy is gated by `[zvcs]` gitconfig and **defaults off**, so it runs in
the dev environment and nowhere else. Enable it in `~/.gitconfig` or a repo's
`.git/config`; stock git ignores the keys:

```gitconfig
[zvcs]
    autoreconcile = true            ; reconcile clean submodules to origin/main (reactive)
    autobump      = true            ; forward-only local pointer bumps + commit (kills the marker)
    interval      = 2               ; debounce window (seconds) for coalescing bursts
    autocrawl     = true            ; background repo-index crawl on daemon start
    crawlroots    = /abs/src /abs/wk ; crawler roots (absolute; default $HOME)
    autostatus    = true            ; maintain zstatus --all
    hook          = /abs/on-change  ; run on ref-change in any indexed repo (typed event env)
    worktreebase  = /abs/worktrees  ; base for zworktree (default ~/.zvcs/worktrees)
```

When anything is enabled, a `git` invocation auto-spawns the daemon (detached,
output to `~/.zvcs/zvcs.log`); it watches indexed repos and reacts by attaching
detached HEADs, fetch-free reconciling, forward-only autobumping, maintaining
status, and firing hooks. A dirty worktree or a diverged/ahead branch is always
skipped — autonomy never regresses or clobbers in-flight work. Headless failures
are recorded in the ledger and surfaced on your **next `git` command** (stderr).

Hooks get a typed environment: `ZVCS_EVENT` (commit/checkout/merge/pull/rebase/
reset), `ZVCS_REPO`, `ZVCS_GIT_DIR`, `ZVCS_OLD_SHA`, `ZVCS_NEW_SHA`, `ZVCS_REF` —
enough for "on commit in X, do Y in repo Z" cross-repo rules.

## [0x06] LAYOUT

| Path | Contents |
|------|----------|
| `src/ported` | Vendored gitoxide crates (`gix` + the `gix-*` library crates), in-tree. A self-contained workspace, excluded from the root and consumed as a path dependency. The `gix`/`ein` CLI binaries and their `gitoxide-core` backend are removed; `git` is the only binary. |
| `src/extensions` | The zvcs crate (library + the `git` binary): `main.rs`/`lib.rs` (entry, `session_key`, notify-on-next-command), `dispatch.rs` (routing), `porcelain/` (git-compat), `lock.rs` (daemon client), `config.rs` (`[zvcs]` settings), `autostart.rs` (daemon auto-spawn), `db.rs` (SQLite ledger/index), `crawler.rs` (repo crawl), `jobpool.rs`/`jobrun.rs`/`index_commit.rs` (async jobs), `worktree.rs` (checkout helper), and `superset/` (`zdaemon`, `zsync`, `zbump`, `reconcile`, `attach`, `watch`, `hooks`, `ledger`, `status`, `oplog`, `snapshot`, `claim`, `queue`, `repl`, `zworktree`). |

## [0x07] STATUS & ROADMAP

Early and in active development.

The coordination and superset layers are implemented and tested: the singleton
daemon with per-repo FIFO lanes; reactive file-watcher autonomy (attach,
autobump-with-commit, fetch-free reconcile); the SQLite ledger + repo index;
async `zcommit`/`zpush` with job control; multi-agent claims; machine-wide
`zstatus`; the cross-repo op ledger (`zlog`/`zundo`); tree-wide snapshots; typed
cross-repo hooks; and per-agent isolated worktrees (`zworktree`). Each is covered
by an integration test, and zvcs↔stock-git interoperability (round-trip read,
`git fsck`, submodule pointer bumps, worktrees) is verified by a regression
suite. See [DESIGN.md](DESIGN.md) for the architecture and the honest list of
partials.

Git compatibility is tracked as two independent numbers, because a subcommand
that dispatches is not thereby correct:

- **Coverage** — every subcommand stock git ships is dispatched natively.
- **Parity** — the share of harness cases whose stdout, exit code, and resulting
  repository state match stock git exactly.

Parity is the number that matters and it is the work that remains. Depth varies
widely per subcommand; some are byte-faithful across their documented flag set,
others implement the common flags and bail terse on the rest. A few subcommands
are honest skeletons that name the missing substrate instead of pretending:
the foreign-SCM bridges (`p4`, `cvsimport`, `cvsserver`, `cvsexportcommit`,
`archimport`) and the shell/Perl tools (`imap-send`, `instaweb`, `subtree`,
`filter-branch`) have no gitoxide backing to port onto.

Mutating subcommands are currently excluded from generated fuzz cases, so their
parity rests on curated cases only. Treat their scores as less well covered than
the read-only ones rather than as evidence of correctness.

## [0x08] DOCUMENTATION

- **Docs hub** — <https://menketechnologies.github.io/zvcs/>
- **Design document** — [DESIGN.md](DESIGN.md) — daemon architecture, concurrency model, autonomous behaviors, ledger/queue
- **Engineering report** — <https://menketechnologies.github.io/zvcs/report.html>
- **gitoxide** — <https://github.com/GitoxideLabs/gitoxide> (the ported library)
- **Source** — <https://github.com/MenkeTechnologies/zvcs>

## [0xFF] LICENSE

MIT — free and open source. See [LICENSE](LICENSE).
