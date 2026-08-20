//! The diffstat block `git commit` prints under its headline.
//!
//! `print_commit_summary()` (sequencer.c:1413) does not walk trees itself. It
//! configures the ordinary revision/diff machinery — `DIFF_FORMAT_SHORTSTAT |
//! DIFF_FORMAT_SUMMARY` (sequencer.c:1466), `show_root_diff = 1`
//! (sequencer.c:1470), `rev.diffopt.detect_rename = DIFF_DETECT_RENAME`
//! (sequencer.c:1473) — and hands the commit to `log_tree_commit()`, which diffs
//! it against its first parent, or against the empty tree when it has none.
//!
//! So the block is `git diff-tree -r -M --shortstat --summary <parent> <commit>`
//! by construction, and any second implementation of it drifts. The two drifts
//! these tests pin, both of which shipped:
//!
//! * rename detection off, so `git mv old new` was reported as a create plus a
//!   delete with the file's whole line count counted twice;
//! * line counts taken from `gix`'s tree-diff statistics, which push both blobs
//!   through the `Mode::ToGit` conversion pipeline first — normalizing away a
//!   CRLF change git counts as rewritten lines.
//!
//! Everything is compared against stock git in a byte-identical fixture: identity
//! and dates are pinned so the abbreviated object id in the headline matches too,
//! which makes the whole block comparable, not just the counts.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A STOCK git to compare against, or `None` on a machine without one.
///
/// Resolved explicitly rather than through `PATH`: on a machine where zvcs
/// shadows `git`, a `PATH` lookup makes the oracle the thing under test.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .map(str::to_owned)
}

const DATE: &str = "1112911993 +0000"; // 2005-04-07 in UTC

fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .output()
        .unwrap()
}

