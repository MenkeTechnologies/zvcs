//! `git merge` with more than one `-s`, and the two rename-shaped divergences
//! next to it.
//!
//! Every expectation here was read off `/opt/homebrew/bin/git` 2.55.0 — stdout,
//! stderr, exit status, `HEAD^{tree}`, the commit's parents, the index stages
//! and the worktree bytes — with `HOME`/`GIT_CONFIG_*` pinned so the host's own
//! configuration cannot reach the fixture, and `core.autocrlf`/`core.eol`/
//! `rerere.enabled` pinned so a CI host that defaults differently still writes
//! the bytes these assertions were measured against.
//!
//! What the cases separate:
//!
//! 1. **The loop exists at all.** `cmd_merge` keeps every `-s` in
//!    `use_strategies` and walks them in order, rewinding between attempts with
//!    `restore_state()` and keeping the one `evaluate_result()` scores best
//!    (builtin/merge.c:1778-1859). Reading only the last `-s` is not a missing
//!    message: `git merge -s ours -s resolve` commits an `ours` merge in stock
//!    and left a conflicted `resolve` merge here, which is a different tree.
//! 2. **The rewind is a real reset.** `restore_state()` is `read-tree -v --reset
//!    -u <head>` followed by `stash apply --index` (builtin/merge.c:403-427). A
//!    loop that only *printed* `Rewinding the tree to pristine...` would leave
//!    the first strategy's conflict markers under the second strategy's report,
//!    so the marker labels are what these cases assert.
//! 3. **`evaluate_result()`'s tie-break is `<=`, not `<`.** On an equal score the
//!    *later* strategy wins and its result is already in the worktree, so no
//!    `Using the … strategy to prepare resolving by hand.` line is printed
//!    (builtin/merge.c:1814-1819).
//! 4. **The attribute unions are over the whole list, not the winner.**
//!    `NO_TRIVIAL` on any `-s` suppresses the in-index pre-pass and
//!    `NO_FAST_FORWARD` on any `-s` forces a merge commit
//!    (builtin/merge.c:1608-1612), so one `-s ort` changes what `-s resolve`
//!    prints and one `-s ours` stops a fast-forward that `ort` would have taken.
//! 5. **`merge-recursive --subtree=<path>`.** `merge-recursive` and
//!    `merge-subtree` are the same `cmd_merge_recursive()`
//!    (builtin/merge-recursive.c:24-100); the flag reaches `parse_merge_opt()`
//!    under either name, so refusing it under one was divergence.
//! 6. **A rename conflict is named by its destination.** merge-ort reports the
//!    path the merged content lands at, not the one it started from.

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

/// `HEAD^{tree}`, trimmed. A merge that prints the right thing and writes the
/// wrong blob is the failure this whole file is built to catch, so the tree is
/// asserted next to every transcript.
fn head_tree(dir: &Path) -> String {
    git(dir, &["rev-parse", "HEAD^{tree}"]).trim().to_string()
}

/// The parents of `HEAD`, space separated — `rev-list --parents -1` minus the
/// commit itself. Empty for a root commit.
fn head_parents(dir: &Path) -> String {
    let line = git(dir, &["rev-list", "--parents", "-1", "HEAD"]);
    line.trim().split_whitespace().skip(1).collect::<Vec<_>>().join(" ")
}

fn index(dir: &Path) -> String {
    git(dir, &["ls-files", "-s"])
}

/// `git unpack-file`'s `mkstemp` suffix is random by construction, so those
/// labels are normalised before comparison. Anything that is *not* a
/// `.merge_file_` name — `HEAD`, a branch name, an object id — survives this
/// untouched and fails the assertion, which is the point: these cases turn on
/// *which* strategy's markers are on disk.
fn mask_temp_labels(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(".merge_file_") {
        out.push_str(&rest[..at]);
        out.push_str(".merge_file_X");
        rest = &rest[at + ".merge_file_".len()..];
        let skip = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
        rest = &rest[skip..];
    }
    out.push_str(rest);
    out
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "zvcs-mergeloop-{tag}-{}-{:?}",
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
    git(&repo, &["config", "rerere.enabled", "false"]);
    git(&repo, &["config", "merge.conflictStyle", "merge"]);
    repo
}

