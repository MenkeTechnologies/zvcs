//! A conflicted path whose stages mix blobs and gitlinks, over a submodule checkout.
//!
//! `run_diff_files()` queues `diff_unmerge()`'s pair with `two->mode` from
//! `check_removed()` and `ce_mode_from_stat()` of the first stage, then compares the
//! stage-2 entry as an ordinary one (diff-lib.c:154-310): a gitlink goes through
//! `match_stat_with_submodule()` and `ce_compare_gitlink()` (diff-lib.c:88-107,
//! read-cache.c:268-332), so a clean checkout at the stage commit queues nothing and
//! a dirty one keeps the stage id on both sides, and a blob over a repository is a
//! type change. zvcs printed every stage-2 pair as `M` with a null post-image, and
//! the `U` pair of an unborn checkout with mode 000000.
//!
//! `submodule summary --files` is that same `diff-files` under
//! `--ignore-submodules=dirty` (builtin/submodule--helper.c:1228-1273), and long
//! `status` prints it as "Submodules changed but not updated"; zvcs only ever
//! summarised the `U` pair.
//!
//! `status` with `core.ignorecase` panicked in the dirwalk whenever the index held a
//! blob and a gitlink at different stages of one path: the case-insensitive
//! directory lookup took the gitlink's own name for a directory with entries below
//! it, which `index_dir_exists()` never does (name-hash.c:61-78, dir.c:1888-1901).
//!
//! Each case runs stock git and zvcs on identical fixtures and compares stdout,
//! stderr and the exit status.

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// What `s0/` holds when the command runs.
#[derive(Clone, Copy, Debug)]
enum Sub {
    /// Checked out at commit `two`, clean.
    Clean,
    /// At `two`, with an untracked file.
    Untracked,
    /// At `two`, with a tracked file changed.
    Modified,
    /// At `one`.
    OtherCommit,
    /// A fresh repository with an unborn branch.
    Unborn,
    /// The directory is gone.
    Removed,
    /// A regular file took its place.
    File,
}

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Two identical repositories, `stock/` and `zvcs/`: a committed `f`, a
    /// submodule `s0` with commits `one` and `two`, and the conflict `stages` for
    /// `s0`. `1`, `2`, `3` are gitlinks at that stage (`two` at stage 2, `one`
    /// elsewhere); `b`, `B`, `T` are blobs at stage 1, 2, 3.
    fn new(tag: &str, stages: &str, sub: Sub) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-files-unmerged-gitlink-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        for side in ["stock", "zvcs"] {
            let work = f.root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            f.git(&work, &["init", "-q", "-b", "main", "."]);
            std::fs::write(work.join("f"), "x\n").unwrap();
            f.git(&work, &["add", "f"]);
            f.git(&work, &["commit", "-q", "-m", "base"]);
            let s0 = work.join("s0");
            std::fs::create_dir_all(&s0).unwrap();
            f.git(&s0, &["init", "-q", "-b", "main", "."]);
            std::fs::write(s0.join("a"), "a\n").unwrap();
            f.git(&s0, &["add", "a"]);
            f.git(&s0, &["commit", "-q", "-m", "one"]);
            std::fs::write(s0.join("a"), "b\n").unwrap();
            f.git(&s0, &["commit", "-q", "-a", "-m", "two"]);
            let two = f.git(&s0, &["rev-parse", "HEAD"]).trim().to_string();
            let one = f.git(&s0, &["rev-parse", "HEAD~1"]).trim().to_string();
            let mut info = String::new();
            for st in stages.split_whitespace() {
                let line = match st {
                    "1" | "3" => format!("160000 {one} {st}\ts0\n"),
                    "2" => format!("160000 {two} 2\ts0\n"),
                    "b" | "B" | "T" => {
                        let stage = match st {
                            "b" => 1,
                            "B" => 2,
                            _ => 3,
                        };
                        let blob = f.git_stdin(&work, &["hash-object", "-w", "--stdin"], st);
                        format!("100644 {} {stage}\ts0\n", blob.trim())
                    }
                    other => panic!("unknown stage token {other}"),
                };
                info.push_str(&line);
            }
            f.git_stdin(&work, &["update-index", "--index-info"], &info);
            match sub {
                Sub::Clean => {}
                Sub::Untracked => std::fs::write(s0.join("u"), "u\n").unwrap(),
                Sub::Modified => std::fs::write(s0.join("a"), "m\n").unwrap(),
                Sub::OtherCommit => {
                    f.git(&s0, &["checkout", "-q", "HEAD~1"]);
                }
                Sub::Unborn => {
                    std::fs::remove_dir_all(&s0).unwrap();
                    std::fs::create_dir_all(&s0).unwrap();
                    f.git(&s0, &["init", "-q"]);
                }
                Sub::Removed => std::fs::remove_dir_all(&s0).unwrap(),
                Sub::File => {
                    std::fs::remove_dir_all(&s0).unwrap();
                    std::fs::write(&s0, "f\n").unwrap();
                }
            }
        }
        f
    }

    fn home(&self) -> PathBuf {
        let home = self.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let (out, err, code) = run(STOCK, dir, &self.home(), args);
        assert_eq!(code, 0, "stock git {args:?} failed: {err}");
        out
    }

    fn git_stdin(&self, dir: &Path, args: &[&str], input: &str) -> String {
        use std::io::Write;
        let mut child = Command::new(STOCK)
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn stock git");
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "stock git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn compare(&self, args: &[&str]) -> (String, String, i32) {
        let home = self.home();
        let stock = run(STOCK, &self.root.join("stock"), &home, args);
        let zvcs = run(ZVCS, &self.root.join("zvcs"), &home, args);
        assert_eq!(stock, zvcs, "git {args:?} differs (left stock, right zvcs)");
        stock
    }
}

