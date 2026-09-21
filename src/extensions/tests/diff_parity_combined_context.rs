//! The two knobs a combined (`-c` / `--cc`) patch reads that a two-way patch also
//! reads, and the one option that has to survive `--diff-merges`' `suppress()`.
//!
//! * `show_patch_diff()` opens with `context = opt->context` (combine-diff.c:1030),
//!   so `-U<n>` shapes a merge's hunks exactly as it shapes a file's.
//! * `make_hunks(sline, cnt, num_parent, rev->dense_combined_merges)`
//!   (combine-diff.c:1204) takes `dense` as an argument, and the function returns
//!   early on `if (!dense) return give_context(...)` (combine-diff.c:621). Only the
//!   dense form — `--cc` — drops a hunk every line of which differs from the same
//!   single parent. A bare `-c` keeps it.
//! * `show_combined_header()` reads `opt->flags.full_index`, `opt->a_prefix` and
//!   `opt->b_prefix` (combine-diff.c:931-933) for the `index`, `---` and `+++`
//!   lines, exactly as a two-way patch header does.
//! * `--combined-all-paths` is cleared by `suppress()` (diff-merges.c:19), which
//!   every `--diff-merges` spelling runs first, and
//!   `diff_merges_setup_revs()` dies without `-c`/`--cc` (diff-merges.c:184-185).
//!
//! Every expectation below was read off stock git 2.55.0 on this fixture before it
//! was written down.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

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

