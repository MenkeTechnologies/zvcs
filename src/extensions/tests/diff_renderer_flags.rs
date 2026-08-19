//! Diff options whose effect is a *renderer* choice rather than a stored field:
//! `--find-copies-harder`'s unmodified copy sources, the options that carry
//! `enable_patch_output()` (`--binary`, `-U<n>`, `--diff-merges=<mode>`), the
//! `check_mask` formats' mutual exclusion, `-I<re>` on the history commands, and
//! the `%G…` family.
//!
//! Every expectation is a byte string measured from stock git 2.55.0; nothing here
//! shells out to a second git, so the suite runs on a headless Linux CI box with
//! only this binary present.
//!
//! Each case is pinned twice: against the bytes stock produces *and* against the
//! command's own default output. The second assertion is the one that matters —
//! a flag that is merely accepted and then dropped exits 0 and prints the default,
//! so only `assert_ne!` against the default can tell "plumbed" from "swallowed".

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A repository with three interesting pairs between `HEAD~1` and `HEAD`:
/// `new/copy.txt` is a byte-identical copy of the *unmodified-at-that-point*
/// `src/b.txt` (the only thing `--find-copies-harder` can find), `src/b.txt` gains
/// a line, and `readme.txt` changes a line whose text `-I` can match.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("zvcs-rendflags-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("new")).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.x"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    std::fs::write(repo.join("src/b.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(repo.join("readme.txt"), "doc line one\ndoc line two\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("src/b.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    std::fs::write(repo.join("readme.txt"), "doc line one CHANGED\ndoc line two\n").unwrap();
    std::fs::write(repo.join("new/copy.txt"), "one\ntwo\nthree\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "c1"]);
    (repo, home)
}

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

/// `diff_setup_done()` (diff.c:5288) turns copy detection on for a lone
/// `--find-copies-harder`, and the tree walk then has to supply the unmodified
/// pairs it exists to search (tree-diff.c:519, 557). Without both halves the flag
/// is accepted and the copy is reported as a plain addition — exit 0, wrong bytes.
#[test]
fn find_copies_harder_finds_an_unmodified_source() {
    let (repo, home) = fixture("fch");

    // The raw listing names the copy, its source and its `C100` score.
    let raw = cmd(&repo, &home, &["diff-tree", "-r", "--find-copies-harder", "HEAD~1", "HEAD"]);
    assert!(raw.status.success(), "{}", err(&raw));
    assert!(
        out(&raw).contains("C100\tsrc/b.txt\tnew/copy.txt"),
        "diff-tree lost the copy record: {}",
        out(&raw)
    );
    let plain = cmd(&repo, &home, &["diff-tree", "-r", "HEAD~1", "HEAD"]);
    assert_ne!(out(&raw), out(&plain), "--find-copies-harder was accepted and dropped");
    assert!(out(&plain).contains("A\tnew/copy.txt"));

    // The same pair through the patch renderers the history commands share.
    for args in [
        &["log", "--no-decorate", "-p", "-1", "--find-copies-harder", "HEAD"][..],
        &["show", "--no-decorate", "--find-copies-harder", "HEAD"][..],
        &["diff", "--find-copies-harder", "HEAD~1", "HEAD"][..],
    ] {
        let o = cmd(&repo, &home, args);
        assert!(o.status.success(), "{args:?}: {}", err(&o));
        assert!(
            out(&o).contains("copy from src/b.txt\ncopy to new/copy.txt\n"),
            "{args:?} lost the copy header: {}",
            out(&o)
        );
        let bare: Vec<&str> = args.iter().copied().filter(|a| *a != "--find-copies-harder").collect();
        assert_ne!(out(&o), out(&cmd(&repo, &home, &bare)), "{args:?} was a no-op");
    }
}

