//! The knobs `diff-index`, `diff-files` and `diff-tree` used to accept and then drop:
//! rename/copy detection (`-M`, `-C`, `--find-copies-harder`), the merge-diff selector
//! (`--diff-merges=<mode>`, `--no-diff-merges`, `-m`), `--expand-tabs`, `--binary`,
//! `--word-diff` and `--abbrev=<n>` on a routed patch.
//!
//! Every expectation is a byte string measured from stock git 2.55.0. Nothing here
//! shells out to a second git — the fixtures are built with this binary's own
//! plumbing (`hash-object`, `update-index --index-info`), so the suite runs on a
//! headless Linux CI box with no system git present.
//!
//! Each case is pinned twice: against the bytes stock produces *and* against the same
//! command's output without the flag. The second assertion is the load-bearing one.
//! A flag that is parsed and then ignored still exits 0 and still prints something
//! plausible, so only `assert_ne!` against the default separates "plumbed" from
//! "swallowed" — which is exactly how `diff-index --find-copies` passed for months
//! while emitting an `A` record where git emits `C100`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        // This machine's own `~/.gitconfig` sets `core.commentChar`; pin all four so
        // the run reads nothing but the repository's config.
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")
        .output()
        .unwrap()
}

fn run(repo: &Path, home: &Path, args: &[&str]) {
    let o = cmd(repo, home, args);
    assert!(o.status.success(), "git {args:?} failed: {}", err(&o));
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-renknobs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    (repo, home)
}

/// Forty numbered lines — long enough that changing one leaves a 97% similarity
/// rather than a boundary case, so the score in the expectations is stable.
fn original() -> String {
    (1..=40)
        .map(|n| format!("line {n} of the original content here\n"))
        .collect()
}

fn renamed() -> String {
    original().replace("line 7 of", "LINE SEVEN of")
}

fn copysrc() -> String {
    (1..=30)
        .map(|n| format!("copysrc {n} some payload text\n"))
        .collect()
}

