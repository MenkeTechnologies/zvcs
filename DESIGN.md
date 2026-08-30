---
title: zvcs Design
---

# zvcs — Design Document

A git-shadowing superset VCS: a pure-Rust `git` binary (built on vendored
gitoxide) plus a singleton coordination daemon that removes three structural
pains of a many-agent, deeply-nested-submodule monorepo.

This document was written in layers, and sections that described work in the
future tense kept that tense after the work landed — four features were still
advertised here as gaps long after they shipped. Where this prose and the code
disagree, **the code is right**: every claim below is anchored to the file that
implements it so the disagreement is findable, and a claim with no such anchor
should be checked before it is relied on.

## 1. Motivating problem

The target workload: one meta repository that is a shell of ~162 git submodules
(some nested one level deeper), worked by ~16 concurrent CLI agents launched from
the **meta root**, one agent per submodule, with cross-submodule work common.
Stock git makes this painful in three specific ways:

1. **`index.lock` contention.** Git guards index writes with an `O_EXCL`
   lockfile that *fails fast* — a contended writer does not queue, it dies
   (`fatal: Unable to create '.git/index.lock': File exists`). Under many agents
   this is a thundering herd of failures and retry loops. The lockfile is held
   for the *entire* index-touching span of an operation because git uses that
   same lockfile as the scratch file for the new index (open → write new index
   into it → `rename()` over `index`). Long hold × fail-fast × many writers =
   constant failure.

2. **Constant `modified: <sub> (new commits)` markers.** The moment an agent
   commits inside a submodule, the parent's recorded gitlink is stale — a purely
   **local** comparison of the submodule's HEAD against the gitlink in the
   parent index/HEAD. The marker persists until the parent *commits* the new
   gitlink. No remote is involved in detecting or fixing it.

3. **The detached-HEAD dance.** `git submodule update` leaves every submodule on
   a detached HEAD at the recorded pointer. Committing there orphans the work.
   So each agent must climb out first: stash → `checkout -B main origin/main` →
   stash pop, every submodule, every session. Committing on a detached HEAD is a
   silent data-loss hazard.

## 2. Architecture: two layers

### Layer 1 — faithful git subcommands (synchronous)

`git add`/`commit`/`push`/`status`/`diff`/… behave **exactly** like git:
synchronous, real exit codes, real output, real semantics. Served natively via
the vendored gitoxide crates (`src/ported`) through
`src/extensions/src/dispatch.rs` → `porcelain/`. No job numbers, no deferral, no
behavior change. Scripts and muscle memory are unaffected.

Two locks sit underneath, with distinct jobs:

- **Fair FIFO lock (zvcs-internal).** Every index-writing porcelain command
  acquires `RepoLock` before its write (`porcelain/commit.rs:85`,
  `porcelain/add.rs:271`, `pull.rs`, `merge.rs`, `fetch.rs`, `reset.rs`,
  `stash.rs`, `switch.rs`, `checkout.rs`, `rebase.rs`, …). A contended writer
  **waits its turn** in the daemon's per-repo FIFO and then succeeds, instead of
  failing on `index.lock`. Same semantics, no fail-retry storm. Already wired.

- **Lane file (the no-daemon fallback).** With no daemon reachable the guard takes
  `<git_dir>/zvcs-lane.lock` with `flock(LOCK_EX)` and holds it for the whole
  command. The fallback cannot be a no-op: an index write is a read-modify-write
  and the port's only index lock is taken at *write* time
  (`gix-index/src/file/write.rs:85`), so two unserialized writers read the same
  base index and each write back their own copy — the loser's entry is gone and
  both exit 0. Measured before the lane file: eight concurrent `git add`s on
  distinct paths, no daemon, lost a write in 9 of 10 trials with `queued=0`. Git
  does not have the bug because it locks before it reads (`builtin/add.c`:
  `repo_hold_locked_index(repo, &lock_file, LOCK_DIE_ON_ERROR)` precedes
  `repo_read_index_preload()`).

  Not `index.lock` itself: `gix-lock` acquires that with `Fail::Immediately`, so a
  zvcs guard holding it makes zvcs's own writer fail ("could not be obtained
  immediately after 1 attempt(s)"). A separate zvcs-owned file excludes zvcs peers
  without touching the writer; foreign writers stay covered by
  `wait_for_foreign_index_lock`.

  | Situation | Behavior |
  |---|---|
  | lane free | taken, held for the command, released on drop (including panic and `SIGKILL` — the kernel owns the lock) |
  | lane held by one of our own ancestors (a hook, a re-exec'd child) | reentrant: proceed without waiting, the ancestor's hold already excludes everyone |
  | lane held by a peer | wait; the budget (`ZVCS_INDEX_LOCK_WAIT_MS`, 2 s) is patience for ONE holder and restarts whenever the pid in the file changes, so a fair queue of N writers is not mistaken for a wedge |
  | one holder past the budget | exit non-zero with the holder's pid — never run unserialized, because that is the silent-loss case |
  | git dir absent, or a mount without `flock` | no lane possible; unserialized, as before |

- **`index.lock` (interop, preserved).** The on-disk `index.lock` is retained
  via `gix-lock` as the cross-implementation guard so **non-zvcs** tools (a hook
  that runs `command git`, `gh`, libgit2 tooling) cannot corrupt the index
  against a concurrent zvcs write. Its role is *demoted*: it is no longer the
  fairness mechanism (the FIFO is), only a brief interop marker held for the
  write+rename window. zvcs holds it for a fraction of git's window because
  staging/tree-build happens off the shared index; the final apply+rename is
  microseconds.

  | Scenario | Behavior |
  |---|---|
  | zvcs peer vs peer | FIFO already serialized them; `index.lock` uncontended when a job writes |
  | external git holds `index.lock` | zvcs waits (bounded), then queues the command as a job — never the raw lock error |
  | zvcs holds `index.lock` briefly | external git sees git's normal lock error (git's contract) |

  The bounded wait is `lock::wait_for_foreign_index_lock` (2 s by default,
  `ZVCS_INDEX_LOCK_WAIT_MS` overrides, `0` disables), run from `dispatch.rs`
  before any `LOCK_VERBS` command. It exists because the FIFO cannot see a
  foreign holder and the ported index writer takes the lockfile with
  `Fail::Immediately` — one attempt, no wait (`gix-index/src/file/write.rs`).
  If the lock outlasts the budget the command is **not** failed: the dispatcher
  matches the error by type (`gix::lock::acquire::Error` in the chain) and
  submits the same argv to the queue, so it runs on the repo's fair lane once
  the lock clears. A queued job's own re-run carries `ZVCS_QUEUED` and never
  re-queues, so a permanently stuck lock fails once instead of spawning jobs
  forever.

- **Ref races (lockfile-free contention).** Two writers can each take and release
  every lock cleanly and still collide: the loser's compare-and-swap on
  `refs/heads/<branch>` is rejected (`ReferenceOutOfDate`) because the winner
  moved the ref between its read and its write. No lockfile is involved, so
  `is_lock_contention` does not see it; `lock::is_ref_race` matches that variant
  through whichever `gix` wrapper carries it (`commit::Error`,
  `reference::edit::Error`, or the bare `prepare::Error` — the wrappers are
  `#[error(transparent)]`, so the inner error is a payload, never a `source()`
  link) and the dispatcher queues the command instead of dropping it. Measured on
  a 32-way `commit` fanout with no daemon: 11–12 of these per run, each formerly a
  hard exit-1 that lost the commit. That measurement predates the lane file above,
  which serializes same-repo writers even with no daemon and so removes most of
  that fanout's races at the source; the classifier stays because a ref can still
  move under a writer that took no index lock at all (a `push`, a bare
  `update-ref`, a peer on a mount without `flock`).

