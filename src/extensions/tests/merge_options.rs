//! Parity checks for `git merge` options that split the merge from the commit:
//! `--squash` and `--no-commit`. Both must perform the merge into the worktree
//! and index yet leave `HEAD` unmoved, differing only in the state they record
//! (`SQUASH_MSG` vs `MERGE_HEAD`/`MERGE_MSG`). The repositories are built with
//! real git so the assertions are about the zvcs binary's behaviour alone; every
//! step is deterministic and runs headless.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Identity env vars git honors ABOVE `user.name`/`user.email` config. This test
/// asserts on the author line SQUASH_MSG carries, so an inherited identity would
/// rewrite what it is checking: CI exports `GIT_AUTHOR_NAME` for the whole job
/// (a fresh runner has none) and every commit here came out authored by that.
const IDENTITY_ENV: [&str; 4] =
    ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"];

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new(BIN);
    for var in IDENTITY_ENV {
        cmd.env_remove(var);
    }
    let ok = cmd
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(BIN);
    for var in IDENTITY_ENV {
        cmd.env_remove(var);
    }
    let out = cmd
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repo with a clean, diverged two-branch history: `main` and `feat` change
/// different files off a common base, so their three-way merge resolves cleanly.
/// Returns (repo dir, home dir, feat commit id).
fn diverged_repo(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let root = std::env::temp_dir().join(format!("zvcs-merge-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&repo, &["add", "base.txt"]);
    git(&repo, &["commit", "-q", "-m", "base"]);

    git(&repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("feat.txt"), "from feat\n").unwrap();
    git(&repo, &["add", "feat.txt"]);
    git(&repo, &["commit", "-q", "-m", "feat-change"]);
    let feat_id = git_out(&repo, &["rev-parse", "feat"]);

    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("main.txt"), "from main\n").unwrap();
    git(&repo, &["add", "main.txt"]);
    git(&repo, &["commit", "-q", "-m", "main-change"]);

    (repo, home, feat_id)
}

fn zvcs_merge(repo: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    // Isolate from any ambient user/system git config (e.g. merge.ff=only) so the
    // clean diverged merge behaves identically everywhere.
    Command::new(BIN)
        .arg("merge")
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

#[test]
fn squash_merges_without_moving_head_and_writes_squash_msg() {
    let (repo, home, _feat) = diverged_repo("squash");
    let head_before = git_out(&repo, &["rev-parse", "HEAD"]);

    let out = zvcs_merge(&repo, &home, &["--squash", "feat"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "merge --squash failed: {}", String::from_utf8_lossy(&out.stderr));

    // git prints both notices, on different streams: measured against 2.55.0,
    // `Automatic merge went well; …` goes to **stderr** (it is `finish()`'s
    // diagnostic) while `Squash commit -- not updating HEAD` goes to stdout.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Automatic merge went well; stopped before committing as requested"),
        "missing stop notice:\n{stderr}"
    );
    assert!(stdout.contains("Squash commit -- not updating HEAD"), "missing squash notice:\n{stdout}");

    // HEAD is untouched, and no MERGE_HEAD is recorded (squash is not a merge).
    assert_eq!(git_out(&repo, &["rev-parse", "HEAD"]), head_before, "squash must not move HEAD");
    assert!(!repo.join(".git/MERGE_HEAD").exists(), "squash must not write MERGE_HEAD");

    // SQUASH_MSG carries the ported `squash_message()` body.
    let squash_msg = std::fs::read_to_string(repo.join(".git/SQUASH_MSG")).expect("SQUASH_MSG written");
    assert!(squash_msg.starts_with("Squashed commit of the following:\n"), "bad header:\n{squash_msg}");
    assert!(squash_msg.contains("commit "), "no commit block:\n{squash_msg}");
    assert!(squash_msg.contains("Author: t <t@e.x>"), "no author line:\n{squash_msg}");
    assert!(squash_msg.contains("    feat-change"), "feat subject not indented into body:\n{squash_msg}");

    // The merge did reach the worktree/index: feat's file is present and staged.
    assert!(repo.join("feat.txt").exists(), "squash must apply the merged tree to the worktree");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn no_commit_records_merge_state_without_committing() {
    let (repo, home, feat_id) = diverged_repo("nocommit");
    let head_before = git_out(&repo, &["rev-parse", "HEAD"]);

    let out = zvcs_merge(&repo, &home, &["--no-commit", "feat"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "merge --no-commit failed: {stderr}");

    // `cmd_merge()` ends with `fprintf(stderr, …)` for this one, and with no
    // verbosity check — `-q` does not silence it either.
    assert!(
        stderr.contains("Automatic merge went well; stopped before committing as requested"),
        "missing stop notice:\n{stderr}"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).matches("Automatic merge went well").count(),
        0,
        "the notice belongs on stderr"
    );

    // HEAD stays put; the merge is left in progress for `git commit` to finish.
    assert_eq!(git_out(&repo, &["rev-parse", "HEAD"]), head_before, "--no-commit must not move HEAD");

    let merge_head = std::fs::read_to_string(repo.join(".git/MERGE_HEAD")).expect("MERGE_HEAD written");
    assert_eq!(merge_head.trim(), feat_id, "MERGE_HEAD must name the merged head");
    let merge_msg = std::fs::read_to_string(repo.join(".git/MERGE_MSG")).expect("MERGE_MSG written");
    assert!(merge_msg.contains("Merge branch 'feat'"), "MERGE_MSG missing default title:\n{merge_msg}");

    // The merged content reached the worktree (feat's file is present).
    assert!(repo.join("feat.txt").exists(), "--no-commit must apply the merged tree to the worktree");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

// ---------------------------------------------------------------------------
// `-Xpatience` / `-Xdiff-algorithm=patience`
// ---------------------------------------------------------------------------

/// A three-way merge whose result depends on which xdiff algorithm computed the
/// two sides' edits. The sequences were found by searching small line sequences
/// for a case where stock git 2.55.0's patience diff differs from *both* myers
/// and histogram, so a merge that quietly substituted either would produce
/// visibly different bytes rather than passing by luck.
///
/// Returns (repo dir, home dir).
fn patience_repo(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-merge-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f.txt"), "c\nb\nc\nd\nc\na\nc\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "base"]);

    git(&repo, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("f.txt"), "c\nb\nc\nd\nc\na\nc\nz\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "side"]);

    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f.txt"), "b\nc\na\nc\na\nb\nb\nd\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "main"]);

    (repo, home)
}

/// What stock git 2.55.0 leaves in `f.txt` for the [`patience_repo`] merge when
/// the diff really is patience.
const PATIENCE_MERGE: &str = "b\nc\na\nc\na\nb\nb\nd\n<<<<<<< HEAD\n=======\nc\na\nc\nz\n>>>>>>> side\n";

/// The same merge under merge-ort's default algorithm (histogram): a different
/// conflict shape entirely, which is what makes the assertion above meaningful.
const HISTOGRAM_MERGE: &str = "b\nc\na\nc\n<<<<<<< HEAD\na\nb\nb\nd\n=======\nz\n>>>>>>> side\n";

#[test]
fn merge_honours_patience_rather_than_refusing_it() {
    for xopt in ["-Xpatience", "-Xdiff-algorithm=patience"] {
        let (repo, home) = patience_repo(&format!("patience{}", xopt.len()));

        let out = zvcs_merge(&repo, &home, &[xopt, "side"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // A conflicted merge exits 1; anything else here means the option was
        // rejected instead of honoured.
        assert_eq!(out.status.code(), Some(1), "{xopt}: unexpected exit\n{stderr}");
        assert!(!stderr.contains("unsupported"), "{xopt} must not be refused:\n{stderr}");

        let merged = std::fs::read_to_string(repo.join("f.txt")).unwrap();
        assert_eq!(merged, PATIENCE_MERGE, "{xopt} did not produce git's patience merge");
        assert_ne!(merged, HISTOGRAM_MERGE, "{xopt} silently fell back to histogram");

        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}

#[test]
fn merge_without_an_x_option_still_uses_histogram() {
    // The negative half of the pair: merge-ort's `init_basic_merge_options()`
    // opens with `DIFF_WITH_ALG(opt, HISTOGRAM_DIFF)`, so an un-asked-for merge
    // of the same fixture must NOT look like the patience one.
    let (repo, home) = patience_repo("default");

    let out = zvcs_merge(&repo, &home, &["side"]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    let merged = std::fs::read_to_string(repo.join("f.txt")).unwrap();
    assert_eq!(merged, HISTOGRAM_MERGE, "default merge algorithm changed");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn merge_file_honours_patience() {
    // `git merge-file` reaches the same text driver directly, one zealous level
    // higher, and had its own patience refusal.
    let (repo, _home) = patience_repo("mergefile");
    let (base, cur, oth) = (repo.join("base"), repo.join("cur"), repo.join("oth"));
    std::fs::write(&base, "c\nb\nc\nd\nc\na\nc\n").unwrap();
    std::fs::write(&cur, "b\nc\na\nc\na\nb\nb\nd\n").unwrap();
    std::fs::write(&oth, "c\nb\nc\nd\nc\na\nc\nz\n").unwrap();

    let run = |algo: &str| {
        let out = Command::new(BIN)
            .args(["merge-file", "-p", &format!("--diff-algorithm={algo}"), "--", ])
            .args([cur.to_str().unwrap(), base.to_str().unwrap(), oth.to_str().unwrap()])
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };

    // Labels default to the operand spellings, which are absolute paths here, so
    // the assertion is on the merged *body* between the markers.
    let (code, patience, err) = run("patience");
    assert_eq!(code, Some(1), "merge-file --diff-algorithm=patience: {err}");
    assert!(!err.contains("unsupported"), "patience must not be refused:\n{err}");
    let (_, myers, _) = run("myers");

    let body = |s: &str| {
        s.lines()
            .map(|l| if l.starts_with("<<<<<<< ") { "<<<".to_string() } else if l.starts_with(">>>>>>> ") { ">>>".to_string() } else { l.to_string() })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(body(&patience), "b\nc\na\nc\na\nb\nb\nd\n<<<\n=======\nc\na\nc\nz\n>>>");
    assert_ne!(body(&patience), body(&myers), "patience silently fell back to myers");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