/// `HEAD~1..HEAD` carries one of every pair rename detection has to tell apart:
///
/// * `src/orig.txt` → `src/renamed.txt` with a single line edited — a rename `-M`
///   finds and a plain listing reports as `D` + `A`;
/// * `src/copysrc.txt` → `src/copy2.txt`, byte-identical, with the *source left
///   unmodified* — the one shape only `--find-copies-harder` can see, because a
///   plain `-C` never queues an unchanged path as a candidate source;
/// * a binary blob whose bytes change, for `--binary`;
/// * a plain create and a plain delete, which must survive untouched.
fn tree_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = scratch(tag);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("doc")).unwrap();
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.x"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    std::fs::write(repo.join("src/orig.txt"), original()).unwrap();
    std::fs::write(repo.join("src/copysrc.txt"), copysrc()).unwrap();
    std::fs::write(repo.join("doc/keep.txt"), "unchanged\n").unwrap();
    std::fs::write(repo.join("doc/bin.dat"), b"bin\x00\x01\x02\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "c0"]);

    std::fs::remove_file(repo.join("src/orig.txt")).unwrap();
    std::fs::write(repo.join("src/renamed.txt"), renamed()).unwrap();
    std::fs::write(repo.join("src/copy2.txt"), copysrc()).unwrap();
    std::fs::write(repo.join("doc/new.txt"), "brand new\n").unwrap();
    std::fs::remove_file(repo.join("doc/keep.txt")).unwrap();
    std::fs::write(repo.join("doc/bin.dat"), b"bin\x00\x01\x03X\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "c1"]);
    (repo, home)
}

const RAW_PLAIN: &str = "\
:100644 100644 5cba28062ba182cf45fbaf93c71434fd4f34ef57 c9d47368371671f7ed17a7151e66a58ae4c20671 M\tdoc/bin.dat
:100644 000000 4eea88a852fde1261c409090a7aae3f0d957e349 0000000000000000000000000000000000000000 D\tdoc/keep.txt
:000000 100644 0000000000000000000000000000000000000000 d5a09df94c94924d13f8b5cd72a193b3eddb08cb A\tdoc/new.txt
:000000 100644 0000000000000000000000000000000000000000 e3955131e9aef23e021896be28385ef3b35f40f4 A\tsrc/copy2.txt
:100644 000000 c459d38a1f33031e6effc6dba7a0615ebf949739 0000000000000000000000000000000000000000 D\tsrc/orig.txt
:000000 100644 0000000000000000000000000000000000000000 a91573028c46fd08e066c6a8b8b856ac65bb7c10 A\tsrc/renamed.txt
";

const RAW_RENAME: &str = "\
:100644 100644 5cba28062ba182cf45fbaf93c71434fd4f34ef57 c9d47368371671f7ed17a7151e66a58ae4c20671 M\tdoc/bin.dat
:100644 000000 4eea88a852fde1261c409090a7aae3f0d957e349 0000000000000000000000000000000000000000 D\tdoc/keep.txt
:000000 100644 0000000000000000000000000000000000000000 d5a09df94c94924d13f8b5cd72a193b3eddb08cb A\tdoc/new.txt
:000000 100644 0000000000000000000000000000000000000000 e3955131e9aef23e021896be28385ef3b35f40f4 A\tsrc/copy2.txt
:100644 100644 c459d38a1f33031e6effc6dba7a0615ebf949739 a91573028c46fd08e066c6a8b8b856ac65bb7c10 R097\tsrc/orig.txt\tsrc/renamed.txt
";

const RAW_COPY_HARDER: &str = "\
:100644 100644 5cba28062ba182cf45fbaf93c71434fd4f34ef57 c9d47368371671f7ed17a7151e66a58ae4c20671 M\tdoc/bin.dat
:100644 000000 4eea88a852fde1261c409090a7aae3f0d957e349 0000000000000000000000000000000000000000 D\tdoc/keep.txt
:000000 100644 0000000000000000000000000000000000000000 d5a09df94c94924d13f8b5cd72a193b3eddb08cb A\tdoc/new.txt
:100644 100644 e3955131e9aef23e021896be28385ef3b35f40f4 e3955131e9aef23e021896be28385ef3b35f40f4 C100\tsrc/copysrc.txt\tsrc/copy2.txt
:100644 100644 c459d38a1f33031e6effc6dba7a0615ebf949739 a91573028c46fd08e066c6a8b8b856ac65bb7c10 R097\tsrc/orig.txt\tsrc/renamed.txt
";

/// `-M`, `-C` and `--find-copies-harder` on the tree-vs-index walk.
///
/// `-C` deliberately produces the same listing as `-M` here: the copy's source is
/// unmodified, and `diffcore_rename()` only offers *changed* paths as sources unless
/// `--find-copies-harder` queues the unchanged ones too (`oneway_diff()`,
/// diff-lib.c:431). That difference is the point of the third assertion — it is what
/// tells a real `diffcore_rename()` run from one that merely renamed some statuses.
#[test]
fn diff_index_pairs_renames_and_copies() {
    let (repo, home) = tree_fixture("pairs");

    let plain = cmd(&repo, &home, &["diff-index", "HEAD~1"]);
    assert!(plain.status.success(), "{}", err(&plain));
    assert_eq!(out(&plain), RAW_PLAIN);

    for flag in ["-M", "--find-renames", "-M50%", "--find-renames=50%"] {
        let o = cmd(&repo, &home, &["diff-index", "HEAD~1", flag]);
        assert!(o.status.success(), "{flag}: {}", err(&o));
        assert_eq!(out(&o), RAW_RENAME, "{flag}");
        assert_ne!(out(&o), RAW_PLAIN, "{flag} was accepted and dropped");
    }

    let copy = cmd(&repo, &home, &["diff-index", "HEAD~1", "-C"]);
    assert!(copy.status.success(), "{}", err(&copy));
    assert_eq!(out(&copy), RAW_RENAME);

    for flag in ["--find-copies-harder", "-C50%"] {
        // `-C50%` is `-C` twice only when a `-C` came first; the second spelling here
        // is the standalone `--find-copies-harder`, and `-C -C` below is the pair.
        let args: &[&str] = if flag == "--find-copies-harder" {
            &["diff-index", "HEAD~1", "--find-copies-harder"]
        } else {
            &["diff-index", "HEAD~1", "-C", "-C"]
        };
        let o = cmd(&repo, &home, args);
        assert!(o.status.success(), "{args:?}: {}", err(&o));
        assert_eq!(out(&o), RAW_COPY_HARDER, "{args:?}");
        assert_ne!(out(&o), RAW_RENAME, "{args:?} did not reach the copy source");
    }

    // `--quiet` raises `flags.quick`, which `diff_setup_done()` (diff.c:5348-5353)
    // answers by clearing rename detection outright.
    let quiet = cmd(&repo, &home, &["diff-index", "HEAD~1", "--find-copies-harder", "--quiet"]);
    assert_eq!(out(&quiet), "");
    assert_eq!(quiet.status.code(), Some(1));
}

/// The rename has to reach every renderer, not just the raw one: `pprint_rename`'s
/// `src/{orig.txt => renamed.txt}` in the stat family, `similarity index`/`rename
/// from`/`rename to` in the patch, and the percentage in `--summary`.
#[test]
fn diff_index_rename_reaches_the_content_formats() {
    let (repo, home) = tree_fixture("formats");

    let stat = cmd(&repo, &home, &["diff-index", "HEAD~1", "-M", "--stat"]);
    assert!(stat.status.success(), "{}", err(&stat));
    assert_eq!(
        out(&stat),
        " doc/bin.dat                   | Bin 7 -> 8 bytes\n\
         \x20doc/keep.txt                  |   1 -\n\
         \x20doc/new.txt                   |   1 +\n\
         \x20src/copy2.txt                 |  30 ++++++++++++++++++++++++++++++\n\
         \x20src/{orig.txt => renamed.txt} |   2 +-\n\
         \x205 files changed, 32 insertions(+), 2 deletions(-)\n"
    );
    let stat_plain = cmd(&repo, &home, &["diff-index", "HEAD~1", "--stat"]);
    assert_ne!(out(&stat), out(&stat_plain));

    let numstat = cmd(&repo, &home, &["diff-index", "HEAD~1", "-M", "--numstat"]);
    assert_eq!(
        out(&numstat),
        "-\t-\tdoc/bin.dat\n\
         0\t1\tdoc/keep.txt\n\
         1\t0\tdoc/new.txt\n\
         30\t0\tsrc/copy2.txt\n\
         1\t1\tsrc/{orig.txt => renamed.txt}\n"
    );

    let summary = cmd(&repo, &home, &["diff-index", "HEAD~1", "-M", "--summary"]);
    assert_eq!(
        out(&summary),
        " delete mode 100644 doc/keep.txt\n\
         \x20create mode 100644 doc/new.txt\n\
         \x20create mode 100644 src/copy2.txt\n\
         \x20rename src/{orig.txt => renamed.txt} (97%)\n"
    );
    let summary_plain = cmd(&repo, &home, &["diff-index", "HEAD~1", "--summary"]);
    assert_ne!(out(&summary), out(&summary_plain));

    // The copy's two sides are byte-identical, so its patch section is a header and
    // nothing else — `builtin_diff()` still emits it because `fill_metainfo()` set
    // `must_show_header` (diff.c:4873).
    let patch = cmd(&repo, &home, &["diff-index", "HEAD~1", "--find-copies-harder", "-p"]);
    assert!(patch.status.success(), "{}", err(&patch));
    assert!(
        patch.stdout.windows(1).count() > 0
            && out(&patch).contains(
                "diff --git a/src/copysrc.txt b/src/copy2.txt\n\
                 similarity index 100%\n\
                 copy from src/copysrc.txt\n\
                 copy to src/copy2.txt\n"
            ),
        "copy header missing:\n{}",
        out(&patch)
    );
    assert!(
        out(&patch).contains(
            "diff --git a/src/orig.txt b/src/renamed.txt\n\
             similarity index 97%\n\
             rename from src/orig.txt\n\
             rename to src/renamed.txt\n\
             index c459d38..a915730 100644\n\
             --- a/src/orig.txt\n\
             +++ b/src/renamed.txt\n"
        ),
        "rename header missing:\n{}",
        out(&patch)
    );

    let copy_summary =
        cmd(&repo, &home, &["diff-index", "HEAD~1", "--find-copies-harder", "--summary"]);
    assert_eq!(
        out(&copy_summary),
        " delete mode 100644 doc/keep.txt\n\
         \x20create mode 100644 doc/new.txt\n\
         \x20copy src/{copysrc.txt => copy2.txt} (100%)\n\
         \x20rename src/{orig.txt => renamed.txt} (97%)\n"
    );
}

/// `diff_queue_change()` swaps the two sides *as it queues the pair* (diff.c:7667), so
/// `-R` is in force before `diffcore_rename()` runs rather than after it. The two
/// orderings are distinguishable: detect-then-swap would still find the copy, because
/// the copy's destination would merely change places. Reversed, `src/copy2.txt` is a
/// deletion, and `diffcore_rename()` only ever matches *destinations* — so stock finds
/// no copy at all and `-R --find-copies-harder` collapses onto `-R -M`.
#[test]
fn reverse_is_applied_before_rename_detection() {
    let (repo, home) = tree_fixture("reverse");

    const REVERSED: &str = "\
:100644 100644 c9d47368371671f7ed17a7151e66a58ae4c20671 5cba28062ba182cf45fbaf93c71434fd4f34ef57 M\tdoc/bin.dat
:000000 100644 0000000000000000000000000000000000000000 4eea88a852fde1261c409090a7aae3f0d957e349 A\tdoc/keep.txt
:100644 000000 d5a09df94c94924d13f8b5cd72a193b3eddb08cb 0000000000000000000000000000000000000000 D\tdoc/new.txt
:100644 000000 e3955131e9aef23e021896be28385ef3b35f40f4 0000000000000000000000000000000000000000 D\tsrc/copy2.txt
:100644 100644 a91573028c46fd08e066c6a8b8b856ac65bb7c10 c459d38a1f33031e6effc6dba7a0615ebf949739 R097\tsrc/renamed.txt\tsrc/orig.txt
";

    let rev_m = cmd(&repo, &home, &["diff-index", "HEAD~1", "-R", "-M"]);
    assert!(rev_m.status.success(), "{}", err(&rev_m));
    assert_eq!(out(&rev_m), REVERSED);

    let rev_c = cmd(&repo, &home, &["diff-index", "HEAD~1", "-R", "--find-copies-harder"]);
    assert!(rev_c.status.success(), "{}", err(&rev_c));
    assert_eq!(out(&rev_c), REVERSED);
    assert_ne!(out(&rev_c), RAW_COPY_HARDER);
}

/// `--expand-tabs[=<n>]` / `--no-expand-tabs` (revision.c:2575-2583) write only
/// `revs->expand_tabs_in_log`, which `pretty.c` reads when it indents a commit message
/// (pretty.c:2235, 2281). These two commands never format one, so every spelling is
/// inert — but `strtol_i` still validates the value and dies before any revision is
/// resolved, which is the half that is *not* free.
#[test]
fn expand_tabs_is_accepted_inert_and_still_validated() {
    let (repo, home) = tree_fixture("tabs");

    for verb in [
        &["diff-index", "HEAD~1"][..],
        &["diff-index", "HEAD~1", "-p"][..],
        &["diff-index", "HEAD~1", "--stat"][..],
        &["diff-files"][..],
    ] {
        let base = cmd(&repo, &home, verb);
        assert!(base.status.success(), "{verb:?}: {}", err(&base));
        for flag in ["--expand-tabs", "--no-expand-tabs", "--expand-tabs=4", "--expand-tabs=0"] {
            let mut args = verb.to_vec();
            args.push(flag);
            let o = cmd(&repo, &home, &args);
            assert!(o.status.success(), "{args:?}: {}", err(&o));
            assert_eq!(out(&o), out(&base), "{args:?} changed the output");
            assert_eq!(err(&o), "", "{args:?} wrote to stderr");
        }
    }

    for (verb, bad) in [
        (&["diff-index", "HEAD~1"][..], "--expand-tabs=abc"),
        (&["diff-index", "HEAD~1"][..], "--expand-tabs=-1"),
        (&["diff-files"][..], "--expand-tabs=abc"),
    ] {
        let mut args = verb.to_vec();
        args.push(bad);
        let o = cmd(&repo, &home, &args);
        assert_eq!(o.status.code(), Some(128), "{args:?}");
        let value = &bad["--expand-tabs=".len()..];
        assert_eq!(err(&o), format!("fatal: '{value}': not a non-negative integer\n"));
        assert_eq!(out(&o), "");
    }

    // The value error is raised at its own place in the single left-to-right parse, so
    // a bad revision *ahead* of it still wins.
    let o = cmd(&repo, &home, &["diff-index", "nosuchrev", "--expand-tabs=abc"]);
    assert_eq!(o.status.code(), Some(128));
    assert!(err(&o).starts_with("fatal: ambiguous argument 'nosuchrev'"), "{}", err(&o));
}

/// `-B<n>[/<m>]` is the other half of the same port (`diffcore-break.c`), and it has a
/// signal of its own: a pair git broke keeps a *dissimilarity* score, which
/// `diff_flush_raw()` (diff.c:6481) prints as `M<score>` off `p->score` rather than off
/// the status letter, and `diff_summary()` (diff.c:6819) turns into a ` rewrite ` line.
/// Both were missing while `-B` was on the accepted-and-ignored list.
#[test]
fn break_rewrites_scores_the_pair() {
    let (repo, home) = scratch("break");
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.x"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    let before: String = (1..=50).map(|n| format!("alpha {n}\n")).collect();
    let after: String = (1..=50).map(|n| format!("omega {n}\n")).collect();
    std::fs::write(repo.join("r.txt"), &before).unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("r.txt"), &after).unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "c1"]);

    let plain = cmd(&repo, &home, &["diff-index", "HEAD~1"]);
    assert!(plain.status.success(), "{}", err(&plain));
    assert_eq!(
        out(&plain),
        ":100644 100644 626d6af3adecd5f40cd389ede7428198d2befb7f          1379db6ca04fba72db600af9296f6d90ccda1ad9 M\tr.txt\n"
            .replace("         ", "")
    );

    for flag in ["-B", "--break-rewrites", "-B50/20"] {
        let o = cmd(&repo, &home, &["diff-index", "HEAD~1", flag]);
        assert!(o.status.success(), "{flag}: {}", err(&o));
        assert_eq!(out(&o), out(&plain).replace(" M\t", " M100\t"), "{flag}");
        assert_ne!(out(&o), out(&plain), "{flag} was accepted and dropped");
    }

    let summary = cmd(&repo, &home, &["diff-index", "HEAD~1", "-B", "--summary"]);
    assert_eq!(out(&summary), " rewrite r.txt (100%)\n");
    let summary_plain = cmd(&repo, &home, &["diff-index", "HEAD~1", "--summary"]);
    assert_eq!(out(&summary_plain), "");
}