- **Stream interleaving (`src/extensions/src/cstdio.rs`).** git orders its two
  streams through C stdio, not explicitly: `stderr` is unbuffered, `stdout` is
  line buffered on a terminal and **fully** buffered anywhere else, and
  `start_command()` flushes with `fflush(NULL)` before every `fork()`
  (run-command.c:743). So `git checkout <branch> 2>&1 | cat` prints `Switched to
  branch …` (stderr) *before* the `show_local_changes()` listing (stdout), while
  the same command on a tty prints them the other way round. Rust's `println!` is
  a `LineWriter` whatever fd 1 is and can only produce the second order, so the
  commands whose output has to match stock byte-for-byte when both streams are
  captured — `checkout`, `switch`, `merge` — route their stdout through this
  module and arm it with `cstdio::defer()` on entry; the dispatcher flushes after
  the command, and `cstdio::before_spawn()` stands in for the pre-`fork` flush.
  Arming is per command, so a shared helper (`merge_apply`, `diff_index`,
  `merge-index`, `merge-one-file`, `read-tree`, `rerere`) can be routed through
  the buffer without changing any command that has not opted in. **The fix for a
  stream-order mismatch is never to move a line to the other stream** — the
  stream assignment is git's and is already right; the ordering is buffering.

### Layer 2 — `z*` superset verbs + singleton daemon

The novel coordination layer stock git cannot have. Verbs live under a `z`
prefix; the daemon hosts the FIFO lock, the file-watcher autonomy, the SQLite
ledger, and the async job queue.

## 3. The singleton daemon

**One** always-on process, state under `~/.zvcs/` (replaces the previous
per-repo daemon, which had no reaper and multiplied immortal processes). Socket
at `~/.zvcs/zvcs.sock`.

Thread topology — **no timers, no polling; everything is reactive**:

- **Acceptor** — owns the `UnixListener`, dispatches connections.
- **Scheduler** — owns `HashMap<RepoKey, RepoState>` (per-repo FIFO lock lane),
  lazily created on first use, dropped when idle. `RepoKey` = canonical
  `git_dir`. Invariant: ≤1 in-flight index writer per repo. This is the evolved
  `worker_loop` (`superset/zdaemon.rs:191`), whose single global critical
  section is shattered into per-repo lanes so unrelated repos never serialize
  against each other.
- **Watcher** — `notify`-based file watches; drives all autonomy (§5).
- **DB writer** — sole owner of the SQLite `Connection`; drains an mpsc of
  write-ops (§6).
- **Job pool** — bounded (~`num_cpus`) workers executing async jobs (§7).
- **Reaper** — idle-timeout shutdown (the per-repo model had none). One process,
  so this is tractable or the daemon simply stays up as the single service.

Wire protocol (line-framed), nine verbs, exactly as `zdaemon.rs` parses them:
`ACQUIRE <client-id> <git-dir>` and its non-blocking `TRYACQUIRE <client-id>
<git-dir>`, `HOLDER <git-dir>`, `RELEASE <client-id>`, `SUBMIT <job-json>`,
`JOBSTOP <id>`, `JOBRESTART <id>`, `STATUS`, `STOP`. The client id comes first;
the repo key is the second field, not the first. `zreindex` and `zrepl` are not
wire verbs — the crawler and the console both go through the ledger directly.

**Lifetime:** a daemon exits when the socket it serves or its own pid file
disappears — the pid file rather than the home directory, because `zvcs_home()`
recreates the directory as a side effect of every log line, so a directory check
could never fire. It matters for the deep-`ZVCS_HOME` case: such a home's socket
falls back to a short `/tmp` path that survives the home's teardown, and without
the pid-file check the daemon outlived the sandbox that created it. A daemon
whose home is explicitly set AND lives under the OS temp dir — the shape every
test harness uses, never a real installation — additionally exits after 5
minutes with no client, so an interrupted test run cannot leave daemons behind
for the rest of the session. The daily-driver daemon in `~/.zvcs` is never
idle-reaped.

**Watch set:** rebuilt from the repo index every 5s, not only at startup. Repos
arrive at any time — the background crawler finds them, `zreindex` adds them, a
clone appears mid-session — and a set fixed at startup left every one of them
unwatched until the daemon was restarted, with its hooks silently never firing.
New paths are registered; a path already watched is skipped rather than watched
twice, and a repo that disappeared keeps its (harmless) watch until restart
rather than racing a repo that is merely being rewritten.

**Control surface:** `git zdaemon <start|stop|restart|reload|status|info|ping|log>`.
`restart`/`reload` STOP the running daemon and respawn it detached (re-reading
`[zvcs]` config, rebuilding the watch set); `ping` is a scriptable liveness check
(exit 0/1); `info` reports pid (from `~/.zvcs/zvcs.pid`, written on start),
socket/home/db paths, the live lane snapshot, and the resolved config; `log [-n
N] [-f]` shows/tails `~/.zvcs/zvcs.log`. These are client-side wrappers over the
existing `STATUS`/`STOP` protocol + the pidfile — no new wire verbs.

## 4. Concurrency model & submodule topology

Each submodule is its own repository with its own index at
`<root>/.git/modules/<path>/index` and its own `index.lock`. A commit inside
submodule `foo` touches only `foo`'s index → **162 independent FIFO lanes**,
fully parallel. Nested submodules nest the same way and add more lanes.

The **one** shared resource is the **root index** (`<root>/.git/index`) — every
submodule *pointer bump* funnels through it. Under stock git that is the
worst-case `O_EXCL` storm. Under zvcs the root is one more FIFO lane, and because
it is a queue the daemon can **coalesce** N pending bumps into one root commit —
which git structurally cannot do.

> Contention note: running git with the working directory at the **meta root**
> resolves every command to the single root index (git discovers the nearest
> `.git` by walking up). A root-cwd `git status` rewrites the root index to
> refresh stat data, and root-cwd `git add`/`commit` stage submodule gitlinks —
> so agents launched at the root all collide on the one root `index.lock`. zvcs
> addresses this by (a) fair-locking the root lane, (b) taking root pointer
> writes off the agents entirely (§5 autobump), and (c) write-free reads: zvcs
> `status`/`diff` never persist the stat-refresh, so they take no lock.

## 5. Autonomous behaviors — file-watcher driven, never poll

The daemon watches local files only and reacts to events. **GitHub is never
contacted by the daemon.** A `git pull`/`fetch` run by an agent updates local
refs, which fires the watcher, which is the only trigger the daemon needs.

Watched paths, per submodule (+ root):

- `.git/modules/<sub>/logs/HEAD` — HEAD moves (a commit, a pull's ff, a
  `submodule update`).
- `.git/modules/<sub>/refs/remotes/origin/main` — a local fetch/pull updated the
  remote-tracking ref.

`[zvcs].interval` is repurposed from a poll period to a **debounce window** so a
burst of events coalesces into one action.

### 5.1 Detached-HEAD elimination

The daemon guarantees every submodule is *attached* to `main`, so agents never
meet a detached HEAD and the stash/attach/pop dance becomes unnecessary.

- **Attach-scan on daemon start** — walk all submodules; attach any detached
  HEAD.
- **Watcher re-attach** — a HEAD-change event that went detached (e.g. from
  `git submodule update`) re-attaches within the debounce window.
