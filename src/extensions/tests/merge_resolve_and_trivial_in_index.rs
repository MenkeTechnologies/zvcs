//! The two merge paths `-s` had been folding onto merge-ort: the `allow_trivial`
//! in-index merge and the out-of-process back-ends.
//!
//! Everything asserted here was read off `/opt/homebrew/bin/git` 2.55.0 —
//! stdout, stderr, exit status, index stages and worktree bytes — with
//! `HOME`/`GIT_CONFIG_*` pinned so the host's own configuration cannot reach the
//! fixture.
//!
//! What the cases separate:
//!
//! 1. **`-s octopus` over one head.** `all_strategy[]` gives `octopus` no
//!    `NO_TRIVIAL` (builtin/merge.c:103), so git tries the in-index merge first
//!    and then `git-merge-octopus`, whose "Reject if this is not an octopus"
//!    guard exits 2 over a single remote. Trivially mergeable ⇒ a merge commit
//!    announced as `In-index merge`; anything else ⇒ exit 2 with nothing moved.
//!    This build ran merge-ort and *committed* in both cases, which is the wrong
//!    merge result rather than the wrong message.
//! 2. **A two-head engine over several heads.** `if (!use_strategies)` picks the
//!    octopus only when no `-s` was given (builtin/merge.c:1600-1606); a named
//!    `-s ort` is used for three heads too and refuses them
//!    (builtin/merge.c:806-808). This build octopused and committed.
//! 3. **`-s resolve`.** It was refused outright with a line git never prints.
//!    It is now `git-merge-resolve.sh`'s chain — `read-tree -u -m --aggressive`,
//!    then `write-tree`, then `merge-index -o git-merge-one-file -a` — so the
//!    conflict markers carry `git unpack-file`'s `.merge_file_XXXXXX` names,
//!    which is the one thing a re-derived merge cannot produce.
//! 4. **A criss-cross history through `-s resolve`.** Two merge bases make a
//!    four-tree `read-tree`, where `unpack_single_entry()` files every tree
//!    before the head under stage 1, the head under 2 and the rest under 3
//!    (unpack-trees.c:1211-1226). This build filed the head under `head_idx`
//!    itself — 3 for four trees — leaving an index with two stage-3 entries and
//!    no stage 2.
//! 5. **Where the index guard fires.** The `allow_trivial` block's own
//!    `repo_index_has_changes()` refusal (builtin/merge.c:1712-1719) is
//!    `error:` alone; the back-end's `diff-index` pre-flight is a different
//!    message on a different stream, and `--no-commit` is what decides which one
//!    a staged change meets.
//! 6. **`-X` on the strategy plumbing.** `cmd_merge_recursive` feeds `--<value>`
//!    to the same `parse_merge_opt()` the porcelain runs (builtin/merge-recursive.c:55-58).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
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
        .env("LC_ALL", "C")
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

fn read(dir: &Path, path: &str) -> String {
    std::fs::read_to_string(dir.join(path)).unwrap()
}

/// `git unpack-file`'s `mkstemp` suffix is random by construction, so the labels
/// are normalised before comparison. Anything that is *not* a `.merge_file_`
/// name — `HEAD`, an object id — survives this and fails the assertion.
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
        "zvcs-mergeres-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// A repository with `core.autocrlf`/`core.eol` pinned, so a `\r` in a fixture
/// survives and a checkout on a CI host that defaults differently still writes
/// the bytes these assertions were measured against.
fn init(tag: &str) -> PathBuf {
    let repo = temp_root(tag);
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    git(&repo, &["config", "core.eol", "lf"]);
    git(&repo, &["config", "rerere.enabled", "false"]);
    repo
}

fn write(repo: &Path, path: &str, body: &str) {
    std::fs::write(repo.join(path), body).unwrap();
}

