//! The operations that keep their state in root refs — `cherry-pick`, `revert`,
//! `rebase`, `merge`, `am`, `notes merge`, `bisect`, `reset`, `commit`,
//! `status` and `log --merge` — in a repository whose references are stored in
//! reftables, against stock git.
//!
//! git writes `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `REBASE_HEAD`, `ORIG_HEAD`,
//! `AUTO_MERGE`, `MERGE_AUTOSTASH`, `NOTES_MERGE_PARTIAL`, `NOTES_MERGE_REF`,
//! `BISECT_HEAD`, `BISECT_EXPECTED_REV` and `refs/bisect/*` through the ref
//! store (sequencer.c:1715,2509,2513,2535-2538,3029-3047; merge-ort.c:4960;
//! builtin/notes.c:808-811,988-999; bisect.c:742-748,1194-1225), so a reftable
//! repository holds them as records in the worktree's stack; only `MERGE_HEAD`
//! and `FETCH_HEAD` stay files (refs.c:887-899).
//!
//! Every case builds the same repository twice with stock git, runs the same
//! command lines on one copy with stock git and on the other with zvcs, and
//! compares each command's output and exit code, then what stock git reads back
//! from both copies: `refs verify`, `for-each-ref --include-root-refs`, every
//! reflog, and the files of each git directory.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Case {
    root: PathBuf,
    stock: &'static str,
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The repositories a case starts from: `main` at `two` (f=b, g=x) and `side`
/// at `three` (f=c, g=y), both from `one` (f=a, g=x), `main` checked out, so
/// picking, merging or rebasing one onto the other conflicts in `f`.
#[derive(Clone, Copy)]
enum Fixture {
    /// Reftable.
    R,
    /// Reftable, plus the linked worktree `wt` on branch `wtb` (at `side`).
    Rw,
    /// The files format, for the files-backend regression cases.
    RFiles,
}

/// One command line, run in `R` or in the linked worktree `wt`.
type Step<'a> = (&'a str, &'a [&'a str]);

