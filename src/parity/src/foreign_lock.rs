//! What each binary does when somebody else is already holding a lock.
//!
//! A lock file left behind by a killed process, or held for a moment by a
//! concurrent `pack-refs`, is the ordinary state of a worktree with sixteen
//! writers. Nothing in this harness planted one, so every case measured a
//! repository whose locks were all free — the one condition under which lock
//! handling cannot be wrong.
//!
//! # The invariant is one-directional, and that is the whole design
//!
//! The tempting check is that both binaries agree, and it is wrong here for the
//! same reason it is wrong in [`crate::concurrent`]: the two are *entitled* to
//! disagree in one direction. Stock git fails a contended writer outright; zvcs
//! may queue it, wait for the holder, or serialize behind it and still succeed.
//! Scoring that as a difference would report the port's fair queue as a defect.
//!
//! So this dimension asserts only the direction that cannot be defended:
//!
//! > **If stock git completes the work with the lock held, so must the port.**
//!
//! Doing *more* than git under contention is the feature. Doing *less* is an
//! availability failure — and it is not hypothetical. `update-ref
//! refs/heads/new HEAD` with a `packed-refs.lock` present succeeds on git, which
//! does not need that lock to write a *loose* ref, and fails on the port with
//! `fatal: … The lock for the packed-ref file could not be obtained`, leaving no
//! ref behind. A stale lock from one killed process therefore blocks every ref
//! creation on this port and none on git.
//!
//! The reverse direction is recorded but not scored, because it is genuinely
//! ambiguous: `pack-refs` with nothing left to pack exits 0 here without taking
//! the lock at all, where git takes it unconditionally and dies. That may be a
//! defect or may be a port that is simply less eager, and this dimension cannot
//! tell which — so it prints the fact and leaves the judgement to a reader,
//! rather than inventing a verdict it cannot support.
//!
//! # Three questions per lock, not one
//!
//! A lock is only interesting against the verbs that meet it, and there are
//! three kinds and each says something different:
//!
//! * **The verb that needs it** must refuse — `pack-refs` against
//!   `packed-refs.lock`, `config --add` against `config.lock`. Without these the
//!   whole dimension would be satisfied by a port that never takes a lock at all.
//! * **The verb that does not need it** must succeed. Git's locks are narrow:
//!   `refs/heads/main.lock` stops `update-ref refs/heads/main` and stops nothing
//!   else, so `update-ref refs/heads/fl-other` and `branch fl-newb` both complete
//!   with it held. A port whose ref writes funnel through one lock fails those
//!   and is unusable in a worktree where any stale `.lock` exists — which is
//!   every worktree that has ever had a writer killed.
//! * **The reader** must never be blocked at all. `status`, `ls-files`, `diff`,
//!   `show-ref`, `for-each-ref`, `symbolic-ref`, `config --list` all complete on
//!   stock with every lock in this file held, because none of them writes.
//!
//! Exit codes are asserted whenever both sides refuse, and they are not uniform
//! even within one lock: measured on git 2.55.0, `index.lock` makes `commit`,
//! `rm --cached`, `checkout` and `reset --hard` exit **128** while `stash push`
//! exits **1**, and `config --add` under `config.lock` exits **255**. A port that
//! spells every lock failure the same way passes a "does it refuse" check and
//! still breaks the `&&` chain of any script that reads `$?`.
//!
//! # Bounded, because a lock nobody releases is a lock a port may wait on
//!
//! The holder planted here never goes away. Stock git cannot wait on it — its
//! lockfile is `O_EXCL` and there is nothing to wait for — but the port has a
//! queue, and a queue's failure mode is waiting forever. Every invocation is
//! therefore run against [`RUN_DEADLINE`] and killed if it outlives it; a killed
//! run reports no exit code, which makes it a refusal, which is scored as one.
//! Silence is the one answer this dimension may not simply hang on.

use anyhow::{Context, Result};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::fixture::{Shape, Templates};