- **Clean vs dirty:**
  - *Clean* → full reconcile: attach `refs/heads/main` and fast-forward the
    worktree to `origin/main` if behind (`reconcile_repo`, `superset/zsync.rs:24`).
  - *Dirty* → **in-place attach**: `refs/heads/main` set to the current
    commit + `HEAD` made symbolic to it. This is a pure ref operation — it does
    **not** move the commit, touch the worktree, or touch the index, so dirty
    changes are preserved untouched. Detachment is removed immediately; the
    catch-up-to-`origin/main` waits for a clean moment (never clobbers).
- **Guard:** never move `main` backward. Only create `main` at HEAD, or
  fast-forward it. A stale detached commit never resets a newer `main`.
- **Reconcile early-return.** The up-to-date path returns before the
  fast-forward, so a submodule detached **at** `origin/main` — the default
  post-`submodule update` state — would be left detached by an early return that
  never reached the attach. `fast_forward_to` therefore attaches inside that
  path: when the local tip already equals the remote tip, a detached HEAD still
  goes through `ensure_attached` and the call reports `up to date, re-attached to
  <mainline>`. Local, no fetch, no clobber.

### 5.2 autobump — kill the `(new commits)` marker

On a submodule HEAD move, debounced and coalesced, the daemon bumps the parent
gitlink to the submodule's **local** HEAD and **commits** it locally.

- **Local only, no network, no auth.** The bump targets `subrepo.head_id()`
  (`superset/zbump.rs:76`) — the submodule's current HEAD — not `origin/main`.
- **Forward-only.** Bump only when the submodule HEAD is a descendant of the
  recorded pointer (`zbump.rs:86-102`); a rewritten/rewound submodule is refused
  and logged, never recorded as a diverged pointer.
- **Coalesced.** One root commit per debounce burst covering every changed
  submodule (message `zvcs: autobump <n> pointer(s)`), not one commit per
  pointer.
- **Committed, not just staged.** Staging alone does **not** clear the
  marker — it only moves it from unstaged to staged; committing is what erases
  it. `zbump` writes the index and then commits, building the tree from HEAD
  plus **only** the bumped gitlinks rather than the raw index, so an unrelated
  `git add` in the worktree is never swept into an autobump commit
  (`zbump.rs`, `index_commit::commit_index_autonomous`).

### 5.3 Reactive reconcile & the no-autopush boundary

- On a remote-tracking ref change (from a local pull) → `reconcile_repo` (ff
  only, clean only). Never polled.
- **No autopush.** The daemon does only local, forward-only, safe ops
  (ff-pull, stage, commit, attach). It **never publishes.** All pushing —
  submodule work → origin, and meta root → origin — stays agent/human
  controlled, in the order the operator already controls (submodule first, then
  root). Because the daemon never pushes, it can never publish a dangling
  gitlink (a root pointer to an unpushed submodule commit).
- On a single-machine topology (operator is the only pusher of the meta root),
  local root stays fast-forwardable to the eventual push. Multi-machine pushes of
  the meta root would reintroduce non-ff on push — outside the current topology.

### 5.4 Failure surfacing — notify-on-next-command

Autonomous ops are headless; all error detail goes to `~/.zvcs/zvcs.log`.
Because async work has no exit code to return, failures are surfaced on the
operator's **next** `git` invocation (at-least-once):

- Failures (`{repo, reason, ts, notified}`) are recorded (§6, `jobs`/failure
  rows).
- `run()` (`src/extensions/src/lib.rs:18`), before dispatch, prints unnotified
  failures for the current repo terse on **stderr**
  (`zvcs: <sub>: autobump refused (not a fast-forward)`), then marks them
  notified. No hint text; stdout stays clean so `$(git …)` capture is unaffected.

### 5.5 Configuration — `[zvcs]`, opt-in, dev-only

All autonomous behavior is gated by `[zvcs]` git config and **defaults OFF**, so
the daemon does nothing unless explicitly enabled. Enable it in the development
environment (the meta repo's `.git/config`, or the machine's `~/.gitconfig`);
leave it unset everywhere else and zvcs is a plain faithful-`git` with a fair
lock and no autonomy.

The autonomy switches live in one table, `zconfig.rs::SETTINGS`, which is what
`git zconfig` lists, sets and validates — that table is authoritative and this
block is an illustration of it, not a second copy to keep in step. Defaults as
declared there:

```gitconfig
[zvcs]
    autoreconcile  = true            ; reconcile submodules to origin/main on change      (default off)
    autobump       = true            ; forward-only submodule gitlink bumps + commit      (default off)
    autocrawl      = true            ; crawl zvcs.crawlroots into the index on start      (default off)
    autostatus     = true            ; recompute a repo status cache when it changes      (default off)
    autohook       = true            ; fire each repo zvcs.hook on change (see 5.6)       (default off)
    autodups       = true            ; fan a commit out to local duplicate checkouts      (default off)
    statusinterval = 10              ; status-cache backstop sweep, seconds (0 disables)  (default 10)
    watchmru       = 512             ; file-watch the N most-recent repos (0 disables)    (default 512)
    interval       = 30              ; autonomy debounce, seconds — always on             (default 30)
```

Seven further `zvcs.*` keys are read outside that table, because they carry a
value rather than gate a loop and `git zconfig <name> on|off` has nothing to say
about them: `crawlroots` (roots for the crawler, whitespace/comma separated,
default `$HOME`), `hook` (the ref-change command, 5.6), `precache` (warm the
derived-answer caches for freshly arrived commits, default **on** — the one key
whose default is not off), `worktreebase` (13), `replvimode`, `topscheme` and
`toppalette` (the repl and `ztop`, which write the last two back themselves).

### 5.6 Hooks — filesystem-driven, across every indexed repo

Because every repo is indexed in the ledger, the daemon can watch them all and
run a **per-repo hook** on ref-change — a hook system with nothing installed in
any `.git/hooks`. Set `[zvcs] hook` (merged config, so a single `~/.gitconfig`
value applies everywhere; a repo may override in its own `.git/config`). When a
hook is configured, `should_watch()` is true and the watcher additionally watches
every indexed repo (deduped, capped at `MAX_WATCHED` with a logged warning — no
silent truncation). On a debounced ref-change the hook runs via `sh -c` with:

- cwd = the repo working directory,
- `ZVCS_REPO` = working dir, `ZVCS_GIT_DIR` = git dir, `ZVCS_EVENT` = `ref-change`.

Hook output goes to `~/.zvcs/zvcs.log`; a failing hook is recorded in the ledger
and surfaced by notify-on-next-command. `zdaemon` starts automatically when a
hook is set, even without other autonomy (`autostart` gates on `should_watch()`).

- `ZvcsConfig::load` (`src/extensions/src/config.rs:28`) reads these; absent keys
  default to `false` (`interval` defaults to a small debounce). `any_autonomous()`
  (`config.rs:42`) is the master gate.
- **Spawn is also gated.** `autostart::ensure_if_configured` (`autostart.rs:18`)
  only launches the daemon when `any_autonomous()` is true — so on a machine
  without the config, no daemon is ever spawned. That is the "otherwise not"
  behavior with zero cost.
- **Naming:** `autoreconcile` is the auto-`zsync` switch and `autobump` the
  auto-`zbump` switch (kept from the existing config; aliases `autosync` /
  `autozbump` can be added if preferred).

## 6. SQLite ledger & repo index (`~/.zvcs/db.sqlite`)

`rusqlite` (bundled SQLite, for cross-arch reproducibility on macOS aarch64 /
Linux x86_64+aarch64), WAL mode. The daemon's **DB-writer thread is the sole
writer**; clients ship records over the socket. Client *read* verbs
(`zjobs`/`zjob`/`zrepos`) open the db read-only (WAL concurrent read) and work
even when the daemon is down.