impl Case {
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-reftable-g3-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Some(Case { root, stock })
    }

    fn run(&self, bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
        let out = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_DEFAULT_REF_FORMAT")
            .env_remove("GIT_TEST_REFTABLE_AUTOCOMPACTION")
            .env_remove("GIT_REFLOG_ACTION")
            .env_remove("GIT_CHERRY_PICK_HELP")
            .env("LC_ALL", "C")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap();
        let side = dir.ancestors().find(|p| p.parent() == Some(self.root.as_path())).unwrap().to_owned();
        let norm = |b: &[u8]| String::from_utf8_lossy(b).replace(side.to_str().unwrap(), "ROOT");
        (norm(&out.stdout), norm(&out.stderr), out.status.code())
    }

    fn build(&self, side: &str, fixture: Fixture) -> PathBuf {
        let dir = self.root.join(side);
        let repo = dir.join("R");
        std::fs::create_dir_all(&dir).unwrap();
        let format = match fixture {
            Fixture::RFiles => "--ref-format=files",
            _ => "--ref-format=reftable",
        };
        let git = |args: &[&str]| {
            let (_, err, code) = self.run(self.stock, &dir, args);
            assert_eq!(code, Some(0), "{args:?}: {err}");
        };
        let write = |name: &str, body: &str| std::fs::write(repo.join(name), body).unwrap();
        git(&["init", "-q", "-b", "main", format, "R"]);
        write("f", "a\n");
        write("g", "x\n");
        git(&["-C", "R", "add", "f", "g"]);
        git(&["-C", "R", "commit", "-q", "-m", "one"]);
        git(&["-C", "R", "branch", "side"]);
        write("f", "b\n");
        git(&["-C", "R", "commit", "-q", "-a", "-m", "two"]);
        git(&["-C", "R", "checkout", "-q", "side"]);
        write("f", "c\n");
        write("g", "y\n");
        git(&["-C", "R", "commit", "-q", "-a", "-m", "three"]);
        git(&["-C", "R", "checkout", "-q", "main"]);
        if let Fixture::Rw = fixture {
            git(&["-C", "R", "branch", "wtb", "side"]);
            git(&["-C", "R", "worktree", "add", "-q", "../wt", "wtb"]);
        }
        dir
    }

    /// Stock git's view of every repository and worktree under `side`.
    fn state(&self, side: &Path) -> String {
        let mut out = String::new();
        for name in ["R", "wt"] {
            let dir = side.join(name);
            if !dir.exists() {
                continue;
            }
            out += &format!("== {name}\n");
            for args in [
                &["refs", "verify"][..],
                &["for-each-ref", "--include-root-refs"],
                &["symbolic-ref", "HEAD"],
                &["reflog", "list"],
            ] {
                let (o, e, code) = self.run(self.stock, &dir, args);
                out += &format!("$ {args:?} {code:?}\n{o}{e}");
            }
            let (names, _, _) = self.run(self.stock, &dir, &["reflog", "list"]);
            for name in names.lines() {
                let (o, e, _) =
                    self.run(self.stock, &dir, &["log", "-g", "--date=raw", "--format=%H %gd %gs %gn", name, "--"]);
                out += &format!("-- {name}\n{o}{e}");
            }
            let (git_dir, _, _) = self.run(self.stock, &dir, &["rev-parse", "--absolute-git-dir"]);
            let git_dir = PathBuf::from(git_dir.trim().replace("ROOT", side.to_str().unwrap()));
            let mut files = Vec::new();
            list_files(&git_dir, &git_dir, &mut files);
            files.sort();
            // A linked worktree's `gitdir` names its side's absolute path.
            let real = std::fs::canonicalize(side).unwrap();
            out += &files.join("\n").replace(real.to_str().unwrap(), "ROOT");
            out.push('\n');
        }
        out
    }

    /// Run `steps` on `fixture` with stock git and with zvcs, and assert every
    /// step's output and exit code, and both resulting repositories, look the
    /// same to stock git.
    fn compare(&self, fixture: Fixture, steps: &[Step<'_>]) {
        self.compare_after(fixture, |_| {}, steps);
    }

    /// [`Case::compare`] with `prepare` run on each copy's `R` first.
    fn compare_after(&self, fixture: Fixture, prepare: impl Fn(&Path), steps: &[Step<'_>]) {
        let stock_side = self.build("s", fixture);
        let zvcs_side = self.build("z", fixture);
        prepare(&stock_side.join("R"));
        prepare(&zvcs_side.join("R"));
        for (cwd, args) in steps {
            let expected = self.run(self.stock, &stock_side.join(cwd), args);
            let actual = self.run(BIN, &zvcs_side.join(cwd), args);
            assert_eq!(actual, expected, "{args:?}: output and exit code");
        }
        assert_eq!(self.state(&zvcs_side), self.state(&stock_side), "{steps:?}: state");
        std::fs::remove_dir_all(&stock_side).unwrap();
        std::fs::remove_dir_all(&zvcs_side).unwrap();
    }
}

/// The files of a git directory other than objects, hooks, the index and the
/// reftable tables (whose names are random), each with its first bytes; the
/// linked worktrees' directories are listed from their own side.
fn list_files(base: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if ["objects", "hooks", "index", "description", "COMMIT_EDITMSG", "exclude", "worktrees"]
            .contains(&name.as_str())
            || name.ends_with(".ref")
        {
            continue;
        }
        if path.is_dir() {
            out.push(format!("{}/", path.strip_prefix(base).unwrap().display()));
            list_files(base, &path, out);
        } else if name == "tables.list" {
            let lines = std::fs::read_to_string(&path).unwrap().lines().count();
            out.push(format!("{}: {lines} tables", path.strip_prefix(base).unwrap().display()));
        } else {
            let text = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&text[..text.len().min(120)]).replace('\n', "|");
            out.push(format!("{}: {text}", path.strip_prefix(base).unwrap().display()));
        }
    }
}

/// A stopped pick leaves `CHERRY_PICK_HEAD` and the merge result's `AUTO_MERGE`
/// as records (sequencer.c:2509; merge-ort.c:4960), `status` reports the pick
/// from them, and `--abort`, `--continue` and `--quit` delete them again.
#[test]
fn cherry_pick_state_refs() {
    let Some(case) = Case::new("cherry-pick") else { return };
    let pick: Step = ("R", &["cherry-pick", "side"]);
    case.compare(Fixture::R, &[pick]);
    case.compare(Fixture::R, &[pick, ("R", &["status"]), ("R", &["cherry-pick", "--abort"])]);
    case.compare(
        Fixture::R,
        &[pick, ("R", &["add", "f"]), ("R", &["cherry-pick", "--continue"]), ("R", &["log", "--oneline"])],
    );
    case.compare(Fixture::R, &[pick, ("R", &["cherry-pick", "--quit"]), ("R", &["status", "--short"])]);
    case.compare(Fixture::R, &[("R", &["cherry-pick", "-n", "side"])]);
    // A clean pick keeps its `AUTO_MERGE`.
    case.compare(Fixture::R, &[("R", &["cherry-pick", "side~1..side", "--allow-empty"])]);
}