fn git(dir: &Path, args: &[&str]) {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// A two-parent merge of a seven-line file with two independent changes:
///
/// * line 2 was changed on *both* branches and resolved to a third text — a hunk
///   both parents differ from, which every combined mode keeps;
/// * line 5 was changed on `main` only and taken verbatim — a hunk only one parent
///   differs from, which `--cc` elides and `-c` keeps.
///
/// Three lines separate them, so at `-U0` and `-U1` they are two hunks and at the
/// default `-U3` they are one. That is what makes both knobs observable at once.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-combined-ctx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    let f = repo.join("f.txt");

    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(&f, "a\nb\nc\nd\ne\nf\ng\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);

    git(&repo, &["checkout", "-q", "-b", "side"]);
    std::fs::write(&f, "a\nSIDE\nc\nd\ne\nf\ng\n").unwrap();
    git(&repo, &["commit", "-q", "-a", "-m", "side"]);

    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(&f, "a\nMAIN\nc\nd\nMAINE\nf\ng\n").unwrap();
    git(&repo, &["commit", "-q", "-a", "-m", "main"]);

    // The merge conflicts on line 2 by construction; its failure is the point, so
    // the exit status is deliberately not asserted here.
    let _ = run(&repo, &["merge", "side"]);
    std::fs::write(&f, "a\nRESOLVED\nc\nd\nMAINE\nf\ng\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "merge"]);
    repo
}

const HEADER: &str = "\
index f2b94c7ecd,53e4cbf30d..82e49b41fb
--- a/f.txt
+++ b/f.txt
";

/// `-c` at zero context: two one-line hunks, the second of which only `main`
/// differs from. Rendering this with `dense` forced on drops that second hunk;
/// ignoring `-U0` merges both into one `@@@ -1,7 -1,7 +1,7 @@@`.
#[test]
fn a_bare_c_keeps_a_single_parent_hunk_and_honours_u0() {
    let repo = fixture("c-u0");
    let o = run(&repo, &["diff-tree", "--no-commit-id", "-c", "-p", "-U0", "HEAD"]);
    assert_eq!(
        stdout(&o),
        format!(
            "diff --combined f.txt\n{HEADER}\
@@@ -2,1 -2,1 +2,1 @@@\n- MAIN\n -SIDE\n++RESOLVED\n\
@@@ -5,1 -5,1 +5,1 @@@\n -e\n +MAINE\n"
        )
    );
}

/// The same command with `--cc`: `make_hunks()` runs its uninteresting-hunk pass
/// and the `-e`/`+MAINE` hunk goes away. This is the half a `dense`-always port
/// gets right, and it has to keep working after the flag is threaded through.
#[test]
fn dense_cc_drops_the_single_parent_hunk_at_u0() {
    let repo = fixture("cc-u0");
    let o = run(&repo, &["diff-tree", "--no-commit-id", "--cc", "-p", "-U0", "HEAD"]);
    assert_eq!(
        stdout(&o),
        format!(
            "diff --cc f.txt\n{HEADER}@@@ -2,1 -2,1 +2,1 @@@\n- MAIN\n -SIDE\n++RESOLVED\n"
        )
    );
}

/// One line of context pulls the two `-c` hunks into one six-line hunk, and leaves
/// `--cc` with a three-line one. A port that ignores `-U<n>` prints the whole
/// seven-line file for both.
#[test]
fn combined_context_width_follows_unified() {
    let repo = fixture("u1");

    let o = run(&repo, &["diff-tree", "--no-commit-id", "-c", "-p", "-U1", "HEAD"]);
    assert_eq!(
        stdout(&o),
        format!(
            "diff --combined f.txt\n{HEADER}\
@@@ -1,6 -1,6 +1,6 @@@\n  a\n- MAIN\n -SIDE\n++RESOLVED\n  c\n  d\n -e\n +MAINE\n  f\n"
        )
    );

    let o = run(&repo, &["diff-tree", "--no-commit-id", "--cc", "-p", "-U1", "HEAD"]);
    assert_eq!(
        stdout(&o),
        format!(
            "diff --cc f.txt\n{HEADER}@@@ -1,3 -1,3 +1,3 @@@\n  a\n- MAIN\n -SIDE\n++RESOLVED\n  c\n"
        )
    );
}

/// `git show` and `git log` render the merge through their own combined-patch
/// call, which used to pass a literal 3 where `opt->context` belongs. The `-U0`
/// answer is the one a hardcoded 3 cannot reach.
#[test]
fn show_and_log_pass_unified_context_to_the_combined_patch() {
    let repo = fixture("porcelain");
    let want = format!(
        "merge\n\ndiff --combined f.txt\n{HEADER}\
@@@ -2,1 -2,1 +2,1 @@@\n- MAIN\n -SIDE\n++RESOLVED\n\
@@@ -5,1 -5,1 +5,1 @@@\n -e\n +MAINE\n"
    );

    let o = run(&repo, &["show", "-c", "-U0", "--format=%s", "HEAD"]);
    assert_eq!(stdout(&o), want);

    let o = run(&repo, &["log", "-1", "-c", "-U0", "--format=%s", "HEAD"]);
    assert_eq!(stdout(&o), want);

    // `--cc -U1` is the dense twin: three lines, not the whole file.
    let o = run(&repo, &["show", "--cc", "-U1", "--format=%s", "HEAD"]);
    assert_eq!(
        stdout(&o),
        format!(
            "merge\n\ndiff --cc f.txt\n{HEADER}\
@@@ -1,3 -1,3 +1,3 @@@\n  a\n- MAIN\n -SIDE\n++RESOLVED\n  c\n"
        )
    );
}

const RAW_ONE: &str = "::100644 100644 100644 f2b94c7ecd3cc2fc38b319a0cd196ab72db331ac \
53e4cbf30d089cda0d93c45332cd4bdf9e4c0d4b 82e49b41fbacb1ba415634d99840326ce4898dd9 MM\tf.txt\n";

/// `-c` runs `set_combined()`, which begins with `suppress()` — and `suppress()`
/// zeroes `combined_all_paths` (diff-merges.c:19). So the flag only takes effect
/// when it is written *after* the last `--diff-merges` spelling on the line.
#[test]
fn combined_all_paths_is_cleared_by_a_later_diff_merges_option() {
    let repo = fixture("allpaths-order");

    let before = run(
        &repo,
        &["diff-tree", "--no-commit-id", "--combined-all-paths", "-c", "--raw", "HEAD"],
    );
    assert_eq!(stdout(&before), RAW_ONE);

    let after = run(
        &repo,
        &["diff-tree", "--no-commit-id", "-c", "--combined-all-paths", "--raw", "HEAD"],
    );
    assert_eq!(stdout(&after), format!("{}\tf.txt\tf.txt\n", RAW_ONE.trim_end_matches('\n')));

    // `git show` shares the rule through its own parser.
    let before = run(&repo, &["show", "--combined-all-paths", "-c", "--raw", "--format=%s", "HEAD"]);
    assert_eq!(
        stdout(&before),
        "merge\n\n::100644 100644 100644 f2b94c7ecd 53e4cbf30d 82e49b41fb MM\tf.txt\n"
    );
    let after = run(&repo, &["show", "-c", "--combined-all-paths", "--raw", "--format=%s", "HEAD"]);
    assert_eq!(
        stdout(&after),
        "merge\n\n::100644 100644 100644 f2b94c7ecd 53e4cbf30d 82e49b41fb MM\tf.txt\tf.txt\tf.txt\n"
    );

    // `-m` is `set_separate()`, which also starts from `suppress()`: the flag is
    // cleared, so the run is a plain per-parent listing and not the `die()` below.
    let m = run(&repo, &["diff-tree", "--no-commit-id", "--combined-all-paths", "-m", "--raw", "HEAD"]);
    assert!(m.status.success(), "{}", String::from_utf8_lossy(&m.stderr));
    assert_eq!(
        stdout(&m),
        ":100644 100644 f2b94c7ecd3cc2fc38b319a0cd196ab72db331ac \
82e49b41fbacb1ba415634d99840326ce4898dd9 M\tf.txt\n\
:100644 100644 53e4cbf30d089cda0d93c45332cd4bdf9e4c0d4b \
82e49b41fbacb1ba415634d99840326ce4898dd9 M\tf.txt\n"
    );
}

/// `diff_merges_setup_revs()`'s only `die()`. It fires from `setup_revisions()`,
/// so it outranks `cmd_diff_tree`'s own leftover-argument usage and the
/// output-format exclusivity check below it.
#[test]
fn combined_all_paths_without_c_is_fatal() {
    let repo = fixture("allpaths-die");
    for extra in [
        vec!["diff-tree", "--combined-all-paths", "--raw", "HEAD"],
        vec!["diff-tree", "--combined-all-paths", "--name-only", "--name-status", "HEAD"],
    ] {
        let o = run(&repo, &extra);
        assert_eq!(o.status.code(), Some(128), "{extra:?}");
        assert_eq!(
            String::from_utf8_lossy(&o.stderr),
            "fatal: --combined-all-paths makes no sense without -c or --cc\n",
            "{extra:?}"
        );
        assert_eq!(stdout(&o), "", "{extra:?}");
    }
}

/// The three header inputs `show_combined_header()` takes besides the path set.
/// All three at once, so a header built from defaults differs in every line:
///
/// ```c
/// int abbrev = opt->flags.full_index ? the_hash_algo->hexsz : DEFAULT_ABBREV;
/// const char *a_prefix = opt->a_prefix ? opt->a_prefix : "a/";
/// const char *b_prefix = opt->b_prefix ? opt->b_prefix : "b/";
/// ```
#[test]
fn the_combined_header_reads_full_index_and_the_path_prefixes() {
    let repo = fixture("header");
    let o = run(
        &repo,
        &[
            "diff-tree",
            "--no-commit-id",
            "-c",
            "-p",
            "-U0",
            "--combined-all-paths",
            "--full-index",
            "--src-prefix=S/",
            "--dst-prefix=D/",
            "HEAD",
        ],
    );
    assert_eq!(
        stdout(&o),
        "diff --combined f.txt\nindex f2b94c7ecd3cc2fc38b319a0cd196ab72db331ac,53e4cbf30d089cda0d93c45332cd4bdf9e4c0d4b..82e49b41fbacb1ba415634d99840326ce4898dd9\n--- S/f.txt\n--- S/f.txt\n+++ D/f.txt\n@@@ -2,1 -2,1 +2,1 @@@\n- MAIN\n -SIDE\n++RESOLVED\n@@@ -5,1 -5,1 +5,1 @@@\n -e\n +MAINE\n"
    );

    // `--no-prefix` empties both, which is the `opt->a_prefix == NULL` case reached
    // from the other side.
    let o = run(&repo, &["show", "--cc", "-U0", "--no-prefix", "--format=%s", "HEAD"]);
    assert_eq!(
        stdout(&o),
        "merge\n\ndiff --cc f.txt\nindex f2b94c7ecd,53e4cbf30d..82e49b41fb\n--- f.txt\n+++ f.txt\n@@@ -2,1 -2,1 +2,1 @@@\n- MAIN\n -SIDE\n++RESOLVED\n"
    );
}