/// Kill a run and everything still in its process group, then reap it.
///
/// A verb that queues may have started helpers, and `fetch` starts one on
/// purpose; killing the named process alone leaves them writing into a fixture
/// the next case is about to rebuild. The run was given a process group of its
/// own at spawn, so the negative pid reaches its children and nothing else. Kill
/// by pid too — anything that called `setsid` has left the group — and kill
/// before reaping, since `wait` frees the pid for reuse.
fn kill_run_group(child: &mut Child) {
    // SAFETY: `kill` is async-signal-safe and takes no memory from this process;
    // the pid is the child's, which is still unreaped here.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// How long one invocation gets before it is killed.
///
/// Every case here is a single command against a small fixture that stock
/// answers in milliseconds; this is four orders of magnitude of headroom, so
/// reaching it means the process is not slow but stuck. Bounded at all because
/// the holder is immortal by design and a port that queues behind it has nothing
/// to be woken by — an unbounded wait would trade a measurable refusal for a
/// wedged harness.
const RUN_DEADLINE: Duration = Duration::from_secs(60);

/// How often the bounded wait re-asks whether the child has exited.
const RUN_POLL: Duration = Duration::from_millis(20);

/// One case: plant a lock, run one command on each side, compare.
#[derive(Clone, Debug)]
pub struct ForeignLockCase {
    pub name: &'static str,
    pub cmd: &'static str,
    pub shape: Shape,
    /// Path under the git directory to create before running, e.g. `index.lock`.
    pub lock: &'static str,
    /// Commands run *before* the lock is planted, to give the verb something to
    /// do. A verb with no work sometimes never reaches the lock at all, which
    /// measures the absence of work rather than the presence of the lock.
    pub setup: &'static [&'static [&'static str]],
    pub argv: &'static [&'static str],
}

impl ForeignLockCase {
    pub fn id(&self) -> String {
        format!(
            "foreign-lock::{}::{}::{}::[{}]",
            self.shape.name(),
            self.name,
            self.lock,
            self.argv.join(" ")
        )
    }
}

/// One side's result.
#[derive(Debug)]
pub struct SideRun {
    pub code: Option<i32>,
    pub first_line: String,
}

impl SideRun {
    pub fn succeeded(&self) -> bool {
        self.code == Some(0)
    }
}

/// Run `argv` with a ceiling on how long it may take.
///
/// Output goes to files rather than pipes: `Child::wait_with_output` is the only
/// convenient way to drain a pipe and it blocks without limit, which is the thing
/// being avoided. Logs are truncated per invocation, and the invocations here are
/// sequential, so one pair of files serves the whole run.
fn run_bounded(mut cmd: Command, logs: &Path, what: &str) -> Result<SideRun> {
    let out_path = logs.join("run.out");
    let err_path = logs.join("run.err");
    let mut child = cmd
        // A group of its own, so a deadline that expires takes the helpers this
        // invocation started with it. See [`kill_run_group`].
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&out_path).context("run stdout")?))
        .stderr(Stdio::from(std::fs::File::create(&err_path).context("run stderr")?))
        .spawn()
        .with_context(|| format!("spawn {what}"))?;

    let deadline = Instant::now() + RUN_DEADLINE;
    let mut code = None;
    let mut killed = false;
    loop {
        match child.try_wait().with_context(|| format!("wait {what}"))? {
            Some(status) => {
                code = status.code();
                break;
            }
            None => {
                if Instant::now() >= deadline {
                    kill_run_group(&mut child);
                    killed = true;
                    break;
                }
                std::thread::sleep(RUN_POLL);
            }
        }
    }

    let read = |p: &Path| {
        std::fs::read(p).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
    };
    let mut text = read(&out_path);
    text.push_str(&read(&err_path));
    let first_line = if killed {
        // Deliberately not the process's own first line: what a reader has to
        // see is that nothing ever answered, and `code: None` alone reads as an
        // ordinary signal death.
        format!("zvcs-parity: still running after {}s, killed", RUN_DEADLINE.as_secs())
    } else {
        text.lines().next().unwrap_or("").to_string()
    };
    Ok(SideRun { code, first_line })
}