/// `REVERT_HEAD` on a stopped revert, a sequence stopped on its second pick,
/// and the commands that conclude or forget them.
#[test]
fn revert_state_refs() {
    let Some(case) = Case::new("revert") else { return };
    case.compare(Fixture::R, &[("R", &["revert", "--no-edit", "HEAD~1"]), ("R", &["status"])]);
    case.compare(
        Fixture::R,
        &[("R", &["revert", "--no-edit", "HEAD", "HEAD~1"]), ("R", &["revert", "--skip"]), ("R", &["log", "--oneline"])],
    );
    case.compare(Fixture::R, &[("R", &["revert", "--no-edit", "HEAD~1"]), ("R", &["revert", "--quit"])]);
    case.compare(Fixture::R, &[("R", &["revert", "-n", "HEAD"]), ("R", &["commit", "-q", "-m", "r"])]);
}

/// `rebase` writes `ORIG_HEAD`, `REBASE_HEAD` and `AUTO_MERGE` through the
/// store and its `HEAD` entries come from the store's own logging: no
/// hand-written line may double them.
#[test]
fn rebase_state_refs_and_head_log() {
    let Some(case) = Case::new("rebase") else { return };
    let start: Step = ("R", &["rebase", "side"]);
    case.compare(Fixture::R, &[start, ("R", &["status"]), ("R", &["rebase", "--abort"])]);
    case.compare(Fixture::R, &[start, ("R", &["rebase", "--skip"])]);
    case.compare(Fixture::R, &[start, ("R", &["add", "f"]), ("R", &["rebase", "--continue"])]);
    case.compare(Fixture::R, &[("R", &["rebase", "--apply", "side"]), ("R", &["rebase", "--abort"])]);
    case.compare(Fixture::R, &[("R", &["rebase", "-i", "side"]), ("R", &["rebase", "--quit"])]);
    // The up-to-date path switches to the named branch: two `HEAD` entries.
    case.compare(Fixture::R, &[("R", &["rebase", "main~1", "main"])]);
    // Rebasing a branch other than the one the fixture leaves checked out;
    // stock git switches to it, so only the rebase is the command under test.
    let on_side = |repo: &Path| {
        let (_, err, code) = case.run(case.stock, repo, &["checkout", "-q", "side"]);
        assert_eq!(code, Some(0), "{err}");
    };
    case.compare_after(Fixture::R, on_side, &[("R", &["rebase", "main"])]);
    // The same with zvcs switching branches first: `checkout` must log through
    // the store too, or a files-format `logs/HEAD` appears.
    case.compare(Fixture::R, &[("R", &["checkout", "-q", "side"]), ("R", &["rebase", "main"])]);
}

/// `merge` keeps `MERGE_HEAD` a file but `AUTO_MERGE`, `ORIG_HEAD` and
/// `MERGE_AUTOSTASH` in the store; `--abort`'s `reset --merge` logs `HEAD`
/// through it.
#[test]
fn merge_state_refs() {
    let Some(case) = Case::new("merge") else { return };
    let merge: Step = ("R", &["merge", "side"]);
    case.compare(Fixture::R, &[merge, ("R", &["log", "--merge", "--oneline"])]);
    case.compare(Fixture::R, &[merge, ("R", &["merge", "--abort"])]);
    case.compare(Fixture::R, &[merge, ("R", &["add", "f"]), ("R", &["commit", "--no-edit", "-q"])]);
    case.compare(Fixture::R, &[merge, ("R", &["merge", "--quit"])]);
    case.compare(Fixture::R, &[merge, ("R", &["reset", "--hard"])]);
}

/// The autostash of a merge lives under `MERGE_AUTOSTASH` until the merge is
/// concluded or aborted.
#[test]
fn merge_autostash_ref() {
    let Some(case) = Case::new("autostash") else { return };
    let dirty = |repo: &Path| std::fs::write(repo.join("g"), "dirty\n").unwrap();
    let merge: Step = ("R", &["merge", "--autostash", "side"]);
    case.compare_after(Fixture::R, dirty, &[merge, ("R", &["rev-parse", "MERGE_AUTOSTASH"])]);
    case.compare_after(Fixture::R, dirty, &[merge, ("R", &["merge", "--abort"]), ("R", &["stash", "list"])]);
    case.compare_after(
        Fixture::R,
        dirty,
        &[merge, ("R", &["add", "f"]), ("R", &["commit", "--no-edit", "-q"]), ("R", &["stash", "list"])],
    );
}

