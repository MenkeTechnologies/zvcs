//! `git bisect` against stock git on twin fixtures, comparing the state files and the
//! `refs/bisect` directory as well as the output — what a differential sweep found
//! apart from stock 2.56.0.
//!
//! * `BISECT_ANCESTORS_OK` is created `0600` (bisect.c:952-953).
//! * `bisect_clean_state()` deletes the refs in one `refs_delete_refs()` transaction
//!   (bisect.c:1194-1210), and the files backend removes empty parents only below
//!   `refs/<kind>/` (refs/files-backend.c:1330-1370): a `reset` leaves an empty
//!   `refs/bisect`, and a `start` that records no ref creates none.
//! * `bisect replay` splits a line on runs of blanks and tabs (builtin/bisect.c:
//!   1153-1165), so `git bisect   good <rev>` replays.
//! * `bisect run` marks through `bisect_state()`, whose `bisect_autostart()` comes
//!   before `check_and_set_terms()` (builtin/bisect.c:1066-1072): a script that ended
//!   the session leaves no `BISECT_TERMS` behind.
//!
//! Skipped when no stock git is available.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(bin);
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        // A `bisect run` script reaches the binary under test as `$G`.
        .env("G", bin)
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

/// `repo` on `main`, c1..c20 each rewriting `f` to its number, `good` tagged at c5
/// and `bad` at c18.
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
    for i in 1..=20 {
        std::fs::write(repo.join("f"), format!("{i}\n")).unwrap();
        g(&repo, &["add", "f"]);
        g(&repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    g(&repo, &["tag", "good", "HEAD~15"]);
    g(&repo, &["tag", "bad", "HEAD~2"]);
    repo
}

/// Everything under `.git` but objects, the index and the sample hooks, with
/// permission bits and contents.
fn snapshot(dir: &Path, rel: &str, out: &mut String) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "objects" | "index" | "hooks") {
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
            out.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
}

fn observe(bin: &str, stock: &str, root: &Path, steps: &[&[&str]]) -> String {
    let repo = fixture(stock, root);
    let home = repo.parent().unwrap().join("home");
    let mut seen = String::new();
    for args in steps {
        // A `!` step is setup, run with `sh -c` in the repository.
        if let ["!", script] = args {
            let st = Command::new("sh").arg("-c").arg(script).current_dir(&repo).status().unwrap();
            assert!(st.success(), "{script}");
            continue;
        }
        let out = run(bin, &repo, &home, args);
        seen.push_str(&format!(
            "$ {args:?}\n{}--\n{}-- {:?}\n",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            out.status.code()
        ));
    }
    snapshot(&repo.join(".git"), ".git", &mut seen);
    seen
}

fn compare(tag: &str, cases: &[&[&[&str]]]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-bisect-sweep-{tag}-{}", std::process::id()));
    for steps in cases {
        let want = observe(stock, stock, &root, steps);
        let got = observe(BIN, stock, &root, steps);
        assert_eq!(got, want, "{tag}: {steps:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn state_files_and_the_refs_directory_match() {
    compare(
        "state",
        &[
            &[&["bisect", "start", "bad", "good"]],
            &[&["bisect", "start", "bad", "good"], &["bisect", "reset"]],
            &[&["bisect", "start", "bad", "good"], &["bisect", "good"], &["bisect", "reset", "HEAD"]],
            &[&["bisect", "start", "nosuch", "good"]],
            &[&["bisect", "start", "--", "f"], &["bisect", "log"]],
            &[&["bisect", "start", "bad^{tree}", "good"]],
            &[&["bisect", "start", "--no-checkout", "bad", "good"], &["bisect", "good"], &["bisect", "reset"]],
        ],
    );
}

#[test]
fn replay_splits_on_runs_of_blanks() {
    compare(
        "replay",
        &[&[
            &["bisect", "start", "bad", "good"],
            &["bisect", "good"],
            &["!", "perl -pe 's/^git bisect good/git bisect \\t good/' .git/BISECT_LOG > ../l"],
            &["bisect", "reset"],
            &["bisect", "replay", "../l"],
            &["bisect", "log"],
        ]],
    );
}

#[test]
fn run_after_a_reset_in_the_script_records_no_terms() {
    compare(
        "run",
        &[
            &[&["bisect", "start", "bad", "good"], &["bisect", "run", "sh", "-c", "\"$G\" bisect reset >/dev/null; exit 0"]],
            &[&["bisect", "start", "bad", "good"], &["bisect", "run", "sh", "-c", "test $(cat f) -lt 9"]],
        ],
    );
}