/// `main` and `side` each change a *different* file, so a three-way merge needs
/// no file-level merging at all — `threeway_merge()`'s `#5ALT`/`#13` arms
/// resolve every path and `trivial_merges_only` is satisfied.
fn disjoint(tag: &str) -> PathBuf {
    let repo = init(tag);
    write(&repo, "f1.txt", "a\nb\nc\n");
    write(&repo, "f2.txt", "x\ny\nz\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "side"]);
    write(&repo, "f1.txt", "A\nb\nc\n");
    git(&repo, &["commit", "-qam", "ours"]);
    git(&repo, &["checkout", "-q", "side"]);
    write(&repo, "f2.txt", "x\ny\nZ\n");
    git(&repo, &["commit", "-qam", "theirs"]);
    git(&repo, &["checkout", "-q", "main"]);
    repo
}

/// Both branches touch the same file in non-overlapping places: the trivial
/// merge declines (`Merge requires file-level merging`) but a content merge is
/// clean.
fn same_file(tag: &str) -> PathBuf {
    let repo = init(tag);
    write(&repo, "f.txt", "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "side"]);
    write(&repo, "f.txt", "L1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n");
    git(&repo, &["commit", "-qam", "ours"]);
    git(&repo, &["checkout", "-q", "side"]);
    write(&repo, "f.txt", "l1\nl2\nl3\nl4\nl5\nl6\nl7\nL8\n");
    git(&repo, &["commit", "-qam", "theirs"]);
    git(&repo, &["checkout", "-q", "main"]);
    repo
}

/// Both branches rewrite the same line — a real content conflict.
fn conflicting(tag: &str) -> PathBuf {
    let repo = init(tag);
    write(&repo, "f.txt", "l1\nl2\nl3\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "side"]);
    write(&repo, "f.txt", "ours\nl2\nl3\n");
    git(&repo, &["commit", "-qam", "ours"]);
    git(&repo, &["checkout", "-q", "side"]);
    write(&repo, "f.txt", "theirs\nl2\nl3\n");
    git(&repo, &["commit", "-qam", "theirs"]);
    git(&repo, &["checkout", "-q", "main"]);
    repo
}

/// Three tips over one base, for the head-count dispatch.
fn three_heads(tag: &str) -> PathBuf {
    let repo = init(tag);
    write(&repo, "a.txt", "a\n");
    write(&repo, "b.txt", "b\n");
    write(&repo, "c.txt", "c\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "s1"]);
    git(&repo, &["branch", "s2"]);
    git(&repo, &["checkout", "-q", "s1"]);
    write(&repo, "a.txt", "A\n");
    git(&repo, &["commit", "-qam", "s1"]);
    git(&repo, &["checkout", "-q", "s2"]);
    write(&repo, "b.txt", "B\n");
    git(&repo, &["commit", "-qam", "s2"]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "c.txt", "C\n");
    git(&repo, &["commit", "-qam", "m"]);
    repo
}

/// `main` and `b` each merge the *other's tip commit* — not the branch, which
/// would fast-forward — so `merge-base --all` reports two bases and the merge
/// reads four trees.
fn criss_cross(tag: &str) -> PathBuf {
    let repo = init(tag);
    write(&repo, "f.txt", "l1\nl2\nl3\nl4\n");
    write(&repo, "g.txt", "p\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "b"]);
    write(&repo, "f.txt", "A1\nl2\nl3\nl4\n");
    git(&repo, &["commit", "-qam", "a1"]);
    let a1 = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    git(&repo, &["checkout", "-q", "b"]);
    write(&repo, "f.txt", "l1\nl2\nl3\nB4\n");
    git(&repo, &["commit", "-qam", "b1"]);
    let b1 = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["merge", "-q", "--no-edit", &b1]);
    git(&repo, &["checkout", "-q", "b"]);
    git(&repo, &["merge", "-q", "--no-edit", &a1]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "f.txt", "A1\nl2\nZZ\nB4\n");
    git(&repo, &["commit", "-qam", "a2"]);
    git(&repo, &["checkout", "-q", "b"]);
    write(&repo, "f.txt", "A1\nQQ\nl3\nB4\n");
    git(&repo, &["commit", "-qam", "b2"]);
    git(&repo, &["checkout", "-q", "main"]);
    assert_eq!(
        git(&repo, &["merge-base", "--all", "HEAD", "b"]).lines().count(),
        2,
        "the fixture must produce two merge bases or it tests nothing"
    );
    repo
}

/// `-s octopus` over a single head, trivially mergeable: `merge_trivial()`
/// commits the index `read_tree_trivial()` wrote and `finish()` announces it as
/// `In-index merge` rather than `Merge made by the '…' strategy.`
/// (builtin/merge.c:1007). The tree is the one the trivial merge produced, and
/// the commit has both parents.
#[test]
fn octopus_over_one_head_takes_the_in_index_merge() {
    let repo = disjoint("oct-trivial");
    let before = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    let out = run(&repo, &["merge", "--no-edit", "-s", "octopus", "side"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Trying really trivial in-index merge...\n\
         Wonderful.\n\
         In-index merge\n \
         f2.txt | 2 +-\n \
         1 file changed, 1 insertion(+), 1 deletion(-)\n"
    );
    assert_eq!(stderr(&out), "");

    // Both sides' changes are in the result, and both tips are parents.
    assert_eq!(read(&repo, "f1.txt"), "A\nb\nc\n");
    assert_eq!(read(&repo, "f2.txt"), "x\ny\nZ\n");
    let parents = git(&repo, &["rev-list", "--parents", "-1", "HEAD"]);
    assert_eq!(parents.split_whitespace().count(), 3, "{parents}");
    assert!(parents.contains(&before));
    // Nothing is left conflicted or in progress.
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    assert!(!repo.join(".git/MERGE_HEAD").exists());
    // `finish()` puts its `msg` in the reflog too, not the strategy name.
    assert_eq!(
        git(&repo, &["reflog", "show", "HEAD", "--format=%gs"]).lines().next(),
        Some("merge side: In-index merge")
    );
}

/// `-s octopus` over a single head that needs file-level merging: the trivial
/// pre-pass fails (`unpack_failed()`, unpack-trees.c:2031) and
/// `git-merge-octopus` declines a one-head merge outright, so `cmd_merge` finds
/// no `best_strategy`. Exit 2 with the repository exactly as it was — the case
/// that used to run merge-ort and commit.
#[test]
fn octopus_over_one_head_refuses_a_file_level_merge() {
    for (tag, repo) in [
        ("oct-nontrivial", same_file("oct-nontrivial")),
        ("oct-conflict", conflicting("oct-conflict")),
    ] {
        let before = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
        let tree_before = git(&repo, &["rev-parse", "HEAD^{tree}"]).trim().to_string();
        let index_before = git(&repo, &["ls-files", "-s"]);
        let file_before = read(&repo, "f.txt");

        let out = run(&repo, &["merge", "--no-edit", "-s", "octopus", "side"]);
        assert_eq!(code(&out), 2, "{tag}: {}", stderr(&out));
        assert_eq!(
            stdout(&out),
            "Trying really trivial in-index merge...\nNope.\n",
            "{tag}"
        );
        assert_eq!(
            stderr(&out),
            "error: Merge requires file-level merging\n\
             Merge with strategy octopus failed.\n",
            "{tag}"
        );

        // The wrong-result guard: no commit, no moved tree, no touched index or
        // worktree, no half-finished merge.
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), before, "{tag}");
        assert_eq!(git(&repo, &["rev-parse", "HEAD^{tree}"]).trim(), tree_before, "{tag}");
        assert_eq!(git(&repo, &["ls-files", "-s"]), index_before, "{tag}");
        assert_eq!(read(&repo, "f.txt"), file_before, "{tag}");
        assert!(!repo.join(".git/MERGE_HEAD").exists(), "{tag}");
        assert_eq!(git(&repo, &["status", "--porcelain"]), "", "{tag}");
        // A failed strategy writes no reflog entry (only the *index* guards do).
        assert_eq!(
            git(&repo, &["reflog", "show", "HEAD", "--format=%gs"]).lines().next(),
            Some("checkout: moving from side to main"),
            "{tag}"
        );
    }
}

/// A named two-head strategy over three heads. `merge_ort_recursive()`'s caller
/// refuses (`error: Not handling anything other than two heads merge.`,
/// builtin/merge.c:806-807) and `git-merge-resolve`'s octopus guard declines in
/// silence; either way the merge fails with 2. Only a *default* `git merge a b`
/// still octopuses.
#[test]
fn a_named_two_head_strategy_refuses_three_heads() {
    for (name, extra_stderr) in [
        ("ort", "error: Not handling anything other than two heads merge.\n"),
        ("recursive", "error: Not handling anything other than two heads merge.\n"),
        ("subtree", "error: Not handling anything other than two heads merge.\n"),
        ("resolve", ""),
    ] {
        let repo = three_heads(&format!("multi-{name}"));
        let before = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

        let out = run(&repo, &["merge", "--no-edit", "-s", name, "s1", "s2"]);
        assert_eq!(code(&out), 2, "-s {name}: {}", stderr(&out));
        assert_eq!(stdout(&out), "", "-s {name}");
        assert_eq!(
            stderr(&out),
            format!("{extra_stderr}Merge with strategy {name} failed.\n"),
            "-s {name}"
        );
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), before, "-s {name}");
        assert_eq!(read(&repo, "a.txt"), "a\n", "-s {name}");
        assert_eq!(read(&repo, "b.txt"), "b\n", "-s {name}");
        assert!(!repo.join(".git/MERGE_HEAD").exists(), "-s {name}");
        // `refs_update_ref("updating ORIG_HEAD", …)` runs before the strategy.
        assert_eq!(git(&repo, &["rev-parse", "ORIG_HEAD"]).trim(), before, "-s {name}");
    }

    // The default is still the octopus, and it still succeeds.
    let repo = three_heads("multi-default");
    let out = run(&repo, &["merge", "--no-edit", "s1", "s2"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("Merge made by the 'octopus' strategy."), "{}", stdout(&out));
    assert_eq!(read(&repo, "a.txt"), "A\n");
    assert_eq!(read(&repo, "b.txt"), "B\n");
    assert_eq!(read(&repo, "c.txt"), "C\n");
}

/// `-s resolve` end to end on a merge the trivial pre-pass declines but
/// `git-merge-one-file` resolves: the script's framing, `merge-index`'s
/// `Auto-merging`, and a commit announced with the strategy name.
#[test]
fn resolve_runs_the_script_chain_and_commits_a_clean_result() {
    let repo = same_file("res-clean");

    let out = run(&repo, &["merge", "--no-edit", "-s", "resolve", "side"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Trying really trivial in-index merge...\n\
         Nope.\n\
         Trying simple merge.\n\
         Simple merge failed, trying Automatic merge.\n\
         Auto-merging f.txt\n\
         Merge made by the 'resolve' strategy.\n \
         f.txt | 2 +-\n \
         1 file changed, 1 insertion(+), 1 deletion(-)\n"
    );
    assert_eq!(stderr(&out), "error: Merge requires file-level merging\n");

    assert_eq!(read(&repo, "f.txt"), "L1\nl2\nl3\nl4\nl5\nl6\nl7\nL8\n");
    // The committed tree is the merged index, not either side's tree.
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).trim().to_string();
    for side in ["HEAD^1^{tree}", "HEAD^2^{tree}"] {
        assert_ne!(git(&repo, &["rev-parse", side]).trim(), tree);
    }
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&repo, &["reflog", "show", "HEAD", "--format=%gs"]).lines().next(),
        Some("merge side: Merge made by the 'resolve' strategy.")
    );
}

