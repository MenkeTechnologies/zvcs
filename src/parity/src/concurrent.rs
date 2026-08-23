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

use anyhow::{Context, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::fixture::{Shape, Templates};

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
}

impl Effect {
    /// The `{i}` in a template becomes the writer's index.
    fn resolve(self, i: usize) -> ResolvedEffect {
        let sub = |s: &str| s.replace("{i}", &i.to_string());
        match self {
            Effect::Staged(p) => ResolvedEffect::Staged(sub(p)),
            Effect::Committed(p) => ResolvedEffect::Committed(sub(p)),
            Effect::Unstaged(p) => ResolvedEffect::Unstaged(sub(p)),
        }
    }
}

#[derive(Clone, Debug)]
enum ResolvedEffect {
    Staged(String),
    Committed(String),
    Unstaged(String),
}

impl ResolvedEffect {
    /// Whether this effect is visible in `repo`, read by stock git.
    fn landed(&self, repo: &Path, stock: &Path) -> bool {
        let listed = |args: &[&str], want: &str| -> bool {
            let out = Command::new(stock)
                .args(args)
                .current_dir(repo)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("LC_ALL", "C")
                .output();
            match out {
                Ok(o) => String::from_utf8_lossy(&o.stdout).lines().any(|l| l == want),
                Err(_) => false,
            }
        };
        match self {
            ResolvedEffect::Staged(p) => listed(&["ls-files"], p),
            ResolvedEffect::Committed(p) => listed(&["ls-tree", "-r", "--name-only", "HEAD"], p),
            ResolvedEffect::Unstaged(p) => !listed(&["ls-files"], p),
        }
    }

    fn describe(&self) -> String {
        match self {
            ResolvedEffect::Staged(p) => format!("{p} staged"),
            ResolvedEffect::Committed(p) => format!("{p} in HEAD"),
            ResolvedEffect::Unstaged(p) => format!("{p} unstaged"),
        }
    }
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
    /// Each writer's argv after the binary name. `{i}` becomes the writer index.
    pub argv: &'static [&'static str],
    /// Files written into the worktree before the barrier opens, `{i}`
    /// substituted — the inputs the writers are racing to record.
    pub prepare: &'static [&'static str],
    /// What writer `i` accomplishes when it succeeds.
    pub effect: Effect,
}

impl ConcurrentCase {
    fn writer_count(&self) -> usize {
        self.writers.min(MAX_WRITERS)
    }

    /// Reproduction recipe, in the same spirit as `Case::id()`.
    pub fn id(&self) -> String {
        format!(
            "concurrent::{}::{}::{}x[{}]",
            self.shape.name(),
            self.name,
            self.writer_count(),
            self.argv.join(" ")
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
    /// Writers whose effect is visible after settling.
    pub landed: usize,
    /// Writers that announced a queued job.
    pub queued: usize,
    /// Writers that exited 0 and whose effect never appeared. The defect.
    pub lost: Vec<String>,
    /// Writers that failed without saying anything on stdout or stderr.
    pub silent_failures: Vec<String>,
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
        let argv: Vec<String> =
            case.argv.iter().map(|a| a.replace("{i}", &i.to_string())).collect();
        // `"$0" "$@"` keeps every argument a single word no matter what it
        // contains, which a generated path with a space in it relies on.
        let script = format!(
            "while [ ! -f '{}' ]; do :; done; exec \"$0\" \"$@\"",
            go.display()
        );
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .arg(bin)
            .args(&argv)
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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn writer {i}"))?;
        children.push(child);
    }

    // Every child is spinning; let the slowest reach its loop, then open the gate.
    std::thread::sleep(Duration::from_millis(300));
    std::fs::File::create(&go).context("open the barrier")?.flush().ok();

    let mut writers = Vec::new();
    for (i, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().with_context(|| format!("wait writer {i}"))?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        writers.push(WriterOutcome {
            index: i,
            code: out.status.code(),
            queued: text.contains("queued job"),
            output: text,
            effect: case.effect.resolve(i),
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
    let mut landed = 0;
    for w in &writers {
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
        queued: writers.iter().filter(|w| w.queued).count(),
        lost,
        silent_failures,
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
            let verdict = if z.honest() {
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
            argv: &["add", "conc{i}.txt"],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
        },
        // The same race with a wider read-modify-write window: a shape with more
        // index entries takes longer to read and write, so the interval in which
        // a stale copy can be written over a fresh one is larger.
        ConcurrentCase {
            name: "add-distinct-paths-wide-window",
            cmd: "add",
            shape: Shape::Branched,
            writers: 8,
            argv: &["add", "conc{i}.txt"],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
        },
        // `add -u` re-reads every tracked path, so its window is wider still and
        // its writers genuinely overlap on entries rather than only on the file.
        ConcurrentCase {
            name: "add-update-tracked",
            cmd: "add",
            shape: Shape::Dirty,
            writers: 4,
            argv: &["add", "conc{i}.txt"],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
        },
        // Staging and committing in one process, which is what a script writes
        // and where a premature exit 0 is most damaging: the commit runs against
        // whatever the add left, and if the add was deferred the commit is empty.
        ConcurrentCase {
            name: "add-then-commit",
            cmd: "commit",
            shape: Shape::Linear,
            writers: 4,
            argv: &["-c", "user.name=A", "commit", "-m", "w{i}", "--only", "conc{i}.txt"],
            prepare: &["conc{i}.txt"],
            effect: Effect::Committed("conc{i}.txt"),
        },
        // Removal is the same race in the other direction: a stale writer that
        // re-adds an entry another writer removed is just as much a lost update,
        // and it is the direction a `Staged` check alone cannot see.
        ConcurrentCase {
            name: "rm-cached-distinct-paths",
            cmd: "rm",
            shape: Shape::Branched,
            writers: 4,
            argv: &["rm", "--cached", "-q", "conc{i}.txt"],
            prepare: &["conc{i}.txt"],
            effect: Effect::Unstaged("conc{i}.txt"),
        },
        // `update-index` is the plumbing under all of it, and the one a hook or a
        // script is most likely to call in a loop.
        ConcurrentCase {
            name: "update-index-add",
            cmd: "update-index",
            shape: Shape::Linear,
            writers: 6,
            argv: &["update-index", "--add", "conc{i}.txt"],
            prepare: &["conc{i}.txt"],
            effect: Effect::Staged("conc{i}.txt"),
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

    /// Every case's effect must name a path its `prepare` actually creates, or
    /// the case measures nothing and passes for it.
    #[test]
    fn every_effect_is_a_path_the_case_prepares() {
        for case in cases() {
            let effect_path = match case.effect {
                Effect::Staged(p) | Effect::Committed(p) | Effect::Unstaged(p) => p,
            };
            assert!(
                case.prepare.contains(&effect_path),
                "{}: effect names {effect_path}, which prepare does not create",
                case.name
            );
        }
    }

    /// `{i}` must appear in every writer's argv, or all N writers run the
    /// identical command and the case is not measuring per-writer effects.
    #[test]
    fn every_case_varies_by_writer() {
        for case in cases() {
            assert!(
                case.argv.iter().any(|a| a.contains("{i}")),
                "{}: argv does not vary by writer",
                case.name
            );
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
            queued: 0,
            lost: vec!["writer3 (conc3.txt staged)".into()],
            silent_failures: Vec::new(),
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
            queued: 3,
            lost: Vec::new(),
            silent_failures: Vec::new(),
            fsck: Some(0),
            index_parses: true,
            orphan_lock: false,
        };
        assert!(side.honest());
        assert!(side.failures().is_empty());
    }
}