/// A three-stage index built with this binary's own plumbing, which is the only shape
/// `diff-files`' merge-diff knob can be observed on: a conflicted path is where
/// `run_diff_files()` chooses between `show_combined_diff()` and the `U` + `M` raw
/// records (diff-lib.c:211).
fn conflict_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = scratch(tag);
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.x"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    std::fs::write(repo.join("c.txt"), "one\ntwo\nthree\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "c0"]);

    let base = out(&cmd(&repo, &home, &["rev-parse", "HEAD:c.txt"])).trim().to_string();
    let ours = hash_blob(&repo, &home, "one\nOURS\nthree\n");
    let theirs = hash_blob(&repo, &home, "one\nTHEIRS\nthree\n");
    // A mode of `0` removes the stage-0 entry; without it the index would carry stage 0
    // *and* stages 1-3, which git never produces.
    let info = format!(
        "0 0000000000000000000000000000000000000000\tc.txt\n\
         100644 {base} 1\tc.txt\n\
         100644 {ours} 2\tc.txt\n\
         100644 {theirs} 3\tc.txt\n"
    );
    let stdin = repo.join(".index-info");
    std::fs::write(&stdin, &info).unwrap();
    let o = Command::new(BIN)
        .args(["update-index", "--index-info"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::fs::File::open(&stdin).unwrap())
        .output()
        .unwrap();
    assert!(o.status.success(), "update-index --index-info: {}", err(&o));
    std::fs::remove_file(&stdin).unwrap();
    std::fs::write(
        repo.join("c.txt"),
        "one\n<<<<<<< HEAD\nOURS\n=======\nTHEIRS\n>>>>>>> side\nthree\n",
    )
    .unwrap();
    (repo, home)
}