/// `diff_opt_binary()` (diff.c:5730) and `diff_opt_unified()` (diff.c:5961) both end
/// in `enable_patch_output()`, so on a plumbing command whose default format is the
/// raw listing either flag replaces that listing with a patch.
#[test]
fn binary_and_unified_turn_the_patch_format_on() {
    let (repo, home) = fixture("enable");

    let patch = out(&cmd(&repo, &home, &["diff-index", "-p", "HEAD~1"]));
    let raw = out(&cmd(&repo, &home, &["diff-index", "HEAD~1"]));
    assert!(patch.starts_with("diff --git "), "{patch}");
    assert!(raw.starts_with(':'), "{raw}");

    // `--binary` alone, with no binary content anywhere in the fixture, still
    // selects the patch format.
    let bin = cmd(&repo, &home, &["diff-index", "--binary", "HEAD~1"]);
    assert!(bin.status.success(), "{}", err(&bin));
    assert_eq!(out(&bin), patch);
    assert_ne!(out(&bin), raw);

    // `-s` is `OPT_SET_INT`, which assigns; a later `enable_patch_output()` clears
    // the `NO_OUTPUT` bit it set, so order decides.
    assert_eq!(out(&cmd(&repo, &home, &["diff-index", "-s", "--binary", "HEAD~1"])), patch);
    assert_eq!(out(&cmd(&repo, &home, &["diff-index", "-s", "-p", "HEAD~1"])), patch);
    assert_eq!(out(&cmd(&repo, &home, &["diff-index", "-p", "-s", "HEAD~1"])), "");
    // …and it discards the formats named before it, so the stat does not survive.
    assert_eq!(out(&cmd(&repo, &home, &["diff-index", "--stat", "-s", "-p", "HEAD~1"])), patch);

    // `-U<n>` on `diff-tree`, whose default is also the raw listing.
    let u1 = cmd(&repo, &home, &["diff-tree", "-r", "-U1", "HEAD~1", "HEAD"]);
    assert!(u1.status.success(), "{}", err(&u1));
    assert!(out(&u1).starts_with("diff --git "), "{}", out(&u1));
    // One line of context, so the tail hunk narrows to `@@ -3 +3,2 @@` — the byte
    // that separates a real `-U1` from `-U3` wearing its name.
    assert!(out(&u1).contains("@@ -3 +3,2 @@ two\n"), "{}", out(&u1));
    assert!(
        out(&cmd(&repo, &home, &["diff-tree", "-r", "-p", "HEAD~1", "HEAD"]))
            .contains("@@ -1,3 +1,4 @@"),
        "the default context should still be 3"
    );
    assert_ne!(out(&u1), out(&cmd(&repo, &home, &["diff-tree", "-r", "HEAD~1", "HEAD"])));
}

/// `diff_merges_setup_revs()` (diff-merges.c:188): every mode but `off`/`none` sets
/// `merges_need_diff`, which fills an empty `output_format` with `DIFF_FORMAT_PATCH`.
/// The two combined modes render `diff --combined`, which is not ported — they must
/// refuse rather than fall back to the raw listing.
#[test]
fn diff_merges_selects_the_patch_format_or_declines() {
    let (repo, home) = fixture("dm");
    let patch = out(&cmd(&repo, &home, &["diff-index", "-p", "HEAD~1"]));
    let raw = out(&cmd(&repo, &home, &["diff-index", "HEAD~1"]));

    for mode in ["on", "m", "1", "first-parent", "separate", "r", "remerge"] {
        let o = cmd(&repo, &home, &["diff-index", &format!("--diff-merges={mode}"), "HEAD~1"]);
        assert!(o.status.success(), "--diff-merges={mode}: {}", err(&o));
        assert_eq!(out(&o), patch, "--diff-merges={mode} did not select the patch format");
        assert_ne!(out(&o), raw);
    }
    for spelling in ["--remerge-diff", "--dd"] {
        let o = cmd(&repo, &home, &["diff-index", spelling, "HEAD~1"]);
        assert!(o.status.success(), "{spelling}: {}", err(&o));
        assert_eq!(out(&o), patch, "{spelling} did not select the patch format");
    }
    // `set_none()` leaves the format alone.
    for mode in ["off", "none"] {
        let o = cmd(&repo, &home, &["diff-index", &format!("--diff-merges={mode}"), "HEAD~1"]);
        assert_eq!(out(&o), raw, "--diff-merges={mode} should change nothing");
    }
    // Not ported, and therefore not silently answered with the raw listing.
    for mode in ["c", "combined", "cc", "dense-combined"] {
        let o = cmd(&repo, &home, &["diff-index", &format!("--diff-merges={mode}"), "HEAD~1"]);
        assert_ne!(out(&o), raw, "--diff-merges={mode} fell back to the raw listing");
        assert_ne!(out(&o), patch, "--diff-merges={mode} rendered a non-combined patch");
    }
    // `set_diff_merges()`'s own `die()` (diff-merges.c:92).
    let bad = cmd(&repo, &home, &["diff-index", "--diff-merges=bogus", "HEAD~1"]);
    assert_eq!(bad.status.code(), Some(128));
    assert_eq!(err(&bad), "fatal: invalid value for '--diff-merges': 'bogus'\n");
}