#[test]
fn mixed_blob_and_gitlink_stages_do_not_panic_status() {
    if !stock_available() {
        return;
    }
    for (tag, stages) in [("bb3", "b B 3"), ("b3", "B 3"), ("b2t", "b 2 T")] {
        let f = Fixture::new(&format!("status-{tag}"), stages, Sub::Clean);
        for icase in ["core.ignorecase=true", "core.ignorecase=false"] {
            let (out, _, code) = f.compare(&["-c", icase, "status", "--porcelain=2"]);
            assert_eq!(code, 0, "{tag}: {out}");
            assert!(out.starts_with("u "), "{tag}: {out}");
            f.compare(&["-c", icase, "status", "--short", "-uall", "--ignored"]);
            f.compare(&["-c", icase, "status"]);
        }
    }
}

#[test]
fn diff_files_compares_the_stage_two_gitlink_as_an_ordinary_entry() {
    if !stock_available() {
        return;
    }
    let subs = [
        ("clean", Sub::Clean),
        ("untracked", Sub::Untracked),
        ("modified", Sub::Modified),
        ("other", Sub::OtherCommit),
        ("unborn", Sub::Unborn),
        ("removed", Sub::Removed),
        ("file", Sub::File),
    ];
    for (stages_tag, stages) in [("uu", "1 2 3"), ("aa", "2 3"), ("ud", "1 2")] {
        for (sub_tag, sub) in subs {
            let f = Fixture::new(&format!("raw-{stages_tag}-{sub_tag}"), stages, sub);
            f.compare(&["diff-files"]);
            f.compare(&["diff-files", "--ignore-submodules=dirty"]);
            f.compare(&["diff-files", "--ignore-submodules=all"]);
            f.compare(&["diff-files", "-3"]);
        }
    }
}

#[test]
fn diff_files_types_a_stage_two_blob_over_a_repository() {
    if !stock_available() {
        return;
    }
    for (tag, stages) in [("bb3", "b B 3"), ("b3", "B 3"), ("b2t", "b 2 T")] {
        for (sub_tag, sub) in [("clean", Sub::Clean), ("removed", Sub::Removed), ("file", Sub::File)] {
            let f = Fixture::new(&format!("type-{tag}-{sub_tag}"), stages, sub);
            f.compare(&["diff-files"]);
            f.compare(&["diff-files", "-1"]);
        }
    }
}

#[test]
fn submodule_summary_files_and_long_status_list_the_stage_two_pair() {
    if !stock_available() {
        return;
    }
    let cases = [
        ("uu-other", "1 2 3", Sub::OtherCommit),
        ("uu-removed", "1 2 3", Sub::Removed),
        ("uu-file", "1 2 3", Sub::File),
        ("uu-clean", "1 2 3", Sub::Clean),
        ("bb3-clean", "b B 3", Sub::Clean),
        ("bb3-other", "b B 3", Sub::OtherCommit),
        ("b3-clean", "B 3", Sub::Clean),
        ("b2t-other", "b 2 T", Sub::OtherCommit),
        ("b2t-removed", "b 2 T", Sub::Removed),
    ];
    for (tag, stages, sub) in cases {
        let f = Fixture::new(&format!("summary-{tag}"), stages, sub);
        f.compare(&["submodule", "summary", "--files"]);
        f.compare(&["-c", "status.submoduleSummary=true", "status"]);
    }
}
