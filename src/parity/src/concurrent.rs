//! Concurrent-writer parity: what N git processes writing one repository at once
//! leave behind.
//!
//! Every other dimension in this harness runs one invocation against a pristine
//! copy and compares it with stock git's. That is the whole of git's behaviour
//! for a single writer and none of it for two, and two is the case this port
//! exists for: sixteen agents and stock git share one worktree on the machine
//! zvcs was written for. Nothing measured that until this module, and the defect
//! it found on its first run had been invisible to every existing check —
//! `fsck --strict` clean, index parsing clean, no leftover lock, and eight `git
//! add` processes reporting success where six entries landed.
//!
//! # Why this is not a differential comparison
//!
//! The obvious shape — run the case against stock, run it against zvcs, diff the
//! bytes — is wrong here, and would have scored the port's best property as a
//! defect.
//!
//! Stock git guards the index with an `O_EXCL` lockfile and does not wait: the
//! losers die with `Unable to create '.git/index.lock': File exists.` and their
//! work is simply not done. zvcs deliberately does something else — it routes
//! contended writers through a per-repo daemon FIFO, so they queue and land in
//! order instead of failing. Under six-way contention stock stages one file and
//! zvcs stages six. Byte-comparing those two makes the fair queue look like a
//! six-way stdout diff, and a harness that reports a superset as a regression
//! gets switched off.
//!
//! So this dimension asserts an **invariant** rather than an oracle's bytes —
//! one both implementations must satisfy no matter which strategy they pick:
//!
//! > **A writer that exits 0 has done its work.**
//!
//! Stock satisfies it by failing the losers honestly: its successful-exit count
//! and its landed-effect count are equal in every trial measured. zvcs may
//! satisfy it by queueing (exit 0, `zvcs: queued job #N`, the effect appears
//! shortly after) or by serializing. What it may not do is report success for
//! work that never happened, because that is the one outcome a caller cannot
//! detect and cannot retry — `git add f && git commit` commits without `f`, and
//! the exit codes say everything worked.
//!
//! Deferral is not loss, and conflating the two is the easy mistake: the first
//! run of this probe called a queued write "lost" because it measured at t+0.
//! [`SETTLE`] is the answer — a writer that announced a queued job is given time
//! to drain before its effect is required.
//!
//! # The control run is what keeps the bar git's
//!
//! Every case runs against stock git too, and an invariant **stock also fails is
//! not scored against the port**. Without that, this module would be measuring a
//! standard I invented rather than parity, and any invariant I chose too
//! strictly would become a permanent phantom failure. It also keeps the module
//! honest in the other direction: if stock ever loses an update under contention,
//! that is git's semantics and the port is entitled to match it.
//!
//! # Writers that are not all the same verb
//!
//! N copies of one command is the cheapest contention to write and the least
//! like the machine this port targets, where the two processes in a repository
//! are hardly ever the same one. The pairs that actually lose work reach the same
//! bytes through *different* code paths — `commit` walking the index a `add` is
//! rewriting, a `pack-refs` folding away the loose ref an `update-ref` is still
//! creating, a `repack` replacing the pack a `commit` is writing an object into —
//! and none of those is expressible as "run this argv N times".
//!
//! [`Role`] is that: a case carries one script per role and writer `i` takes role
//! `i % roles`, so a six-writer case with two roles is three of each, released
//! together against one repository. Each role names its own [`Effect`], because
//! an `add` and a `commit` have finished different work and asking one question
//! about both would have to be the weaker of the two.
//!
//! # Nothing here may wedge the run
//!
//! A concurrency case is the only place in this harness where a child can be
//! *waiting* rather than working, and the port has a lock daemon it can wait on.
//! `wait_with_output()` would hand a run's whole future to whichever writer
//! deadlocked, so writers are reaped against [`WRITER_DEADLINE`] instead: their
//! output goes to files under the barrier directory (a pipe nobody is draining
//! is its own way to deadlock), the join polls `try_wait`, and a writer still
//! running at the deadline is killed and scored as the availability failure it
//! is. The ceiling is per case, not per writer, so a case's cost is bounded no
//! matter how its writers interleave.

use anyhow::{Context, Result};
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::fixture::{Shape, Templates};

