//! The order stdout and stderr lines come out in when a caller captures both.
//!
//! git never orders its two streams explicitly — the order falls out of C stdio.
//! `stderr` is unbuffered, and `stdout` is line buffered only when it "can be
//! determined not to refer to an interactive device" (C99 7.19.3p7); pointed at a
//! pipe or a file it is fully buffered and nothing reaches the fd until `exit()`.
//! So a command that writes to both reverses itself depending on where stdout
//! goes, and the captured order — the one every script, CI log and parity harness
//! sees — puts the stderr lines first:
//!
//! ```text
//! $ git checkout feature 2>&1 | cat
//! Switched to branch 'feature'      <- stderr, immediate
//! M       README.md                 <- stdout, flushed by exit()
//! ```
//!
//! `builtin/checkout.c` is where the split comes from: the `Switched to branch
//! '%s'` / `Switched to a new branch '%s'` messages go through `fprintf(stderr,
//! …)`, while `show_local_changes()` and `report_tracking()` `printf` their
//! blocks to stdout. `builtin/merge.c:1665` is the same shape for a
//! fast-forward: `printf(_("Updating %s..%s\n"))` runs *before*
//! `checkout_fast_forward()` (merge.c:1680), whose refusal writes `error: The
//! following untracked working tree files …` to stderr — so stock's captured
//! output has the refusal first and the `Updating` line last.
//!
//! A port that writes stdout with Rust's `println!` — a `LineWriter` whatever fd
//! 1 is — always produces the interactive order and never this one. These tests
//! pin the captured order by giving the child ONE file for both streams, which is
//! the only way the interleaving is observable: capturing the two separately
//! throws away exactly the information under test.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

fn cmd(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE);
    c
}

fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) {
    let out = cmd(bin, repo, home, args).output().unwrap();
    assert!(
        out.status.success(),
        "{bin} {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run `args` with **one** file behind both fd 1 and fd 2, and return what landed
/// in it. `try_clone` dups the descriptor, so the two share a file offset and the
/// bytes appear in the order the child actually wrote them — a `2>&1` pipe in
/// every respect that matters here, minus a reader.
fn merged(bin: &str, repo: &Path, home: &Path, sink: &Path, args: &[&str]) -> String {
    let _ = std::fs::remove_file(sink);
    let f = File::create(sink).unwrap();
    let g = f.try_clone().unwrap();
    let status = cmd(bin, repo, home, args)
        .stdout(Stdio::from(f))
        .stderr(Stdio::from(g))
        .status()
        .unwrap();
    let text = std::fs::read_to_string(sink).unwrap();
    // Exit status is not what these tests are about, but a command that died
    // before writing would make the ordering assertion vacuous.
    assert!(
        !text.is_empty(),
        "{bin} {args:?} wrote nothing (status {status:?})"
    );
    text
}

/// `home`, `repo` and a `capture` sink, all outside the worktree so the sink
/// never shows up as an untracked path in the output being measured.
fn work_area(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-ssorder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    (repo, home, root.join("capture"))
}

/// `main` with two commits and a `feature` branch one commit further along, left
/// with `README.md` modified in the worktree so a checkout has something for
/// `show_local_changes()` to print.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (repo, home, sink) = work_area(tag);
    let git = stock_git().expect("checked by the caller");
    run(&git, &repo, &home, &["init", "-q", "-b", "main", "."]);
    run(&git, &repo, &home, &["config", "user.name", "A U Thor"]);
    run(&git, &repo, &home, &["config", "user.email", "author@example.com"]);
    std::fs::write(repo.join("README.md"), "hello\nworld\n").unwrap();
    run(&git, &repo, &home, &["add", "README.md"]);
    run(&git, &repo, &home, &["commit", "-q", "-m", "first commit"]);
    run(&git, &repo, &home, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("f.txt"), "f contents\n").unwrap();
    run(&git, &repo, &home, &["add", "f.txt"]);
    run(&git, &repo, &home, &["commit", "-q", "-m", "feature commit"]);
    run(&git, &repo, &home, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("README.md"), "hello\nworld\nlocal\n").unwrap();
    (repo, home, sink)
}

/// Index of the line that begins with `prefix`, for ordering assertions.
fn line_at(text: &str, prefix: &str) -> usize {
    text.lines()
        .position(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("no line starting {prefix:?} in:\n{text}"))
}

/// Every branch-transition form: the stderr `Switched to …` line has to precede
/// the stdout worktree-change listing in a captured run, and the whole capture
/// has to be byte-identical to stock's.
#[test]
fn switching_branches_prints_the_stderr_line_before_the_stdout_listing() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    // `checkout -m` takes the three-way path and `--orphan` the unborn-branch
    // path, both of which reach a different `Switched to …` call site than a
    // plain `checkout`/`switch`.
    for (tag, args, marker) in [
        ("checkout", vec!["checkout", "feature"], "Switched to branch 'feature'"),
        ("checkout-m", vec!["checkout", "-m", "feature"], "Switched to branch 'feature'"),
        ("switch", vec!["switch", "feature"], "Switched to branch 'feature'"),
        (
            "orphan",
            vec!["checkout", "--orphan", "fresh"],
            "Switched to a new branch 'fresh'",
        ),
    ] {
        let (zrepo, zhome, zsink) = fixture(&format!("z-{tag}"));
        let (grepo, ghome, gsink) = fixture(&format!("g-{tag}"));
        let z = merged(BIN, &zrepo, &zhome, &zsink, &args);
        let g = merged(&git, &grepo, &ghome, &gsink, &args);

        assert_eq!(g, z, "`git {}` capture differs from stock", args.join(" "));
        assert!(
            line_at(&z, marker) < line_at(&z, "M\t"),
            "`git {}`: the stderr transition line must come out before the \
             stdout worktree listing when both are captured together:\n{z}",
            args.join(" ")
        );

        let _ = std::fs::remove_dir_all(zrepo.parent().unwrap());
        let _ = std::fs::remove_dir_all(grepo.parent().unwrap());
    }
}