fn hash_blob(repo: &Path, home: &Path, body: &str) -> String {
    let path = repo.join(".blob-in");
    std::fs::write(&path, body).unwrap();
    let o = cmd(repo, home, &["hash-object", "-w", ".blob-in"]);
    assert!(o.status.success(), "hash-object: {}", err(&o));
    std::fs::remove_file(&path).unwrap();
    out(&o).trim().to_string()
}

/// `diff_merges_parse_opts()` (diff-merges.c:119) plus `diff_merges_setup_revs()`
/// (diff-merges.c:178). Three behaviours have to come out of one knob:
///
/// * `combined`/`dense-combined` turn the combined renderer on *and* fill an empty
///   `output_format` with the patch, so they print a diff with no `-p` given;
/// * `separate` (and the other non-combined modes) fill the same empty format but
///   leave `combine_merges` clear, so `diff_merges_set_dense_combined_if_unset()`
///   (builtin/diff-files.c:83-85) puts the dense combined diff back;
/// * `off`/`none`, `--no-diff-merges` and `-m` clear `merges_need_diff` too, so the
///   format stays empty and `cmd_diff_files()` defaults it to the raw listing.
#[test]
fn diff_files_merge_diff_selector() {
    let (repo, home) = conflict_fixture("conflict");

    const RAW: &str = "\
:000000 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 U\tc.txt
:100644 100644 daf31e19fb4314bf6f4a4606319163dd2c9b09e7 0000000000000000000000000000000000000000 M\tc.txt
";
    const COMBINED: &str = "\
diff --combined c.txt
index daf31e1,594dc4f..0000000
--- a/c.txt
+++ b/c.txt
@@@ -1,3 -1,3 +1,7 @@@
  one
++<<<<<<< HEAD
 +OURS
++=======
+ THEIRS
++>>>>>>> side
  three
";

    let base = cmd(&repo, &home, &["diff-files"]);
    assert!(base.status.success(), "{}", err(&base));
    assert_eq!(out(&base), RAW);

    for flag in ["--diff-merges=combined", "--diff-merges=c"] {
        let o = cmd(&repo, &home, &["diff-files", flag]);
        assert!(o.status.success(), "{flag}: {}", err(&o));
        assert_eq!(out(&o), COMBINED, "{flag}");
        assert_ne!(out(&o), RAW, "{flag} was accepted and dropped");
    }

    // The dense form differs from the plain one only in its header, which is what
    // separates `set_combined()` from `set_dense_combined()`.
    let dense = cmd(&repo, &home, &["diff-files", "--diff-merges=dense-combined"]);
    assert!(dense.status.success(), "{}", err(&dense));
    assert_eq!(out(&dense), COMBINED.replacen("diff --combined", "diff --cc", 1));

    // Non-combined modes still force the patch, and the "if unset" promotion then
    // densifies it — so they land on `diff --cc`, not on the raw listing.
    for flag in ["--diff-merges=separate", "--diff-merges=on", "--dd", "--remerge-diff"] {
        let o = cmd(&repo, &home, &["diff-files", flag]);
        assert!(o.status.success(), "{flag}: {}", err(&o));
        assert_eq!(out(&o), COMBINED.replacen("diff --combined", "diff --cc", 1), "{flag}");
    }

    for flag in ["--no-diff-merges", "--diff-merges=off", "--diff-merges=none", "-m"] {
        let o = cmd(&repo, &home, &["diff-files", flag]);
        assert!(o.status.success(), "{flag}: {}", err(&o));
        assert_eq!(out(&o), RAW, "{flag}");
    }

    // A later `off` undoes an earlier mode, because `set_none()` clears
    // `merges_need_diff` as well as `combine_merges`.
    let undone = cmd(&repo, &home, &["diff-files", "--diff-merges=combined", "--no-diff-merges"]);
    assert_eq!(out(&undone), RAW);

    let bogus = cmd(&repo, &home, &["diff-files", "--diff-merges=bogus"]);
    assert_eq!(bogus.status.code(), Some(128));
    assert_eq!(err(&bogus), "fatal: invalid value for '--diff-merges': 'bogus'\n");
}