/// `notes merge` with a conflict stores `NOTES_MERGE_PARTIAL` and the symbolic
/// `NOTES_MERGE_REF` in the store (builtin/notes.c:988-999); `--commit` and
/// `--abort` read and delete them there.
#[test]
fn notes_merge_refs() {
    let Some(case) = Case::new("notes") else { return };
    let setup: [Step; 3] = [
        ("R", &["notes", "add", "-m", "a", "HEAD"]),
        ("R", &["notes", "--ref=other", "add", "-m", "b", "HEAD"]),
        ("R", &["notes", "merge", "other"]),
    ];
    case.compare(Fixture::R, &setup);
    let mut steps = setup.to_vec();
    steps.push(("R", &["notes", "merge", "--abort"]));
    case.compare(Fixture::R, &steps);
    let mut steps = setup.to_vec();
    steps.push(("R", &["notes", "merge", "--commit"]));
    steps.push(("R", &["notes", "show", "HEAD"]));
    case.compare(Fixture::R, &steps);
}

/// `bisect` keeps `refs/bisect/*`, `BISECT_EXPECTED_REV` and, with
/// `--no-checkout`, `BISECT_HEAD` in the worktree's stack, and `reset` deletes
/// them in one transaction (bisect.c:1194-1225), measured both on a
/// `--no-checkout` session and after a checked-out one, where it also runs
/// `git checkout` back to the starting branch.
#[test]
fn bisect_refs() {
    let Some(case) = Case::new("bisect") else { return };
    let start: Step = ("R", &["bisect", "start", "side", "side~1"]);
    case.compare(Fixture::R, &[start]);
    case.compare(Fixture::R, &[start, ("R", &["bisect", "good"]), ("R", &["bisect", "log"])]);
    case.compare(
        Fixture::R,
        &[
            ("R", &["bisect", "start", "--no-checkout", "side", "side~1"]),
            ("R", &["rev-parse", "BISECT_HEAD"]),
            ("R", &["bisect", "reset"]),
        ],
    );
    case.compare(Fixture::R, &[start, ("R", &["bisect", "run", "test", "-f", "g"])]);
    // `reset` after a checked-out session, its `git checkout` back to `main`
    // included.
    case.compare(Fixture::R, &[start, ("R", &["bisect", "reset"])]);
}

/// `am` and `reset` move `ORIG_HEAD` and `REBASE_HEAD` through the store, and
/// `am --skip`'s `remove_branch_state()` drops the fallback's `AUTO_MERGE`.
#[test]
fn am_and_reset_refs() {
    let Some(case) = Case::new("am") else { return };
    let patch = ("R", &["format-patch", "-q", "-1", "side", "-o", "p"][..]);
    case.compare(Fixture::R, &[patch, ("R", &["am", "-3", "p/0001-three.patch"]), ("R", &["am", "--skip"])]);
    case.compare(Fixture::R, &[patch, ("R", &["am", "-3", "p/0001-three.patch"]), ("R", &["am", "--abort"])]);
    case.compare(Fixture::R, &[("R", &["reset", "--hard", "HEAD~1"]), ("R", &["reset", "--keep", "ORIG_HEAD"])]);
}

/// In a linked worktree the state refs go to that worktree's own stack: the
/// main worktree sees none of them, and the worktree's `HEAD` log is written
/// once per update.
#[test]
fn linked_worktree_state_refs() {
    let Some(case) = Case::new("worktree") else { return };
    case.compare(Fixture::Rw, &[("wt", &["cherry-pick", "main"]), ("R", &["status", "--short"])]);
    case.compare(Fixture::Rw, &[("wt", &["rebase", "main"]), ("wt", &["rebase", "--abort"])]);
    case.compare(Fixture::Rw, &[("wt", &["merge", "main"]), ("wt", &["merge", "--abort"])]);
    case.compare(Fixture::Rw, &[("wt", &["bisect", "start", "HEAD", "HEAD~1"]), ("R", &["for-each-ref"])]);
}

/// The files backend keeps writing these refs as loose files.
#[test]
fn files_backend_unchanged() {
    let Some(case) = Case::new("files") else { return };
    case.compare(Fixture::RFiles, &[("R", &["cherry-pick", "side"])]);
    case.compare(Fixture::RFiles, &[("R", &["rebase", "side"]), ("R", &["rebase", "--abort"])]);
    case.compare(Fixture::RFiles, &[("R", &["merge", "side"]), ("R", &["merge", "--abort"])]);
    case.compare(Fixture::RFiles, &[("R", &["bisect", "start", "--no-checkout", "side", "side~1"])]);
    case.compare(
        Fixture::RFiles,
        &[
            ("R", &["notes", "add", "-m", "a", "HEAD"]),
            ("R", &["notes", "--ref=other", "add", "-m", "b", "HEAD"]),
            ("R", &["notes", "merge", "other"]),
        ],
    );
}