/// A conflicted `-s resolve`. The markers are labelled with `git unpack-file`'s
/// temporary names, which is only reachable by actually running
/// `merge-index -o git-merge-one-file`: a re-derived tree merge would label them
/// `HEAD` and the merged head's id.
#[test]
fn resolve_conflicts_carry_merge_file_labels_and_three_stages() {
    let repo = conflicting("res-conflict");
    let before = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    let out = run(&repo, &["merge", "--no-edit", "-s", "resolve", "side"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Trying really trivial in-index merge...\n\
         Nope.\n\
         Trying simple merge.\n\
         Simple merge failed, trying Automatic merge.\n\
         Auto-merging f.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(
        stderr(&out),
        "error: Merge requires file-level merging\n\
         ERROR: content conflict in f.txt\n\
         fatal: merge program failed\n"
    );

    assert_eq!(
        mask_temp_labels(&read(&repo, "f.txt")),
        "<<<<<<< .merge_file_X\nours\n=======\ntheirs\n>>>>>>> .merge_file_X\nl2\nl3\n"
    );
    let stages: Vec<String> = git(&repo, &["ls-files", "-s"])
        .lines()
        .map(|l| l.split_whitespace().nth(2).unwrap().to_string())
        .collect();
    assert_eq!(stages, ["1", "2", "3"]);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), before);
    for name in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE"] {
        assert!(repo.join(".git").join(name).exists(), "{name}");
    }
    assert!(!repo.join(".git/AUTO_MERGE").exists(), "the script back-ends write no AUTO_MERGE");
}