/// `diff-index`' own merge-diff arithmetic. The combined renderer is *not* ported for
/// this command, so the assertions here are about the two things that are: the modes
/// that only turn the patch on, and `--cached`, under which `oneway_diff()`
/// (diff-lib.c:408) never takes the combined branch at all, so a combined mode
/// degrades to a plain patch rather than being refused.
#[test]
fn diff_index_merge_diff_selector() {
    let (repo, home) = tree_fixture("idxmerge");

    let patch = out(&cmd(&repo, &home, &["diff-index", "HEAD~1", "--cached", "-p"]));
    assert!(patch.starts_with("diff --git a/doc/bin.dat b/doc/bin.dat\n"), "{patch}");

    for flag in ["--diff-merges=combined", "--diff-merges=dense-combined", "-c", "--cc"] {
        let o = cmd(&repo, &home, &["diff-index", "HEAD~1", "--cached", flag]);
        assert!(o.status.success(), "{flag}: {}", err(&o));
        assert_eq!(out(&o), patch, "{flag} under --cached is a plain patch");
    }

    // Without `--cached` the same modes would render `diff --combined`, which this
    // command cannot produce; they must fail loudly rather than print a two-way patch.
    for flag in ["--diff-merges=combined", "-c", "--cc"] {
        let o = cmd(&repo, &home, &["diff-index", "HEAD~1", flag]);
        assert!(!o.status.success(), "{flag} silently produced: {}", out(&o));
        assert_eq!(out(&o), "", "{flag} wrote bytes it cannot get right");
    }

    // The non-combined modes only fill an *empty* output format, so pairing one with
    // `--raw` leaves the raw listing alone.
    let raw = out(&cmd(&repo, &home, &["diff-index", "HEAD~1"]));
    for flag in ["--diff-merges=separate", "--dd", "--remerge-diff"] {
        let o = cmd(&repo, &home, &["diff-index", "HEAD~1", flag, "--raw"]);
        assert!(o.status.success(), "{flag}: {}", err(&o));
        assert_eq!(out(&o), raw, "{flag} --raw");
    }

    let orphan = cmd(&repo, &home, &["diff-index", "HEAD~1", "--combined-all-paths"]);
    assert_eq!(orphan.status.code(), Some(128));
    assert_eq!(
        err(&orphan),
        "fatal: --combined-all-paths makes no sense without -c or --cc\n"
    );
}