The ledger holds *mutable, coordinated* state only — jobs, events, claims,
messages, repo status. The derived-answer caches are **not** here; they are
zero-copy rkyv images under `~/.zvcs/cache/` (§6a).

```
repos(
  id INTEGER PK, git_dir TEXT UNIQUE, workdir TEXT,
  mainline TEXT, discovered_at, last_seen
)
jobs(
  id INTEGER PK,           -- the job number shown to the user
  repo_id INTEGER REF repos,
  kind TEXT,               -- commit | push | sync | bump | reconcile | crawl
  argv TEXT, paths TEXT, message TEXT,      -- json
  session_key TEXT,        -- ZVCS_SESSION (attribution + notify scoping)
  state TEXT,              -- queued | running | done | failed | stopped
  exit_code INTEGER, sha_before TEXT, sha_after TEXT,
  stdout TEXT, stderr TEXT,
  parent_job_id INTEGER,   -- set on restart
  notified_at TIMESTAMP,   -- NULL + failed = pending notification
  created_at, started_at, finished_at
)
```

- **`repos`** is the index of git repositories the daemon knows about — fed by a
  **crawler** (whole-device `.git` discovery via `ignore`, permission-denied
  paths logged and skipped) plus the meta repo's own submodule walk. This is the
  "index all git repos on the storage device" capability. `git zreindex [path]`
  forces a rescan. Rows are keyed on the **absolute, symlink-resolved** git dir, so
  one repo can never occupy two rows under two spellings of its path. Discovery is
  additive, so it is paired with a **prune**: every crawl, plus a daemon sweep at
  startup and on the hourly housekeeping timer, stats each indexed git dir and drops
  the ones that are gone (deleted repos, throwaway `$TMPDIR` checkouts). Job and
  event history is detached (`repo_id` → NULL) rather than deleted, so it survives
  without re-attaching to whichever repo later reuses the rowid. Without the sweep
  the index only grows, and the repo count degrades into a count of paths that no
  longer exist.
- **`jobs`** is the ledger of every async job (§7) and the record behind
  notify-on-next-command (§5.4).

## 6a. Derived-answer caches (`~/.zvcs/cache/`, `rcache.rs`)

Tree diffs, blames and object abbreviations are pure functions of the inputs
their key names: no *event* can invalidate an entry, because the objects behind
it never change. What the immutability does not buy is protection from a key
that names too little — an entry answers for every input the key left out, and
since these images live in `~/.zvcs/cache` keyed by commit id, a key that is
short by one option is wrong in every repository on the machine rather than in
the one that wrote it. The blame memo was keyed `(suspect, path, algo)` with
`opts.bottom` outside the key, so one `git blame A..B -- f` poisoned every later
plain blame of that file everywhere, persisted, with nothing to indicate it had
happened (fixed in v0.16.0 by keeping a bottom-limited blame out of the cache
entirely, the treatment `--reverse` and `--ignore-rev` already had). The rule
that follows is the one `blame.rs` now states case by case: an option that
changes the answer either appears in the key or disqualifies the entry, and the
cache carries no way to check that for itself.

Purity in that sense earns none of what SQLite provides — no concurrent
mutation, no triggers, no queries — while costing a query planner, row decoding
and an allocation per column to hand back bytes already sitting on disk. They
live in **rkyv** images instead, read in place out of an mmap.

```
~/.zvcs/cache/<name>.rkyv   base image: header + sorted index + byte heap
~/.zvcs/cache/<name>.log    append journal: [u32 klen][u32 vlen][key][val]
~/.zvcs/cache/<name>.lock   flock target, writers only
```

- **Layout.** One archive holding a dense index of pointer-free records
  (`hash: u64`, four `u32` offsets) over a byte heap. A lookup binary-searches on
  the key hash, then compares the full key bytes out of the heap — a collision
  costs one extra compare and can never serve a wrong answer. Nothing is
  decoded, allocated or copied on a hit; the returned slice points into the
  mapping. Keeping the index pointer-free is also what makes
  `rkyv::check_archived_root` cheap enough to run on every command: validating
  it is a bounds check plus a scan of plain integers.
- **Portability.** `archive_le` + `size_32`, and the header pins the writing
  build's pointer width, so an image written on macOS aarch64 is readable on
  Linux x86_64/aarch64. No crate version is pinned: entries are keyed by
  content, so they stay correct across releases, and pinning would discard the
  whole warm cache on every bump.
- **Writes.** Rewriting a multi-megabyte image per put would be slower than the
  SQLite insert it replaces, so writers append to a journal and fold it into the
  base once it crosses 1 MiB. Readers merge the two; the threshold is what bounds
  the journal scan a reader pays.
- **Concurrency.** Readers take no lock. Writers hold `flock(LOCK_EX)` across an
  append and across a compaction, and a compaction lands by atomic rename, so a
  reader maps either the whole old image or the whole new one. Every remaining
  race (stale base, truncated journal, torn tail record) degrades to a cache
  miss — a recomputation the command would have done anyway — which is what lets
  the read path stay lock-free.
- **Migration.** `db.rs` drains the old `treediff`/`blame`/`abbrev` tables into
  these images and drops them on every read-write ledger open. It is deliberately
  not version-gated: during an upgrade an older binary still running recreates
  those tables from its own schema, and a once-per-db import would strand
  whatever landed after it ran.

## 7. Async queue & `z` write-verbs

Opt-in fire-and-forget for an agent's own operations. The autonomous daemon
handles pointers without these; the queue is for agent-initiated content
commits/pushes that should not block.

- **`zcommit <paths> -m <msg> [--push]`** — one atomic job: build the tree from
  HEAD + the given paths (tree-editor, enabled in `Cargo.toml`), commit, and
  optionally push. Atomic-per-job (stage+commit in one unit) so concurrent
  sessions cannot interleave via a shared index. Returns a job#.
- **`zpush [<refspec>]`** — async push with a **ls-refs pre-flight**: one ref
  advertisement (no packfile) determines the remote tip. If the remote holds a
  commit the local lacks (or diverged), the push is refused **before enqueue**
  (`pull first`) instead of failing async later. The "object absent" case is
  itself the non-ff signal, so no object transfer is needed. The pre-flight runs
  client-side (has a tty), which also warms the credential cache for the
  daemon's headless push.
- **Job lifecycle:** `queued → running → {done | failed | stopped}`. Stop is
  **cooperative** (jobs are daemon threads, not processes) via a per-job
  `AtomicBool should_interrupt` (the pattern already in
  `superset/zsync.rs:46`); long ops (fetch/push) abort at the next gix
  checkpoint. Restart re-enqueues a **new** row with `parent_job_id` set.
- **Output discipline:** job# → **stderr**, suppressed when stdout is not a tty,
  so scripted `$(git …)` capture is unaffected.
- **Controls:** `zjobs [--repo] [--state] [-n]`, `zjob <id>`,
  `zjob stop <id>`, `zjob restart <id>`.
- **Note (deferred by design, kept for reference):** shadowing *bare*
  `git add`/`commit`/`push` into the queue is intentionally **not** done — it
  would break git's synchronous exit-code/editor/output contract and require a
  per-session staging index and status/diff overlay to stay correct. Async lives
  behind the explicit `z` verbs; bare git stays faithful (Layer 1).

## 8. Verb surface

