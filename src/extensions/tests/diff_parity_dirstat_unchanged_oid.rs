//! `--dirstat` over pairs whose two blobs are the same object.
//!
//! `show_dirstat()` (diff.c:3390-3399) asks one question before any other:
//!
//! ```c
//! if (p->one->oid_valid && p->two->oid_valid &&
//!     oideq(&p->one->oid, &p->two->oid)) {
//!         damage = 0;
//!         goto found_damage;
//! }
//! ```
//!
//! It sits *above* the `options->flags.dirstat_by_file` branch, so a pure rename
//! and a mode-only change are charged zero damage in `--dirstat-by-file` exactly
//! as they are in the default content mode. `conclude_dirstat()`'s `if (!changed)`
//! and `gather_dirstat()`'s `if (sum_changes)` (diff.c:3328) then keep such a
//! directory out of the listing entirely — even at a zero cut-off, where every
//! other directory qualifies.
//!
//! Every expectation below was read off stock git 2.55.0 on this fixture before it
//! was written down.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("COLUMNS")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Four pairs across four directories, two of which carry no content change at
/// all:
///
/// | pair                           | raw status | damage |
/// |--------------------------------|-----------|--------|
/// | `doc/keep.txt`                 | `M`       | content |
/// | `new/added.txt`                | `A`       | the whole file |
/// | `src/moved.txt → lib/moved.txt`| `R100`    | none — same blob |
/// | `src/mode.txt`                 | `M` (mode)| none — same blob |
///
/// So `lib/` and `src/` must never appear, in any dirstat mode, at any cut-off.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-dirstat-oid-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("doc")).unwrap();
    let repo = root.canonicalize().unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("src/moved.txt"), "alpha\nbeta\ngamma\n").unwrap();
    std::fs::write(repo.join("src/mode.txt"), "one\n").unwrap();
    std::fs::write(repo.join("doc/keep.txt"), "x\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);

    std::fs::create_dir_all(repo.join("lib")).unwrap();
    std::fs::create_dir_all(repo.join("new")).unwrap();
    git(&repo, &["mv", "src/moved.txt", "lib/moved.txt"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            repo.join("src/mode.txt"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::fs::write(repo.join("doc/keep.txt"), "x\nCHANGED\n").unwrap();
    std::fs::write(repo.join("new/added.txt"), "fresh\ncontent\nhere\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c1"]);
    repo
}

/// The rename and the mode change are invisible to every mode. `--dirstat-by-file`
/// is the sharp one: charging each *changed pair* one unit — which is what the C
/// does only once the oid test has been passed — would make this four files at
/// 25.0% apiece and list `lib/` and `src/` alongside `doc/` and `new/`.
#[test]
fn an_oid_identical_pair_is_zero_damage_in_every_dirstat_mode() {
    let repo = fixture("modes");

    let by_file = "  50.0% doc/\n  50.0% new/\n";
    for mode in ["--dirstat-by-file", "--dirstat=files", "--dirstat=files,0"] {
        let o = run(&repo, &["diff", "-M", mode, "HEAD~1", "HEAD"]);
        assert_eq!(stdout(&o), by_file, "{mode}");
    }

    // The content mode weighs bytes (`diffcore_count_changes()`), so the two rows
    // are uneven — but they are still the only two rows.
    for mode in ["--dirstat", "--dirstat=changes,0,cumulative"] {
        let o = run(&repo, &["diff", "-M", mode, "HEAD~1", "HEAD"]);
        assert_eq!(stdout(&o), "  29.6% doc/\n  70.3% new/\n", "{mode}");
    }

    // `--dirstat=lines` is `show_dirstat_by_line()`, a different function with no
    // oid test of its own — it reaches the same answer because a rename and a mode
    // change have no added or deleted lines to count.
    let o = run(&repo, &["diff", "-M", "--dirstat=lines", "HEAD~1", "HEAD"]);
    assert_eq!(stdout(&o), "  25.0% doc/\n  75.0% new/\n");
}

/// A zero permille cut-off is the case `gather_dirstat()`'s `if (sum_changes)`
/// guard exists for: without it a directory holding only oid-identical pairs
/// prints as ` 0.0%`.
#[test]
fn a_zero_cutoff_still_omits_a_directory_with_no_damage() {
    let repo = fixture("cutoff");
    for (mode, want) in [
        ("--dirstat=0", "  29.6% doc/\n  70.3% new/\n"),
        ("--dirstat=0,cumulative", "  29.6% doc/\n  70.3% new/\n"),
        ("--dirstat=files,0,cumulative", "  50.0% doc/\n  50.0% new/\n"),
    ] {
        let out = stdout(&run(&repo, &["diff", "-M", mode, "HEAD~1", "HEAD"]));
        // The sharp part: no `   0.0% lib/` / `   0.0% src/` row, which is what a
        // missing `if (sum_changes)` guard prints once the cut-off stops filtering.
        assert_eq!(out, want, "{mode}");
    }
}

/// `git show`/`git log` render a commit's dirstat through their own walk
/// (`commit_dirstat`), not through `git diff`'s — the oid test has to be in both.
#[test]
fn show_and_log_agree_with_diff_on_an_oid_identical_pair() {
    let repo = fixture("history");
    let want = "  50.0% doc/\n  50.0% new/\n";

    let o = run(&repo, &["show", "-M", "--dirstat-by-file", "--format=", "HEAD"]);
    assert_eq!(stdout(&o), want);

    let o = run(&repo, &["log", "-1", "-M", "--dirstat=files,0", "--format=", "HEAD"]);
    assert_eq!(stdout(&o), want);
}
