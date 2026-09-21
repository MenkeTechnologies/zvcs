//! Four things `git merge` gets from machinery that sits *around* the merge
//! engine, each measured against `/usr/local/bin/git` 2.55.0 with `HOME` and
//! `GIT_CONFIG_*` pinned so the host's own configuration cannot reach a fixture.
//!
//! 1. **The index refresh `cmd_merge` runs before it merges.**
//!    `refresh_index(the_repository->index, REFRESH_QUIET, NULL, NULL, NULL)`
//!    (builtin/merge.c:1702) and `repo_refresh_and_write_index(…, REFRESH_QUIET,
//!    SKIP_IF_UNCHANGED, …)` (builtin/merge.c:795, :994) are not hygiene. Every
//!    path that decides what a merge may overwrite reads the index's `stat`
//!    data, and `threeway_merge()` checks `verify_uptodate(index, o)`
//!    (unpack-trees.c:2873) *before* it records the merge as needing file-level
//!    merging (`o->internal.nontrivial_merge = 1`, :2877). Up-to-dateness is
//!    decided from `stat` alone — never from content — so a file whose mtime
//!    moved while its bytes did not is "not uptodate" until something repairs
//!    the entry. That state is ordinary: an editor that rewrites in place, a
//!    `touch`, a copy that does not preserve times, a checkout on another
//!    machine. Without the refresh the trivial pre-pass dies with
//!    `error: Entry 'f.txt' not uptodate. Cannot merge.` where git reports
//!    `error: Merge requires file-level merging` (unpack-trees.c:2031) and
//!    carries on, and `git-merge-octopus`'s `read-tree -u -m --aggressive`
//!    (git-merge-octopus.sh:98) fails outright instead of falling through to
//!    `git merge-index -o git-merge-one-file -a` (:103). The strategy-loop
//!    refresh has to reach the index *file*, not just memory, because those two
//!    back-ends are separate programs.
//!
//! 2. **`--quiet` does not take the conflict notice away.**
//!    `suggest_conflicts()` ends in a bare `printf` with no verbosity test
//!    (builtin/merge.c:1065-1066), so `-q`, `--quiet` and `merge.verbosity=0`
//!    all still say the worktree is conflicted.
//!
//! 3. **stdout is flushed once the merge's messages are out.**
//!    `merge_display_update_messages()` closes with `diff_warn_rename_limit()`
//!    (merge-ort.c:4879-4881), which opens with `fflush(stdout)` *before*
//!    deciding whether it has a warning at all (diff.c:7038-7049). git's stdout
//!    is fully buffered when it is a pipe, so that incidental flush is the only
//!    reason `Auto-merging <path>` reaches a `2>&1` capture ahead of the stderr
//!    line that follows it.
//!
//! 4. **What a merge-base merge puts in the virtual ancestor.** merge-ort
//!    resolves a rename/rename(1to2) the same way at every `call_depth` —
//!    one content merge copied into *both* destinations
//!    (`memcpy(&side1->stages[1], …)` / `memcpy(&side2->stages[2], …)`,
//!    merge-ort.c:3026, 3043), the recursion level reaching only the marker
//!    width at :3017 — and it moves a file out of a directory's way at every
//!    `call_depth` too, gating only the blob left behind on
//!    `index = opt->priv->call_depth ? 0 : side` (merge-ort.c:4368). Resolving
//!    either with the ancestor instead hands the outer merge a base that still
//!    carries the old name, and the outer merge then re-raises a conflict git
//!    had already settled one level down.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", dir.join("nonexistent-global"))
        .env("GIT_CONFIG_SYSTEM", dir.join("nonexistent-system"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .env("LC_ALL", "C");
    c
}

fn run(dir: &Path, args: &[&str]) -> Output {
    cmd(dir, args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("the child exited normally")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Both streams into one file, the way a shell's `> log 2>&1` joins them, so the
/// order they were written in is what comes back. This is the only way to see a
/// flush: read separately, each stream is complete and in order whatever the
/// buffering did.
fn interleaved(dir: &Path, args: &[&str]) -> (String, i32) {
    let log = dir.join("interleaved.log");
    let file = std::fs::File::create(&log).unwrap();
    let dup = file.try_clone().unwrap();
    let status = cmd(dir, args)
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(dup))
        .status()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    let text = std::fs::read_to_string(&log).unwrap();
    std::fs::remove_file(&log).unwrap();
    (text, status.code().expect("the child exited normally"))
}

/// `git unpack-file`'s `mkstemp` suffix is random by construction; nothing else
/// in these fixtures is, so only that one name is normalised.
fn mask_temp_labels(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(".merge_file_") {
        out.push_str(&rest[..at]);
        out.push_str(".merge_file_X");
        rest = &rest[at + ".merge_file_".len()..];
        let skip = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        rest = &rest[skip..];
    }
    out.push_str(rest);
    out
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "zvcs-mergestale-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn init(tag: &str) -> PathBuf {
    let repo = temp_root(tag);
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    git(&repo, &["config", "core.eol", "lf"]);
    // rerere would add a stderr line of its own to the interleaving cases and
    // replay resolutions into the others; neither is what is under test here.
    git(&repo, &["config", "rerere.enabled", "false"]);
    repo
}

fn write(repo: &Path, path: &str, body: &str) {
    std::fs::write(repo.join(path), body).unwrap();
}

/// Move one worktree file's mtime back without touching a byte of it — the
/// "stat data is stale, content is not" state `refresh_index()` exists to
/// repair. An explicit timestamp rather than a re-write, so the case does not
/// depend on the filesystem's timestamp granularity or on which fields git was
/// built to trust.
fn age(repo: &Path, path: &str) {
    let before = std::fs::read(repo.join(path)).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(repo.join(path))
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_accessed(old).set_modified(old))
        .unwrap();
    assert_eq!(
        std::fs::read(repo.join(path)).unwrap(),
        before,
        "aging {path} must not change its content"
    );
}

/// `main` and `side` each rewrite the same line of `f.txt`, so every strategy
/// below has to reach file-level merging for it. `o1` and `o2` add a file each
/// and touch nothing else, which is what makes the octopus get as far as its
/// third head.
fn conflicting(tag: &str) -> PathBuf {
    let repo = init(tag);
    write(&repo, "f.txt", "l1\nl2\nl3\nl4\nl5\n");
    write(&repo, "k.txt", "k\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    for branch in ["side", "o1", "o2"] {
        git(&repo, &["branch", branch]);
    }
    git(&repo, &["checkout", "-q", "side"]);
    write(&repo, "f.txt", "l1\nl2\nSIDE\nl4\nl5\n");
    git(&repo, &["commit", "-qam", "side"]);
    git(&repo, &["checkout", "-q", "o1"]);
    write(&repo, "a.txt", "a\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "o1"]);
    git(&repo, &["checkout", "-q", "o2"]);
    write(&repo, "b.txt", "b\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "o2"]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "f.txt", "l1\nl2\nMAIN\nl4\nl5\n");
    git(&repo, &["commit", "-qam", "main"]);
    repo
}

/// The criss-cross both virtual-ancestor cases need: `x1` and `x2` each merge
/// the other side away with `-s ours`, so the two of them have two merge bases
/// and neither tree carries the other's change. `k.txt` is edited on both after
/// the merges purely so the outer merge has one honest content conflict to
/// report — that line is the control, and it must be the *only* one.
fn criss_cross(tag: &str, shape: &str) -> PathBuf {
    let repo = init(tag);
    write(&repo, "k.txt", "k\n");
    match shape {
        "rename" => write(&repo, "ren.txt", &(1..=40).map(|n| format!("r{n}\n")).collect::<String>()),
        "directory" => write(&repo, "df", "dfbase\n"),
        other => panic!("unknown shape {other}"),
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "side"]);

    match shape {
        // main renames the file one way, side renames it another:
        // rename/rename(1to2).
        "rename" => git(&repo, &["mv", "ren.txt", "A.txt"]),
        // main replaces the file with a directory of the same name, side edits
        // the file: a file/directory conflict wrapped around a modify/delete.
        _ => {
            git(&repo, &["rm", "-q", "df"]);
            std::fs::create_dir(repo.join("df")).unwrap();
            write(&repo, "df/in.txt", "inside\n");
            git(&repo, &["add", "-A"]);
            String::new()
        }
    };
    git(&repo, &["commit", "-qam", "mainchange"]);
    git(&repo, &["checkout", "-q", "side"]);
    match shape {
        "rename" => git(&repo, &["mv", "ren.txt", "B.txt"]),
        _ => {
            write(&repo, "df", "dfside\n");
            String::new()
        }
    };
    git(&repo, &["commit", "-qam", "sidechange"]);

    git(&repo, &["checkout", "-q", "-b", "x1", "main"]);
    git(&repo, &["merge", "-q", "-s", "ours", "side", "-m", "x1m"]);
    git(&repo, &["checkout", "-q", "-b", "x2", "side"]);
    git(&repo, &["merge", "-q", "-s", "ours", "main", "-m", "x2m"]);
    git(&repo, &["checkout", "-q", "x1"]);
    write(&repo, "k.txt", "k\ny\n");
    git(&repo, &["commit", "-qam", "x1k"]);
    git(&repo, &["checkout", "-q", "x2"]);
    write(&repo, "k.txt", "k\nz\n");
    git(&repo, &["commit", "-qam", "x2k"]);
    git(&repo, &["checkout", "-q", "x1"]);
    assert_eq!(
        git(&repo, &["merge-base", "--all", "x1", "x2"]).lines().count(),
        2,
        "the fixture is only a criss-cross if the two heads have two merge bases"
    );
    repo
}

/// The `allow_trivial` pre-pass over a stale-but-unchanged file. git refreshes
/// the entry first, so `threeway_merge()` gets past `verify_uptodate()` and
/// reaches the verdict that actually applies: this merge needs file-level
/// merging. `-s resolve` is the strategy that shows the whole chain, since
/// `git-merge-resolve` then prints each of its own steps.
#[test]
fn a_stale_stat_entry_still_reaches_the_file_level_merging_verdict() {
    let repo = conflicting("stale-trivial");
    age(&repo, "f.txt");

    let out = run(&repo, &["merge", "-s", "resolve", "side"]);
    assert_eq!(code(&out), 1);
    assert_eq!(
        stderr(&out),
        "error: Merge requires file-level merging\n\
         ERROR: content conflict in f.txt\n\
         fatal: merge program failed\n",
        "the refusal is unpack_trees()' trivial-merge verdict, not verify_uptodate()'s"
    );
    assert!(
        !stderr(&out).contains("not uptodate"),
        "a file whose bytes never changed is not a reason to refuse: {}",
        stderr(&out)
    );
    assert_eq!(
        stdout(&out),
        "Trying really trivial in-index merge...\n\
         Nope.\n\
         Trying simple merge.\n\
         Simple merge failed, trying Automatic merge.\n\
         Auto-merging f.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    // The merge really happened: the back-end's own conflict markers are in the
    // worktree, carrying `git unpack-file`'s temporary names.
    let f = mask_temp_labels(&std::fs::read_to_string(repo.join("f.txt")).unwrap());
    assert_eq!(
        f,
        "l1\nl2\n\
         <<<<<<< .merge_file_X\n\
         MAIN\n\
         =======\n\
         SIDE\n\
         >>>>>>> .merge_file_X\n\
         l4\nl5\n"
    );
}

/// The same stale entry through the octopus, which is where the missing refresh
/// cost a merge rather than a message: `git-merge-octopus` reads the index off
/// disk, so its `read-tree -u -m --aggressive` refuses the third head and the
/// whole octopus dies at exit 2 with nothing merged, instead of falling through
/// to `git merge-index -o git-merge-one-file -a` and leaving a conflict to fix.
#[test]
fn a_stale_stat_entry_does_not_abort_the_octopus_before_its_last_head() {
    let repo = conflicting("stale-octopus");
    age(&repo, "f.txt");

    let out = run(&repo, &["merge", "o1", "o2", "side"]);
    assert_eq!(code(&out), 1, "a hand-resolvable last head is exit 1, not 2");
    assert_eq!(
        stdout(&out),
        "Trying simple merge with o1\n\
         Trying simple merge with o2\n\
         Trying simple merge with side\n\
         Simple merge did not work, trying automatic merge.\n\
         Auto-merging f.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(
        stderr(&out),
        "ERROR: content conflict in f.txt\nfatal: merge program failed\n"
    );
    assert!(
        !stderr(&out).contains("Merge with strategy octopus failed"),
        "the octopus handled this merge: {}",
        stderr(&out)
    );
    // The first two heads are merged and staged, and only `f.txt` is left
    // conflicted — the state a failed octopus would never have produced.
    assert_eq!(
        git(&repo, &["ls-files", "--stage"])
            .lines()
            .map(|l| l.split('\t').next_back().unwrap().to_string())
            .collect::<Vec<_>>()
            .join(" "),
        "a.txt b.txt f.txt f.txt f.txt k.txt"
    );
    assert_eq!(
        git(&repo, &["ls-files", "--unmerged"]).lines().count(),
        3,
        "only f.txt is unmerged, at all three stages"
    );
}

/// `suggest_conflicts()`'s closing line is a `printf` with no verbosity test.
/// Every quiet spelling keeps it, and it is the only thing they keep.
#[test]
fn quiet_still_says_the_merge_failed() {
    for quiet in [
        vec!["merge", "-q", "side"],
        vec!["merge", "--quiet", "side"],
        vec!["-c", "merge.verbosity=0", "merge", "side"],
    ] {
        let repo = conflicting(&format!("quiet-{}", quiet.join("-").replace(['=', '.'], "_")));
        let out = run(&repo, &quiet);
        assert_eq!(code(&out), 1, "{quiet:?}");
        assert!(
            stdout(&out).ends_with("Automatic merge failed; fix conflicts and then commit the result.\n"),
            "{quiet:?} dropped the notice: {:?}",
            stdout(&out)
        );
    }
    // `merge.verbosity=0` keeps *only* that line: the engine's own messages are
    // what the verbosity knob silences.
    let repo = conflicting("quiet-verbosity-only");
    let out = run(&repo, &["-c", "merge.verbosity=0", "merge", "side"]);
    assert_eq!(
        stdout(&out),
        "Automatic merge failed; fix conflicts and then commit the result.\n"
    );
}

/// With both streams pointed at one file, git's `Auto-merging <path>` still
/// lands ahead of the stderr line that comes after it, because
/// `merge_display_update_messages()` ends in a `fflush(stdout)` it inherits
/// from `diff_warn_rename_limit()`. Without that flush the stdout block waits
/// for `exit()` and the whole capture reads back inside out.
#[test]
fn the_message_block_is_flushed_before_the_next_stderr_line() {
    let repo = init("flush-order");
    write(&repo, "g.txt", "g1\ng2\ng3\ng4\ng5\ng6\ng7\ng8\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "side"]);
    git(&repo, &["checkout", "-q", "side"]);
    write(&repo, "g.txt", "g1\ng2\ng3\ng4\ng5\ng6\ng7\ng8-side\n");
    git(&repo, &["commit", "-qam", "side"]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "g.txt", "g1-main\ng2\ng3\ng4\ng5\ng6\ng7\ng8\n");
    git(&repo, &["commit", "-qam", "main"]);

    // A clean merge, so the only stderr is `finish()`'s parting line
    // (builtin/merge.c:1869) and the ordering question is unambiguous.
    let (text, status) = interleaved(&repo, &["merge", "--no-commit", "side"]);
    assert_eq!(status, 0);
    assert_eq!(
        text,
        "Auto-merging g.txt\n\
         Automatic merge went well; stopped before committing as requested\n"
    );

    // And a conflicting one, where the notice `suggest_conflicts()` prints last
    // must still come last.
    let repo = conflicting("flush-order-conflict");
    let (text, status) = interleaved(&repo, &["merge", "--no-commit", "side"]);
    assert_eq!(status, 1);
    assert_eq!(
        text,
        "Auto-merging f.txt\n\
         CONFLICT (content): Merge conflict in f.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
}

/// A criss-cross whose two merge bases disagree about a rename/rename(1to2).
/// merge-ort settles it one level down by writing the content merge to both new
/// names, so the outer merge sees `A.txt` unchanged-in-ours/deleted-in-theirs
/// and `B.txt` the mirror of that, deletes both, and has nothing to say about
/// either. Resolving the inner conflict with the ancestor instead leaves
/// `ren.txt` in the virtual base, and the outer merge re-detects the rename.
#[test]
fn a_recursive_base_settles_rename_rename_instead_of_deferring_it() {
    let repo = criss_cross("cc-rename", "rename");

    let out = run(&repo, &["merge-tree", "--write-tree", "x1", "x2"]);
    assert_eq!(code(&out), 1);
    assert_eq!(
        stdout(&out),
        "5b7592989cda4e6a4c290c7d1947154fba0c6730\n\
         100644 b68fde2a051d9af2fe3ff4c96c0898e5a3212e4d 1\tk.txt\n\
         100644 e8d3f5436ed8fc254d849111fea4294869b8e9bd 2\tk.txt\n\
         100644 d3b03cc5a6aaa10d44667267fb4669f870825c42 3\tk.txt\n\
         \n\
         Auto-merging k.txt\n\
         CONFLICT (content): Merge conflict in k.txt\n",
        "the merged tree is byte-for-byte git's, renames included"
    );

    let out = run(&repo, &["merge", "x2"]);
    assert_eq!(code(&out), 1);
    assert_eq!(
        stdout(&out),
        "Auto-merging k.txt\n\
         CONFLICT (content): Merge conflict in k.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n",
        "the rename was decided in the virtual ancestor and is not reported again"
    );
    assert_eq!(
        git(&repo, &["ls-files", "--stage"])
            .lines()
            .map(|l| l.split('\t').next_back().unwrap().to_string())
            .collect::<Vec<_>>()
            .join(" "),
        "k.txt k.txt k.txt",
        "neither destination survives: each is deleted on the side that did not make it"
    );
    for gone in ["A.txt", "B.txt", "ren.txt"] {
        assert!(!repo.join(gone).exists(), "{gone} should not be in the worktree");
    }
}

/// The same criss-cross around a file replaced by a directory. merge-ort moves
/// the file aside at every recursion level, so the virtual ancestor carries the
/// directory plus a `df~<branch>` holding the *ancestor's* blob, and the outer
/// merge simply takes `side`'s file back. Keeping the ancestor's `df` whole
/// instead drops the directory and re-raises the file/directory conflict.
#[test]
fn a_recursive_base_moves_a_file_out_of_a_directorys_way() {
    let repo = criss_cross("cc-directory", "directory");

    let out = run(&repo, &["merge-tree", "--write-tree", "x1", "x2"]);
    assert_eq!(code(&out), 1);
    assert_eq!(
        stdout(&out),
        "5511e3ddf2c27421f69e69b3d62fd5c423aec93b\n\
         100644 b68fde2a051d9af2fe3ff4c96c0898e5a3212e4d 1\tk.txt\n\
         100644 e8d3f5436ed8fc254d849111fea4294869b8e9bd 2\tk.txt\n\
         100644 d3b03cc5a6aaa10d44667267fb4669f870825c42 3\tk.txt\n\
         \n\
         Auto-merging k.txt\n\
         CONFLICT (content): Merge conflict in k.txt\n"
    );

    let out = run(&repo, &["merge", "x2"]);
    assert_eq!(code(&out), 1);
    assert_eq!(
        stdout(&out),
        "Auto-merging k.txt\n\
         CONFLICT (content): Merge conflict in k.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n",
        "no file/directory conflict: the virtual ancestor already had the directory"
    );
    assert_eq!(
        git(&repo, &["ls-files", "--stage"]),
        "100644 610dbd210bf840c13f8de74241d759a1c16588a1 0\tdf\n\
         100644 b68fde2a051d9af2fe3ff4c96c0898e5a3212e4d 1\tk.txt\n\
         100644 e8d3f5436ed8fc254d849111fea4294869b8e9bd 2\tk.txt\n\
         100644 d3b03cc5a6aaa10d44667267fb4669f870825c42 3\tk.txt\n",
        "`df` comes back as side's plain file, at stage 0"
    );
    assert_eq!(std::fs::read_to_string(repo.join("df")).unwrap(), "dfside\n");
    assert!(
        !repo.join("df/in.txt").exists(),
        "the directory is gone with the side that dropped it"
    );
}