| Verb | Layer | Sync | Purpose |
|---|---|---|---|
| `git add/commit/push/status/diff/…` | 1 | sync | faithful git via gitoxide + fair lock |
| `git zsync [<path>…]` | 2 | — | reconcile submodules to `origin/main`, kept attached, ff-only |
| `git zbump [<path>…]` | 2 | — | forward-only, coalesced, local pointer bumps (+ commit) |
| `git zdaemon start\|stop\|status` | 2 | ctl | the singleton coordinator |
| `git zcommit <paths> -m … [--push]` | 2 | async | atomic changeset job |
| `git zpush [<refspec>]` | 2 | async | push job + ls-refs pre-flight |
| `git zjobs` / `git zjob <id>[ stop\|restart]` | 2 | read/ctl | job ledger status & control |
| `git zrepos` / `git zreindex [path]` | 2 | read/ctl | indexed-repo listing & rescan |
| `git zrepl` | 2 | interactive | line REPL into the live daemon |
| `git znative add\|load\|remove\|list\|info\|update\|gc` | 2 | read/ctl | the plugin package manager (§18) — install native (cdylib) and script (`git-<verb>`) plugins into one content-addressed store |
| `git <plugin verb>` | plugin | sync | a verb an installed plugin provides, served from the `dlopen`ed library or the stored executable |

## 9. Design principles / non-goals

- **No polling, ever.** All autonomy is file-watcher reactive. `git pull` is the
  only trigger needed; the daemon never contacts GitHub.
- **Faithful git.** Layer-1 subcommands never change semantics; async is opt-in
  behind `z` verbs.
- **Local-first.** Pointer bumps and detached-HEAD healing are purely local, no
  network, no auth.
- **Daemon never publishes.** No autopush; the operator controls all pushes.
- **Forward-only, never clobber.** Pointer bumps and ff-reconcile refuse
  anything that would regress or diverge; dirty worktrees are never touched
  (except the no-clobber in-place attach).
- **Single writer per shared resource.** Root index via the coalesced root lane;
  SQLite via the sole DB-writer thread.

## 10. What exists vs. to-do

| Piece | Status |
|---|---|
| Faithful git subcommands (`porcelain/`) | built |
| Fair FIFO lock under git writes (`lock.rs`, wired across porcelain) | built |
| `zsync` submodule reconcile / attach-on-ff (`zsync.rs`) | built |
| `zbump` forward-only coalesced bump **+ commit** (`zbump.rs`, `index_commit.rs`) | built |
| Singleton daemon in `~/.zvcs`, per-repo lanes (`zdaemon.rs`) | built |
| `notify` watch layer (submodule `refs`/`logs`) (`watch.rs`) | built |
| Detached-HEAD attach-scan + in-place attach + early-return fix (`attach.rs`) | built |
| autobump stage→commit gap + debounce (`watch.rs`, `zbump.rs`) | built |
| Reactive reconcile on remote-tracking change (`reconcile_repo_local`) | built |
| Failure log + notify-on-next-command (`db.rs`, `lib.rs`) | built |
| SQLite `jobs` + `repos` (rusqlite bundled, WAL) (`db.rs`) | built |
| Zero-copy rkyv caches for tree diffs / blames / abbreviations (`rcache.rs`) | built |
| Crawler + `zreindex`/`zrepos` (pipe-clean, prunes deleted) (`crawler.rs`, `ledger.rs`) | built |
| Filesystem hooks across all indexed repos (`hooks.rs`, `watch.rs`, `zvcs.hook`) | built |
| `zcommit`/`zpush` async via daemon `SUBMIT` (`queue.rs`, `jobrun.rs`) | built |
| `zjobs`/`zjob` + `zrepl` (`ledger.rs`, `repl.rs`) | built |
| Plugin system: C ABI + `dlopen` host + package manager (`src/plugin`, `plugin_host.rs`, `pkg/`) | built |

**Resolved partials** (all landed with tests):
- **`zpush` pre-flight is a live `ls-refs`** (`queue.rs`) — one ref advertisement
  (no packfile) reads the remote's current tip; falls back to the network-free
  remote-tracking comparison when the remote is unreachable. Test:
  `push_preflight.rs` (both the live and fallback paths).
- **Crawl-on-start** is available, config-gated by `[zvcs] autocrawl`
  (`crawler.rs`); `git zreindex` still triggers an on-demand rescan. Test:
  `autocrawl.rs`.
- **Job control** (`jobpool.rs`): a **bounded** worker pool (cores, capped)
  executes jobs; `zjob stop` cancels a running job (kills its child) or marks a
  queued one `stopped`; `zjob restart` clones a job parent-linked and re-enqueues
  it. Test: `jobctl.rs`.
- **autobump refusals** are recorded to the ledger (`watch.rs` →
  `db::record_failure`) and surfaced by notify-on-next-command. `zbump_run`
  returns structured refusals. Delivery tested in `notify.rs`.
- **Interop `index.lock`**: verified — `gix::index::File::write` acquires
  `<index>.lock` via `gix_lock` (`Fail::Immediately`) and renames over `index`,
  so every index-writing path emits the on-disk lockfile and respects an
  external one.

**Remaining minor notes** (intentional / low-risk):
- On an **external** process holding `index.lock`, the ported index writer
  itself fails fast (`gix_lock`, `Fail::Immediately`), so the wait is layered
  above it: a lock-taking verb polls the path for `ZVCS_INDEX_LOCK_WAIT_MS`
  (default 2s) before running and queues the command as a job if the budget runs
  out (`lock::wait_for_foreign_index_lock`, `dispatch.rs`). The caller never sees
  the raw lock error. zvcs-vs-zvcs fairness is the daemon FIFO, which is a
  separate lock and a separate budget.
- `zjob stop` of a *mid-run* job (child-kill path) is implemented but not covered
  by a deterministic test (jobs finish too fast to race reliably); the
  queued-stop and finished-stop paths are tested.

## 11. Implementation phases (all landed — see §10 for partials)

- **P1 — Singleton daemon + watch layer + detached-HEAD healing.** ✅ Fixed
  socket `~/.zvcs/zvcs.sock` (`ZVCS_SOCK` override); per-repo lanes
  (`ACQUIRE <client-id> <git-dir>`); timer loops deleted; `notify` watches;
  attach-scan on start + watcher re-attach (clean ff / dirty in-place);
  reconcile early-return fix. Tests: `attach.rs`, `coordination.rs`.
- **P2 — Debounced autobump + commit (marker killer).** ✅ `zbump` commits the
  coalesced bumps (`index_commit.rs`); debounce window from `interval`. Test:
  `autonomy.rs` (submodule commit → autobump clears the marker).
- **P3 — Reactive reconcile + failure surfacing.** ✅ Remote-tracking event →
  `reconcile_repo_local` (fetch-free); notify-on-next-command in `run()`. Test:
  `notify.rs`.
- **P4 — SQLite ledger + repo index.** ✅ `jobs` + `repos` (WAL), crawler,
  `zrepos`/`zreindex`. Test: `ledger.rs`.
- **P5 — Async write-verbs.** ✅ `zcommit`/`zpush` via daemon `SUBMIT`,
  `zjobs`/`zjob`, network-free push pre-flight. `zjob stop`/`restart` are
  wired through the daemon's `JOBSTOP`/`JOBRESTART` verbs. Tests: `queue.rs`,
  `push_preflight.rs`, `jobctl.rs`.
- **P6 — `zrepl`.** ✅ Interactive verb console. Test: `repl.rs`.

**Rollback:** autonomous behaviors stay behind `[zvcs]` config flags — off →
the daemon reverts to a pure fair-lock coordinator (current behavior). The
socket-path move keeps a fallback to `<git-dir>/zvcs.sock` if `~/.zvcs` cannot be
created. autobump commits are ordinary commits (`git revert`/`reset`).