fn ok(bin: &str, repo: &Path, home: &Path, args: &[&str]) {
    let out = run(bin, repo, home, args);
    assert!(
        out.status.success(),
        "{bin} {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn work_area(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-csstat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    (repo, home)
}

fn init(git: &str, repo: &Path, home: &Path) {
    ok(git, repo, home, &["init", "-q", "-b", "main", "."]);
    ok(git, repo, home, &["config", "user.name", "A U Thor"]);
    ok(git, repo, home, &["config", "user.email", "author@example.com"]);
}

/// Run `build` in a fresh fixture, then `commit_args` under each binary, and
/// require the two summaries to be byte-identical.
fn compare(tag: &str, build: impl Fn(&str, &Path, &Path), commit_args: &[&str]) -> String {
    let git = stock_git().expect("checked by the caller");
    let (zrepo, zhome) = work_area(&format!("z-{tag}"));
    let (grepo, ghome) = work_area(&format!("g-{tag}"));
    init(&git, &zrepo, &zhome);
    init(&git, &grepo, &ghome);
    build(&git, &zrepo, &zhome);
    build(&git, &grepo, &ghome);

    let z = run(BIN, &zrepo, &zhome, commit_args);
    let g = run(&git, &grepo, &ghome, commit_args);
    assert_eq!(
        g.status.code(),
        z.status.code(),
        "exit codes differ for `git {}`",
        commit_args.join(" ")
    );
    let zs = String::from_utf8_lossy(&z.stdout).into_owned();
    let gs = String::from_utf8_lossy(&g.stdout).into_owned();
    assert_eq!(
        gs, zs,
        "`git {}` summary differs from stock\n--- zvcs stderr ---\n{}",
        commit_args.join(" "),
        String::from_utf8_lossy(&z.stderr)
    );
    let _ = std::fs::remove_dir_all(zrepo.parent().unwrap());
    let _ = std::fs::remove_dir_all(grepo.parent().unwrap());
    zs
}

/// A rename is one changed file plus a `rename` summary line, not a create/delete
/// pair — `detect_rename = DIFF_DETECT_RENAME` (sequencer.c:1473) is not optional.
#[test]
fn a_renamed_file_is_summarized_as_a_rename() {
    let Some(_) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let out = compare(
        "rename",
        |git, repo, home| {
            let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
            std::fs::write(repo.join("old.txt"), body).unwrap();
            ok(git, repo, home, &["add", "old.txt"]);
            ok(git, repo, home, &["commit", "-q", "-m", "one"]);
            ok(git, repo, home, &["mv", "old.txt", "new.txt"]);
        },
        &["commit", "-m", "two"],
    );
    assert!(
        out.contains(" rename old.txt => new.txt (100%)")
            && out.contains(" 1 file changed, 0 insertions(+), 0 deletions(-)"),
        "a pure rename must be one changed file and a rename line:\n{out}"
    );
}

/// A commit that rewrites LF content as CRLF changes every line it touches. The
/// count has to come from the blob diff, not from a pipeline that normalizes the
/// line endings on both sides before comparing them.
#[test]
fn crlf_rewrites_are_counted_as_changed_lines() {
    let Some(_) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let out = compare(
        "crlf",
        |git, repo, home| {
            std::fs::write(repo.join("a.txt"), "alpha\nbeta\n").unwrap();
            ok(git, repo, home, &["add", "a.txt"]);
            ok(git, repo, home, &["commit", "-q", "-m", "one"]);
            std::fs::write(repo.join("a.txt"), "alpha\r\nbeta\r\ngamma\r\n").unwrap();
            ok(git, repo, home, &["add", "a.txt"]);
        },
        &["commit", "-m", "two"],
    );
    assert!(
        out.contains(" 1 file changed, 3 insertions(+), 2 deletions(-)"),
        "two rewritten lines plus one added, not one insertion:\n{out}"
    );
}

/// `--amend` diffs the amended commit against **HEAD's parent**, so the summary
/// covers the whole commit, not the delta the amend added.
#[test]
fn amend_summarizes_against_the_parent_of_head() {
    let Some(_) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let out = compare(
        "amend",
        |git, repo, home| {
            std::fs::write(repo.join("README.md"), "hello\nworld\n").unwrap();
            std::fs::write(repo.join("a.txt"), "alpha\nbeta\n").unwrap();
            ok(git, repo, home, &["add", "."]);
            ok(git, repo, home, &["commit", "-q", "-m", "first commit"]);
            std::fs::write(repo.join("README.md"), "hello \nworld\nthird\n").unwrap();
            std::fs::write(repo.join("a.txt"), "alpha\r\nbeta\r\ngamma\r\n").unwrap();
            ok(git, repo, home, &["add", "."]);
            ok(git, repo, home, &["commit", "-q", "-m", "second commit with changes"]);
            // Nothing further is staged: the amend rewrites the same tree, and the
            // summary must still describe it against the *first* commit.
        },
        &["commit", "--amend", "--no-edit"],
    );
    assert!(
        out.contains(" 2 files changed, 5 insertions(+), 3 deletions(-)"),
        "the amend summary must cover the commit, not the (empty) amend delta:\n{out}"
    );
}

/// A root commit has no parent, so `show_root_diff` diffs it against the empty
/// tree; an `--amend` of a root commit takes that path too while still not being
/// labelled `(root-commit)`, since `HEAD` already existed.
#[test]
fn a_root_commit_and_its_amend_diff_against_the_empty_tree() {
    let Some(_) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let root = compare(
        "root",
        |git, repo, home| {
            std::fs::create_dir_all(repo.join("src/deep")).unwrap();
            std::fs::write(repo.join("src/deep/x.txt"), "a\n").unwrap();
            std::fs::write(repo.join("b.bin"), b"bin\0data\n").unwrap();
            ok(git, repo, home, &["add", "."]);
        },
        &["commit", "-m", "root"],
    );
    assert!(
        root.contains("(root-commit)")
            && root.contains(" create mode 100644 src/deep/x.txt")
            && root.contains(" 2 files changed, 1 insertion(+)"),
        "a root commit is summarized against the empty tree, recursively:\n{root}"
    );

    let amended = compare(
        "root-amend",
        |git, repo, home| {
            std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
            ok(git, repo, home, &["add", "."]);
            ok(git, repo, home, &["commit", "-q", "-m", "root"]);
            std::fs::write(repo.join("f.txt"), "a\nb\nc\n").unwrap();
            ok(git, repo, home, &["add", "."]);
        },
        &["commit", "--amend", "--no-edit"],
    );
    assert!(
        !amended.contains("(root-commit)") && amended.contains(" 1 file changed, 3 insertions(+)"),
        "an amended root keeps the empty-tree diff but loses the (root-commit) label:\n{amended}"
    );
}

/// A mode change with no content change is a changed file with no line counts,
/// plus a `mode change` summary line.
#[test]
fn a_mode_change_is_a_changed_file_with_no_lines() {
    let Some(_) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let out = compare(
        "mode",
        |git, repo, home| {
            std::fs::write(repo.join("e.sh"), "e\n").unwrap();
            ok(git, repo, home, &["add", "."]);
            ok(git, repo, home, &["commit", "-q", "-m", "one"]);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    repo.join("e.sh"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            }
            ok(git, repo, home, &["add", "."]);
        },
        &["commit", "-m", "two"],
    );
    assert!(
        out.contains(" mode change 100644 => 100755 e.sh"),
        "a mode-only change must still be summarized:\n{out}"
    );
}

/// `log_tree_commit()` prints no diff for a commit with more than one parent, so
/// a merge commit's summary is the headline alone.
#[test]
fn a_merge_commit_prints_no_diffstat() {
    let Some(_) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let out = compare(
        "merge",
        |git, repo, home| {
            std::fs::write(repo.join("a.txt"), "a\n").unwrap();
            ok(git, repo, home, &["add", "."]);
            ok(git, repo, home, &["commit", "-q", "-m", "one"]);
            ok(git, repo, home, &["checkout", "-q", "-b", "br"]);
            std::fs::write(repo.join("z.txt"), "z\n").unwrap();
            ok(git, repo, home, &["add", "."]);
            ok(git, repo, home, &["commit", "-q", "-m", "br"]);
            ok(git, repo, home, &["checkout", "-q", "main"]);
            std::fs::write(repo.join("q.txt"), "q\n").unwrap();
            ok(git, repo, home, &["add", "."]);
            ok(git, repo, home, &["commit", "-q", "-m", "q"]);
            // Leave the merge staged but uncommitted so `commit` concludes it.
            let out = run(git, repo, home, &["merge", "--no-commit", "--no-ff", "br"]);
            assert!(
                out.status.success() || out.status.code() == Some(1),
                "fixture merge failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        },
        &["commit", "-m", "merge br"],
    );
    assert!(
        !out.contains("file changed") && !out.contains("files changed"),
        "a merge commit prints its headline and nothing else:\n{out}"
    );
}
