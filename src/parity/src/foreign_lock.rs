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

use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::fixture::{Shape, Templates};

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

fn run_one(bin: &Path, repo: &Path, home: &Path, argv: &[String]) -> Result<SideRun> {
    let out = Command::new(bin)
        .args(argv)
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
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run {}", bin.display()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(SideRun {
        code: out.status.code(),
        first_line: text.lines().next().unwrap_or("").to_string(),
    })
}

fn prepare(bin: &Path, repo: &Path, home: &Path, case: &ForeignLockCase) -> Result<()> {
    for step in case.setup {
        let argv: Vec<String> = step.iter().map(|s| (*s).to_string()).collect();
        run_one(bin, repo, home, &argv)?;
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

    let mut build = |name: &str, bin: &Path| -> Result<std::path::PathBuf> {
        let repo = workdir.join(name);
        let _ = std::fs::remove_dir_all(&repo);
        templates.instantiate(case.shape, &repo)?;
        // Each side sets its own fixture up with its OWN binary, so a difference
        // in the setup verbs cannot be mistaken for a difference in the verb
        // under test.
        prepare(bin, &repo, home, case)?;
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
        run_one(zvcs_bin, &zvcs_repo, home, &argv),
        run_one(stock, &stock_repo, home, &argv),
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
}