## 12. Novel superset features (built on the daemon/db substrate)

These are capabilities stock git has no equivalent for — each exists only because
zvcs has a machine-wide daemon, a db of every repo, file-watchers, an op ledger,
and session attribution. All tested.

- **Multi-agent claim/lease** — `zclaim` / `zunclaim` / `zwho`. An advisory,
  session-attributed lease (one per repo, race-safe via the db PK) so N agents
  signal "I'm working this repo." `claims` table. Test: `claim.rs`.
- **Machine-wide instant status** — `zstatus` (live for the cwd repo) /
  `zstatus --all` (every indexed repo, from the db). The daemon maintains each
  watched repo's dirty/detached/sync/head in `repo_status` on ref-change
  (`zvcs.autostatus`), so `--all` is a pre-computed, zero-walk read. `sync` is
  merge-base derived (up-to-date/ahead/behind/diverged). Dirtiness is tracked-
  change based (like `git diff`) and refreshed on ref events, not worktree edits.
  Tests: `status.rs`, `status_daemon.rs`.
- **Cross-repo op ledger + rewind** — `zlog` merges every indexed repo's HEAD
  reflog into one time-ordered, machine-wide timeline; `zundo` rewinds a repo one
  step (`reset --hard` to the previous HEAD, refuses on dirty). Test: `oplog.rs`.
- **Typed cross-repo hooks** — the hook (`zvcs.hook`) fires with a typed event
  classified from the reflog (`ZVCS_EVENT` = commit/checkout/merge/pull/rebase/
  reset/…) plus `ZVCS_OLD_SHA`/`ZVCS_NEW_SHA`/`ZVCS_REF`, enabling "on commit in
  X, do Y in repo Z" rules. Test: `hook_event.rs`.
- **Tree-wide snapshot/restore** — `zsnapshot <name>` records the HEAD of the
  repo + every nested submodule as one restore point; `zrestore <name>` resets
  the whole tree back (`reset --hard` per repo, keeps untracked); `zsnapshots`
  lists them. `snapshots` table. Test: `snapshot.rs`.
- **Tree-wide stash** — `zstash [<name>]` parks uncommitted work across every
  dirty repo in the tree as one unit; `zunstash` restores it (LIFO); `zstashes`
  lists them. `stashes` table. Restore is same-HEAD only (3-way apply onto a
  moved HEAD is unported; the stash is kept, never lost). Test: `zstash.rs`.
- **Bring-tree-to-latest** — `zup [<path>]` fetches + fast-forwards the top-level
  repo and every nested submodule to `origin/main`, attached, skipping
  dirty/diverged (reuses `reconcile_repo`; `zsync` is submodules-only). Test:
  `zup.rs`.
- **Fan-out over all/subset** — `zforeach [selectors] -- <cmd>` runs a command
  across the selected indexed repos in parallel (bounded pool), lane-aware by
  composition (a zvcs write acquires its own lane). Shared `select` module:
  `--repo`/`--dirty`/`--ahead`/`--behind`/`--claimed`/`--session`, all fast db
  queries. Test: `zforeach.rs`. This is the general "do anything, everywhere"
  primitive; the machine-wide read verbs (`zrepos`/`zstatus --all`/`zlog`)
  complete the coverage.
- **Hook management + fix** — `zhook set/unset/show/list/test` manage `zvcs.hook`
  and fire it once for testing. Fixes a real bug: **per-repo (local) hooks never
  fired** unless a hook was also set on the daemon's repo, because the watch-all
  gate keyed on the daemon's config. New `zvcs.autohook` master switch makes the
  watcher cover every indexed repo and fire each repo's *own* hook. Tests:
  `zhook.rs`, `hook_event.rs`.
- **Directory triggers** — `ztrigger DIR <cmd>` / `zwatch DIR` watch **any**
  directory (git repo or not) and run a command on any file change under it. They
  are independent of git: the trigger is stored in a dedicated `triggers` table
  (`path` PRIMARY KEY, `command`), NOT in `.git/config`, so `git ztrigger
  ~/Desktop 'say 45'` works on a plain directory. `db::{set_trigger, remove_trigger,
  list_triggers, has_triggers}` back the verbs; the daemon reads the trigger set
  directly, watches each path recursively (whole-dir), and runs its command via
  `hooks::run_command` (cwd = the dir, `$ZVCS_DIR` in env). The daemon starts for
  triggers even outside a repo (`spawn_if_configured`/`autostart` check
  `db::has_triggers()`, falling back to `ZvcsConfig::default()`). Repo git-hooks
  (`zhook`/`autohook`, `zvcs.hook` in config) are a separate mechanism, unchanged.
  - **No debounce, whole-dir** (`watch.rs`). A trigger fires the instant a
    `notify` event arrives — a coalescing window that waits for global silence
    never fires on a busy machine. `Target::{armed, command, watch_root()}` drive
    registration, event attribution (`collect`, deepest watch-root wins), and the
    fire loop: a target with a `command` is a directory trigger (always fires); a
    `command: None` target is a repo hook (fires only when the git-hook config is
    enabled). Startup is O(triggers + armed repos), not O(indexed repos), so the
    daemon reaches the watching state instantly regardless of index size.
    Caveats: fires on **every** change under the dir (including a repo's `.git`
    churn), and a command that writes into the watched dir self-retriggers. Tests:
    `trigger.rs`, `watch.rs::armed_repo_matches_plain_worktree_file_events`.
  - **Leading-edge throttle + live views** (`trigger.rs`, `watch.rs`). One file
    action emits several fs events, so a naive trigger fires ~5× per save. Each
    trigger carries `throttle_ms` (default 500ms): the daemon fires on the first
    event, then coalesces events within the window into one fire (counted, not
    run) — immediate, not the laggy trailing debounce. `--throttle <dur>` sets it,
    `0` disables. Every real fire is appended to `~/.zvcs/fires.log`
    (`epoch\tok\tcoalesced\tpath`, self-bounding at ~1MB). `git ztrigger tail`
    follows it live; `git ztrigger top` redraws an in-place HUD (per-trigger fire
    count, coalesced events, fires/sec over a 10s window, last-fired) — the model
    is zthrottle's live gauge cluster. Proven: one file = one fire (was ~5).

**New `[zvcs]` config keys:** `autostatus` (maintain `zstatus --all`) and
`autohook` (fire per-repo local hooks), plus the existing
`autoreconcile`/`autobump`/`hook`/`autocrawl`/`crawlroots`/`interval`/`worktreebase`.
`zvcs.hook` and claims/snapshots/oplog/zforeach work without a daemon (client-side
db); `zstatus --all` freshness and hook firing need the daemon watching.

A forked zsh completion (`completions/_git`) shadows the system `_git`, adding a
`zvcs_commands` group and `_git-z*` argument completers for all verbs.

## 13. Isolated worktrees per agent (`zworktree`)

The multi-agent collision fix: instead of N agents sharing one meta tree, each
gets a **private physical checkout**. `git zworktree add <name>` provisions a
complete isolated worktree of the current repo + every nested submodule at
`<base>/<name>/` (`zvcs.worktreebase`, default `~/.zvcs/worktrees`). `list` and
`remove <name>` manage them (tracked in a `worktrees` table).

Each repo in the tree becomes a **linked git worktree** — separate index + HEAD +
working directory on a fresh `zwt/<name>` branch — that **shares the object
store** (the worktree's `.git` is a pointer file; no re-clone, unlike
`git submodule update` which clones each submodule per agent). gix has no
worktree-create API, so zvcs writes git's exact bookkeeping directly
(`<gitdir>/worktrees/<name>/{HEAD,commondir,gitdir,index}` + the `.git` pointer),
which stock git recognizes (`git worktree list`/`fsck`). Verified: a commit in a
worktree does not move the original; the worktree has no own object store.

