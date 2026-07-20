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
| **`index.lock` contention.** Git guards index writes with an `O_EXCL` lockfile; a contended writer does not wait, it *fails* (`Unable to create '.git/index.lock'`). Under N agents that is a thundering herd of retries with no fairness. | **`zdaemon`** — a per-repo coordinator replaces the flock with a FIFO userspace barrier. A contended `ACQUIRE` blocks in arrival order and is answered `GRANTED` only when its turn comes; first-come-first-served, no starvation. |
| **Detached HEAD by default.** `git submodule update` leaves every submodule on a detached HEAD, so committed work is orphaned unless the caller re-attaches by hand. | **`zsync`** — reconciles each submodule to its tracked mainline (`origin/main`, falling back to `origin/master`) and leaves `HEAD` *attached* to that branch. Fast-forward only; a dirty worktree is skipped untouched. |
| **Stale / regressed submodule pointers.** A blanket `git add` of a stale submodule worktree can move the parent's recorded gitlink *backwards*. | **`zbump`** — forward-only gitlink bumps: a pointer is staged only when the submodule's current HEAD is a descendant of the commit the parent already records. |

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

| Verb | Namespace | Status |
|------|-----------|--------|
| `zdaemon` | superset | Implemented — `start` / `stop` / `status`. |
| `zsync` | superset | Implemented — reconcile submodules to mainline, kept attached. |
| `zbump` | superset | Implemented — forward-only gitlink bumps. |
| every stock subcommand | git-compat | Dispatched — depth varies; see the parity report. |

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

**`git zsync [<submodule-path>...]`** — Reconcile every submodule (or the named
subset) to its tracked mainline. Mainline detection prefers
`refs/remotes/origin/main`, falls back to `origin/master`, and skips the repo if
neither exists. The operation fetches origin, then fast-forwards only: a dirty
worktree is skipped, and an unpushed local commit is never regressed or
clobbered. On a genuine fast-forward the local mainline branch is advanced,
`HEAD` is re-attached to it, and the clean worktree plus index are moved to the
new tree by writing only the files that actually changed.

**`git zbump [<submodule-path>...]`** — Advance the parent's recorded gitlink to
each submodule worktree's current HEAD, but **only** when that HEAD is a
descendant of the pointer already recorded (a fast-forward). It never regresses
or diverges a pointer. The parent index is opened once, mutated, and staged once
at the end, so tools on `PATH` observe the same staged index.

**`git zdaemon <start|stop|status>`** — The per-repo coordinator. See below.

## [0x05] THE zdaemon COORDINATOR

`zdaemon` is the linchpin of the concurrency story — zvcs's fair replacement for
`index.lock`. A single worker thread owns the abstract critical section and
drains an mpsc channel of requests *in arrival order*; that arrival order is the
fairness guarantee.

Clients reach it through `RepoLock::acquire` (`src/extensions/src/lock.rs`), an
RAII guard that routes every index-mutating operation through the daemon's FIFO
queue and returns only when the caller holds the lock. Release is automatic on
drop, and the daemon also auto-releases on socket EOF, so a crashed holder can
never wedge the repo. If no daemon is reachable the lock degrades to a **no-op
guard** — the operation still runs (stock-git behavior minus the fair queue).

Wire protocol — line-based, one request per line, over the unix socket at
`<git-dir>/zvcs.sock`:

| Line | Direction | Meaning |
|------|-----------|---------|
| `ACQUIRE <id>` | client → daemon | Enqueue a lock request; answered `GRANTED` at the FIFO head. |
| `RELEASE <id>` | client → daemon | Current holder releases; the next waiter is granted. |
| `STATUS` | client → daemon | Reply `holder=<id\|none> queue=<depth>`, then close. |
| `STOP` | client → daemon | Reply `STOPPING`, remove the socket, exit. |
| `GRANTED` | daemon → client | The lock is now yours. |
| `ERR <reason>` | daemon → client | Malformed request. |

### Autonomous mode

The superset verbs also run automatically, gated by `[zvcs]` gitconfig, so they
never have to be typed:

```gitconfig
[zvcs]
    autoreconcile = true   ; keep every clean repo (this one + submodules) at origin/main
    autobump      = true   ; forward-only submodule gitlink bumps
    interval      = 30     ; seconds between passes (default 30)
```

When any autonomy is enabled, any `git` invocation auto-spawns the per-repo
daemon (detached, output to `<git-dir>/zvcs.log`); its background timer threads
run `reconcile_tree` (fast-forward every clean repo to its mainline) and/or
`zbump` on `interval`. A dirty worktree or a diverged/ahead local branch is
always skipped — autonomy never regresses or clobbers in-flight work.

## [0x06] LAYOUT

| Path | Contents |
|------|----------|
| `src/ported` | Vendored gitoxide crates (`gix` + the `gix-*` library crates), in-tree. A self-contained workspace, excluded from the root and consumed as a path dependency. The `gix`/`ein` CLI binaries and their `gitoxide-core` backend are removed; `git` is the only binary. |
| `src/extensions` | The zvcs crate (a library + the `git` binary): `main.rs`/`lib.rs` (entry), `dispatch.rs` (routing), `porcelain.rs` (git-compat), `lock.rs` (daemon client), `config.rs` (`[zvcs]` settings), `autostart.rs` (daemon auto-spawn), and `superset/` (`zdaemon.rs`, `zsync.rs`, `zbump.rs`, `reconcile.rs`). |

## [0x07] STATUS & ROADMAP

Early and in active development.

The coordination layer is implemented: the `git` shadow binary and dispatch, all
three superset verbs (`zdaemon`, `zsync`, `zbump`) with the `RepoLock` daemon
client, and the autonomous `[zvcs]` mode (config-gated auto-spawn + background
reconcile/bump). FCFS lock serialization and the fetch→ff→attach→worktree
reconcile are covered by tests.

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
- **Engineering report** — <https://menketechnologies.github.io/zvcs/report.html>
- **gitoxide** — <https://github.com/GitoxideLabs/gitoxide> (the ported library)
- **Source** — <https://github.com/MenkeTechnologies/zvcs>

## [0xFF] LICENSE

MIT — free and open source. See [LICENSE](LICENSE).