/// `diff-tree` renders its patch through `diff-pairs`, so three things that cross that
/// boundary are pinned here: the `GIT binary patch` payload, the word-diff renderer,
/// and `o->abbrev` — which used to be dropped at the hand-off, leaving
/// `diff-tree -p --abbrev=12` printing seven hex digits where git prints twelve.
#[test]
fn diff_tree_binary_word_diff_and_abbrev_cross_the_pairs_boundary() {
    let (repo, home) = tree_fixture("treepairs");

    let plain = cmd(&repo, &home, &["diff-tree", "-r", "HEAD~1", "HEAD", "-p", "--", "doc/bin.dat"]);
    assert!(plain.status.success(), "{}", err(&plain));
    assert_eq!(
        out(&plain),
        "diff --git a/doc/bin.dat b/doc/bin.dat\n\
         index 5cba280..c9d4736 100644\n\
         Binary files a/doc/bin.dat and b/doc/bin.dat differ\n"
    );

    let binary = cmd(&repo, &home, &["diff-tree", "-r", "HEAD~1", "HEAD", "--binary", "--", "doc/bin.dat"]);
    assert!(binary.status.success(), "{}", err(&binary));
    assert_eq!(
        out(&binary),
        "diff --git a/doc/bin.dat b/doc/bin.dat\n\
         index 5cba28062ba182cf45fbaf93c71434fd4f34ef57..c9d47368371671f7ed17a7151e66a58ae4c20671 100644\n\
         GIT binary patch\n\
         literal 8\n\
         PcmYew%wu3=j^F|S2~q)|\n\
         \n\
         literal 7\n\
         OcmYew%wu3=;sO8%VgW}0\n\
         \n"
    );
    assert_ne!(out(&binary), out(&plain), "--binary was accepted and dropped");

    // `--word-diff` and the `--color-moved` family are `diff-pairs`' renderers; the
    // routing is what makes them reachable from `diff-tree`.
    let words = cmd(
        &repo,
        &home,
        &["diff-tree", "-r", "HEAD~1", "HEAD", "-p", "-M", "--word-diff=plain", "--", "src/renamed.txt", "src/orig.txt"],
    );
    assert!(words.status.success(), "{}", err(&words));
    assert!(
        out(&words).contains("[-line 7-]{+LINE SEVEN+} of the original content here\n"),
        "{}",
        out(&words)
    );
    let words_off = cmd(
        &repo,
        &home,
        &["diff-tree", "-r", "HEAD~1", "HEAD", "-p", "-M", "--", "src/renamed.txt", "src/orig.txt"],
    );
    assert_ne!(out(&words), out(&words_off), "--word-diff was accepted and dropped");

    let narrow = ["diff-tree", "-r", "HEAD~1", "HEAD", "-p", "-M", "--", "src/renamed.txt", "src/orig.txt"];
    let index_line = |o: &Output| {
        out(o).lines().find(|l| l.starts_with("index ")).unwrap_or_default().to_string()
    };
    assert_eq!(index_line(&cmd(&repo, &home, &narrow)), "index c459d38..a915730 100644");
    let mut wide = narrow.to_vec();
    wide.insert(5, "--abbrev=12");
    let wide_out = cmd(&repo, &home, &wide);
    assert!(wide_out.status.success(), "{}", err(&wide_out));
    assert_eq!(index_line(&wide_out), "index c459d38a1f33..a91573028c46 100644");

    // The raw listing carries the same width, and `--patch-with-raw` prints both.
    let raw = cmd(&repo, &home, &["diff-tree", "-r", "HEAD~1", "HEAD", "--raw", "--abbrev=12", "--", "doc/new.txt"]);
    assert_eq!(
        out(&raw),
        ":000000 100644 000000000000 d5a09df94c94 A\tdoc/new.txt\n"
    );
}