/// `diff_setup_done()`'s first `HAS_MULTI_BITS` (diff.c:5259). `-s` is `OPT_SET_INT`
/// and replaces the word, so `--name-only -s` is fine while `-s --name-only` is fatal.
#[test]
fn the_four_exclusive_output_formats_conflict() {
    let (repo, home) = fixture("mask");
    const MSG: &str =
        "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together\n";

    for (verb, tail) in [("diff-index", vec!["HEAD~1"]), ("diff-tree", vec!["-r", "HEAD~1", "HEAD"])] {
        for second in ["--name-only", "--name-status", "--check"] {
            let mut args = vec![verb, "-s", second];
            args.extend_from_slice(&tail);
            let o = cmd(&repo, &home, &args);
            assert_eq!(o.status.code(), Some(128), "{args:?} should be fatal: {}", out(&o));
            assert_eq!(err(&o), MSG, "{args:?}");
        }
    }
    // The reverse order is not a conflict at all: `-s` discarded the earlier bit.
    let ok = cmd(&repo, &home, &["diff-index", "--name-only", "-s", "HEAD~1"]);
    assert!(ok.status.success(), "{}", err(&ok));
    assert_eq!(out(&ok), "");
    // …and a format after that `-s` wins outright.
    let after = cmd(&repo, &home, &["diff-index", "--name-only", "-s", "-p", "HEAD~1"]);
    assert_eq!(out(&after), out(&cmd(&repo, &home, &["diff-index", "-p", "HEAD~1"])));
    // A bad revision is resolved first, so it still wins over the conflict.
    let rev = cmd(&repo, &home, &["diff-index", "-s", "--name-only", "nosuchrev"]);
    assert_eq!(rev.status.code(), Some(128));
    assert!(err(&rev).starts_with("fatal: ambiguous argument 'nosuchrev'"), "{}", err(&rev));
}

/// `-I<re>` / `--ignore-matching-lines=<re>` (`diff_opt_ignore_regex()`, diff.c:5859)
/// on the history commands, which needed the compiled patterns to travel with the
/// per-worker options.
#[test]
fn ignore_matching_lines_reaches_log_and_show() {
    let (repo, home) = fixture("ire");

    for (verb, extra) in [("log", vec!["--no-decorate", "-p", "-1"]), ("show", vec!["--no-decorate"])] {
        let mut bare = vec![verb];
        bare.extend_from_slice(&extra);
        bare.push("HEAD");
        let plain = out(&cmd(&repo, &home, &bare));
        assert!(plain.contains("doc line one CHANGED"), "{verb}: {plain}");

        for spelling in ["-Idoc", "--ignore-matching-lines=doc"] {
            let mut args = vec![verb];
            args.extend_from_slice(&extra);
            args.push(spelling);
            args.push("HEAD");
            let o = cmd(&repo, &home, &args);
            assert!(o.status.success(), "{args:?}: {}", err(&o));
            assert!(
                !out(&o).contains("doc line one CHANGED"),
                "{args:?} kept the ignored hunk: {}",
                out(&o)
            );
            // The unrelated file still diffs, so the pattern narrowed rather than
            // silencing the command.
            assert!(out(&o).contains("+four"), "{args:?}: {}", out(&o));
            assert_ne!(out(&o), plain, "{args:?} was accepted and dropped");
        }
        // The separated form eats the next argv slot, as `PARSE_OPT` requires.
        let mut sep = vec![verb];
        sep.extend_from_slice(&extra);
        sep.extend_from_slice(&["-I", "doc", "HEAD"]);
        assert_ne!(out(&cmd(&repo, &home, &sep)), plain);

        // `regcomp` failing is `error:` + parse-options' 129, not a panic.
        let mut bad = vec![verb];
        bad.extend_from_slice(&extra);
        bad.extend_from_slice(&["-I[", "HEAD"]);
        let o = cmd(&repo, &home, &bad);
        assert_eq!(o.status.code(), Some(129), "{bad:?}: {}", err(&o));
        assert_eq!(err(&o), "error: invalid regex given to -I: '['\n");
    }
}

