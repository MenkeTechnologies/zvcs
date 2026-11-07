//! `git bisect` against stock git on identical fixtures: the `RUN_SETUP` cwd, the
//! `bisect_checkout()` child, the `bisect_skipped_commits()` walk, skipped merge bases,
//! and the operand checks of `start`, `reset` and `replay`.
//!
//! * `{ "bisect", cmd_bisect, RUN_SETUP }` (git.c) moves the process to the top of the
//!   work tree, so `bisect run` starts its command there, and a step that checks out a
//!   commit without the subdirectory the command was typed in still works.
//! * `bisect_checkout()` runs `git checkout -q <hex> --` as a child (bisect.c:753-754),
//!   so from inside `.git` the child's `this operation must be run in a work tree`
//!   fails the step and `bisect_start()` cleans the session away.
//! * `bisect_skipped_commits()` (builtin/bisect.c:701-741) lists `<bad> ^<good>…` with
//!   no first-parent restriction, over the parents `try_to_simplify_commit()` pruned
//!   in place during the step's own pathspec walk.
//! * `check_merge_bases()` warns about a skipped merge base and goes on
//!   (bisect.c:831-844, :875-876).
//! * `bisect start <tree>` reports `peel_onion()`'s type error; `bisect reset <tree>`
//!   passes `repo_get_oid_commit()` and is refused by the `checkout` child; `replay`
//!   takes exactly one operand; a replayed log without `start` stops at
//!   `bisect_next()`'s `bisect_autostart()`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// `(exit, stdout, stderr)` with the side's root replaced by `<root>`.
type Out = (i32, String, String);

struct World {
    root: PathBuf,
    bin: String,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl World {
    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Out {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", self.root.parent().unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_PAGER", "cat")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("TZ", "UTC")
            .env("LC_ALL", "C");
        let out = cmd.output().unwrap();
        let root = self.root.to_string_lossy().into_owned();
        let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
        (out.status.code().unwrap_or(-1), text(&out.stdout), text(&out.stderr))
    }

    fn git(&self, args: &[&str]) -> Out {
        self.run_in(&self.repo(), args)
    }

    /// The `BISECT_*` files with their contents, and `HEAD`.
    fn state(&self) -> Vec<(String, String)> {
        let git_dir = self.repo().join(".git");
        let mut files: Vec<(String, String)> = std::fs::read_dir(&git_dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("BISECT_") || n == "HEAD")
            .map(|n| {
                let body = std::fs::read_to_string(git_dir.join(&n)).unwrap_or_default();
                (n, body)
            })
            .collect();
        files.sort();
        files
    }
}

/// The template repository, built with stock git; each commit is a minute after the last.
struct Builder {
    world: World,
    date: u64,
}

impl Builder {
    fn git_dated(&mut self, args: &[&str]) {
        self.date += 60;
        let stamp = format!("{} +0000", self.date);
        let st = Command::new(&self.world.bin)
            .args(["-c", "user.name=T", "-c", "user.email=t@e"])
            .args(args)
            .current_dir(self.world.repo())
            .env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", &stamp)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.world.root)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "{args:?}");
    }