Ergonomics: one command from the meta root replaces `git worktree add` for the
parent plus one per submodule; the agent is launched in its private tree and
works normally across submodules with zero cross-agent collisions.

**`remove` deletes only what it can prove it wrote.** Tearing a worktree down
means deleting each repository's `<gitdir>/worktrees/<name>/`, and the only thing
naming that directory is the worktree's own `.git` pointer file — plain text in a
directory an agent has write access to. So `remove` treats the pointer as a claim,
not an instruction: it deletes a directory outside the worktree only when the
round trip `add` created still closes (`<wt>/.git` names `<M>`, and `<M>/gitdir`
names `<wt>/.git` back), the path ends in `worktrees/<name>`, and `<M>` is a real
directory with a `commondir` beside it. Anything else — an unreadable or malformed
`.git`, a relative or absolute target leading elsewhere, a symlink standing in for
the metadata directory, a missing pointer — is refused by name on stderr, left on
disk, and exits non-zero. A pointer resolving *inside* the tree (a nested clone an
agent made in its own worktree) needs no separate deletion and is not refused. The
same applies to the deletions themselves: a `remove_dir_all` that fails is
reported rather than discarded, so `remove` cannot report success over metadata it
left behind.

## 14. The parallel fleet layer (`parallel_map` + `[selectors]`)

The many-repo half of the design. Every repo in the machine-wide index
(§6) is addressable as a fleet, and a family of verbs act on the whole set — or a
narrowed subset — concurrently.

- **One selection grammar.** `Selector` (`src/extensions/src/superset/select.rs`)
  is the single `[selectors]` parser every fleet verb shares: a bare path pattern
  (case-insensitive substring, repeatable, ANDed), `--dirty`/`--ahead`/`--behind`
  (read from the status cache), and `--claimed`/`--session` (from the claims
  table). `SELECTOR_FLAGS`/`SELECTOR_VERBS` are consts, test-guarded so the repl
  completion and the parser cannot drift; `git zselectors` / `git help zselectors`
  document it as a first-class topic.
- **One worker pool.** `parallel_map` (`query.rs`) is a scoped-thread pool over
  `(git_dir, workdir)` pairs, bounded to the machine's cores, work-stealing via a
  single atomic counter — the same primitive `zforeach` uses. Queries are native
  gix/filesystem reads (no fork, no ported-porcelain dependency), so they are fast
  and reliable across thousands of repos.
- **Three verb shapes over that substrate:** *queries* (`zheads`, `zdirty`,
  `zsize`, `zcommits`, `zpristine`, …), *analytics* (`zgrep`, `zahead`/`zbehind`
  and the detailed `zunpushed`/`zunpulled`, `zauthors`, `zhot`, `zconflicts`,
  `zdivergent`, `zorphans`), and *mutations* (`zfetch`, `zgc`, `zreset`, `zabort`,
  `zcheckout`, `zcommitall`, `zpushall`, …), which fan a git operation across the
  selection through this binary's own porcelain and the fair per-repo lane. `--json`
  emits NDJSON for tooling. Ops that don't apply are skipped, never forced.

## 15. The status cache + live monitoring

`zdashboard` and `ztop` must be instant across thousands of repos, so neither
walks the fleet live — both read a **daemon-maintained status cache**
(`repo_status`: dirty, detached, sync ∈ ahead/behind/diverged/up-to-date/
no-upstream, head, head_sha, updated_at). A producer/consumer maintainer (`statusd`)
keeps it warm: N read-only compute workers feed one batching writer (WAL is
single-writer, so this avoids lock thrash), hungry on first run then throttled,
and mtime-gated so an unchanged repo is skipped. The instant file-watcher (§5)
updates the most-recently-used repos reactively.

`ztop` (`superset/ztop.rs`) is a ratatui TUI that reads that cache each frame and
sorts by **churn** — `updated_at` recency plus a live burst when a repo's
`head_sha` changes while watching — so activity rises to the top and persists.
Rendering is ported from htoprs: its 31 colorschemes (exact palettes, remapped
onto the element layout) with a live picker and palette editor, an F1 help
overlay, sort-by-column, `/` search, and a toast, drawn with htoprs's own
`Buffer`/`Style` cell primitives. `zcommands` and `zevents` are the streaming
counterparts (§16 and the events table).

`zppid` (`superset/zppid.rs`) is the per-process commit tally that shares the
dashboard. The same dispatcher seam that logs commands also credits commits: for a
commit-producing verb (`commit`, `commit-tree`, `merge`, `cherry-pick`, `revert`,
`am`, `rebase`), `dispatch::run` snapshots HEAD before the command and, if it
advanced to a new tip afterward, upserts one row into the `ppids` table. A no-op
`commit` or rejected merge never advances HEAD, so it counts nothing, and `.then`
skips the gix HEAD probe for every non-commit verb, so the hot path is unchanged.

The identity is the **responsible process**, not `getppid()`. `getppid()` is
useless as an identity here: a `git commit` is run by a throwaway shell (an agent
spawns a fresh `zsh -c …` per command), so the parent pid differs on every commit —
keying on it yields a flood of one-commit rows, never a stable "N agents" view.
`responsible_process()` instead walks up the parent chain, skips transient wrapper
shells (a shell invoked with `-c`, detected from its `ps` args), and stops at the
first durable process — a real program (an agent, editor, daemon) or an interactive
login shell. That pid is stable across every commit one agent makes, so N
concurrent agents map to N rows. Each row also stores the process's command name and
cwd (via `ps` and, for the cwd, `/proc/<pid>/cwd` on Linux or `lsof` on macOS) so
the pid is legible. An explicit `ZVCS_SESSION` still overrides the key. `git zppid`
lists the table (pid, live/dead via `kill(pid,0)`, commits, last-seen, cmd, cwd),
and the dashboard adds a PROCESSES tile plus a header line with the process count
and the busiest pid.

The same durable-process resolution feeds a per-process *command* tally
(`git zprocs`): the dispatcher records one row per (process, verb) in the
`proc_verbs` table for every **mutating** verb — `commit`, `push`, `add`, `merge`,
`rebase`, `reset`, `checkout`, `cherry-pick`, `revert`, `tag`, `stash`, `fetch`,
`pull`. Only mutating verbs are tracked, deliberately: reads (`rev-parse`,
`status`, `log`) fire on every prompt and would put the process-walk on the hot
path, so they are excluded and the walk stays as cheap as the commit tally. The
resolution runs once per mutating command and is shared between the commit tally
and the verb tally (`commit` counts in both, on a real HEAD advance). `zprocs`
joins `proc_verbs` with `ppids` to show each process's pid/cmd/cwd and its verb
breakdown (`commit:12 push:8 add:5 …`), busiest first.

## 16. Command logging + AOP interception

Because the one binary is the sole dispatcher (`dispatch::run`), it is the natural
seam for two orthogonal cross-cutting features, both hooked at the top of that
function and both a single `stat` when inactive:

- **`zcommands`** — a fleet command log. When enabled (a marker file), every
  invocation appends `ts pid ppid cwd argv` to `$ZVCS_HOME/commands.log`
  (atomic O_APPEND), and the verb tails it live. The `ppid` records which
  agent/shell ran each command.