#[derive(Debug)]
pub enum Verdict {
    /// Both refused, or the port did at least as much as git.
    Agree,
    /// git completed the work and the port refused it. The scored failure.
    PortRefusedWhatGitDid,
    /// The port succeeded where git refused — recorded, never scored. See the
    /// module header for why this cannot be adjudicated here.
    PortDidMore,
    /// Both refused, with different exit codes.
    ///
    /// Scored, and the one axis here with no superset defence: the fair queue
    /// explains why the port might *succeed* where git fails, and explains
    /// nothing about why a refusal should carry a different number. git spells a
    /// fatal lock failure 128; a caller that branches on `$?` — every `&&` chain,
    /// every CI gate — sees 1 and reads it as an ordinary error.
    RefusedWithDifferentCode,
    Skipped(String),
}

#[derive(Debug)]
pub struct Outcome {
    pub id: String,
    pub verdict: Verdict,
    pub zvcs: Option<SideRun>,
    pub stock: Option<SideRun>,
}

fn run_one(bin: &Path, repo: &Path, home: &Path, argv: &[String], logs: &Path) -> Result<SideRun> {
    let mut cmd = Command::new(bin);
    cmd.args(argv)
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
        // A short budget so a port that *waits* for the holder still finishes:
        // the holder here never goes away, so an unbounded wait would read as a
        // hang and tell us nothing about what the command would have done.
        .env("ZVCS_INDEX_LOCK_WAIT_MS", "300")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    run_bounded(cmd, logs, &format!("{} {}", bin.display(), argv.join(" ")))
}

fn prepare(
    bin: &Path,
    repo: &Path,
    home: &Path,
    case: &ForeignLockCase,
    logs: &Path,
) -> Result<()> {
    for step in case.setup {
        let argv: Vec<String> = step.iter().map(|s| (*s).to_string()).collect();
        run_one(bin, repo, home, &argv, logs)?;
    }
    // Planted after setup, never before: a lock held while the fixture is being
    // built would change what the setup itself managed to do, and the case would
    // then be measuring two different repositories.
    let lock = repo.join(".git").join(case.lock);
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&lock, b"held by the parity harness\n")
        .with_context(|| format!("plant {}", lock.display()))?;
    Ok(())
}

pub fn run_foreign_lock_case(
    case: &ForeignLockCase,
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
    let home = &templates.home;
    // Outside the fixtures on purpose: a log file inside a repository would show
    // up as an untracked path and change what `status --porcelain` prints, which
    // is one of the answers this dimension compares.
    let logs = workdir.join("run-output");
    if let Err(e) = std::fs::create_dir_all(&logs) {
        return Outcome { id, verdict: Verdict::Skipped(e.to_string()), zvcs: None, stock: None };
    }

    let mut build = |name: &str, bin: &Path| -> Result<std::path::PathBuf> {
        let repo = workdir.join(name);
        let _ = std::fs::remove_dir_all(&repo);
        templates.instantiate(case.shape, &repo)?;
        // Each side sets its own fixture up with its OWN binary, so a difference
        // in the setup verbs cannot be mistaken for a difference in the verb
        // under test.
        prepare(bin, &repo, home, case, &logs)?;
        Ok(repo)
    };

    let (zvcs_repo, stock_repo) = match (build("fl-zvcs", zvcs_bin), build("fl-stock", stock)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            return Outcome { id, verdict: Verdict::Skipped(e.to_string()), zvcs: None, stock: None }
        }
    };

    let argv: Vec<String> = case.argv.iter().map(|s| (*s).to_string()).collect();
    match (
        run_one(zvcs_bin, &zvcs_repo, home, &argv, &logs),
        run_one(stock, &stock_repo, home, &argv, &logs),
    ) {
        (Ok(z), Ok(s)) => {
            let verdict = match (s.succeeded(), z.succeeded()) {
                (true, false) => Verdict::PortRefusedWhatGitDid,
                (false, true) => Verdict::PortDidMore,
                (false, false) if s.code != z.code => Verdict::RefusedWithDifferentCode,
                _ => Verdict::Agree,
            };
            Outcome { id, verdict, zvcs: Some(z), stock: Some(s) }
        }
        (Err(e), _) | (_, Err(e)) => {
            Outcome { id, verdict: Verdict::Skipped(e.to_string()), zvcs: None, stock: None }
        }
    }
}