fn write(repo: &Path, path: &str, body: &str) {
    std::fs::write(repo.join(path), body).unwrap();
}

/// `main` and `side` both rewrite the middle line of `f.txt`, so every strategy
/// has to do a real file-level merge and every one of them conflicts. Each side
/// also adds a file of its own, so a strategy that silently kept only one side's
/// tree is visible in the index.
fn conflicting(tag: &str) -> PathBuf {
    let repo = init(tag);
    write(&repo, "f.txt", "a\nb\nc\n");
    write(&repo, "keep.txt", "base\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "side"]);
    write(&repo, "f.txt", "a\nMAIN\nc\n");
    write(&repo, "m.txt", "mainonly\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "main2"]);
    git(&repo, &["checkout", "-q", "side"]);
    write(&repo, "f.txt", "a\nSIDE\nc\n");
    write(&repo, "s.txt", "sideonly\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "side2"]);
    git(&repo, &["checkout", "-q", "main"]);
    repo
}

/// `side` is strictly ahead of `main`: the merge can fast-forward unless some
/// `-s` in the list carries `NO_FAST_FORWARD`.
fn fast_forwardable(tag: &str) -> PathBuf {
    let repo = init(tag);
    write(&repo, "f.txt", "a\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "side"]);
    git(&repo, &["checkout", "-q", "side"]);
    write(&repo, "f.txt", "a\nb\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "side2"]);
    git(&repo, &["checkout", "-q", "main"]);
    repo
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// `-s ours -s resolve`: `ours` is tried first, returns 0, and the loop breaks —
/// "This strategy worked; no point in trying another." (builtin/merge.c:1812).
///
/// This is the case that made reading only the last `-s` a wrong *result* rather
/// than a wrong message: stock commits `main`'s tree with `side` as a second
/// parent at exit 0, where taking `resolve` alone leaves a conflicted index at
/// exit 1. The tree hash is what separates them.
#[test]
fn the_first_strategy_that_succeeds_ends_the_loop() {
    let repo = conflicting("ours-first");
    let head_before = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let side = git(&repo, &["rev-parse", "side"]).trim().to_string();
    let tree_before = head_tree(&repo);

    let out = run(&repo, &["merge", "-s", "ours", "-s", "resolve", "--no-edit", "side"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Trying merge strategy ours...\nMerge made by the 'ours' strategy.\n"
    );
    // `-s ours` keeps our tree verbatim, so the merge commit's tree is the tree
    // `HEAD` already had — and `resolve` never ran, so nothing conflicted.
    assert_eq!(head_tree(&repo), tree_before);
    assert_eq!(head_parents(&repo), format!("{head_before} {side}"));
    assert_eq!(git(&repo, &["log", "-1", "--format=%s"]).trim(), "Merge branch 'side'");
    assert_eq!(read(&repo, "f.txt"), "a\nMAIN\nc\n");
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

/// `-s ort -s octopus` over one head: `ort` conflicts (score kept), `octopus`
/// refuses, so `best_strategy` is `ort` while `wt_strategy` is `octopus`. git
/// then rewinds once more and re-runs the winner to "prepare resolving by hand"
/// (builtin/merge.c:1848-1858) — the only path that prints that line.
#[test]
fn a_worse_last_attempt_makes_git_re_run_the_best_one() {
    let repo = conflicting("rerun-best");
    let tree_before = head_tree(&repo);

    let out = run(&repo, &["merge", "-s", "ort", "-s", "octopus", "--no-edit", "side"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Trying merge strategy ort...\n\
         Auto-merging f.txt\n\
         CONFLICT (content): Merge conflict in f.txt\n\
         Rewinding the tree to pristine...\n\
         Trying merge strategy octopus...\n\
         Rewinding the tree to pristine...\n\
         Using the ort strategy to prepare resolving by hand.\n\
         Auto-merging f.txt\n\
         CONFLICT (content): Merge conflict in f.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    // Nothing was committed, so `HEAD` and its tree are where they were.
    assert_eq!(head_tree(&repo), tree_before);
    assert_eq!(git(&repo, &["log", "-1", "--format=%s"]).trim(), "main2");
    // The re-run left merge-ort's own markers — `HEAD`/`side`, not `resolve`'s
    // `.merge_file_` names — and exactly the three stages stock records.
    assert_eq!(read(&repo, "f.txt"), "a\n<<<<<<< HEAD\nMAIN\n=======\nSIDE\n>>>>>>> side\nc\n");
    assert_eq!(
        index(&repo),
        "100644 de980441c3ab03a8c07dda1ad27b8a11f39deb1e 1\tf.txt\n\
         100644 af703352c64a2d88d4f62818fa68e6ae91241dfd 2\tf.txt\n\
         100644 f794161ca7f359f1bc311e2276a9a3d89a5bbec8 3\tf.txt\n\
         100644 df967b96a579e45a18b8251732d16804b2e56a55 0\tkeep.txt\n\
         100644 9b4be140fe02cfe8bed759848b74f1988f75a242 0\tm.txt\n\
         100644 b6835df1f8a46ecd6ceb504296738c5169f27f14 0\ts.txt\n"
    );
}

/// Every `-s` refusing leaves `best_strategy` unset. With more than one in the
/// list git says `No merge strategy handled the merge.`; with one it names it
/// (builtin/merge.c:1840-1846). `octopus` carries no `NO_TRIVIAL`, so listing it
/// twice still runs the in-index pre-pass first.
#[test]
fn no_strategy_handling_the_merge_is_its_own_message() {
    let repo = conflicting("none-handled");
    let tree_before = head_tree(&repo);
    let index_before = index(&repo);

    let out = run(&repo, &["merge", "-s", "octopus", "-s", "octopus", "--no-edit", "side"]);
    assert_eq!(code(&out), 2);
    assert_eq!(
        stdout(&out),
        "Trying really trivial in-index merge...\n\
         Nope.\n\
         Trying merge strategy octopus...\n\
         Rewinding the tree to pristine...\n\
         Trying merge strategy octopus...\n"
    );
    assert_eq!(
        stderr(&out),
        "error: Merge requires file-level merging\n\
         No merge strategy handled the merge.\n"
    );
    // A refused merge moves nothing at all.
    assert_eq!(head_tree(&repo), tree_before);
    assert_eq!(index(&repo), index_before);
    assert_eq!(read(&repo, "f.txt"), "a\nMAIN\nc\n");
}

/// The rewind between attempts is `read-tree -v --reset -u <head>` plus
/// `stash apply --index` (builtin/merge.c:403-427), not a printed line.
///
/// `-s resolve -s ort`: `resolve` runs first and writes conflict markers labelled
/// with `git unpack-file`'s `.merge_file_XXXXXX` temporaries; `ort` then runs
/// over a *pristine* tree and writes markers labelled `HEAD`/`side`. If the
/// rewind were a no-op, `ort` would have found a conflicted index and refused.
#[test]
fn the_rewind_between_attempts_really_resets_the_tree() {
    let repo = conflicting("real-rewind");

    let out = run(&repo, &["merge", "-s", "resolve", "-s", "ort", "--no-edit", "side"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Trying merge strategy resolve...\n\
         Trying simple merge.\n\
         Simple merge failed, trying Automatic merge.\n\
         Auto-merging f.txt\n\
         Rewinding the tree to pristine...\n\
         Trying merge strategy ort...\n\
         Auto-merging f.txt\n\
         CONFLICT (content): Merge conflict in f.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(
        stderr(&out),
        "ERROR: content conflict in f.txt\nfatal: merge program failed\n"
    );
    // `ort`'s labels, not `resolve`'s. `mask_temp_labels` leaves `HEAD` and
    // `side` visible, so a leftover `.merge_file_X` here would fail loudly.
    assert_eq!(
        mask_temp_labels(&read(&repo, "f.txt")),
        "a\n<<<<<<< HEAD\nMAIN\n=======\nSIDE\n>>>>>>> side\nc\n"
    );
    assert_eq!(
        index(&repo),
        "100644 de980441c3ab03a8c07dda1ad27b8a11f39deb1e 1\tf.txt\n\
         100644 af703352c64a2d88d4f62818fa68e6ae91241dfd 2\tf.txt\n\
         100644 f794161ca7f359f1bc311e2276a9a3d89a5bbec8 3\tf.txt\n\
         100644 df967b96a579e45a18b8251732d16804b2e56a55 0\tkeep.txt\n\
         100644 9b4be140fe02cfe8bed759848b74f1988f75a242 0\tm.txt\n\
         100644 b6835df1f8a46ecd6ceb504296738c5169f27f14 0\ts.txt\n"
    );
}

/// `if (best_cnt <= 0 || cnt <= best_cnt)` (builtin/merge.c:1816) — `<=`, so on a
/// tie the later strategy replaces the earlier one and its result is already in
/// the worktree. No `Using the … strategy` line is printed, and the markers on
/// disk are `resolve`'s temporaries.
///
/// Both files conflict on both strategies, so the two scores are equal by
/// construction; a `<` comparison would keep `ort` and re-run it, which changes
/// both the transcript and the bytes on disk.
#[test]
fn an_equal_score_keeps_the_later_strategy() {
    let repo = init("tie-break");
    write(&repo, "f.txt", "a\nb\nc\n");
    write(&repo, "g.txt", "x\ny\nz\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "side"]);
    write(&repo, "f.txt", "a\nMAIN\nc\n");
    write(&repo, "g.txt", "x\nMAIN\nz\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "main2"]);
    git(&repo, &["checkout", "-q", "side"]);
    write(&repo, "f.txt", "a\nSIDE\nc\n");
    write(&repo, "g.txt", "x\nSIDE\nz\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "side2"]);
    git(&repo, &["checkout", "-q", "main"]);

    let out = run(&repo, &["merge", "-s", "ort", "-s", "resolve", "--no-edit", "side"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(
        !stdout(&out).contains("Using the"),
        "a tie must not re-run the winner:\n{}",
        stdout(&out)
    );
    assert_eq!(
        stdout(&out),
        "Trying merge strategy ort...\n\
         Auto-merging f.txt\n\
         CONFLICT (content): Merge conflict in f.txt\n\
         Auto-merging g.txt\n\
         CONFLICT (content): Merge conflict in g.txt\n\
         Rewinding the tree to pristine...\n\
         Trying merge strategy resolve...\n\
         Trying simple merge.\n\
         Simple merge failed, trying Automatic merge.\n\
         Auto-merging f.txt\n\
         Auto-merging g.txt\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    // `resolve`'s markers survived, so `resolve` is what the tie kept.
    assert_eq!(
        mask_temp_labels(&read(&repo, "f.txt")),
        "a\n<<<<<<< .merge_file_X\nMAIN\n=======\nSIDE\n>>>>>>> .merge_file_X\nc\n"
    );
    assert_eq!(
        index(&repo),
        "100644 de980441c3ab03a8c07dda1ad27b8a11f39deb1e 1\tf.txt\n\
         100644 af703352c64a2d88d4f62818fa68e6ae91241dfd 2\tf.txt\n\
         100644 f794161ca7f359f1bc311e2276a9a3d89a5bbec8 3\tf.txt\n\
         100644 04ec35a6dc0776b83fdb3d9d238007c7dea360c8 1\tg.txt\n\
         100644 4e6509465b1dc987e65b11313e24c197e4a57d26 2\tg.txt\n\
         100644 983a5a6b3866aadc813fa3137f623b05ac37cf93 3\tg.txt\n"
    );
}

// ---------------------------------------------------------------------------
// The attribute unions
// ---------------------------------------------------------------------------

/// `if (use_strategies[i]->attr & NO_TRIVIAL) allow_trivial = 0;` runs over the
/// whole list (builtin/merge.c:1611-1612), so one `-s ort` suppresses the
/// in-index pre-pass that a lone `-s resolve` reaches. The two halves differ in
/// nothing but the extra `-s`.
#[test]
fn no_trivial_is_a_union_over_the_whole_strategy_list() {
    let solo = conflicting("trivial-solo");
    let out = run(&solo, &["merge", "-s", "resolve", "--no-edit", "side"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(
        stdout(&out).starts_with("Trying really trivial in-index merge...\nNope.\n"),
        "a lone -s resolve reaches the pre-pass:\n{}",
        stdout(&out)
    );

    let union = conflicting("trivial-union");
    let out = run(&union, &["merge", "-s", "ort", "-s", "resolve", "--no-edit", "side"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(
        !stdout(&out).contains("Trying really trivial in-index merge..."),
        "one -s ort suppresses it for the whole list:\n{}",
        stdout(&out)
    );
    assert!(stdout(&out).starts_with("Trying merge strategy ort...\n"));
}

/// `if (use_strategies[i]->attr & NO_FAST_FORWARD) fast_forward = FF_NO;`
/// (builtin/merge.c:1609-1610) is a union too: `-s ort -s ours` over a
/// fast-forwardable history records a merge commit *made by `ort`*, because
/// `ours` was in the list when `fast_forward` was decided and `ort` is what
/// answered the merge. Without a `NO_FAST_FORWARD` name the same history simply
/// fast-forwards.
#[test]
fn no_fast_forward_is_a_union_over_the_whole_strategy_list() {
    let repo = fast_forwardable("noff-union");
    let head_before = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let side = git(&repo, &["rev-parse", "side"]).trim().to_string();

    let out = run(&repo, &["merge", "-s", "ort", "-s", "ours", "--no-edit", "side"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Trying merge strategy ort...\n\
         Merge made by the 'ort' strategy.\n \
         f.txt | 1 +\n \
         1 file changed, 1 insertion(+)\n"
    );
    // A real merge commit: two parents, and the tree is `side`'s content.
    assert_eq!(head_parents(&repo), format!("{head_before} {side}"));
    assert_eq!(head_tree(&repo), "196228cfdfdd814964f14972b3cdac82c2628dc7");
    assert_eq!(read(&repo, "f.txt"), "a\nb\n");

    let plain = fast_forwardable("noff-absent");
    let out = run(&plain, &["merge", "-s", "resolve", "-s", "ort", "--no-edit", "side"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        stdout(&out).contains("Fast-forward"),
        "neither name carries NO_FAST_FORWARD:\n{}",
        stdout(&out)
    );
    // A fast-forward records no merge: one parent, same tree.
    assert_eq!(head_parents(&plain).split_whitespace().count(), 1);
    assert_eq!(head_tree(&plain), "196228cfdfdd814964f14972b3cdac82c2628dc7");
}

/// Several heads with a two-head engine named first: `merge_ort_recursive()`
/// refuses anything but two heads (builtin/merge.c:809-812) and the loop moves
/// on to the octopus, which handles them. The `error:` line stock prints from the
/// refused attempt stays on stderr even though the merge as a whole succeeds.
#[test]
fn a_refused_two_head_engine_falls_through_to_the_octopus() {
    let repo = init("three-heads");
    write(&repo, "f.txt", "base\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "b"]);
    git(&repo, &["branch", "c"]);
    git(&repo, &["checkout", "-q", "b"]);
    write(&repo, "b.txt", "bb\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "bcommit"]);
    git(&repo, &["checkout", "-q", "c"]);
    write(&repo, "c.txt", "cc\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "ccommit"]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "m.txt", "mm\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "mcommit"]);

    let out = run(&repo, &["merge", "-s", "ort", "-s", "octopus", "--no-edit", "b", "c"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Trying merge strategy ort...\n\
         Rewinding the tree to pristine...\n\
         Trying merge strategy octopus...\n\
         Trying simple merge with b\n\
         Trying simple merge with c\n\
         Merge made by the 'octopus' strategy.\n \
         b.txt | 1 +\n \
         c.txt | 1 +\n \
         2 files changed, 2 insertions(+)\n \
         create mode 100644 b.txt\n \
         create mode 100644 c.txt\n"
    );
    assert_eq!(stderr(&out), "error: Not handling anything other than two heads merge.\n");
    assert_eq!(head_tree(&repo), "79a91e8e1a628aedd2b45c6268f1ddcbb90f1a6b");
    assert_eq!(head_parents(&repo).split_whitespace().count(), 3);
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%s"]).trim(),
        "Merge branches 'b' and 'c'"
    );
}

// ---------------------------------------------------------------------------
// merge-recursive --subtree
// ---------------------------------------------------------------------------

/// `merge-recursive` and `merge-subtree` are one program: `cmd_merge_recursive()`
/// only checks whether `argv[0]` ends in `-subtree` to seed `o.subtree_shift`
/// (builtin/merge-recursive.c:38-39), and both names then run the same
/// `parse_merge_opt()`, whose `subtree=<path>` branch is at merge-ort.c:5553.
/// The shift itself happens inside `merge_ort_internal()` (merge-ort.c:5243-5248)
/// and aligns *remote* and *base* onto *head*.
///
/// The fixture puts the same file at `f.txt` on `main` and at `sub/f.txt` on the
/// side history, so the shift is what turns an unrelated `sub/f.txt` into a
/// content merge of `f.txt`.
#[test]
fn merge_recursive_honours_the_subtree_shift() {
    let repo = init("subtree-shift");
    write(&repo, "f.txt", "a\nb\nc\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["checkout", "-q", "--orphan", "side"]);
    git(&repo, &["rm", "-q", "-rf", "."]);
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    write(&repo, "sub/f.txt", "a\nb\nc\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "subbase"]);
    git(&repo, &["branch", "subbase"]);
    write(&repo, "sub/f.txt", "a\nSIDE\nc\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "subside"]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "f.txt", "a\nMAIN\nc\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "main2"]);

    let out = run(
        &repo,
        &["merge-recursive", "--subtree=sub", "subbase", "--", "main", "side"],
    );
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Auto-merging f.txt\nCONFLICT (content): Merge conflict in f.txt\n"
    );
    assert_eq!(stderr(&out), "");
    // The shift put `side`'s `sub/f.txt` opposite `main`'s `f.txt`, so the three
    // stages are all `f.txt` and the markers carry the operand names git's
    // `better_branch_name` produces.
    assert_eq!(
        index(&repo),
        "100644 de980441c3ab03a8c07dda1ad27b8a11f39deb1e 1\tf.txt\n\
         100644 af703352c64a2d88d4f62818fa68e6ae91241dfd 2\tf.txt\n\
         100644 f794161ca7f359f1bc311e2276a9a3d89a5bbec8 3\tf.txt\n"
    );
    assert_eq!(
        read(&repo, "f.txt"),
        "a\n<<<<<<< main\nMAIN\n=======\nSIDE\n>>>>>>> side\nc\n"
    );
}

// ---------------------------------------------------------------------------
// Rename message naming
// ---------------------------------------------------------------------------

/// merge-ort names a conflict by where the merged content *ended up*. When `HEAD`
/// renames `old.txt` to `new.txt` and edits it while `side` edits `old.txt`,
/// `handle_content_merge()` runs against the new name and reports it.
///
/// `gix-merge` spreads that across the resolution's `final_location` and the
/// `Change::Rewrite`'s `location`; reading the *ours* change's location instead
/// named the pre-rename path on every rename conflict. Rename detection is on by
/// default, so this is asserted without `-X` as well as with it.
#[test]
fn a_rename_conflict_is_named_by_its_destination() {
    for (tag, extra) in [("rename-default", None), ("rename-x", Some("-Xfind-renames=50%"))] {
        let repo = init(tag);
        write(&repo, "old.txt", "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "base"]);
        git(&repo, &["branch", "side"]);
        git(&repo, &["mv", "old.txt", "new.txt"]);
        write(&repo, "new.txt", "l1\nMAIN\nl3\nl4\nl5\nl6\nl7\nl8\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "mainren"]);
        git(&repo, &["checkout", "-q", "side"]);
        write(&repo, "old.txt", "l1\nSIDE\nl3\nl4\nl5\nl6\nl7\nl8\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "sideedit"]);
        git(&repo, &["checkout", "-q", "main"]);

        let mut args = vec!["merge", "--no-edit"];
        if let Some(x) = extra {
            args.push(x);
        }
        args.push("side");
        let out = run(&repo, &args);

        assert_eq!(code(&out), 1, "{tag}: {}", stderr(&out));
        assert_eq!(
            stdout(&out),
            "Auto-merging new.txt\n\
             CONFLICT (content): Merge conflict in new.txt\n\
             Automatic merge failed; fix conflicts and then commit the result.\n",
            "{tag}: the destination names the conflict, not the source"
        );
        // The stages sit under the destination too, and `old.txt` is gone.
        assert_eq!(
            index(&repo),
            "100644 a52ef2749cf75ef78cc19a23edb04982ec54ab95 1\tnew.txt\n\
             100644 edd00de8467c711a2482ee57cd231a5c5668a447 2\tnew.txt\n\
             100644 8d973878d204a67ad5a93bcd08ff102b29a42b93 3\tnew.txt\n",
            "{tag}"
        );
        assert!(!repo.join("old.txt").exists(), "{tag}: the rename source is gone");
    }
}