    /// Write `files`, commit everything as `name` and tag it `name`.
    fn commit(&mut self, name: &str, files: &[(&str, String)]) {
        for (path, body) in files {
            let p = self.world.repo().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        assert_eq!(self.world.git(&["add", "-A"]).0, 0);
        self.git_dated(&["commit", "-q", "-m", name]);
        assert_eq!(self.world.git(&["tag", name]).0, 0);
    }

    fn merge(&mut self, branch: &str, name: &str) {
        self.git_dated(&["merge", "-q", "--no-edit", "-m", name, branch]);
        assert_eq!(self.world.git(&["tag", name]).0, 0);
    }

    fn checkout(&self, args: &[&str]) {
        let mut full = vec!["checkout", "-q"];
        full.extend_from_slice(args);
        assert_eq!(self.world.git(&full).0, 0, "{args:?}");
    }
}

/// Build `shape` with stock git once, then copy it into a stock world and a zvcs world.
fn twins(label: &str, stock: &str, shape: &str) -> (World, World) {
    let base = std::env::temp_dir().join(format!("zvcs-bisect-setup-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let base = base.canonicalize().unwrap();
    let mut b = Builder { world: World { root: base.join("template"), bin: stock.to_owned() }, date: 1_700_000_000 };
    std::fs::create_dir_all(b.world.repo()).unwrap();
    assert_eq!(b.world.git(&["init", "-q", "-b", "main"]).0, 0);
    let line = |s: &str| format!("{s}\n");
    match shape {
        // c0..c15 rewrite `f`; `a/x` appears at c3, `b/y` at c5.
        "linear" => {
            for i in 0..=15 {
                let n = line(&i.to_string());
                let mut files = vec![("f", n.clone())];
                if i == 3 || i == 9 {
                    files.push(("a/x", n.clone()));
                }
                if i == 5 || i == 11 {
                    files.push(("b/y", n.clone()));
                }
                b.commit(&format!("c{i}"), &files);
            }
        }
        // main c0..c3, side s1..s3 off c1 merged as m1, c4..c7, side2 t1..t2 off c5
        // merged as m2, c8..c11.
        "merges" => {
            for i in 0..=3 {
                b.commit(&format!("c{i}"), &[("f", line(&i.to_string()))]);
            }
            b.checkout(&["-b", "side", "c1"]);
            for i in 1..=3 {
                b.commit(&format!("s{i}"), &[("s", line(&format!("s{i}")))]);
            }
            b.checkout(&["main"]);
            b.merge("side", "m1");
            for i in 4..=7 {
                b.commit(&format!("c{i}"), &[("f", line(&i.to_string()))]);
            }
            b.checkout(&["-b", "side2", "c5"]);
            for i in 1..=2 {
                b.commit(&format!("t{i}"), &[("t", line(&format!("t{i}")))]);
            }
            b.checkout(&["main"]);
            b.merge("side2", "m2");
            for i in 8..=11 {
                b.commit(&format!("c{i}"), &[("f", line(&i.to_string()))]);
            }
        }
        // c0..c10 on main, o1..o3 off c3 on `other`.
        "forked" => {
            for i in 0..=10 {
                b.commit(&format!("c{i}"), &[("f", line(&i.to_string()))]);
            }
            b.checkout(&["-b", "other", "c3"]);
            for i in 1..=3 {
                b.commit(&format!("o{i}"), &[("o", line(&format!("o{i}")))]);
            }
            b.checkout(&["main"]);
        }
        _ => unreachable!(),
    }
    let template = b.world;
    let side = |name: &str, bin: &str| {
        let root = base.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let st = Command::new("cp").arg("-R").arg(template.repo()).arg(root.join("repo")).status().unwrap();
        assert!(st.success());
        World { root, bin: bin.to_owned() }
    };
    (side("stock", stock), side("zvcs", ZVCS))
}

/// Run every step on both worlds, comparing the answers and the session state after each.
fn compare(label: &str, shape: &str, steps: &[(&str, &[&str])]) {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let (s, z) = twins(label, stock, shape);
    for (dir, args) in steps {
        let (want, got) = (s.run_in(&s.repo().join(dir), args), z.run_in(&z.repo().join(dir), args));
        assert_eq!(got, want, "{label}: {dir} {args:?}");
        assert_eq!(z.state(), s.state(), "{label}: state after {dir} {args:?}");
    }
    let status = |w: &World| w.git(&["status", "--porcelain=v2", "--branch"]);
    assert_eq!(status(&z), status(&s), "{label}: status");
}

#[test]
fn run_starts_its_command_at_the_top_of_the_work_tree() {
    compare(
        "run-top",
        "linear",
        &[
            ("a", &["bisect", "start", "c15", "c4"]),
            ("a", &["bisect", "run", "sh", "-c", "pwd; test $(cat f) -lt 8"]),
            ("a", &["bisect", "log"]),
        ],
    );
}

#[test]
fn a_step_that_removes_the_subdirectory_still_checks_out() {
    compare(
        "run-subdir-gone",
        "linear",
        &[
            ("a", &["bisect", "start", "c15", "c0"]),
            ("a", &["bisect", "run", "/nonexistent-bisect-script.sh"]),
            ("", &["bisect", "log"]),
        ],
    );
    compare(
        "mark-subdir-gone",
        "linear",
        &[("a", &["bisect", "start", "c15", "c0"]), ("a", &["bisect", "good", "c0"]), ("a", &["bisect", "bad", "c1"])],
    );
}

#[test]
fn a_step_from_inside_the_git_dir_fails_in_the_checkout_child() {
    compare(
        "inside-git-dir",
        "linear",
        &[(".git", &["bisect", "start", "c15", "c0"]), (".git", &["bisect", "log"])],
    );
}

#[test]
fn only_skipped_left_lists_the_whole_range_under_first_parent() {
    compare(
        "skipped-first-parent",
        "merges",
        &[
            ("", &["bisect", "start", "--first-parent", "c11", "c0"]),
            ("", &["bisect", "run", "sh", "-c", "exit 125"]),
            ("", &["bisect", "log"]),
        ],
    );
}

#[test]
fn only_skipped_left_follows_the_parents_the_pathspec_pruned() {
    compare(
        "skipped-pathspec",
        "merges",
        &[
            ("", &["bisect", "start", "c11", "c0", "--", "b", "f"]),
            ("", &["bisect", "run", "sh", "-c", "exit 125"]),
            ("", &["bisect", "log"]),
        ],
    );
}

#[test]
fn a_skipped_merge_base_is_warned_about_and_passed() {
    compare(
        "skipped-merge-base",
        "forked",
        &[("", &["bisect", "start", "c10", "o3"]), ("", &["bisect", "skip"]), ("", &["bisect", "log"])],
    );
    compare(
        "skipped-merge-base-two-goods",
        "forked",
        &[("", &["bisect", "start", "c10", "o3", "o1"]), ("", &["bisect", "skip"])],
    );
}

#[test]
fn start_reports_the_peel_error_of_a_tree_operand() {
    compare("start-tree", "linear", &[("", &["bisect", "start", "c15^{tree}", "c0"])]);
    compare("start-tree-dashdash", "linear", &[("", &["bisect", "start", "c15^{tree}", "c0", "--"])]);
    compare("start-blob-peel", "linear", &[("", &["bisect", "start", "c15^{blob}", "c0"])]);
}

#[test]
fn reset_to_a_tree_or_blob_is_refused_by_checkout() {
    compare(
        "reset-tree",
        "linear",
        &[("", &["bisect", "start", "c15", "c0"]), ("", &["bisect", "reset", "c3^{tree}"])],
    );
    compare(
        "reset-blob",
        "linear",
        &[("", &["bisect", "start", "c15", "c0"]), ("", &["bisect", "reset", "c3:f"])],
    );
}

#[test]
fn replay_takes_exactly_one_log_and_needs_a_start() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let (s, z) = twins("replay", stock, "linear");
    for w in [&s, &z] {
        std::fs::write(w.root.join("plain.log"), "git bisect start 'c15' 'c0'\ngit bisect good c7\n").unwrap();
        std::fs::write(w.root.join("nostart.log"), "git bisect good c5\ngit bisect bad c10\n").unwrap();
    }
    for args in [
        &["bisect", "replay", "../plain.log", "../plain.log"][..],
        &["bisect", "replay", "../nostart.log"],
        &["bisect", "log"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        assert_eq!(z.state(), s.state(), "state after {args:?}");
    }
}