/// The curated foreign-lock corpus.
pub fn cases() -> Vec<ForeignLockCase> {
    vec![
        // The measured availability failure: git does not need `packed-refs.lock`
        // to create a loose ref whose name is not already packed, so it succeeds;
        // the port refuses and leaves no ref.
        ForeignLockCase {
            name: "update-ref-creates-loose",
            cmd: "update-ref",
            shape: Shape::Linear,
            lock: "packed-refs.lock",
            setup: &[],
            argv: &["update-ref", "refs/heads/fl-new", "HEAD"],
        },
        // The same claim through the porcelain a person actually types.
        ForeignLockCase {
            name: "branch-creates-loose",
            cmd: "branch",
            shape: Shape::Linear,
            lock: "packed-refs.lock",
            setup: &[],
            argv: &["branch", "fl-branch"],
        },
        // A tag is a third ref namespace reaching the same backend.
        ForeignLockCase {
            name: "tag-creates-loose",
            cmd: "tag",
            shape: Shape::Linear,
            lock: "packed-refs.lock",
            setup: &[],
            argv: &["tag", "fl-tag"],
        },
        // `pack-refs` genuinely needs the lock, so both sides must refuse. This is
        // the case that keeps a fix for the three above from being "stop taking
        // the lock anywhere".
        ForeignLockCase {
            name: "pack-refs-needs-the-lock",
            cmd: "pack-refs",
            shape: Shape::Linear,
            // Something unpacked to pack, so the verb reaches the lock rather than
            // returning early with nothing to do.
            setup: &[&["branch", "fl-topack"]],
            lock: "packed-refs.lock",
            argv: &["pack-refs", "--all"],
        },
        // The index side of the same question. The port may queue or wait here
        // and still succeed — that is the fair-queue feature, recorded as
        // `PortDidMore` rather than scored.
        ForeignLockCase {
            name: "add-under-a-held-index-lock",
            cmd: "add",
            shape: Shape::Dirty,
            lock: "index.lock",
            setup: &[],
            argv: &["add", "README.md"],
        },
        // Reading must never be blocked by a writer's lock, on either side.
        ForeignLockCase {
            name: "status-reads-under-a-held-index-lock",
            cmd: "status",
            shape: Shape::Dirty,
            lock: "index.lock",
            setup: &[],
            argv: &["status", "--porcelain"],
        },

        // ---- index.lock, the rest of the verbs that meet it -------------------
        //
        // Six cases above measured two verbs against this lock. It is the lock
        // every porcelain that touches a file takes, and the exit code it
        // produces is not uniform, so each verb has to be asked separately.
        //
        // Measured on git 2.55.0 with `.git/index.lock` held: 128, and the
        // commit is not made. `commit` is the one where a wrong answer is
        // expensive twice over — a caller that reads 0 goes on to push.
        ForeignLockCase {
            name: "commit-under-a-held-index-lock",
            cmd: "commit",
            shape: Shape::Dirty,
            lock: "index.lock",
            setup: &[],
            argv: &["commit", "-q", "-m", "fl-commit"],
        },
        // Measured: 128. The removal direction of the same lock — a port that
        // guards writes but not un-writes would pass the `add` case and fail here.
        ForeignLockCase {
            name: "rm-cached-under-a-held-index-lock",
            cmd: "rm",
            shape: Shape::Dirty,
            lock: "index.lock",
            setup: &[],
            argv: &["rm", "--cached", "-q", "staged.txt"],
        },
        // Measured: 128. `checkout` rewrites the index wholesale rather than
        // editing entries, which is a different path to the same lock.
        ForeignLockCase {
            name: "checkout-under-a-held-index-lock",
            cmd: "checkout",
            shape: Shape::Branched,
            lock: "index.lock",
            setup: &[],
            argv: &["checkout", "-q", "feature"],
        },
        // Measured: 128. The destructive one: a `reset --hard` that reports
        // success without taking the lock has thrown work away and said it did
        // not, which is the only outcome here worse than refusing.
        ForeignLockCase {
            name: "reset-hard-under-a-held-index-lock",
            cmd: "reset",
            shape: Shape::Dirty,
            lock: "index.lock",
            setup: &[],
            argv: &["reset", "-q", "--hard"],
        },
        // Measured: **1**, not 128 — `stash push` reports the lock failure as an
        // `error:` and exits 1 where every verb above it here exits 128. The case
        // exists for that difference: a port that spells all lock failures alike
        // refuses correctly and still reports the wrong number to a script.
        ForeignLockCase {
            name: "stash-push-under-a-held-index-lock",
            cmd: "stash",
            shape: Shape::Stashed,
            lock: "index.lock",
            setup: &[],
            argv: &["stash", "push", "-q", "-m", "fl-stash"],
        },
        // Two more readers that must not be blocked. Measured: both 0, with the
        // lock held, on stock.
        ForeignLockCase {
            name: "ls-files-reads-under-a-held-index-lock",
            cmd: "ls-files",
            shape: Shape::Dirty,
            lock: "index.lock",
            setup: &[],
            argv: &["ls-files"],
        },
        ForeignLockCase {
            name: "diff-reads-under-a-held-index-lock",
            cmd: "diff",
            shape: Shape::Dirty,
            lock: "index.lock",
            setup: &[],
            argv: &["diff", "--name-only"],
        },

        // ---- HEAD.lock --------------------------------------------------------
        //
        // A lock nothing in this file reached, and the one a killed `commit`
        // leaves behind most often. Measured on git 2.55.0 with `.git/HEAD.lock`
        // held: `commit` and `checkout` both die 128 on `cannot lock ref 'HEAD'`,
        // while `status` and `symbolic-ref HEAD` both answer normally.
        ForeignLockCase {
            name: "commit-under-a-held-head-lock",
            cmd: "commit",
            shape: Shape::Dirty,
            lock: "HEAD.lock",
            setup: &[],
            argv: &["commit", "-q", "-m", "fl-head"],
        },
        ForeignLockCase {
            name: "checkout-under-a-held-head-lock",
            cmd: "checkout",
            shape: Shape::Branched,
            lock: "HEAD.lock",
            setup: &[],
            argv: &["checkout", "-q", "feature"],
        },
        ForeignLockCase {
            name: "status-reads-under-a-held-head-lock",
            cmd: "status",
            shape: Shape::Dirty,
            lock: "HEAD.lock",
            setup: &[],
            argv: &["status", "--porcelain"],
        },
        // Resolving `HEAD` while `HEAD.lock` is held is the narrowest form of the
        // reader question: the lock names the very file being read.
        ForeignLockCase {
            name: "symbolic-ref-reads-under-a-held-head-lock",
            cmd: "symbolic-ref",
            shape: Shape::Linear,
            lock: "HEAD.lock",
            setup: &[],
            argv: &["symbolic-ref", "HEAD"],
        },

        // ---- a single ref's own lock -----------------------------------------
        //
        // The narrowness question. Git locks one ref at a time, so with
        // `.git/refs/heads/main.lock` held: writing `refs/heads/main` dies 128
        // (measured, both through `update-ref` and through `commit`), and writing
        // *any other* ref succeeds (measured 0, both through `update-ref` and
        // through `branch`).
        //
        // The success half is the availability half, and it is the one that
        // matters on the machine this port targets: a stale `<ref>.lock` from one
        // killed process must not stop every other ref in the repository from
        // being written, or a single crash makes the worktree read-only.
        ForeignLockCase {
            name: "update-ref-under-its-own-ref-lock",
            cmd: "update-ref",
            shape: Shape::Linear,
            lock: "refs/heads/main.lock",
            setup: &[],
            argv: &["update-ref", "refs/heads/main", "HEAD"],
        },
        ForeignLockCase {
            name: "commit-under-its-branch-lock",
            cmd: "commit",
            shape: Shape::Dirty,
            lock: "refs/heads/main.lock",
            setup: &[],
            argv: &["commit", "-q", "-m", "fl-branchlock"],
        },
        ForeignLockCase {
            name: "update-ref-other-ref-under-a-held-ref-lock",
            cmd: "update-ref",
            shape: Shape::Linear,
            lock: "refs/heads/main.lock",
            setup: &[],
            argv: &["update-ref", "refs/heads/fl-other", "HEAD"],
        },
        ForeignLockCase {
            name: "branch-under-a-held-ref-lock",
            cmd: "branch",
            shape: Shape::Linear,
            lock: "refs/heads/main.lock",
            setup: &[],
            argv: &["branch", "fl-newb"],
        },
        // Reading the very ref whose lock is held. Measured: 0, with the object
        // id printed — the lock reserves the right to *replace* `refs/heads/main`
        // and says nothing about the value already in it, so a reader that waits
        // for it is waiting for a writer that may never come.
        ForeignLockCase {
            name: "show-ref-reads-under-a-held-ref-lock",
            cmd: "show-ref",
            shape: Shape::Linear,
            lock: "refs/heads/main.lock",
            setup: &[],
            argv: &["show-ref", "--verify", "refs/heads/main"],
        },

        // ---- packed-refs.lock, the deletion half ------------------------------
        //
        // Creating a loose ref does not need this lock (the four cases at the top
        // of the file), but *deleting* one does, because a deletion has to prove
        // the name is not also packed. Measured on git 2.55.0 against a loose
        // `refs/heads/feature`: both spellings exit **1**, not 128 — a third exit
        // code for a lock failure inside one file.
        ForeignLockCase {
            name: "update-ref-delete-under-a-held-packed-refs-lock",
            cmd: "update-ref",
            shape: Shape::Branched,
            lock: "packed-refs.lock",
            setup: &[],
            argv: &["update-ref", "-d", "refs/heads/feature"],
        },
        ForeignLockCase {
            name: "branch-delete-under-a-held-packed-refs-lock",
            cmd: "branch",
            shape: Shape::Branched,
            lock: "packed-refs.lock",
            setup: &[],
            argv: &["branch", "-D", "feature"],
        },
        // The ref readers. `packed-refs` is the file a reader is most likely to
        // catch mid-rewrite, and neither of these may refuse because of it.
        // Measured: both 0.
        ForeignLockCase {
            name: "for-each-ref-reads-under-a-held-packed-refs-lock",
            cmd: "for-each-ref",
            shape: Shape::Branched,
            lock: "packed-refs.lock",
            setup: &[],
            argv: &["for-each-ref", "--format=%(refname)"],
        },
        ForeignLockCase {
            name: "show-ref-reads-under-a-held-packed-refs-lock",
            cmd: "show-ref",
            shape: Shape::Branched,
            lock: "packed-refs.lock",
            setup: &[],
            argv: &["show-ref"],
        },

        // ---- config.lock ------------------------------------------------------
        //
        // A whole-file rewrite behind its own lock, and the only one in this file
        // whose refusal is neither 1 nor 128: measured 255, with `error: could not
        // lock config file .git/config: File exists`. Reading is unaffected
        // (measured 0), which it must be — every single git invocation reads the
        // config, so a port that let this lock block reads could not run at all
        // in a repository somebody was configuring.
        ForeignLockCase {
            name: "config-add-under-a-held-config-lock",
            cmd: "config",
            shape: Shape::Linear,
            lock: "config.lock",
            setup: &[],
            argv: &["config", "--add", "fl.key", "flvalue"],
        },
        ForeignLockCase {
            name: "config-reads-under-a-held-config-lock",
            cmd: "config",
            shape: Shape::Linear,
            lock: "config.lock",
            setup: &[],
            argv: &["config", "--list"],
        },

        // ---- shallow.lock -----------------------------------------------------
        //
        // The lock that only a depth-changing fetch takes. Measured on git 2.55.0
        // with `.git/shallow.lock` held: `fetch --depth=1` dies 128 even in a
        // repository that is not shallow yet — the file it would create is the
        // one being locked — while a plain `fetch`, which never touches
        // `.git/shallow`, completes with 0.
        //
        // The pair is the point. A port that treats any `*.lock` under the git
        // directory as "somebody is writing, refuse" passes the first and fails
        // the second, which turns one stale file into a repository that can never
        // fetch again.
        ForeignLockCase {
            name: "fetch-depth-under-a-held-shallow-lock",
            cmd: "fetch",
            shape: Shape::BehindRemote,
            lock: "shallow.lock",
            setup: &[],
            argv: &["fetch", "-q", "--depth=1", "origin", "main"],
        },
        // The same lock, the same failure, through the porcelain a person types —
        // and a *third* exit code for it: measured **1**, because `pull` reports
        // its child fetch's death as its own ordinary failure rather than passing
        // 128 along. Worth a case of its own precisely because it is the number a
        // `git pull && …` chain reads, and because a port that normalises every
        // lock failure to one code cannot produce both 128 here and 1 there.
        ForeignLockCase {
            name: "pull-depth-under-a-held-shallow-lock",
            cmd: "pull",
            shape: Shape::BehindRemote,
            lock: "shallow.lock",
            setup: &[],
            argv: &["pull", "-q", "--depth=1", "origin", "main"],
        },
        ForeignLockCase {
            name: "plain-fetch-ignores-a-held-shallow-lock",
            cmd: "fetch",
            shape: Shape::BehindRemote,
            lock: "shallow.lock",
            setup: &[],
            argv: &["fetch", "-q", "origin"],
        },

        // ---- a lock file git does not actually use ----------------------------
        //
        // `MERGE_HEAD` is written with a plain create-and-rename, not through the
        // lockfile API, so `.git/MERGE_HEAD.lock` is a file git has no opinion
        // about: measured on git 2.55.0, a three-way merge completes with 0 while
        // it sits there.
        //
        // Which makes this the control for the whole file in the other direction.
        // Every case above rewards honoring a lock; without one that rewards
        // *ignoring* a name that merely looks like a lock, "refuse whenever
        // anything matching `*.lock` exists" would score as a perfect
        // implementation — and a worktree accumulates such files exactly when it
        // is busiest.
        ForeignLockCase {
            name: "merge-ignores-a-held-merge-head-lock",
            cmd: "merge",
            shape: Shape::MergeableDirty,
            lock: "MERGE_HEAD.lock",
            setup: &[],
            argv: &["merge", "--no-edit", "-m", "fl-merge", "div-cold"],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_plants_a_lock_file_name() {
        for case in cases() {
            assert!(
                case.lock.ends_with(".lock"),
                "{}: {} is not a lock file",
                case.name,
                case.lock
            );
            assert!(
                !case.lock.starts_with('/') && !case.lock.contains(".."),
                "{}: {} must stay inside the git directory",
                case.name,
                case.lock
            );
        }
    }

    #[test]
    fn ids_are_unique_and_name_the_lock() {
        let mut seen = std::collections::HashSet::new();
        for case in cases() {
            let id = case.id();
            assert!(id.starts_with("foreign-lock::"), "{id}");
            assert!(id.contains(case.lock), "{id} does not name the lock it plants");
            assert!(seen.insert(id.clone()), "duplicate case id {id}");
        }
    }

    /// The corpus must contain at least one case where git genuinely needs the
    /// lock. Without it, "stop taking the lock" would score as a clean fix for
    /// every remaining case, and this dimension would be actively misleading.
    #[test]
    fn the_corpus_pins_a_case_that_must_still_refuse() {
        assert!(
            cases().iter().any(|c| c.name == "pack-refs-needs-the-lock"),
            "the corpus has no case asserting that a needed lock is still honored"
        );
    }

    /// And the mirror of that test, which the first one cannot supply: a case
    /// where the lock file is one git ignores entirely. Without it, "refuse
    /// whenever anything named `*.lock` exists under the git directory" satisfies
    /// every other case in the file, and this dimension would be endorsing the
    /// implementation that makes a stale lock fatal to the whole repository.
    #[test]
    fn the_corpus_pins_a_lock_that_must_be_ignored() {
        assert!(
            cases().iter().any(|c| c.name == "merge-ignores-a-held-merge-head-lock"),
            "the corpus has no case asserting that a lock git does not take is ignored"
        );
    }

    /// Every lock this file plants must be met by at least one **reader**.
    ///
    /// Reads are what a repository does all day, and a lock is only ever held by
    /// somebody else's write, so a port that let any of these block a read would
    /// be unusable in a worktree with two processes in it — which is the only
    /// worktree this dimension is about. Locks with no reader case are locks
    /// whose availability half is unmeasured.
    #[test]
    fn every_lock_is_met_by_a_reader() {
        const READERS: &[&str] =
            &["status", "ls-files", "diff", "show-ref", "for-each-ref", "symbolic-ref", "config"];
        let mut locks: std::collections::BTreeSet<&str> =
            cases().iter().map(|c| c.lock).collect();
        // `shallow.lock` and `MERGE_HEAD.lock` are met by a *writer* that must
        // still succeed (a plain fetch, a merge), which answers the same question
        // — the lock must not become a blanket refusal — through the only verbs
        // that reach those files at all. No reader touches either one.
        locks.remove("shallow.lock");
        locks.remove("MERGE_HEAD.lock");
        for lock in locks {
            assert!(
                cases()
                    .iter()
                    .any(|c| c.lock == lock && READERS.contains(&c.cmd)),
                "no reader is measured against {lock}"
            );
        }
    }

    /// A lock is worth planting only where some verb is required to *refuse* it
    /// and some other verb is required to *complete* despite it. A lock with only
    /// refusals rewards over-locking; a lock with only successes rewards never
    /// locking. Both halves, per lock, or the lock proves nothing.
    #[test]
    fn contested_locks_have_both_halves() {
        for lock in ["index.lock", "packed-refs.lock", "refs/heads/main.lock", "config.lock"] {
            let n = cases().iter().filter(|c| c.lock == lock).count();
            assert!(n >= 2, "{lock} has {n} case(s), too few to say anything");
        }
    }

    /// A killed run has no exit code, and `succeeded()` has to read that as "did
    /// not do the work" — otherwise a port that hangs forever would be scored as
    /// having completed, which is the opposite of what happened.
    #[test]
    fn a_run_with_no_exit_code_did_not_succeed() {
        let killed = SideRun {
            code: None,
            first_line: "zvcs-parity: still running after 60s, killed".into(),
        };
        assert!(!killed.succeeded());
        // And against a stock run that finished, it is the scored direction.
        let stock = SideRun { code: Some(0), first_line: String::new() };
        assert!(stock.succeeded());
    }
}