/// The `%G…` family (pretty.c:1659-1710). On an unsigned commit
/// `parse_signed_commit()` answers 0, so every field is the value
/// `check_signature()` starts from — including `%GT`, which prints the *name* of
/// `TRUST_UNDEFINED` rather than nothing.
#[test]
fn the_signature_placeholder_family_renders() {
    let (repo, home) = fixture("gfam");

    let o = cmd(&repo, &home, &["log", "--format=[%GT][%GS][%GG][%GP][%G?]", "-1"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "[undefined][][][][N]\n");
    // `%GT` is the one that would survive a stub that printed nothing.
    assert_eq!(out(&cmd(&repo, &home, &["log", "--format=%GT", "-1"])), "undefined\n");
    assert_eq!(out(&cmd(&repo, &home, &["show", "-s", "--format=%GT", "HEAD"])), "undefined\n");

    // `%GF` prints `sigc->fingerprint`, which the shared verifier does not fill for
    // an ssh signature; it stays refused rather than printing an empty field where
    // stock prints one. This pins that refusal so it is not quietly widened without
    // the verifier being fixed first.
    let gf = cmd(&repo, &home, &["log", "--format=%GF", "-1"]);
    assert!(!gf.status.success(), "%GF was accepted without the field behind it");
}

/// `show_signature()` (log-tree.c:580, called at :851) runs for every pretty format.
/// An unsigned commit prints nothing at all, which is what makes the flag safe to
/// accept here — but it must be *accepted*, not refused.
#[test]
fn show_signature_is_accepted_and_silent_on_an_unsigned_commit() {
    let (repo, home) = fixture("sigflag");

    for (verb, extra) in [("log", vec!["--no-decorate", "-1"]), ("show", vec!["--no-decorate"])] {
        let mut bare = vec![verb];
        bare.extend_from_slice(&extra);
        bare.push("HEAD");
        let plain = cmd(&repo, &home, &bare);

        for spelling in ["--show-signature", "--no-show-signature"] {
            let mut args = vec![verb];
            args.extend_from_slice(&extra);
            args.push(spelling);
            args.push("HEAD");
            let o = cmd(&repo, &home, &args);
            assert!(o.status.success(), "{args:?}: {}", err(&o));
            assert_eq!(out(&o), out(&plain), "{args:?} changed an unsigned commit's output");
        }
    }
    // Every built-in format reaches the same call site, so none of them may refuse.
    for pretty in ["oneline", "short", "medium", "full", "fuller", "raw", "email"] {
        let o = cmd(
            &repo,
            &home,
            &["log", "--no-decorate", "--show-signature", &format!("--pretty={pretty}"), "-1"],
        );
        assert!(o.status.success(), "--pretty={pretty}: {}", err(&o));
    }
}

/// Diff options that set a `diff_options` field to the value these commands already
/// run at. They are accepted rather than refused, and accepting them must leave the
/// output byte-identical — the check that separates a proven no-op from a swallowed
/// flag that should have changed something.
#[test]
fn the_provable_noop_diff_options_change_nothing() {
    let (repo, home) = fixture("noop");
    const NOOPS: &[&str] = &[
        "--no-compact-summary",
        "--no-color-moved",
        "--color-moved=no",
        "--no-color-moved-ws",
        "--word-diff=none",
        "--no-textconv",
        "--no-ext-diff",
        "--no-relative",
        "--ita-visible-in-index",
        "--ita-invisible-in-index",
        "--rename-empty",
    ];

    for (verb, extra) in [
        ("log", vec!["--no-decorate", "-p", "-1"]),
        ("show", vec!["--no-decorate"]),
    ] {
        let mut bare = vec![verb];
        bare.extend_from_slice(&extra);
        bare.push("HEAD");
        let plain = cmd(&repo, &home, &bare);
        assert!(plain.status.success(), "{}", err(&plain));

        for flag in NOOPS {
            let mut args = vec![verb];
            args.extend_from_slice(&extra);
            args.push(flag);
            args.push("HEAD");
            let o = cmd(&repo, &home, &args);
            assert!(o.status.success(), "{verb} {flag}: {}", err(&o));
            assert_eq!(out(&o), out(&plain), "{verb} {flag} changed the output");
            assert_eq!(err(&o), "", "{verb} {flag} wrote to stderr");
        }
    }

    // `--no-rename-empty` is deliberately *not* in that list. It is accepted, and
    // on this fixture — which holds no empty-file rename — it prints the same bytes;
    // but it is not inert, and the case below is what proves the difference.
    let o = cmd(&repo, &home, &["log", "--no-decorate", "-p", "-1", "--no-rename-empty", "HEAD"]);
    assert!(o.status.success(), "--no-rename-empty: {}", err(&o));
    assert_eq!(err(&o), "", "--no-rename-empty wrote to stderr");
}

/// `--no-rename-empty` (`o->flags.rename_empty = 0`): `record_if_better()`
/// (diffcore-rename.c) refuses a pair whose surviving side is an empty blob, so an
/// empty file that moved reports as a deletion plus an addition instead of an
/// `R100`. Both halves are measured from stock git 2.55.0 — the bytes it prints,
/// and the default it has to differ from.
#[test]
fn no_rename_empty_splits_an_empty_file_rename() {
    let root =
        std::env::temp_dir().join(format!("zvcs-rendflags-emptyrename-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("empty.txt"), "").unwrap();
    std::fs::write(repo.join("other.txt"), "x\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "c0"]);
    std::fs::rename(repo.join("empty.txt"), repo.join("moved.txt")).unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "c1"]);

    let plain = cmd(&repo, &home, &["log", "--no-decorate", "-p", "-1", "--format=%s", "HEAD"]);
    assert!(plain.status.success(), "{}", err(&plain));
    assert_eq!(
        out(&plain),
        concat!(
            "c1\n",
            "\n",
            "diff --git a/empty.txt b/moved.txt\n",
            "similarity index 100%\n",
            "rename from empty.txt\n",
            "rename to moved.txt\n",
        ),
    );

    let split = cmd(
        &repo,
        &home,
        &["log", "--no-decorate", "-p", "-1", "--format=%s", "--no-rename-empty", "HEAD"],
    );
    assert!(split.status.success(), "{}", err(&split));
    assert_eq!(
        out(&split),
        concat!(
            "c1\n",
            "\n",
            "diff --git a/empty.txt b/empty.txt\n",
            "deleted file mode 100644\n",
            "index e69de29..0000000\n",
            "diff --git a/moved.txt b/moved.txt\n",
            "new file mode 100644\n",
            "index 0000000..e69de29\n",
        ),
    );
    assert_ne!(out(&split), out(&plain), "--no-rename-empty was swallowed");

    // `--rename-empty` is `diff_setup()`'s default, so it puts the `R100` back.
    let back = cmd(
        &repo,
        &home,
        &[
            "log",
            "--no-decorate",
            "-p",
            "-1",
            "--format=%s",
            "--no-rename-empty",
            "--rename-empty",
            "HEAD",
        ],
    );
    assert_eq!(out(&back), out(&plain), "the last spelling on the line wins");

    let _ = std::fs::remove_dir_all(&root);
}