/// Two merge bases: `read-tree` reads four trees, and the stage each kept entry
/// carries is 1 for every tree before the head, 2 for the head and 3 beyond
/// (unpack-trees.c:1211-1226) — *not* the slot number. Filing the head under
/// `head_idx` produced two stage-3 entries and no stage 2, which is an index
/// `git checkout --ours` cannot read.
///
/// The trivial pre-pass is skipped here: its `!common->next` guard
/// (builtin/merge.c:1699) fails with several bases.
#[test]
fn resolve_over_two_merge_bases_stages_the_head_at_two() {
    let repo = criss_cross("res-crisscross");

    let out = run(&repo, &["merge", "--no-edit", "-s", "resolve", "b"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Trying simple merge.\n\
         Simple merge failed, trying Automatic merge.\n\
         Auto-merging f.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );

    let listing = git(&repo, &["ls-files", "-s"]);
    let conflicted: Vec<(&str, &str)> = listing
        .lines()
        .map(|l| {
            let mut f = l.split_whitespace();
            let id = f.nth(1).unwrap();
            (f.next().unwrap(), id)
        })
        .filter(|(_, _)| true)
        .collect();
    let stages: Vec<&str> = conflicted.iter().map(|(s, _)| *s).collect();
    assert_eq!(stages, ["1", "2", "3", "0"], "{listing}");

    // Stage 2 must be *our* blob and stage 3 *theirs*; the old bug put our blob
    // at stage 3, which the stage numbers alone would not have caught.
    let ours = git(&repo, &["rev-parse", "HEAD:f.txt"]).trim().to_string();
    let theirs = git(&repo, &["rev-parse", "b:f.txt"]).trim().to_string();
    assert_eq!(conflicted[1].1, ours, "stage 2 is HEAD's blob");
    assert_eq!(conflicted[2].1, theirs, "stage 3 is the merged head's blob");

    assert_eq!(
        mask_temp_labels(&read(&repo, "f.txt")),
        "A1\n<<<<<<< .merge_file_X\nl2\nZZ\n=======\nQQ\nl3\n>>>>>>> .merge_file_X\nB4\n"
    );
    assert_eq!(read(&repo, "g.txt"), "p\n");
}

/// Which index guard a staged change meets depends on `option_commit`. With it
/// set, the `allow_trivial` block's own `repo_index_has_changes()` refuses first
/// — `error:` on stderr, paths space-joined, and no `Merge with strategy …`
/// line. `--no-commit` skips the whole block, so the change reaches
/// `git-merge-resolve`'s `diff-index` pre-flight instead: a different message,
/// on stdout, four-space indented, followed by the failure line.
#[test]
fn a_staged_change_meets_a_different_guard_with_and_without_no_commit() {
    let repo = disjoint("res-staged");
    write(&repo, "f1.txt", "zz");
    git(&repo, &["add", "f1.txt"]);
    let out = run(&repo, &["merge", "--no-edit", "-s", "resolve", "side"]);
    assert_eq!(code(&out), 2);
    assert_eq!(stdout(&out), "");
    assert_eq!(
        stderr(&out),
        "error: Your local changes to the following files would be overwritten by merge:\n  f1.txt\n"
    );
    // No reflog entry: only merge-ort's own index guard logs `updating HEAD`.
    assert_eq!(
        git(&repo, &["reflog", "show", "HEAD", "--format=%gs"]).lines().next(),
        Some("checkout: moving from side to main")
    );

    let repo = disjoint("res-staged-nc");
    write(&repo, "f1.txt", "zz");
    git(&repo, &["add", "f1.txt"]);
    let out = run(&repo, &["merge", "--no-edit", "--no-commit", "-s", "resolve", "side"]);
    assert_eq!(code(&out), 2);
    assert_eq!(
        stdout(&out),
        "Error: Your local changes to the following files would be overwritten by merge\n    f1.txt\n"
    );
    assert_eq!(stderr(&out), "Merge with strategy resolve failed.\n");
}

/// Both lines of the pre-pass are bare `printf`s (builtin/merge.c:1723, 1730)
/// and `Wonderful.` likewise (builtin/merge.c:1000), so `-q` keeps all three and
/// silences only `finish()`'s message and diffstat.
#[test]
fn quiet_silences_the_finish_line_but_not_the_pre_pass() {
    let repo = disjoint("res-quiet");
    let out = run(&repo, &["merge", "--no-edit", "-q", "-s", "resolve", "side"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "Trying really trivial in-index merge...\nWonderful.\n");
    assert_eq!(stderr(&out), "");
    assert_eq!(read(&repo, "f2.txt"), "x\ny\nZ\n");
}

/// `--no-commit` and `--squash` clear `option_commit`, which is one of the
/// `allow_trivial` block's four conditions (builtin/merge.c:1701) — so neither
/// prints the pre-pass, and `-s octopus` goes straight to the back-end that
/// declines a single head.
#[test]
fn option_commit_gates_the_pre_pass() {
    for flag in ["--no-commit", "--squash"] {
        let repo = disjoint(&format!("gate{}", flag.trim_start_matches('-')));
        let out = run(&repo, &["merge", "--no-edit", flag, "-s", "octopus", "side"]);
        assert_eq!(code(&out), 2, "{flag}: {}", stderr(&out));
        assert_eq!(stdout(&out), "", "{flag}");
        assert_eq!(stderr(&out), "Merge with strategy octopus failed.\n", "{flag}");
        assert_eq!(read(&repo, "f2.txt"), "x\ny\nz\n", "{flag}");
    }

    // `-s resolve` reaches its back-end there instead, which merges cleanly and
    // stops before committing.
    let repo = disjoint("gate-resolve-nc");
    let out = run(&repo, &["merge", "--no-edit", "--no-commit", "-s", "resolve", "side"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "Trying simple merge.\n");
    assert_eq!(
        stderr(&out),
        "Automatic merge went well; stopped before committing as requested\n"
    );
    assert_eq!(read(&repo, "f2.txt"), "x\ny\nZ\n");
    assert!(repo.join(".git/MERGE_HEAD").exists());
}

/// `-X` on `-s resolve` is not parsed by merge: `try_merge_command()` re-spells
/// it `--<value>` on the back-end's command line (merge.c:31-32), the script
/// interpolates it unquoted into `git read-tree`, and read-tree's own scan
/// rejects it — `|| exit 2`.
#[test]
fn a_strategy_option_reaches_resolve_as_a_read_tree_argument() {
    let repo = conflicting("res-xours");
    let out = run(&repo, &["merge", "--no-edit", "-s", "resolve", "-Xours", "side"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert_eq!(stdout(&out), "Trying really trivial in-index merge...\nNope.\n");
    let err = stderr(&out);
    assert!(err.starts_with("error: Merge requires file-level merging\n"), "{err}");
    assert!(err.contains("error: unknown option `ours'\n"), "{err}");
    assert!(err.contains("usage: git read-tree "), "{err}");
    assert!(err.ends_with("Merge with strategy resolve failed.\n"), "{err}");
    // Untouched: read-tree refused before it wrote anything.
    assert_eq!(read(&repo, "f.txt"), "ours\nl2\nl3\n");
    assert!(!repo.join(".git/MERGE_HEAD").exists());
}

/// The strategy plumbing runs the same `parse_merge_opt()`
/// (builtin/merge-recursive.c:55-58), so `--ours`/`--theirs` and the
/// `--ignore-*-space*` family reach the merge there too. They used to be
/// refused, which made `git merge-recursive --ours` fail where `git merge -s
/// recursive -Xours` succeeded.
#[test]
fn the_strategy_plumbing_honours_the_same_merge_options() {
    for cmd in ["merge-recursive", "merge-subtree", "merge-recursive-ours"] {
        let repo = conflicting(&format!("plumb-{cmd}"));
        let base = git(&repo, &["merge-base", "HEAD", "side"]).trim().to_string();
        let side = git(&repo, &["rev-parse", "side"]).trim().to_string();

        let out = run(&repo, &[cmd, "--ours", &base, "--", "HEAD", &side]);
        assert_eq!(code(&out), 0, "{cmd} --ours: {}", stderr(&out));
        assert_eq!(stdout(&out), "Auto-merging f.txt\n", "{cmd} --ours");
        assert_eq!(read(&repo, "f.txt"), "ours\nl2\nl3\n", "{cmd} --ours");

        let repo = conflicting(&format!("plumbt-{cmd}"));
        let out = run(&repo, &[cmd, "--theirs", &base, "--", "HEAD", &side]);
        assert_eq!(code(&out), 0, "{cmd} --theirs: {}", stderr(&out));
        assert_eq!(read(&repo, "f.txt"), "theirs\nl2\nl3\n", "{cmd} --theirs");

        // An option git itself rejects is still git's own refusal.
        let repo = conflicting(&format!("plumbx-{cmd}"));
        let out = run(&repo, &[cmd, "--nosuchopt", &base, "--", "HEAD", &side]);
        assert_eq!(code(&out), 128, "{cmd} --nosuchopt");
        assert_eq!(stderr(&out), "fatal: unknown option --nosuchopt\n", "{cmd}");
    }
}