- **`zintercept`** — aspect-oriented interception, ported from zshrs
  (`src/extensions/src/superset/intercepts.rs`). `AdviceKind` (before/after/around),
  `Intercept`, and `intercept_matches` (exact/glob/`all`) are ported faithfully;
  adapted to zvcs's per-process model, the registry persists to
  `$ZVCS_HOME/intercepts.tsv` and advice is a shell command run with
  `INTERCEPT_NAME`/`ARGS`/`CMD` (and `STATUS`/`MS`/`US` for after) in the
  environment. `maybe_intercept` orchestrates before → around/after; an around
  advice runs `eval "$INTERCEPT_CMD"` to proceed, and a `ZVCS_INTERCEPTED` env guard
  stops re-interception of the wrapped command.

## 17. Coordination, event automation & scripting verbs

Built on the same substrate (daemon, db, watchers, event feed), a further set of
verbs turns the machine-wide state into things scripts and agents can act on.

**The event feed** (`db.rs` `events` table). Two `AFTER UPDATE` triggers on
`repo_status` turn a status change into a typed row: a moving `head_sha` →
`commit` (keyed on the peeled commit id, not the invariant branch name), a
`dirty`/`sync` flip → `status`. Both the whole-tree poller (statusd) and the
instant watcher write through `upsert_status`, so both feed it with no per-call
plumbing; triggers fire on UPDATE only, so a repo's cold-start INSERT never
floods. `add`/`stage` emit a `stage` event directly on a successful index write;
the reconcile path emits `reconcile` on a real fast-forward. `zevents`/`ztail`
tail the feed live; `zsince <duration|snapshot>` is the bounded delta.

**Semantic-event automation** — `zon`. Where `ztrigger` reacts to raw filesystem
events, `zon` reacts to typed feed events. Subscriptions (`subscriptions` table:
kind, repo substring, command) are matched by a daemon loop that watches the feed
and runs each command via the shell with `ZVCS_EVENT`/`ZVCS_REPO`/`ZVCS_DETAIL`/
`ZVCS_SHA` set.

**Selective autonomy** — `zpin`/`zunpin`. A `repos.pinned` flag; the daemon's
`react()` refuses autobump and reconcile for a pinned repo (attach still runs —
re-attaching a detached HEAD doesn't move the pointer). Freeze part of the tree
from autonomy without turning it off machine-wide.

**Agent coordination.** `zcontend` reads claims + the per-repo job backlog and
reports the contested set (claimed *and* queued). `zbroadcast`/`zhandoff` are
inter-agent IPC over the db (`messages` + per-session `message_reads`, so a
broadcast is delivered once per agent; `zhandoff` reassigns a claim and notifies
the receiver). `zwaitfor <clean|idle|synced|repo sha>` blocks until a tree-wide
state holds — a barrier on *state*, where `zwait`/`zbarrier` are job-scoped.

**Topology & time-travel.** `zgraph` groups indexed repos by origin URL into dup
groups (the same upstream checked out N times — a relationship git has no command
for). `zrewind <duration>` restores the whole tree (repo + submodules) to the
HEAD each had at a wall-clock time, from the reflog, reusing the porcelain
`reset --hard` (dirty repos refused, reflog-expiry-bounded).

**Scripting output** — `--json`. Every read verb (query, analytics, discovery,
coordination) takes `--json` and emits NDJSON: one object per line, so `jq -c`
streams and `jq -s` slurps to an array. A shared `json_flag`/`emit_json` helper
(`query.rs`) strips the flag before selector parsing and prints uniformly; the
integration test `tests/json_output.rs` runs the real binary and parses every
verb's `--json` output, so the guarantee can't drift.

## 18. The plugin system (`znative`)

Ported from zshrs, which generalised zsh's `Src/module.c` — `dlopen`ing C
modules that call `addbuiltin` against the shell's own symbols — into a stable,
versioned C ABI so third parties ship a compiled `cdylib` and load it at
runtime. Here the same machinery is retargeted from shell builtins to git
subcommands: `git znative` installs plugins, and dispatch serves their verbs
from a `dlopen`ed library rather than forking a `git-<verb>` script off `PATH`.

**Three parts.**

- **`src/plugin`** (crate `zvcs-native`, lib `znative`) — the ABI. `#[repr(C)]`
  structs and `extern "C"` function pointers, no dependencies, compiled into
  both the host and every plugin so the two agree on the exact layout. A
  `declare_plugin!` macro emits the one exported symbol (`zvcs_native_init`) and
  the trampolines that adapt each C-ABI handler to `fn(&Host, &Args) -> c_int`.
  The zshrs table's shell entries map to VCS ones: `register_builtin` →
  `register_verb`, `eval` → `run` (a subcommand run in-process, no fork),
  `getvar`/`setvar` → `config_get`/`config_set`, the structured
  `getfunction`/`addfunction` pair → `object_read`/`object_write`, and the
  compsys-function override (`register_compfn` + `comp_dispatch`) →
  `register_override` + `dispatch_verb`. `repo_info` and `resolve_rev` have no
  shell analogue and are new.
- **`plugin_host.rs`** — the runtime. `dlopen`, the magic + `ABI_VERSION` gate,
  the staging buffers that tag a plugin's registrations with its name once
  `init` returns it, the verb and override registries, and `unload`, which
  purges the registries *before* the `dlclose` so no live function pointer
  survives it.
- **`pkg/`** — the package manager: `manifest` (`znative.toml`), `store`
  (`$ZVCS_HOME/pkg/` + `installed.toml`), `resolver` (`owner/repo`, `git+URL`,
  `path:DIR`, `@ref` pins), `commands`.

**Two kinds, one store.** A **native** plugin is a Rust cdylib; a **script**
plugin is a repository of `git-<verb>` executables, which is the shape every
third-party git subcommand already ships in. Both install into the same
content-addressed store, both are SHA-256 pinned, and the kind is auto-detected
when no manifest declares it. The clone runs through this binary's own native
`clone`, so installing a plugin needs no second VCS on the machine.

**Where a verb resolves.** A plugin verb is consulted in `lib.rs` after builtins
and aliases and before `external::try_dashed` — the slot git gives dashed
externals. An override is consulted at the top of `dispatch::run`, next to the
AOP intercept hook of §16, and delegates to the built-in implementation through
`dispatch_verb`, which pushes the verb on a thread-local bypass stack so the
override cannot re-enter itself. `git znative` is exempt from overriding, so a
misbehaving plugin cannot lock you out of removing it.

**The per-process adaptation.** This is the one place the zshrs design could not
be copied. A shell loads every plugin once into a process that lives for hours;
`git` is a fresh process per command, so loading anything eagerly would put a
`dlopen` on the hot path of every invocation. Instead the verbs a native plugin
registers are *discovered by loading it once at install time* — never declared,
so the record cannot lie — and recorded in the index. Two derived tables,
`verbs.tsv` and `overrides.tsv`, answer "who owns this verb" with one `stat`
and at most one small read, and exactly one library is then loaded. Both tables
are deleted rather than written empty when they have no rows, so a machine with
no plugins installed pays two failed `stat`s per command and never opens a file.
They are pure projections of `installed.toml`; `git znative load` rebuilds them.

[ZNATIVE.md](ZNATIVE.md) documents the command surface, the store layout and the
ABI a plugin is written against; `examples/` holds three runnable plugins —
`plugin-hello` (minimal + an override), `plugin-wip` (real work through
`host.run`), and `plugin-todo` (the script kind).

Refusals are checked at install, where they can be reported, rather than at
dispatch: a plugin cannot *add* a verb that is already a git command or that
another installed plugin owns, and cannot *override* a verb that does not exist
(the row would land in a table dispatch never consults for it).