/// Kill a writer and everything still in its process group, then reap it.
///
/// The writer is `/bin/sh`, and every step of its script is a `git` it forked, so
/// killing the shell alone leaves a `git` running in a repository the next case
/// is about to delete and rebuild — the same forgotten-grandchild shape that once
/// parked a whole run. [`CommandExt::process_group`] gave the writer a group of
/// its own at spawn, so the negative pid reaches its children and cannot reach
/// anything this harness did not start.
///
/// Kill by pid as well: a process that called `setsid` has left the group, and
/// only the direct child is certainly still in it. Kill before reap, because
/// `wait` frees the pid and a freed pid may already belong to somebody else.
fn kill_writer_group(child: &mut Child) {
    // SAFETY: `kill` is async-signal-safe and takes no memory from this process;
    // the pid is the child's, which is still unreaped here.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// How long a queued writer has to drain before its effect is required.
///
/// zvcs's queue is asynchronous by design: a contended writer prints `zvcs:
/// queued job #N` and exits 0 while the job runs behind it. Measured drain for a
/// six-way `add` contention was under two seconds; this is that with room, and
/// it is only ever paid by a case that actually announced a queued job.
///
/// Deliberately generous. A too-short settle reports deferral as data loss,
/// which is a false alarm on the port's headline feature — the expensive kind of
/// wrong, because it trains a reader to disbelieve the dimension.
const SETTLE: Duration = Duration::from_secs(20);

/// How often the settle loop re-asks whether every effect has landed.
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// Ceiling on concurrent writers per case.
///
/// Not a tuning knob: spawning many processes per case, across many cases, is
/// how this harness once exhausted the machine's fork capacity and took every
/// shell on it down with it. A case that needs more contention than this to
/// reproduce a defect should widen the read-modify-write window instead (a
/// larger [`Shape`]), which is both cheaper and more faithful to the real
/// failure.
const MAX_WRITERS: usize = 8;

/// How long *all* of a side's writers together get before the survivors are
/// killed.
///
/// The bound this dimension cannot do without. Every other case in the harness
/// is one process that either answers or dies; a concurrency case releases up to
/// [`MAX_WRITERS`] of them into a repository where one of them may be waiting on
/// a lock the port's daemon never hands over — and an unbounded join turns that
/// into a wedged run, which is strictly worse than no case at all because it
/// takes every later case down with it.
///
/// Wildly larger than any writer needs: the slowest verb in this corpus is `gc`
/// on the packed shape, measured at well under a second, and even a queued
/// writer is meant to have drained inside [`SETTLE`]. Anything still running here
/// is not slow, it is stuck, and killing it is how the run finds out.
const WRITER_DEADLINE: Duration = Duration::from_secs(90);

/// How often the bounded join re-asks whether a writer has exited.
const JOIN_POLL: Duration = Duration::from_millis(25);

/// What a writer was supposed to accomplish, and therefore how to tell whether
/// it did.
///
/// Every variant is observed with **stock git**, never with the binary under
/// test. A port that mis-writes the index and then mis-reads it back
/// symmetrically would otherwise confirm its own work — the same reason the
/// interop dimension asks stock to use what the port wrote.
#[derive(Clone, Copy, Debug)]
pub enum Effect {
    /// The path is present in `git ls-files` — it reached the index.
    Staged(&'static str),
    /// The path is present in `git ls-tree -r HEAD` — it reached a commit.
    Committed(&'static str),
    /// The path is absent from `git ls-files` — it was removed from the index.
    Unstaged(&'static str),
    /// The subject line appears somewhere in `HEAD`'s history.
    ///
    /// The only effect that survives N writers contending over the *same* ref:
    /// each commit builds on whatever the last one left, so all N can coexist,
    /// and a writer whose ref update was lost leaves an orphaned commit that
    /// never reaches the history. That is the classic lost-commit race, and no
    /// path- or ref-existence check can see it — the ref exists either way, and
    /// every file involved is present either way.
    LoggedMessage(&'static str),
    /// The fully-qualified ref exists, per `git for-each-ref`.
    ///
    /// A different lock entirely from the three above: refs are guarded per-ref
    /// by `<ref>.lock` and the packed-refs file by its own, neither of which is
    /// the index lock. A port can serialize index writes perfectly and still lose
    /// a branch, so the ref path has to be asked separately rather than assumed
    /// to follow.
    RefExists(&'static str),
    /// The fully-qualified ref does **not** exist, per `git for-each-ref`.
    ///
    /// Deletion is the half of the refs backend `RefExists` cannot reach, and it
    /// is the half `packed-refs` makes interesting: deleting a ref means removing
    /// the loose file *and* rewriting `packed-refs` without it, so a writer that
    /// rewrites a stale snapshot resurrects a ref another writer already deleted
    /// — and reports success doing it.
    ///
    /// One-sided by construction, deliberately: a writer whose *creation* step
    /// failed leaves the ref absent for the wrong reason, and this effect calls
    /// that landed. That direction is a missed defect, never an invented one,
    /// which is the only direction an absence check is allowed to be wrong in.
    RefAbsent(&'static str),
    /// The ref exists and points at exactly the same object as another ref.
    ///
    /// The only effect here that can catch a *reader* misbehaving. `update-ref
    /// refs/heads/mirror{i} refs/heads/main` resolves one ref and writes another
    /// inside one process; a resolve that read a half-rewritten `packed-refs` and
    /// came back with the wrong object still exits 0 and still creates a ref, so
    /// [`Effect::RefExists`] would call that landed. Comparing the two objects is
    /// what turns "the reader saw something" into "the reader saw the truth".
    RefMirrors {
        /// The ref the writer creates.
        refname: &'static str,
        /// The ref it was told to copy — which nothing in such a case moves.
        of: &'static str,
    },
    /// `git config --get-all <key>` lists this value.
    ///
    /// A fourth lock again: `config.lock` guards a whole-file rewrite, so two
    /// `--add`s of one key are a textbook read-modify-write, and a lost one
    /// leaves no trace anywhere else — the file still parses, the key is still
    /// present, and only the count is wrong. Measured stock behaviour for the
    /// losers is `error: could not lock config file .git/config: File exists`
    /// with exit 255.
    ConfigValue {
        key: &'static str,
        value: &'static str,
    },
    /// `git stash list` carries an entry whose message ends with this.
    ///
    /// Stash writes the index, a new commit and `refs/stash` in one go, and its
    /// entries live in the *reflog* of `refs/stash`: only the newest stash commit
    /// is reachable from the ref itself, so [`Effect::LoggedMessage`] finds
    /// exactly one of N however many landed and would report every other writer
    /// as lost. `stash list` is the only view of the stack.
    StashEntry(&'static str),
    /// `git worktree list` reports a worktree whose directory has this name.
    ///
    /// Registration lives in `.git/worktrees/<name>`, which `worktree prune`
    /// deletes when it judges the entry stale — so a `worktree add` racing a
    /// `prune` can be un-registered moments after reporting success, and the
    /// branch it created survives to hide it. Matched on the directory's last
    /// component, never on the path, so no absolute path enters a comparison.
    WorktreeRegistered(&'static str),
}

impl Effect {
    /// The `{i}` in a template becomes the writer's index.
    fn resolve(self, i: usize) -> ResolvedEffect {
        let sub = |s: &str| s.replace("{i}", &i.to_string());
        match self {
            Effect::Staged(p) => ResolvedEffect::Staged(sub(p)),
            Effect::Committed(p) => ResolvedEffect::Committed(sub(p)),
            Effect::Unstaged(p) => ResolvedEffect::Unstaged(sub(p)),
            Effect::LoggedMessage(m) => ResolvedEffect::LoggedMessage(sub(m)),
            Effect::RefExists(r) => ResolvedEffect::RefExists(sub(r)),
            Effect::RefAbsent(r) => ResolvedEffect::RefAbsent(sub(r)),
            Effect::RefMirrors { refname, of } => {
                ResolvedEffect::RefMirrors { refname: sub(refname), of: sub(of) }
            }
            Effect::ConfigValue { key, value } => {
                ResolvedEffect::ConfigValue { key: sub(key), value: sub(value) }
            }
            Effect::StashEntry(m) => ResolvedEffect::StashEntry(sub(m)),
            Effect::WorktreeRegistered(n) => ResolvedEffect::WorktreeRegistered(sub(n)),
        }
    }
}

#[derive(Clone, Debug)]
enum ResolvedEffect {
    Staged(String),
    Committed(String),
    Unstaged(String),
    LoggedMessage(String),
    RefExists(String),
    RefAbsent(String),
    RefMirrors { refname: String, of: String },
    ConfigValue { key: String, value: String },
    StashEntry(String),
    WorktreeRegistered(String),
}

impl ResolvedEffect {
    /// Whether this effect is visible in `repo`, read by stock git.
    fn landed(&self, repo: &Path, stock: &Path) -> bool {
        // Stdout is read whatever the exit code, which is what the existence
        // checks have always done: a verb that fails prints nothing on stdout, so
        // a failure and an empty answer are the same observation either way, and
        // requiring success here would change what `Unstaged` means.
        let lines = |args: &[&str]| -> Vec<String> {
            let out = Command::new(stock)
                .args(args)
                .current_dir(repo)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("LC_ALL", "C")
                .output();
            match out {
                Ok(o) => String::from_utf8_lossy(&o.stdout).lines().map(str::to_owned).collect(),
                Err(_) => Vec::new(),
            }
        };
        let listed = |args: &[&str], want: &str| -> bool { lines(args).iter().any(|l| l == want) };
        // Object identity, unlike everything above, has to distinguish "the ref
        // is missing" from "the ref reads as the empty string", so this one does
        // insist on a successful `rev-parse`.
        let object = |rev: &str| -> Option<String> {
            let out = Command::new(stock)
                .args(["rev-parse", "--verify", "--quiet", rev])
                .current_dir(repo)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("LC_ALL", "C")
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if id.is_empty() {
                None
            } else {
                Some(id)
            }
        };
        match self {
            ResolvedEffect::Staged(p) => listed(&["ls-files"], p),
            ResolvedEffect::Committed(p) => listed(&["ls-tree", "-r", "--name-only", "HEAD"], p),
            ResolvedEffect::Unstaged(p) => !listed(&["ls-files"], p),
            // `for-each-ref` reads loose and packed refs alike, so a ref that a
            // concurrent `pack-refs` moved into `packed-refs` still counts as
            // present — the question is whether the ref exists, not where it is
            // stored.
            // `--all` rather than `HEAD`: a commit whose ref update was lost is
            // still reachable from a reflog or another ref in some designs, and
            // the claim being tested is that the work reached the repository's
            // history, not that it reached one particular ref. Being generous
            // here means a failure is unambiguous.
            ResolvedEffect::LoggedMessage(m) => {
                listed(&["log", "--all", "--format=%s"], m)
            }
            ResolvedEffect::RefExists(r) => listed(&["for-each-ref", "--format=%(refname)"], r),
            ResolvedEffect::RefAbsent(r) => !listed(&["for-each-ref", "--format=%(refname)"], r),
            ResolvedEffect::RefMirrors { refname, of } => {
                match (object(refname), object(of)) {
                    (Some(a), Some(b)) => a == b,
                    // A missing `of` is a broken case, not a landed effect: the
                    // ref being copied is one no case moves, so it can only be
                    // absent if the fixture or the run went wrong, and calling
                    // that "landed" would hide it.
                    _ => false,
                }
            }
            ResolvedEffect::ConfigValue { key, value } => {
                listed(&["config", "--get-all", key], value)
            }
            // `%gs` is the reflog subject, which is where a stash message lives;
            // the entry reads `On <branch>: <message>`, so the branch name would
            // have to be baked into every case if this matched whole lines.
            ResolvedEffect::StashEntry(m) => lines(&["stash", "list", "--format=%gs"])
                .iter()
                .any(|l| l.ends_with(&format!(" {m}")) || l == m),
            ResolvedEffect::WorktreeRegistered(n) => {
                let tail = format!("/{n}");
                lines(&["worktree", "list", "--porcelain"])
                    .iter()
                    .any(|l| l.starts_with("worktree ") && l.ends_with(&tail))
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            ResolvedEffect::Staged(p) => format!("{p} staged"),
            ResolvedEffect::Committed(p) => format!("{p} in HEAD"),
            ResolvedEffect::Unstaged(p) => format!("{p} unstaged"),
            ResolvedEffect::LoggedMessage(m) => format!("commit {m} in history"),
            ResolvedEffect::RefExists(r) => format!("{r} exists"),
            ResolvedEffect::RefAbsent(r) => format!("{r} deleted"),
            ResolvedEffect::RefMirrors { refname, of } => format!("{refname} points at {of}"),
            ResolvedEffect::ConfigValue { key, value } => format!("{key}={value} in config"),
            ResolvedEffect::StashEntry(m) => format!("stash entry {m}"),
            ResolvedEffect::WorktreeRegistered(n) => format!("worktree {n} registered"),
        }
    }
}

/// One kind of writer: a script, and what finishing it accomplishes.
///
/// A case's own `steps`/`effect` are role 0; anything in [`ConcurrentCase::also`]
/// follows, and writer `i` takes role `i % roles`. Two roles over six writers is
/// three processes running each script, released together — which is the only way
/// to put `commit` and `add`, or `pack-refs` and `update-ref`, into the same
/// window.
#[derive(Clone, Copy, Debug)]
pub struct Role {
    /// What this role runs after the barrier opens, joined by `&&`, `{i}`
    /// substituted. Same shape and same meaning as [`ConcurrentCase::steps`].
    pub steps: &'static [&'static [&'static str]],
    /// What a writer of this role accomplishes when it succeeds.
    ///
    /// Per role rather than per case because roles finish different work:
    /// asking one question about an `add` and a `commit` would have to be the
    /// weaker of the two questions, and the weaker one is the one a lost write
    /// slips past.
    pub effect: Effect,
}

/// One concurrent-writer case: N processes released at the same instant against
/// one repository.
#[derive(Clone, Debug)]
pub struct ConcurrentCase {
    /// Stable id, used in report lines and `--only`.
    pub name: &'static str,
    /// Subcommand, for `--only` filtering alongside the rest of the harness.
    pub cmd: &'static str,
    /// Repository the writers share.
    pub shape: Shape,
    /// How many writers run at once. Clamped to [`MAX_WRITERS`].
    pub writers: usize,
    /// What each writer runs after the barrier opens, as one or more commands
    /// joined by `&&`. `{i}` becomes the writer index.
    ///
    /// More than one command because the shape that matters most is the one every
    /// script writes — `git add f && git commit -m x` — where a premature exit 0
    /// from the first command silently changes what the second one does. A single
    /// argv cannot express that, and expressing it as two separate writers would
    /// measure two independent commands rather than a dependency between them.
    pub steps: &'static [&'static [&'static str]],
    /// Files written into the worktree before the barrier opens, `{i}`
    /// substituted — the inputs the writers are racing to record.
    pub prepare: &'static [&'static str],
    /// What writer `i` accomplishes when it succeeds, for role 0.
    pub effect: Effect,
    /// Further roles beyond role 0. Empty means every writer runs the same
    /// script, which is what every case did before roles existed.
    pub also: &'static [Role],
    /// Point `ZVCS_SOCK` at a path that cannot exist, so the port's contended
    /// writers take its **daemon-less fallback** rather than its queue.
    ///
    /// The port has two ways to serialize a contended write and they are not the
    /// same code: the per-repo daemon FIFO, and a fallback for when no daemon is
    /// reachable. The fallback is the one with the worse history — `06b110bd36
    /// lock: with no daemon the fallback held nothing, and writers lost work
    /// saying they had not` — and it is also the path a user gets by default,
    /// because nothing starts a daemon for them.
    ///
    /// Without this the corpus measures whichever path the machine happens to
    /// offer, and a daemon left running by another test would silently retire
    /// every fallback case in the run. Stock git has never heard of the variable,
    /// so the control side is unchanged and the comparison stays fair.
    pub no_daemon: bool,
}

impl ConcurrentCase {
    fn writer_count(&self) -> usize {
        self.writers.min(MAX_WRITERS)
    }

    /// Role 0 and everything in `also`, in the order writers take them.
    fn roles(&self) -> Vec<Role> {
        let mut out = vec![Role { steps: self.steps, effect: self.effect }];
        out.extend_from_slice(self.also);
        out
    }

    /// Which role writer `i` runs. Round-robin, so the roles are as evenly
    /// represented as the writer count allows and every role is in the window.
    fn role_for(&self, i: usize) -> Role {
        let roles = self.roles();
        roles[i % roles.len()]
    }

    /// Reproduction recipe, in the same spirit as `Case::id()`.
    ///
    /// Every role's script is named, separated by ` | `, because with roles the
    /// argv of one writer is no longer the argv of the case — an id carrying only
    /// role 0 would read as a reproduction recipe and leave out half the race.
    pub fn id(&self) -> String {
        let script = self
            .roles()
            .iter()
            .map(|r| r.steps.iter().map(|s| s.join(" ")).collect::<Vec<_>>().join(" && "))
            .collect::<Vec<_>>()
            .join(" | ");
        format!(
            "concurrent::{}::{}::{}x{}[{script}]",
            self.shape.name(),
            self.name,
            self.writer_count(),
            if self.no_daemon { "no-daemon" } else { "" },
        )
    }
}

/// One writer's observed result.
#[derive(Debug)]
struct WriterOutcome {
    index: usize,
    code: Option<i32>,
    output: String,
    effect: ResolvedEffect,
    /// Whether this writer announced that its work had been deferred.
    queued: bool,
    /// Whether the case's deadline expired with this writer still running, so
    /// the harness killed it.
    timed_out: bool,
}

impl WriterOutcome {
    fn succeeded(&self) -> bool {
        self.code == Some(0)
    }
}

/// What one side (stock, or the port) did with a case.
#[derive(Debug)]
pub struct SideOutcome {
    /// Writers that exited 0.
    pub exited_ok: usize,
    /// Writers whose effect is visible after settling, whatever they exited.
    pub landed: usize,
    /// Writers that exited 0 AND whose effect is visible — the numerator the
    /// invariant is actually about. Reported beside `exited_ok` because the raw
    /// `landed` count can equal it while a write is still lost: a writer that
    /// failed late can leave its effect behind and make up the difference.
    pub exited_ok_landed: usize,
    /// Writers that announced a queued job.
    pub queued: usize,
    /// Writers that exited 0 and whose effect never appeared. The defect.
    pub lost: Vec<String>,
    /// Writers that failed without saying anything on stdout or stderr.
    pub silent_failures: Vec<String>,
    /// Writers still running when the case's deadline expired, and killed.
    ///
    /// Scored like any other broken invariant, and it is one: a writer that
    /// neither finishes nor fails is the worst outcome available to a caller —
    /// worse than the lost write this module was built for, because a lost write
    /// at least lets the script that follows it run. It is also the outcome a
    /// lock daemon can produce and stock git structurally cannot, `O_EXCL` having
    /// nothing to wait on.
    pub timed_out: Vec<String>,
    /// `git fsck --strict` exit code, read by stock.
    pub fsck: Option<i32>,
    /// Whether stock could parse the index it was left.
    pub index_parses: bool,
    /// A lockfile nobody released.
    pub orphan_lock: bool,
}

impl SideOutcome {
    /// Whether every invariant held.
    pub fn honest(&self) -> bool {
        self.lost.is_empty()
            && self.silent_failures.is_empty()
            && self.timed_out.is_empty()
            && self.fsck == Some(0)
            && self.index_parses
            && !self.orphan_lock
    }

    /// The failed invariants, named.
    pub fn failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.lost.is_empty() {
            out.push(format!(
                "{} writer(s) exited 0 and did nothing: {}",
                self.lost.len(),
                self.lost.join(", ")
            ));
        }
        for s in &self.silent_failures {
            out.push(format!("failed with no diagnostic: {s}"));
        }
        for s in &self.timed_out {
            out.push(format!(
                "never returned, killed after {}s: {s}",
                WRITER_DEADLINE.as_secs()
            ));
        }
        if self.fsck != Some(0) {
            out.push(format!("fsck --strict exited {:?}", self.fsck));
        }
        if !self.index_parses {
            out.push("stock git cannot parse the index".to_string());
        }
        if self.orphan_lock {
            out.push("a lockfile was left behind".to_string());
        }
        out
    }
}

/// How a case scored once both sides had run.
#[derive(Debug)]
pub enum Verdict {
    /// Every invariant held for the port.
    Honest,
    /// The port broke an invariant stock kept. The only scored failure.
    Defect,
    /// Both sides broke the same invariant, so the bar is git's own behaviour
    /// and the port is entitled to match it.
    ControlAlsoFails,
    /// The case could not be measured (fixture or spawn failure).
    Skipped(String),
    /// No writer succeeded on either side, so "a writer that exits 0 has done its
    /// work" held for want of any writer that exited 0.
    ///
    /// Its own verdict because it is the failure mode this dimension is most
    /// likely to hide from itself. `add-then-commit` scored a clean pass while
    /// every one of its four writers died on `commit --only` naming an untracked
    /// path: nothing contended, nothing landed, and the invariant was vacuously
    /// true. A case that measures nothing must say so, not report success.
    Vacuous(String),
}

/// A case's full result, both sides.
#[derive(Debug)]
pub struct Outcome {
    pub id: String,
    pub verdict: Verdict,
    pub zvcs: Option<SideOutcome>,
    pub stock: Option<SideOutcome>,
}

/// Release N writers against one repository at the same instant and see what
/// they leave.
///
/// The barrier matters. Spawning N children in a loop staggers them by however
/// long a `fork`+`exec` takes, which on a warm machine is long enough for each
/// to finish before the next begins — the race then simply does not happen and
/// the case passes for the wrong reason. Each child instead spins on a sentinel
/// file and `exec`s the real command only once it appears, so the interesting
/// window is entered by every writer together.
fn run_side(
    case: &ConcurrentCase,
    bin: &Path,
    repo: &Path,
    stock: &Path,
    home: &Path,
) -> Result<SideOutcome> {
    let n = case.writer_count();
    let control = repo.join(".zvcs-parity-barrier");
    std::fs::create_dir_all(&control).context("barrier dir")?;
    let go = control.join("GO");
    // Each writer's output goes to its own file rather than a pipe. Piping N
    // children and then draining them one at a time is a deadlock of the
    // harness's own making — a writer that fills its pipe buffer blocks until
    // something reads it, and nothing does until the join reaches it — and a
    // bounded join is worth nothing if the thing it is bounding is the harness.
    let logs = control.join("out");
    std::fs::create_dir_all(&logs).context("writer log dir")?;

    for i in 0..n {
        for template in case.prepare {
            let name = template.replace("{i}", &i.to_string());
            let path = repo.join(&name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&path, format!("content for writer {i}\n"))
                .with_context(|| format!("prepare {name}"))?;
        }
    }

    let mut children = Vec::new();
    for i in 0..n {
        // Every token is single-quoted, so a path or a message containing a space,
        // a glob character or a `$` reaches the binary as one literal word. The
        // binary itself is quoted the same way and referenced as `$0`, which keeps
        // a workdir with a space in it from splitting into two arguments.
        let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
        let script_body = case
            .role_for(i)
            .steps
            .iter()
            .map(|step| {
                let args = step
                    .iter()
                    .map(|a| quote(&a.replace("{i}", &i.to_string())))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("\"$0\" {args}")
            })
            .collect::<Vec<_>>()
            // `&&`, not `;`: a step that fails must stop the writer, because the
            // whole point of a multi-step case is that the later command depends
            // on the earlier one having actually happened.
            .join(" && ");
        let script = format!(
            "while [ ! -f {} ]; do :; done; {script_body}",
            quote(&go.display().to_string())
        );
        let mut cmd = Command::new("/bin/sh");
        // A group of the writer's own, so a deadline that expires can take the
        // `git` it forked with it. See [`kill_writer_group`].
        cmd.process_group(0);
        if case.no_daemon {
            // A socket path inside the barrier directory that is never created.
            // "Unreachable" has to be a path the port will try and fail to
            // connect to, not an unset variable, which would only mean "use the
            // default" and leave a stray daemon able to answer.
            cmd.env("ZVCS_SOCK", control.join("no-such-daemon.sock"));
        }
        let child = cmd
            .arg("-c")
            .arg(script)
            .arg(bin)
            .current_dir(repo)
            .env("HOME", home)
            .env("ZVCS_HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                std::fs::File::create(logs.join(format!("{i}.out"))).context("writer stdout")?,
            ))
            .stderr(Stdio::from(
                std::fs::File::create(logs.join(format!("{i}.err"))).context("writer stderr")?,
            ))
            .spawn()
            .with_context(|| format!("spawn writer {i}"))?;
        children.push(child);
    }

    // Every child is spinning; let the slowest reach its loop, then open the gate.
    std::thread::sleep(Duration::from_millis(300));
    std::fs::File::create(&go).context("open the barrier")?.flush().ok();

    // One deadline for the whole side, not one per writer: what has to be bounded
    // is the case's cost, and N per-writer budgets multiply into N times the
    // ceiling a reader thought they were setting.
    let deadline = Instant::now() + WRITER_DEADLINE;
    let mut writers = Vec::new();
    for (i, mut child) in children.into_iter().enumerate() {
        let mut code = None;
        let mut timed_out = false;
        loop {
            match child.try_wait().with_context(|| format!("wait writer {i}"))? {
                Some(status) => {
                    code = status.code();
                    break;
                }
                None => {
                    if Instant::now() >= deadline {
                        kill_writer_group(&mut child);
                        timed_out = true;
                        break;
                    }
                    std::thread::sleep(JOIN_POLL);
                }
            }
        }
        let read = |ext: &str| {
            std::fs::read(logs.join(format!("{i}.{ext}")))
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        let mut text = read("out");
        text.push_str(&read("err"));
        if timed_out {
            text.push_str("\nzvcs-parity: killed at the case deadline\n");
        }
        writers.push(WriterOutcome {
            index: i,
            code,
            queued: text.contains("queued job"),
            output: text,
            effect: case.role_for(i).effect.resolve(i),
            timed_out,
        });
    }

    // A writer that announced a queued job has not finished; wait for the
    // repository to settle before asking whether its work is there. Poll rather
    // than sleeping the whole budget so a run that has already settled costs
    // nothing.
    let expects_settle = writers.iter().any(|w| w.queued);
    if expects_settle {
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            let all_landed = writers
                .iter()
                .filter(|w| w.succeeded())
                .all(|w| w.effect.landed(repo, stock));
            if all_landed {
                break;
            }
            std::thread::sleep(SETTLE_POLL);
        }
    }

    let mut lost = Vec::new();
    let mut silent_failures = Vec::new();
    let mut timed_out = Vec::new();
    let mut landed = 0;
    for w in &writers {
        if w.timed_out {
            timed_out.push(format!("writer{} ({})", w.index, w.effect.describe()));
        }
        if w.effect.landed(repo, stock) {
            landed += 1;
        } else if w.succeeded() {
            lost.push(format!("writer{} ({})", w.index, w.effect.describe()));
        }
        if !w.succeeded() && w.output.trim().is_empty() {
            silent_failures.push(format!("writer{} rc={:?}", w.index, w.code));
        }
    }

    let probe = |args: &[&str]| -> Option<i32> {
        Command::new(stock)
            .args(args)
            .current_dir(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()
            .and_then(|s| s.code())
    };

    let git_dir = repo.join(".git");
    Ok(SideOutcome {
        exited_ok: writers.iter().filter(|w| w.succeeded()).count(),
        landed,
        exited_ok_landed: writers
            .iter()
            .filter(|w| w.succeeded() && w.effect.landed(repo, stock))
            .count(),
        queued: writers.iter().filter(|w| w.queued).count(),
        lost,
        silent_failures,
        timed_out,
        fsck: probe(&["fsck", "--strict"]),
        index_parses: probe(&["ls-files", "--stage"]) == Some(0),
        orphan_lock: git_dir.join("index.lock").exists(),
    })
}

/// Run one case against the port and against stock, and score it.
pub fn run_concurrent_case(
    case: &ConcurrentCase,
    zvcs_bin: &Path,
    templates: &Templates,
    workdir: &Path,
) -> Outcome {
    let id = case.id();
    let stock = match crate::stock::git() {
        Ok(p) => p,
        Err(e) => {
            return Outcome { id, verdict: Verdict::Skipped(e.to_string()), zvcs: None, stock: None }
        }
    };

    let mut build = |name: &str| -> Result<PathBuf> {
        let repo = workdir.join(name);
        let _ = std::fs::remove_dir_all(&repo);
        templates.instantiate(case.shape, &repo)?;
        Ok(repo)
    };

    let (zvcs_repo, stock_repo) = match (build("conc-zvcs"), build("conc-stock")) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            return Outcome { id, verdict: Verdict::Skipped(e.to_string()), zvcs: None, stock: None }
        }
    };

    let home = &templates.home;
    let zvcs_side = run_side(case, zvcs_bin, &zvcs_repo, stock, home);
    let stock_side = run_side(case, stock, &stock_repo, stock, home);

    match (zvcs_side, stock_side) {
        (Ok(z), Ok(s)) => {
            let verdict = if z.exited_ok == 0 && s.exited_ok == 0 {
                // Checked before honesty: with no successful writer anywhere, the
                // invariant is trivially satisfied and a `Honest` verdict here
                // would be the harness congratulating itself for a broken case.
                Verdict::Vacuous(format!(
                    "no writer succeeded on either side ({} writers each)",
                    case.writer_count()
                ))
            } else if z.honest() {
                Verdict::Honest
            } else if !s.honest() {
                // Stock breaks it too: the bar is git's, not this module's.
                Verdict::ControlAlsoFails
            } else {
                Verdict::Defect
            };
            Outcome { id, verdict, zvcs: Some(z), stock: Some(s) }
        }
        (Err(e), _) | (_, Err(e)) => {
            Outcome { id, verdict: Verdict::Skipped(e.to_string()), zvcs: None, stock: None }
        }
    }
}

/// The curated concurrent corpus.
///
/// Each case is a shape of contention that actually happens on the machine this
/// port targets, not an adversarial construction: several agents staging
/// different files, several committing, one staging while another removes.
pub fn cases() -> Vec<ConcurrentCase> {
    vec![
        // The floor case, and the one that found the read-modify-write race:
        // N writers each staging a file only they touch. No writer conflicts
        // with another over content — the only shared resource is the index —
        // so every writer *should* succeed and every entry *should* land.
        ConcurrentCase {
            name: "add-distinct-paths",
            cmd: "add",
            shape: Shape::Linear,
            writers: 8,
            steps: &[&["add", "conc{i}.txt"]],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
            also: &[],
            no_daemon: false,
        },
        // The same race with a wider read-modify-write window: a shape with more
        // index entries takes longer to read and write, so the interval in which
        // a stale copy can be written over a fresh one is larger.
        ConcurrentCase {
            name: "add-distinct-paths-wide-window",
            cmd: "add",
            shape: Shape::Branched,
            writers: 8,
            steps: &[&["add", "conc{i}.txt"]],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
            also: &[],
            no_daemon: false,
        },
        // `add -u` re-reads every tracked path, so its window is wider still and
        // its writers genuinely overlap on entries rather than only on the file.
        ConcurrentCase {
            name: "add-update-tracked",
            cmd: "add",
            shape: Shape::Dirty,
            writers: 4,
            steps: &[&["add", "conc{i}.txt"]],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
            also: &[],
            no_daemon: false,
        },
        // Staging and committing in one process, which is what a script writes
        // and where a premature exit 0 is most damaging: the commit runs against
        // whatever the add left, and if the add was deferred the commit is empty.
        ConcurrentCase {
            name: "add-then-commit",
            cmd: "commit",
            shape: Shape::Linear,
            writers: 4,
            steps: &[&["add", "conc{i}.txt"], &["commit", "-q", "-m", "w{i}"]],
            prepare: &["conc{i}.txt"],
            effect: Effect::Committed("conc{i}.txt"),
            also: &[],
            no_daemon: false,
        },
        // Removal is the same race in the other direction: a stale writer that
        // re-adds an entry another writer removed is just as much a lost update,
        // and it is the direction a `Staged` check alone cannot see.
        ConcurrentCase {
            name: "rm-cached-distinct-paths",
            cmd: "rm",
            shape: Shape::Branched,
            writers: 4,
            steps: &[&["add", "conc{i}.txt"], &["rm", "--cached", "-q", "conc{i}.txt"]],
            prepare: &["conc{i}.txt"],
            effect: Effect::Unstaged("conc{i}.txt"),
            also: &[],
            no_daemon: false,
        },
        // `update-index` is the plumbing under all of it, and the one a hook or a
        // script is most likely to call in a loop.
        ConcurrentCase {
            name: "update-index-add",
            cmd: "update-index",
            shape: Shape::Linear,
            writers: 6,
            steps: &[&["update-index", "--add", "conc{i}.txt"]],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
            also: &[],
            no_daemon: false,
        },
        // N writers contending over ONE ref, which is the ref race that can
        // actually lose work. Each commit builds on whatever the last one left, so
        // all N belong in the history; a writer whose `refs/heads/main` update was
        // lost leaves its commit orphaned, reachable from nothing. Nothing else in
        // this corpus can see that — the ref exists either way and every file is
        // present either way, so only asking whether the *commit* reached the
        // history distinguishes them.
        ConcurrentCase {
            name: "commit-same-branch",
            cmd: "commit",
            shape: Shape::Linear,
            writers: 6,
            steps: &[&["commit", "-q", "--allow-empty", "-m", "concmsg{i}"]],
            prepare: &[],
            effect: Effect::LoggedMessage("concmsg{i}"),
            also: &[],
            no_daemon: false,
        },
        // Refs are a different lock, and a port that serializes the index
        // perfectly can still lose a branch. This one is a floor case rather than
        // a race: N distinct branches take N distinct `<ref>.lock` files, so
        // nothing contends and both sides land all N. It is here to catch the
        // opposite defect — a port that funnels every ref write through one lock
        // and drops the losers — not to reproduce a race.
        ConcurrentCase {
            name: "branch-create-distinct",
            cmd: "branch",
            shape: Shape::Branched,
            writers: 8,
            steps: &[&["branch", "conc{i}"]],
            prepare: &[],
            effect: Effect::RefExists("refs/heads/conc{i}"),
            also: &[],
            no_daemon: false,
        },
        // Tags go through a different builtin and a different ref namespace, and
        // an annotated tag also writes an object first — so a lost tag can mean a
        // ref that was never created or an object the ref never pointed at.
        ConcurrentCase {
            name: "tag-create-distinct",
            cmd: "tag",
            shape: Shape::Branched,
            writers: 8,
            steps: &[&["tag", "-m", "t{i}", "conctag{i}"]],
            prepare: &[],
            effect: Effect::RefExists("refs/tags/conctag{i}"),
            also: &[],
            no_daemon: false,
        },
        // ---- writers that are not the same verb -------------------------------
        //
        // Everything above is N copies of one command, which is the contention
        // this port is least likely to meet: two processes in one repository are
        // hardly ever the same one. From here down each case puts two different
        // code paths into the same window, which is where the read-modify-write
        // intervals actually differ in length and a stale write has somewhere to
        // land.
        //
        // `commit` reads the whole index and writes a tree from it; `add` reads
        // the whole index and writes one entry into it. Both take `index.lock`,
        // so a port that queues them fairly must still not let a commit publish a
        // tree built from an index it read before a queued `add` was applied —
        // the `add` would exit 0, the entry would appear afterwards, and the
        // commit in between would be missing a file nobody can tell was dropped.
        // Measured on stock over four trials: every writer that exited 0 had its
        // effect, with the losers dying on `index.lock` or on `cannot lock ref
        // 'HEAD': is at … but expected …`.
        ConcurrentCase {
            name: "add-vs-commit",
            cmd: "commit",
            shape: Shape::Branched,
            writers: 6,
            steps: &[&["add", "conc{i}.txt"]],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
            also: &[Role {
                steps: &[&["commit", "-q", "--allow-empty", "-m", "concmsg{i}"]],
                effect: Effect::LoggedMessage("concmsg{i}"),
            }],
            no_daemon: false,
        },
        // `config --add` is a read-modify-write of a whole file guarded by
        // `config.lock`, and the only lost update in this corpus that leaves the
        // repository perfectly valid: the file parses, the key is there, and only
        // the number of values is wrong. `git config --get-all` is the sole
        // witness. Stock's losers exit 255 with `could not lock config file`.
        ConcurrentCase {
            name: "config-add-same-key",
            cmd: "config",
            shape: Shape::Linear,
            writers: 6,
            steps: &[&["config", "--add", "conc.key", "concval{i}"]],
            prepare: &[],
            effect: Effect::ConfigValue { key: "conc.key", value: "concval{i}" },
            also: &[],
            no_daemon: false,
        },
        // The refs backend's own read-modify-write: `pack-refs` rewrites
        // `packed-refs` from a snapshot of every ref it could see, so a loose ref
        // created after that snapshot and deleted from disk during the fold is a
        // ref that existed, reported success, and is gone. Every writer both
        // creates and packs, so each one's snapshot is taken while five others
        // are creating. Stock keeps all six over three trials.
        ConcurrentCase {
            name: "update-ref-vs-pack-refs",
            cmd: "pack-refs",
            shape: Shape::Branched,
            writers: 6,
            steps: &[&["update-ref", "refs/heads/urp{i}", "HEAD"], &["pack-refs", "--all"]],
            prepare: &[],
            effect: Effect::RefExists("refs/heads/urp{i}"),
            also: &[],
            no_daemon: false,
        },
        // Deletion through the same backend, which is the direction that can
        // *resurrect*: removing a ref means unlinking the loose file and
        // rewriting `packed-refs` without it, so a concurrent fold working from a
        // stale snapshot can put back a ref whose deletion already reported
        // success. Stock deletes all six over three trials, occasionally losing a
        // writer honestly to `cannot lock ref … unable to resolve reference`.
        ConcurrentCase {
            name: "pack-refs-vs-update-ref-delete",
            cmd: "update-ref",
            shape: Shape::Branched,
            writers: 6,
            steps: &[
                &["update-ref", "refs/heads/concdel{i}", "HEAD"],
                &["pack-refs", "--all"],
                &["update-ref", "-d", "refs/heads/concdel{i}"],
            ],
            prepare: &[],
            effect: Effect::RefAbsent("refs/heads/concdel{i}"),
            also: &[],
            no_daemon: false,
        },
        // A reader in the window, scored. Half the writers fold `packed-refs`
        // while the other half resolve `refs/heads/main` and copy it — and
        // because a resolve that returns the wrong object still exits 0 and still
        // writes a ref, the assertion is that the copy points at what it copied,
        // not merely that it exists. Nothing in this case moves `main`, so the
        // expected object is fixed and the comparison is between two refs in the
        // finished repository rather than against anything the run observed.
        ConcurrentCase {
            name: "mirror-main-while-packing",
            cmd: "for-each-ref",
            shape: Shape::Branched,
            writers: 6,
            steps: &[&["update-ref", "refs/heads/concmirror{i}", "refs/heads/main"]],
            prepare: &[],
            effect: Effect::RefMirrors {
                refname: "refs/heads/concmirror{i}",
                of: "refs/heads/main",
            },
            also: &[Role {
                steps: &[
                    &["pack-refs", "--all"],
                    &["update-ref", "refs/heads/concmirror{i}", "refs/heads/main"],
                ],
                effect: Effect::RefMirrors {
                    refname: "refs/heads/concmirror{i}",
                    of: "refs/heads/main",
                },
            }],
            no_daemon: false,
        },
        // `checkout -b` is the one porcelain that writes `HEAD`, a new ref and the
        // index in a single invocation, so it contends on all three locks at once
        // — and `HEAD` is the lock nothing else in this corpus takes deliberately.
        // Stock's losers die on `index.lock` with 128.
        ConcurrentCase {
            name: "checkout-new-branch",
            cmd: "checkout",
            shape: Shape::Linear,
            writers: 4,
            steps: &[&["checkout", "-q", "-b", "concbr{i}"]],
            prepare: &[],
            effect: Effect::RefExists("refs/heads/concbr{i}"),
            also: &[],
            no_daemon: false,
        },
        // `stash push` writes the index, two new commits and `refs/stash` in one
        // verb, with every writer contending over that one ref — and it reaches
        // that ref through the index the `add` in front of it just wrote, so the
        // two steps contend on different locks in sequence. Each writer stages
        // and stashes only its own path, so each genuinely has something to save
        // and an exit 0 with an empty stack is unambiguous.
        //
        // Staged rather than untracked, which cost a run to learn: the obvious
        // spelling is `stash push -u -- conc{i}.txt` on an untracked path, and
        // the port rejects that with `pathspec ':(prefix:0)conc{i}.txt' did not
        // match any file(s) known to git` **with no contention at all**. All four
        // writers then failed for a reason that had nothing to do with the race,
        // the case measured nothing on the port's side, and it still scored a
        // clean pass because the invariant held for want of any writer that
        // exited 0 — the shape [`Verdict::Vacuous`] exists to catch, hidden here
        // because stock's side did succeed. Staging first is the spelling both
        // sides complete, so the race is actually entered.
        //
        // Measured on stock: entries can *exceed* the successful-exit count — a
        // `stash push` that fails writing the index has already pushed — which
        // the invariant permits, being about writers that claimed success and not
        // about the total.
        ConcurrentCase {
            name: "stash-push-distinct-paths",
            cmd: "stash",
            shape: Shape::Stashed,
            writers: 4,
            steps: &[
                &["add", "conc{i}.txt"],
                &["stash", "push", "-q", "-m", "concstash{i}", "--", "conc{i}.txt"],
            ],
            prepare: &["conc{i}.txt"],
            effect: Effect::StashEntry("concstash{i}"),
            also: &[],
            no_daemon: false,
        },
        // Object creation racing object *deletion*. `gc --prune=now` drops the
        // two-week grace period that exists precisely so a concurrent writer's
        // half-referenced objects survive, so this is the shape where a port that
        // prunes by reachability at the wrong instant deletes an object a commit
        // is about to point at. `fsck --strict` — already an invariant here — is
        // what sees it; the commit messages are what say whether the writers that
        // claimed success got their commits. Stock over three trials: successful
        // exits and landed commits equal, fsck clean.
        ConcurrentCase {
            name: "gc-prune-vs-commit",
            cmd: "gc",
            shape: Shape::Branched,
            writers: 4,
            steps: &[&["commit", "-q", "--allow-empty", "-m", "concgc{i}"]],
            prepare: &[],
            effect: Effect::LoggedMessage("concgc{i}"),
            also: &[Role {
                steps: &[
                    &["gc", "--quiet", "--prune=now"],
                    &["commit", "-q", "--allow-empty", "-m", "concgc{i}"],
                ],
                effect: Effect::LoggedMessage("concgc{i}"),
            }],
            no_daemon: false,
        },
        // The pack pair. `repack -a -d` replaces every pack and unlinks the old
        // ones while `index-pack` is reading a pack and the commits are writing
        // loose objects — an object store whose file set changes under a reader.
        // The index-pack half writes its output under a per-writer name so the
        // writers race the store rather than each other's output file, and the
        // commit after each half is what makes either side's success checkable.
        // Stock over four trials: nothing lost, `fsck --strict` clean.
        ConcurrentCase {
            name: "index-pack-vs-repack",
            cmd: "repack",
            shape: Shape::Packed,
            writers: 4,
            steps: &[
                &["index-pack", "-o", "concip{i}.idx", "packs/sample.pack"],
                &["commit", "-q", "--allow-empty", "-m", "concip{i}"],
            ],
            prepare: &[],
            effect: Effect::LoggedMessage("concip{i}"),
            also: &[Role {
                steps: &[
                    &["repack", "-q", "-a", "-d"],
                    &["commit", "-q", "--allow-empty", "-m", "concip{i}"],
                ],
                effect: Effect::LoggedMessage("concip{i}"),
            }],
            no_daemon: false,
        },
        // `worktree add` registers an administrative directory under
        // `.git/worktrees/`; `worktree prune` deletes the ones it judges stale,
        // and a half-registered entry looks exactly like a stale one. The branch
        // each add creates survives a prune, so asking whether the *worktree* is
        // still registered is the only question that separates "added" from
        // "added and swept away by the other process". Stock keeps all four over
        // three trials.
        ConcurrentCase {
            name: "worktree-add-vs-prune",
            cmd: "worktree",
            shape: Shape::Linear,
            writers: 4,
            steps: &[&["worktree", "add", "-q", "concwt{i}"], &["worktree", "prune"]],
            prepare: &[],
            effect: Effect::WorktreeRegistered("concwt{i}"),
            also: &[],
            no_daemon: false,
        },
        // `fetch` writes remote-tracking refs and new objects; `gc` moves and
        // removes objects underneath it. The fixture's remote lives inside the
        // repository at a relative URL, so this costs a local transfer rather
        // than a network. Each writer then records what it saw in its own ref, so
        // one verb's failure cannot be mistaken for the other's.
        ConcurrentCase {
            name: "fetch-vs-gc",
            cmd: "fetch",
            shape: Shape::BehindRemote,
            writers: 4,
            steps: &[
                &["fetch", "-q", "origin"],
                &["update-ref", "refs/heads/concfetch{i}", "refs/remotes/origin/main"],
            ],
            prepare: &[],
            effect: Effect::RefExists("refs/heads/concfetch{i}"),
            also: &[Role {
                steps: &[&["gc", "--quiet"], &["update-ref", "refs/heads/concfetch{i}", "HEAD"]],
                effect: Effect::RefExists("refs/heads/concfetch{i}"),
            }],
            no_daemon: false,
        },
        // ---- the same races with no daemon to queue through --------------------
        //
        // Everything above lets the port pick whichever of its two serialization
        // paths it can reach, which on a machine with a coordinator running is
        // the queue and on a user's machine is the fallback — so the corpus was
        // measuring whichever the harness happened to get. These two pin the
        // fallback: `ZVCS_SOCK` names a socket that will never exist, so the
        // daemon is unreachable by construction rather than by luck.
        //
        // The fallback is the path with the history. Its lock once held nothing
        // at all, and eight writers lost each other's entries while every one of
        // them exited 0 — the defect this whole module was built around — so it
        // is the path that has to keep being asked.
        ConcurrentCase {
            name: "add-distinct-paths-no-daemon",
            cmd: "add",
            shape: Shape::Linear,
            writers: 8,
            steps: &[&["add", "conc{i}.txt"]],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
            also: &[],
            no_daemon: true,
        },
        // And the fallback under two different verbs, where a writer that
        // serializes by re-reading has to notice a commit moved underneath it
        // rather than only that another `add` did.
        ConcurrentCase {
            name: "add-vs-commit-no-daemon",
            cmd: "commit",
            shape: Shape::Branched,
            writers: 6,
            steps: &[&["add", "conc{i}.txt"]],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
            also: &[Role {
                steps: &[&["commit", "-q", "--allow-empty", "-m", "concnd{i}"]],
                effect: Effect::LoggedMessage("concnd{i}"),
            }],
            no_daemon: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_counts_are_capped() {
        for case in cases() {
            assert!(
                case.writer_count() <= MAX_WRITERS,
                "{} would spawn {} writers",
                case.name,
                case.writer_count()
            );
            assert!(case.writers > 1, "{} is not a concurrency case", case.name);
        }
    }

    /// Every case whose effect is a worktree path must actually create that path,
    /// or the case measures nothing and passes for it. A ref effect creates no
    /// file, so it is exempt — but it must still name a fully-qualified ref, since
    /// `for-each-ref --format=%(refname)` prints nothing else and a short name
    /// would silently never match.
    #[test]
    fn every_effect_is_reachable_from_what_the_case_sets_up() {
        for case in cases() {
            for role in case.roles() {
                match role.effect {
                    Effect::Staged(p) | Effect::Committed(p) | Effect::Unstaged(p) => assert!(
                        case.prepare.contains(&p),
                        "{}: effect names {p}, which prepare does not create",
                        case.name
                    ),
                    Effect::LoggedMessage(m) => assert!(
                        !m.is_empty(),
                        "{}: an empty subject can never be matched in a log",
                        case.name
                    ),
                    Effect::RefExists(r) | Effect::RefAbsent(r) => assert!(
                        r.starts_with("refs/"),
                        "{}: ref effect {r} is not fully qualified, so it can never match",
                        case.name
                    ),
                    Effect::RefMirrors { refname, of } => {
                        assert!(
                            refname.starts_with("refs/") && of.starts_with("refs/"),
                            "{}: mirror effect {refname}/{of} is not fully qualified",
                            case.name
                        );
                        assert_ne!(
                            refname, of,
                            "{}: a ref mirroring itself is true for free",
                            case.name
                        );
                    }
                    Effect::ConfigValue { key, value } => {
                        assert!(
                            key.contains('.'),
                            "{}: {key} is not a section.key and `config --get-all` \
                             would reject it",
                            case.name
                        );
                        assert!(
                            !value.is_empty(),
                            "{}: an empty value cannot be told from an absent one",
                            case.name
                        );
                    }
                    Effect::StashEntry(m) => assert!(
                        !m.is_empty(),
                        "{}: an empty stash message matches every entry",
                        case.name
                    ),
                    Effect::WorktreeRegistered(n) => assert!(
                        !n.is_empty() && !n.contains('/'),
                        "{}: {n} is matched as a directory's last component",
                        case.name
                    ),
                }
            }
        }
    }

    /// `{i}` must appear in every writer's argv, or all N writers run the
    /// identical command and the case is not measuring per-writer effects.
    ///
    /// Per role, not per case: a case that varied only in role 0 would still have
    /// every writer of role 1 running one identical command, and their shared
    /// effect would then be landed by whichever of them won rather than by each
    /// of them — the exact way a lost write hides.
    #[test]
    fn every_case_varies_by_writer() {
        for case in cases() {
            for (k, role) in case.roles().iter().enumerate() {
                assert!(
                    role.steps.iter().flat_map(|s| s.iter()).any(|a| a.contains("{i}")),
                    "{}: role {k} argv does not vary by writer",
                    case.name
                );
            }
        }
    }

    /// A case must have at least as many writers as roles, or a role it declares
    /// never runs and the case measures a race it claims to set up but does not.
    #[test]
    fn every_role_gets_a_writer() {
        for case in cases() {
            let roles = case.roles().len();
            assert!(
                case.writer_count() >= roles,
                "{}: {roles} roles over {} writers leaves one unused",
                case.name,
                case.writer_count()
            );
            let used: std::collections::HashSet<usize> =
                (0..case.writer_count()).map(|i| i % roles).collect();
            assert_eq!(used.len(), roles, "{}: not every role is taken", case.name);
        }
    }

    /// Round-robin, so a two-role case really is half and half rather than one
    /// writer of the second kind in a crowd of the first.
    #[test]
    fn roles_alternate_across_writers() {
        let case = cases()
            .into_iter()
            .find(|c| c.name == "add-vs-commit")
            .expect("add-vs-commit is the two-role floor case");
        assert_eq!(case.roles().len(), 2);
        assert_eq!(case.role_for(0).steps, case.steps);
        assert_eq!(case.role_for(2).steps, case.steps);
        assert_eq!(case.role_for(1).steps, case.also[0].steps);
        assert_eq!(case.role_for(3).steps, case.also[0].steps);
    }

    /// The id is the reproduction recipe, so a multi-role case has to name every
    /// script in it — an id carrying only role 0 describes half a race.
    #[test]
    fn ids_name_every_role() {
        for case in cases() {
            let id = case.id();
            for role in case.roles() {
                let head = role.steps[0].join(" ");
                assert!(id.contains(&head), "{id} does not mention {head}");
            }
        }
    }

    #[test]
    fn ids_are_unique_and_name_their_shape() {
        let mut seen = std::collections::HashSet::new();
        for case in cases() {
            let id = case.id();
            assert!(id.starts_with("concurrent::"), "{id}");
            assert!(seen.insert(id.clone()), "duplicate case id {id}");
        }
    }

    /// A resolved effect substitutes the writer index everywhere `{i}` appears.
    #[test]
    fn effects_resolve_the_writer_index() {
        match Effect::Staged("conc{i}.txt").resolve(3) {
            ResolvedEffect::Staged(p) => assert_eq!(p, "conc3.txt"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// The invariant summary must call a side dishonest for each failure mode
    /// independently — a lost update with a clean fsck is still a defect, which
    /// is exactly the shape that survived every other dimension.
    #[test]
    fn a_lost_update_alone_is_dishonest() {
        let side = SideOutcome {
            exited_ok: 8,
            landed: 6,
            exited_ok_landed: 6,
            queued: 0,
            lost: vec!["writer3 (conc3.txt staged)".into()],
            silent_failures: Vec::new(),
            timed_out: Vec::new(),
            fsck: Some(0),
            index_parses: true,
            orphan_lock: false,
        };
        assert!(!side.honest());
        assert!(side.failures()[0].contains("exited 0 and did nothing"));
    }

    #[test]
    fn a_clean_side_is_honest() {
        let side = SideOutcome {
            exited_ok: 8,
            landed: 8,
            exited_ok_landed: 8,
            queued: 3,
            lost: Vec::new(),
            silent_failures: Vec::new(),
            timed_out: Vec::new(),
            fsck: Some(0),
            index_parses: true,
            orphan_lock: false,
        };
        assert!(side.honest());
        assert!(side.failures().is_empty());
    }

    /// A writer that never came back is a failure in its own right, and it has to
    /// be one *independently*: everything else about such a side looks clean —
    /// nothing was lost, because nothing claimed success — and a summary that
    /// scored only the other invariants would report a wedged writer as a pass.
    #[test]
    fn a_killed_writer_alone_is_dishonest() {
        let side = SideOutcome {
            exited_ok: 3,
            landed: 3,
            exited_ok_landed: 3,
            queued: 4,
            lost: Vec::new(),
            silent_failures: Vec::new(),
            timed_out: vec!["writer3 (conc3.txt staged)".into()],
            fsck: Some(0),
            index_parses: true,
            orphan_lock: false,
        };
        assert!(!side.honest());
        assert!(
            side.failures().iter().any(|f| f.contains("never returned")),
            "{:?}",
            side.failures()
        );
    }

    /// The two effects that read a *value* rather than a name substitute the
    /// writer index into it, which is what keeps six writers from all landing one
    /// another's work.
    #[test]
    fn value_effects_resolve_the_writer_index() {
        match (Effect::ConfigValue { key: "conc.key", value: "concval{i}" }).resolve(5) {
            ResolvedEffect::ConfigValue { key, value } => {
                assert_eq!(key, "conc.key");
                assert_eq!(value, "concval5");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        match (Effect::RefMirrors { refname: "refs/heads/m{i}", of: "refs/heads/main" }).resolve(2) {
            ResolvedEffect::RefMirrors { refname, of } => {
                assert_eq!(refname, "refs/heads/m2");
                assert_eq!(of, "refs/heads/main");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// The corpus must keep measuring pairs of *different* verbs. Deleting the
    /// last multi-role case would leave a corpus of N-copies-of-one-command,
    /// which is the shape this dimension was widened to stop being.
    #[test]
    fn the_corpus_races_different_verbs_against_each_other() {
        let mixed: Vec<&str> =
            cases().iter().filter(|c| !c.also.is_empty()).map(|c| c.name).collect();
        assert!(mixed.len() >= 4, "only {mixed:?} put two code paths in one window");
    }

    /// Every lock this corpus means to contend over must have a case that reaches
    /// it. Named by the verb that takes the lock, because the lock itself is an
    /// implementation detail of whichever backend a side chose.
    #[test]
    fn the_corpus_covers_every_lock_it_claims_to() {
        let names: Vec<&str> = cases().iter().map(|c| c.name).collect();
        for wanted in [
            "add-distinct-paths",             // index.lock
            "commit-same-branch",             // one ref, N writers
            "update-ref-vs-pack-refs",        // packed-refs.lock, creation
            "pack-refs-vs-update-ref-delete", // packed-refs.lock, deletion
            "config-add-same-key",            // config.lock
            "checkout-new-branch",            // HEAD.lock
            "stash-push-distinct-paths",      // refs/stash
            "worktree-add-vs-prune",          // .git/worktrees registration
        ] {
            assert!(names.contains(&wanted), "the corpus lost {wanted}");
        }
    }

    /// The daemon-less fallback must keep being measured. Its lock is the one
    /// that has already shipped holding nothing, and a corpus that only ever
    /// reached the queue would have scored that as clean.
    #[test]
    fn the_corpus_pins_the_daemon_less_fallback() {
        let fallback: Vec<&str> =
            cases().iter().filter(|c| c.no_daemon).map(|c| c.name).collect();
        assert!(
            fallback.len() >= 2,
            "only {fallback:?} force the fallback path"
        );
        // And the ids have to say so, or two cases differing only in which
        // serialization path they took would be indistinguishable in a report.
        for case in cases().iter().filter(|c| c.no_daemon) {
            assert!(case.id().contains("no-daemon"), "{}", case.id());
        }
    }
}
