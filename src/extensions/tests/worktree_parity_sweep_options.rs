//! `git worktree` against stock git on twin fixtures: the long-option half of each
//! subcommand's `parse_options()`, and the `add`/`lock`/`remove`/`list`/`repair`
//! behaviour a differential sweep found apart from stock 2.56.0.
//!
//! * Long options resolve by unique abbreviation (`--det`, `--porc`, `--for`), an
//!   ambiguous one is refused with both names (parse-options.c:519-594), every
//!   `OPT_BOOL`/`OPT_STRING` has its `--no-` form, and `--` or `--end-of-options`
//!   ends the options of every subcommand (parse-options.c:1112-1124).
//! * `lock --no-reason` writes `(null)` (worktree.c:1183, :1211), `add --lock
//!   --reason ""` an empty file (`write_file()`, wrapper.c:769-782).
//! * `add -f -B <branch held elsewhere>` is refused by the child `git branch --force`
//!   (branch.c:515-518) at 255; `add <path> -` is `@{-1}` and checks that branch out
//!   (worktree.c:891-892, refs.c:761-781); a non-commit `<commit-ish>` reports
//!   `object … is a tree, not a commit` per lookup.
//! * `add` under `core.logAllRefUpdates=false` writes no `logs/HEAD`, an `--orphan`
//!   add creates no `logs/` at all (refs/files-backend.c:3678-3686), and a branch a
//!   linked worktree is bisecting is in use (worktree.c:500-525).
//! * `add` runs `post-checkout` in the new worktree and exits with its status
//!   (worktree.c:605-620).
//! * `add -b -x` and `add -- -n` are refused by the child `git branch` as options;
//!   `--orphan -b ""` by `validate_new_branchname()` in-process, at 128.
//! * `remove` reports a failed deletion with `error:` and still drops the
//!   administrative directory, at 255 (worktree.c:1463-1477); a worktree is found
//!   case-insensitively under `core.ignoreCase` (`fspathcmp()`).
//! * `list` keeps a worktree whose `gitdir` holds only a newline (worktree.c:152-163)
//!   and takes its `prunable` reason from `should_prune_worktree()`.
//! * `copy_filtered_worktree_config()` unsets `core.bare` as `git config --unset`
//!   does, removing the section it empties (worktree.c:377-387).
//! * A command run from a deleted directory dies `Unable to read current working
//!   directory` (setup.c:1948-1949).
//!
//! Skipped when no stock git is available.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(bin);
    c.args(args);
    pinned(c, dir, home)
}

/// `script` under `sh -c` with the binary under test as `$G`, for a step whose shell
/// has to stand in a directory the command itself deletes.
fn run_sh(bin: &str, dir: &Path, home: &Path, script: &str) -> Output {
    let mut c = Command::new("sh");
    c.arg("-c").arg(script).env("G", bin);
    pinned(c, dir, home)
}

fn pinned(mut c: Command, dir: &Path, home: &Path) -> Output {
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .expect("spawn git")
}

fn sh(dir: &Path, script: &str) {
    let st = Command::new("sh").arg("-c").arg(script).current_dir(dir).status().unwrap();
    assert!(st.success(), "{script}");
}

/// `repo` on `main` (c1..c6), `b1` at c4, `b2` at c2, `side` with one commit off c3,
/// and the linked worktrees `wt1` (on `b1`), `wt2` (detached at c5), `wt3` (on `b2`,
/// locked with a reason) and `wt4` (on `gone`, its directory deleted).
fn fixture(stock: &str, root: &Path) -> PathBuf {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let g = |dir: &Path, args: &[&str]| {
        let mut full = vec!["-c", "maintenance.auto=false"];
        full.extend_from_slice(args);
        let out = run(stock, dir, &home, &full);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    g(&root, &["init", "-q", "-b", "main", "repo"]);
    let repo = root.join("repo");
    for i in 1..=6 {
        std::fs::write(repo.join("f"), format!("{i}\n")).unwrap();
        g(&repo, &["add", "f"]);
        g(&repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    g(&repo, &["tag", "-a", "-m", "ann", "v2", "HEAD~2"]);
    g(&repo, &["branch", "b1", "HEAD~2"]);
    g(&repo, &["branch", "b2", "HEAD~4"]);
    g(&repo, &["checkout", "-q", "-b", "side", "HEAD~3"]);
    std::fs::write(repo.join("s"), "s\n").unwrap();
    g(&repo, &["add", "s"]);
    g(&repo, &["commit", "-q", "-m", "s1"]);
    g(&repo, &["checkout", "-q", "main"]);
    g(&repo, &["worktree", "add", "-q", "../wt1", "b1"]);
    g(&repo, &["worktree", "add", "-q", "--detach", "../wt2", "HEAD~1"]);
    g(&repo, &["worktree", "add", "-q", "--lock", "--reason", "because", "../wt3", "b2"]);
    g(&repo, &["worktree", "add", "-q", "-b", "gone", "../wt4"]);
    std::fs::remove_dir_all(root.join("wt4")).unwrap();
    repo
}

/// Every file under `dir` except object storage, indexes and sample hooks, with its
/// permission bits and, for administrative files, its contents.
fn snapshot(dir: &Path, rel: &str, out: &mut String) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "objects" | "index" | "hooks" | "home") {
            continue;
        }
        let path = e.path();
        let shown = format!("{rel}/{name}");
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            out.push_str(&format!("D {shown}\n"));
            snapshot(&path, &shown, out);
        } else {
            use std::os::unix::fs::PermissionsExt;
            out.push_str(&format!("F {shown} {:o}\n", meta.permissions().mode() & 0o777));
            if shown.contains(".git") {
                out.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
            }
        }
    }
}