/// A fast-forward blocked by an untracked file: `Updating <a>..<b>` is written to
/// stdout before the refusal is written to stderr (builtin/merge.c:1665 vs 1680),
/// so a captured run shows the refusal first.
#[test]
fn a_blocked_fast_forward_prints_the_refusal_before_the_updating_line() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    // `feature` adds `f.txt`; leaving an untracked `f.txt` on `main` is what makes
    // `checkout_fast_forward()` refuse after the `Updating` line is already in the
    // stdio buffer.
    let build = |tag: &str| {
        let (repo, home, sink) = fixture(tag);
        std::fs::write(repo.join("README.md"), "hello\nworld\n").unwrap();
        std::fs::write(repo.join("f.txt"), "in the way\n").unwrap();
        (repo, home, sink)
    };
    let (zrepo, zhome, zsink) = build("z-ff");
    let (grepo, ghome, gsink) = build("g-ff");
    let args = ["merge", "feature"];
    let z = merged(BIN, &zrepo, &zhome, &zsink, &args);
    let g = merged(&git, &grepo, &ghome, &gsink, &args);

    assert_eq!(g, z, "`git merge feature` capture differs from stock");
    assert!(
        line_at(&z, "error: The following untracked") < line_at(&z, "Updating "),
        "the stderr refusal must come out before the buffered `Updating` line:\n{z}"
    );

    let _ = std::fs::remove_dir_all(zrepo.parent().unwrap());
    let _ = std::fs::remove_dir_all(grepo.parent().unwrap());
}

/// A clean fast-forward writes only to stdout, so buffering must not reorder it
/// against itself: `Updating`, `Fast-forward`, then the diffstat.
#[test]
fn a_clean_fast_forward_keeps_its_stdout_lines_in_order() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let build = |tag: &str| {
        let (repo, home, sink) = fixture(tag);
        std::fs::write(repo.join("README.md"), "hello\nworld\n").unwrap();
        (repo, home, sink)
    };
    let (zrepo, zhome, zsink) = build("z-ffok");
    let (grepo, ghome, gsink) = build("g-ffok");
    let args = ["merge", "feature"];
    let z = merged(BIN, &zrepo, &zhome, &zsink, &args);
    let g = merged(&git, &grepo, &ghome, &gsink, &args);

    assert_eq!(g, z, "`git merge feature` capture differs from stock");
    assert!(
        line_at(&z, "Updating ") < line_at(&z, "Fast-forward")
            && line_at(&z, "Fast-forward") < line_at(&z, " f.txt"),
        "a stdout-only run must keep its own order:\n{z}"
    );

    let _ = std::fs::remove_dir_all(zrepo.parent().unwrap());
    let _ = std::fs::remove_dir_all(grepo.parent().unwrap());
}

/// The buffer must not swallow output on the way out: a plumbing command that
/// never arms it, and a `checkout` that does, both have to deliver their stdout.
#[test]
fn buffered_stdout_still_reaches_a_separate_capture() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let (repo, home, _sink) = fixture("z-plain");
    let out = cmd(BIN, &repo, &home, &["checkout", "feature"]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "M\tREADME.md\n",
        "the deferred stdout half must still be flushed before exit"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Switched to branch 'feature'\n",
        "the transition line stays on stderr"
    );

    let plumbing = cmd(BIN, &repo, &home, &["diff-index", "--name-status", "HEAD"])
        .output()
        .unwrap();
    let stock = Command::new(&git)
        .args(["diff-index", "--name-status", "HEAD"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&stock.stdout),
        String::from_utf8_lossy(&plumbing.stdout),
        "`diff-index` never arms the buffer and must be unchanged"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
