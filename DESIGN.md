---
title: zvcs Design
---

# zvcs — Design Document

A git-shadowing superset VCS: a pure-Rust `git` binary (built on vendored
gitoxide) plus a singleton coordination daemon that removes three structural
pains of a many-agent, deeply-nested-submodule monorepo.

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
  | external git holds `index.lock` | zvcs waits/retries (bounded), does not fail |
  | zvcs holds `index.lock` briefly | external git sees git's normal lock error (git's contract) |

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

Wire protocol (line-framed) extends the current
`ACQUIRE`/`RELEASE`/`STATUS`/`STOP` with a repo key on `ACQUIRE`
(`ACQUIRE <git-dir> <client-id>`), plus `SUBMIT`/`JOB`, `JOBSTOP`,
`JOBRESTART`, `REINDEX`, and a `REPL` upgrade.

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
- **Reconcile early-return fix.** `reconcile_repo` currently returns
  `"up to date"` (`superset/zsync.rs:68`) *before* the attach step
  (`:86-116`), so a submodule detached **at** `origin/main` — the default
  post-`submodule update` state — is left detached. Fix: in the up-to-date path,
  if HEAD is detached, attach it (local, no fetch).

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
- **Gap to close:** `zbump` today stages and stops (`zbump.rs:141-144`,
  `index.write`, no commit). Staging alone does **not** clear the marker — it
  only moves from unstaged to staged. Committing is what erases it. Closing this
  stage→commit gap is the line of work that removes the marker.

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

```gitconfig
[zvcs]
    autoreconcile = true            ; auto-zsync: keep clean submodules attached at origin/main (reactive)
    autobump      = true            ; auto-zbump: forward-only local pointer bumps + commit (kills the marker)
    interval      = 2               ; debounce window (seconds) for coalescing bump/attach bursts
    autocrawl     = true            ; background repo-index crawl on daemon start (opt-in)
    crawlroots    = ~/src ~/work    ; roots for the crawler (whitespace/comma separated; default $HOME)
    hook          = ~/bin/on-change ; run on ref-change in any indexed repo (see §5.6)
```

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
  forces a rescan.
- **`jobs`** is the ledger of every async job (§7) and the record behind
  notify-on-next-command (§5.4).

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
| Crawler + `zreindex`/`zrepos` (pipe-clean, prunes deleted) (`crawler.rs`, `ledger.rs`) | built |
| Filesystem hooks across all indexed repos (`hooks.rs`, `watch.rs`, `zvcs.hook`) | built |
| `zcommit`/`zpush` async via daemon `SUBMIT` (`queue.rs`, `jobrun.rs`) | built |
| `zjobs`/`zjob` + `zrepl` (`ledger.rs`, `repl.rs`) | built |

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
- On an **external** process holding `index.lock`, a zvcs index write fails
  fast (matching git) rather than bounded-waiting; zvcs-vs-zvcs fairness is the
  daemon FIFO, and external contention is rare.
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
  `zjobs`/`zjob`, network-free push pre-flight. Tests: `queue.rs`,
  `push_preflight.rs`. (`zjob stop`/`restart` still to wire — §10.)
- **P6 — `zrepl`.** ✅ Interactive verb console. Test: `repl.rs`.

**Rollback:** autonomous behaviors stay behind `[zvcs]` config flags — off →
the daemon reverts to a pure fair-lock coordinator (current behavior). The
socket-path move keeps a fallback to `<git-dir>/zvcs.sock` if `~/.zvcs` cannot be
created. autobump commits are ordinary commits (`git revert`/`reset`).