enum Step<'a> {
    /// A command run with the binary under test, in `repo` joined with the directory.
    Git(&'a str, &'a [&'a str]),
    /// Setup run with `sh -c` in `repo`.
    Sh(&'a str),
    /// A script run with `sh -c` in `repo`, the binary under test as `$G`.
    With(&'a str),
}

fn observe(bin: &str, stock: &str, root: &Path, steps: &[Step<'_>]) -> String {
    let repo = fixture(stock, root);
    let root = repo.parent().unwrap().to_path_buf();
    let home = root.join("home");
    let mut seen = String::new();
    for step in steps {
        match step {
            Step::Sh(script) => sh(&repo, script),
            Step::With(script) => {
                let out = run_sh(bin, &repo, &home, script);
                seen.push_str(&format!(
                    "$ {script}\n{}--\n{}-- {:?}\n",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                    out.status.code()
                ));
            }
            Step::Git(dir, args) => {
                let out = run(bin, &repo.join(dir), &home, args);
                seen.push_str(&format!(
                    "$ {args:?}\n{}--\n{}-- {:?}\n",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                    out.status.code()
                ));
            }
        }
    }
    let list = run(stock, &repo, &home, &["worktree", "list", "--porcelain"]);
    seen.push_str(&String::from_utf8_lossy(&list.stdout));
    let refs = run(stock, &repo, &home, &["for-each-ref"]);
    seen.push_str(&String::from_utf8_lossy(&refs.stdout));
    snapshot(&root, "", &mut seen);
    seen
}

fn compare(tag: &str, cases: &[&[Step<'_>]]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-wt-sweep-{tag}-{}", std::process::id()));
    for steps in cases {
        let want = observe(stock, stock, &root, steps);
        let got = observe(BIN, stock, &root, steps);
        assert_eq!(got, want, "{tag}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

use Step::{Git, Sh, With};

#[test]
fn long_options_abbreviate_negate_and_end_at_dashdash() {
    compare(
        "options",
        &[
            &[Git(".", &["worktree", "lock", "--", "../wt1"])],
            &[Git(".", &["worktree", "lock", "--end-of-options", "../wt1"])],
            &[Git(".", &["worktree", "lock", "--reas", "x", "../wt1"])],
            &[Git(".", &["worktree", "lock", "--reason=x", "--", "../wt1"])],
            &[Git(".", &["worktree", "unlock", "--", "../wt3"])],
            &[Git(".", &["worktree", "move", "--", "../wt1", "../m"])],
            &[Git(".", &["worktree", "move", "--for", "../wt3", "../m"])],
            &[Git(".", &["worktree", "move", "--re", "../wt1", "../m"])],
            &[Git(".", &["worktree", "remove", "--end-of-options", "../wt1"])],
            &[Git(".", &["worktree", "remove", "--forc", "../wt3"])],
            &[Git(".", &["worktree", "prune", "--dry", "-v"])],
            &[Git(".", &["worktree", "prune", "--verb", "--exp=now"])],
            &[Git(".", &["worktree", "list", "--porc"])],
            &[Git(".", &["worktree", "list", "--end-of-options"])],
            &[Git(".", &["worktree", "list", "--exp"])],
            &[Git(".", &["worktree", "repair", "--rel", "../wt1"])],
            &[Git(".", &["worktree", "add", "--det", "../n"])],
            &[Git(".", &["worktree", "add", "--orph", "../n"])],
            &[Git(".", &["worktree", "add", "--re", "x", "../n"])],
            &[Git(".", &["worktree", "add", "--no-lock", "../n"])],
            &[Git(".", &["worktree", "add", "--lock", "--no-reason", "../n"])],
            &[Git(".", &["worktree", "add", "--no-detach", "--no-orphan", "--no-quiet", "../n"])],
            &[Git(".", &["worktree", "add", "--end-of-options", "../n"])],
            &[Git(".", &["worktree", "add", "--tr", "-b", "z", "../n", "side"])],
        ],
    );
}

#[test]
fn lock_reasons_are_written_as_write_file_writes_them() {
    compare(
        "lock",
        &[
            &[Git(".", &["worktree", "lock", "--no-reason", "../wt1"]), Git(".", &["worktree", "list", "-v"])],
            &[Git(".", &["worktree", "lock", "--reason", "x", "--no-reason", "../wt1"])],
            &[Git(".", &["worktree", "add", "--lock", "--reason", "", "../n"])],
        ],
    );
}

#[test]
fn add_resolves_its_branch_like_the_child_git_branch() {
    compare(
        "add",
        &[
            &[Git(".", &["worktree", "add", "-f", "-B", "b1", "../n", "main"])],
            &[Git(".", &["worktree", "add", "-f", "-B", "gone", "../n"])],
            &[Git(".", &["worktree", "add", "../n", "-"])],
            &[Git(".", &["worktree", "add", "../n", "@{-1}"])],
            &[Git(".", &["worktree", "add", "../n", "v2^{tree}"])],
            &[Git(".", &["worktree", "add", "-b", "-x", "../n"])],
            &[Git(".", &["worktree", "add", "--", "-n"])],
            &[Git(".", &["worktree", "add", "--orphan", "-b", "", "../n"])],
            &[Git(".", &["worktree", "add", "--orphan", "../n"])],
            &[Git(".", &["-c", "core.logAllRefUpdates=false", "worktree", "add", "-b", "q", "../n"])],
            &[
                Git("../wt1", &["bisect", "start", "HEAD", "HEAD~1"]),
                Git(".", &["branch", "-f", "main", "HEAD"]),
                Git(".", &["worktree", "add", "../n", "b1"]),
            ],
        ],
    );
}

#[test]
fn add_runs_post_checkout_in_the_new_worktree() {
    compare(
        "hook",
        &[
            &[
                Sh("mkdir -p .git/hooks && printf '#!/bin/sh\\necho pc \"$@\" \"$(basename \"$PWD\")\" \"${GIT_DIR-unset}\"\\nexit 3\\n' > .git/hooks/post-checkout && chmod +x .git/hooks/post-checkout"),
                Git(".", &["worktree", "add", "../n"]),
                Git(".", &["worktree", "add", "--no-checkout", "../n2"]),
                Git(".", &["worktree", "add", "-q", "--detach", "../n3", "HEAD~1"]),
            ],
        ],
    );
}

#[test]
fn remove_list_repair_and_config_follow_the_c() {
    compare(
        "admin",
        &[
            &[Git(".", &["-c", "core.ignoreCase=true", "worktree", "remove", "../WT1"])],
            &[Git(".", &["-c", "core.ignoreCase=false", "worktree", "remove", "../WT1"])],
            &[
                Sh("echo '' > .git/worktrees/wt1/gitdir"),
                Git(".", &["worktree", "list"]),
                Git(".", &["worktree", "list", "-v"]),
                Git(".", &["worktree", "prune", "-n", "-v"]),
            ],
            &[Sh("rm .git/worktrees/wt1/gitdir"), Git(".", &["worktree", "repair", "../wt1"])],
            &[
                Git(".", &["config", "extensions.worktreeConfig", "true"]),
                Git(".", &["config", "--worktree", "core.bare", "true"]),
                Git(".", &["worktree", "add", "-q", "../n"]),
            ],
            &[With("cd ../wt1 && \"$G\" worktree remove ../wt2; \"$G\" worktree remove .; \"$G\" worktree list")],
            &[
                Sh("chmod 555 ../wt1"),
                Git(".", &["worktree", "remove", "../wt1"]),
                Sh("chmod 755 ../wt1 2>/dev/null; true"),
            ],
        ],
    );
}
