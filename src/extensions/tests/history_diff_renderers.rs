//! The diff renderers `git log`/`git show`/`git whatchanged` share with `git diff`,
//! plus the two `format-patch` selectors `git rebase --apply` drives.
//!
//! These are the options that used to be *refused* by the history commands even
//! though `git diff` rendered them: `--dirstat` and its parameter block,
//! `--compact-summary`, `--relative`, `--diff-filter`, `--output-indicator-*`,
//! `--word-diff`, and the whole `--diff-merges=` family on `diff-tree`.
//!
//! Every expectation is a byte string measured from stock git 2.55.0 on the fixture
//! built below; nothing here shells out to a second git, so the suite runs on a
//! headless Linux CI box with only this binary present.
//!
//! Each case is pinned twice: against the bytes stock produces *and* against the
//! command's own default output. The second assertion is the load-bearing one —
//! a flag that is merely accepted and then dropped exits 0 and prints the default,
//! so only `assert_ne!` against the default separates "plumbed" from "swallowed".

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
        .env("GIT_PAGER", "cat")
        .env("TERM", "dumb")
        .env("COLUMNS", "80")
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
    let root = std::env::temp_dir().join(format!("zvcs-histrend-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(home.join(".gitconfig"), "").unwrap();
    (repo.canonicalize().unwrap(), home.canonicalize().unwrap())
}

/// Two commits whose change set spans three directories with very different damage
/// shares (which is what makes `--dirstat`'s percentages discriminating), a
/// creation, a deletion, a rename and an added line with trailing whitespace.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = scratch(tag);
    std::fs::create_dir_all(repo.join("src/core")).unwrap();
    std::fs::create_dir_all(repo.join("src/util")).unwrap();
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("src/core/a.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();
    std::fs::write(repo.join("src/util/b.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(repo.join("docs/readme.txt"), "doc line one\ndoc line two\n").unwrap();
    std::fs::write(repo.join("root.txt"), "keep\n").unwrap();
    std::fs::write(repo.join("docs/dropped.txt"), "dropped one\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "base"]);
    std::fs::write(repo.join("src/core/a.txt"), "alpha\nBETA changed here\ngamma\ndelta\nepsilon\n")
        .unwrap();
    std::fs::write(repo.join("src/util/b.txt"), "one\ntwo\nthree\nfour \n").unwrap();
    std::fs::write(repo.join("docs/readme.txt"), "doc line one modified words here\ndoc line two\n")
        .unwrap();
    std::fs::remove_file(repo.join("docs/dropped.txt")).unwrap();
    std::fs::write(repo.join("docs/added.txt"), "brand new\n").unwrap();
    run(&repo, &home, &["mv", "root.txt", "renamed.txt"]);
    std::fs::write(repo.join("renamed.txt"), "keep\nmore\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "second"]);
    (repo, home)
}

/// A merge with a real conflict resolution on both files, so every `--diff-merges`
/// mode has something distinct to say about it.
fn merge_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = scratch(tag);
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "alpha\nbeta\ngamma\n").unwrap();
    std::fs::write(repo.join("g.txt"), "shared\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "base"]);
    run(&repo, &home, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("f.txt"), "alpha\nBETA-side\ngamma\n").unwrap();
    std::fs::write(repo.join("g.txt"), "shared\nside-only\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "side-change"]);
    run(&repo, &home, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f.txt"), "alpha\nbeta\nGAMMA-main\n").unwrap();
    std::fs::write(repo.join("g.txt"), "shared\nmain-only\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "main-change"]);
    // The merge conflicts; the resolution is written by hand so the result is
    // reproducible whichever way the merge driver reports the conflict.
    let _ = cmd(&repo, &home, &["merge", "--no-ff", "--no-commit", "side"]);
    std::fs::write(repo.join("f.txt"), "alpha\nBETA-side\nGAMMA-main\n").unwrap();
    std::fs::write(repo.join("g.txt"), "shared\nside-only\nmain-only\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "merged"]);
    (repo, home)
}

/// `--dirstat`'s three damage models, and the fact that `show_dirstat()` is the one
/// format writer `diff_flush()` never counts into `separator` (diff.c:7238 sits
/// outside the block that does `separator++`), so `--dirstat -p` runs the patch
/// straight on where `--stat -p` inserts a blank line.
#[test]
fn dirstat_damage_models_and_its_missing_separator() {
    let (repo, home) = fixture("dirstat");

    let base = ["log", "--no-decorate", "-1"];
    let plain = out(&cmd(&repo, &home, &base));

    // Default mode: per-pair *byte* damage, so the one-line docs change outweighs
    // the four-line src/util change.
    let content = cmd(&repo, &home, &[&base[..], &["--dirstat"][..]].concat());
    assert!(content.status.success(), "{}", err(&content));
    assert!(
        out(&content).ends_with("  61.8% docs/\n  28.1% src/core/\n   5.4% src/util/\n"),
        "--dirstat: {}",
        out(&content)
    );
    assert_ne!(out(&content), plain, "--dirstat was accepted and dropped");

    // `--dirstat-by-file` charges one unit per changed file, so the five pairs
    // split evenly and the two-file `src/` subtrees fall below nothing.
    let by_file = cmd(&repo, &home, &[&base[..], &["--dirstat-by-file"][..]].concat());
    assert!(
        out(&by_file).ends_with("  50.0% docs/\n  16.6% src/core/\n  16.6% src/util/\n"),
        "--dirstat-by-file: {}",
        out(&by_file)
    );
    assert_ne!(out(&by_file), out(&content), "--dirstat-by-file used the content model");

    // `--cumulative` adds the parent directories' rolled-up shares.
    let cumulative = cmd(&repo, &home, &[&base[..], &["--cumulative"][..]].concat());
    assert!(out(&cumulative).ends_with("   5.4% src/util/\n  33.6% src/\n"), "{}", out(&cumulative));
    assert_ne!(out(&cumulative), out(&content), "--cumulative was accepted and dropped");

    // `--dirstat=<params>` reaches the same `struct dirstat_opts`.
    let params = cmd(&repo, &home, &[&base[..], &["--dirstat=files,cumulative"][..]].concat());
    assert!(out(&params).contains("  50.0% docs/\n"), "{}", out(&params));
    assert!(out(&params).contains("% src/\n"), "cumulative lost: {}", out(&params));

    // A bad parameter is `parse_dirstat_opt()`'s `die()`, exit 128.
    let bad = cmd(&repo, &home, &[&base[..], &["--dirstat=nonsuch"][..]].concat());
    assert_eq!(bad.status.code(), Some(128), "{}", err(&bad));
    assert!(
        err(&bad).starts_with("fatal: Failed to parse --dirstat/-X option parameter:\n"),
        "{}",
        err(&bad)
    );

    // The separator rule: dirstat earns none, stat does.
    let ds_patch = out(&cmd(&repo, &home, &["log", "--no-decorate", "-p", "-1", "--dirstat"]));
    assert!(
        ds_patch.contains("   5.4% src/util/\ndiff --git a/docs/added.txt"),
        "--dirstat -p inserted a separator it does not own: {ds_patch}"
    );
    let st_patch = out(&cmd(&repo, &home, &["log", "--no-decorate", "-p", "-1", "--stat"]));
    assert!(
        st_patch.contains("3 deletions(-)\n\ndiff --git a/docs/added.txt"),
        "--stat -p lost its separator: {st_patch}"
    );
}

/// `--compact-summary` (`diff_opt_compact_summary()`): the ` (<comment>)` suffix
/// `fill_print_name()` derives from the two sides' mode words, and the fact that the
/// flag also turns `--stat` on while `--no-compact-summary` only clears the suffix.
#[test]
fn compact_summary_annotates_and_implies_stat() {
    let (repo, home) = fixture("compact");
    for verb in [&["log", "--no-decorate", "-1"][..], &["show", "--no-decorate", "HEAD"][..]] {
        let o = cmd(&repo, &home, &[verb, &["--compact-summary"][..]].concat());
        assert!(o.status.success(), "{verb:?}: {}", err(&o));
        assert!(
            out(&o).contains(" docs/added.txt (new)    | 1 +\n docs/dropped.txt (gone) | 1 -\n"),
            "{verb:?} lost the compact annotations: {}",
            out(&o)
        );
        // It implied `--stat`: the summary line is there without one being asked for.
        assert!(out(&o).contains("6 files changed, 6 insertions(+), 3 deletions(-)\n"));
        assert_ne!(out(&o), out(&cmd(&repo, &home, verb)), "{verb:?}: flag was a no-op");

        // `--no-compact-summary` clears only the annotation, so the stat block the
        // earlier flag turned on survives without it.
        let off = cmd(&repo, &home, &[verb, &["--compact-summary", "--no-compact-summary"][..]].concat());
        assert!(out(&off).contains(" docs/added.txt          | 1 +\n"), "{verb:?}: {}", out(&off));
        assert!(!out(&off).contains("(new)"), "{verb:?}: {}", out(&off));
    }
}

/// `--relative[=<path>]` is two separate things: `diff_queue()`'s prefix test
/// (diff.c:7630) narrows every format, while `strip_prefix()` (diff.c:5009) shortens
/// only the patch, raw, name and stat writers. `diff_summary()` and `show_dirstat()`
/// never call it, so both keep the repository-root name.
#[test]
fn relative_narrows_every_format_but_shortens_only_some() {
    let (repo, home) = fixture("relative");
    let base = ["log", "--no-decorate", "-1", "--relative=src"];

    let stat = out(&cmd(&repo, &home, &[&base[..], &["--stat"][..]].concat()));
    assert!(
        stat.ends_with(" core/a.txt | 3 ++-\n util/b.txt | 1 +\n 2 files changed, 3 insertions(+), 1 deletion(-)\n"),
        "--relative --stat: {stat}"
    );

    let raw = out(&cmd(&repo, &home, &[&base[..], &["--raw"][..]].concat()));
    assert!(raw.contains("M\tcore/a.txt\n"), "--relative --raw: {raw}");
    assert!(!raw.contains("src/core"), "--relative --raw kept the prefix: {raw}");

    let names = out(&cmd(&repo, &home, &[&base[..], &["--name-only"][..]].concat()));
    assert_eq!(names.lines().filter(|l| !l.is_empty()).last(), Some("util/b.txt"));

    // Narrowed but *not* shortened: the dirstat rows still name `src/core/`.
    let ds = out(&cmd(&repo, &home, &[&base[..], &["--dirstat"][..]].concat()));
    assert!(ds.ends_with("  83.7% src/core/\n  16.2% src/util/\n"), "--relative --dirstat: {ds}");

    // The patch is both narrowed and shortened.
    let patch = out(&cmd(&repo, &home, &[&base[..], &["-p"][..]].concat()));
    assert!(patch.contains("diff --git a/core/a.txt b/core/a.txt\n"), "{patch}");
    assert!(!patch.contains("docs/readme.txt"), "--relative did not narrow the patch: {patch}");

    // `--no-relative` puts everything back.
    let off = out(&cmd(
        &repo,
        &home,
        &["log", "--no-decorate", "-1", "--relative=src", "--no-relative", "--raw"],
    ));
    assert!(off.contains("M\tsrc/core/a.txt\n"), "--no-relative: {off}");

    // `git show` shares the same machinery.
    let shown = out(&cmd(&repo, &home, &["show", "--no-decorate", "--relative=src", "--stat", "HEAD"]));
    assert!(shown.contains(" core/a.txt | 3 ++-\n"), "show --relative: {shown}");
}

/// `--diff-filter` is a queue filter, so `cmd_log_init_finish()` (builtin/log.c:333)
/// clears `always_show_header` for it exactly as it does for the pickaxe: a commit
/// the filter emptied prints nothing at all, not even its header. `revision.c:3149`
/// additionally raises `revs->diff` for it, which is why the queue is still built
/// under `-s`.
#[test]
fn diff_filter_selects_pairs_and_suppresses_emptied_commits() {
    let (repo, home) = fixture("filter");

    let raw = cmd(&repo, &home, &["log", "--no-decorate", "--diff-filter=M", "--raw"]);
    assert!(raw.status.success(), "{}", err(&raw));
    let text = out(&raw);
    assert!(text.contains("M\tdocs/readme.txt\n"), "{text}");
    assert!(!text.contains("\tdocs/added.txt"), "an addition survived --diff-filter=M: {text}");
    // The root commit is all additions, so the filter empties it and it disappears.
    assert!(!text.contains("base"), "an emptied commit kept its header: {text}");
    assert_ne!(
        text,
        out(&cmd(&repo, &home, &["log", "--no-decorate", "--raw"])),
        "--diff-filter was accepted and dropped"
    );

    // A lowercase letter excludes; unlisted statuses then stay.
    let excl = out(&cmd(&repo, &home, &["log", "--no-decorate", "-1", "--diff-filter=m", "--raw"]));
    assert!(excl.contains("A\tdocs/added.txt\n"), "{excl}");
    assert!(!excl.contains("M\tdocs/readme.txt\n"), "{excl}");

    // Under `-s` nothing is rendered, but the queue still decides the header.
    let silent = out(&cmd(&repo, &home, &["log", "--no-decorate", "-s", "--diff-filter=M"]));
    assert!(silent.contains("second"), "-s --diff-filter lost the surviving commit: {silent}");
    assert!(!silent.contains("base"), "-s --diff-filter kept an emptied commit: {silent}");

    // `git show` suppresses the same way.
    let none = cmd(&repo, &home, &["show", "--no-decorate", "--diff-filter=C", "HEAD"]);
    assert!(none.status.success(), "{}", err(&none));
    assert_eq!(out(&none), "", "show printed a header for an emptied queue: {}", out(&none));
}

/// `--output-indicator-new`/`-old`/`-context` (`diff_opt_char()`, diff.c:5593):
/// `emit_line_ws_markup()` (diff.c:1369) substitutes the byte at emit time, so the
/// `+++`/`---` file headers keep their own characters and an empty value stores the
/// NUL that `emit_line_0()` declines to write.
#[test]
fn output_indicators_replace_only_the_body_signs() {
    let (repo, home) = fixture("indicator");
    for verb in [
        &["diff", "HEAD~1", "HEAD"][..],
        &["log", "--no-decorate", "-p", "-1"][..],
        &["show", "--no-decorate", "HEAD"][..],
    ] {
        let o = cmd(&repo, &home, &[verb, &["--output-indicator-new=@"][..]].concat());
        assert!(o.status.success(), "{verb:?}: {}", err(&o));
        assert!(
            out(&o).contains("-doc line one\n@doc line one modified words here\n doc line two\n"),
            "{verb:?}: {}",
            out(&o)
        );
        // The `+++ b/...` header is not a body line and keeps its own signs.
        assert!(out(&o).contains("+++ b/docs/readme.txt\n"), "{verb:?}: {}", out(&o));
        assert_ne!(out(&o), out(&cmd(&repo, &home, verb)), "{verb:?}: flag was a no-op");

        // All three at once.
        let all = cmd(
            &repo,
            &home,
            &[
                verb,
                &["--output-indicator-old=%", "--output-indicator-context=~"][..],
            ]
            .concat(),
        );
        assert!(
            out(&all).contains("%doc line one\n+doc line one modified words here\n~doc line two\n"),
            "{verb:?}: {}",
            out(&all)
        );

        // An empty value drops the sign entirely.
        let empty = cmd(&repo, &home, &[verb, &["--output-indicator-new="][..]].concat());
        assert!(
            out(&empty).contains("-doc line one\ndoc line one modified words here\n"),
            "{verb:?}: {}",
            out(&empty)
        );

        // More than one byte is `error: <name> expects a character, got '<arg>'`.
        let bad = cmd(&repo, &home, &[verb, &["--output-indicator-new=ab"][..]].concat());
        assert_eq!(bad.status.code(), Some(129), "{verb:?}: {}", err(&bad));
        assert_eq!(err(&bad), "error: output-indicator-new expects a character, got 'ab'\n");
    }
}

/// The `--word-diff` / `--ws-error-highlight` family re-emits the assembled patch
/// rather than changing how it is generated, so the history commands run it through
/// the same `fn_out_consume()` chain `git diff` uses.
#[test]
fn word_diff_rewrites_the_history_patch_too() {
    let (repo, home) = fixture("worddiff");
    for verb in [&["log", "--no-decorate", "-p", "-1"][..], &["show", "--no-decorate", "HEAD"][..]] {
        let plain = cmd(&repo, &home, &[verb, &["--word-diff"][..]].concat());
        assert!(plain.status.success(), "{verb:?}: {}", err(&plain));
        assert!(
            out(&plain).contains("doc line one {+modified words here+}\ndoc line two\n"),
            "{verb:?}: {}",
            out(&plain)
        );
        assert_ne!(out(&plain), out(&cmd(&repo, &home, verb)), "{verb:?}: --word-diff was a no-op");

        // `porcelain` keeps the sign column and terminates each record with `~`.
        let porc = cmd(&repo, &home, &[verb, &["--word-diff=porcelain"][..]].concat());
        assert!(out(&porc).contains("+modified words here\n~\n"), "{verb:?}: {}", out(&porc));
        assert_ne!(out(&porc), out(&plain), "{verb:?}: =porcelain rendered as =plain");

        // A bad mode is rejected rather than silently downgraded.
        let bad = cmd(&repo, &home, &[verb, &["--word-diff=nonsuch"][..]].concat());
        assert_eq!(bad.status.code(), Some(129), "{verb:?}: {}", err(&bad));
        assert_eq!(err(&bad), "error: bad --word-diff argument: nonsuch\n");

        // `--ws-error-highlight` validates its value and is inert with color off.
        let wseh = cmd(&repo, &home, &[verb, &["--ws-error-highlight=all"][..]].concat());
        assert!(wseh.status.success(), "{verb:?}: {}", err(&wseh));
        assert_eq!(out(&wseh), out(&cmd(&repo, &home, verb)));
        let bad_wseh = cmd(&repo, &home, &[verb, &["--ws-error-highlight=nope"][..]].concat());
        assert_eq!(bad_wseh.status.code(), Some(129), "{verb:?}: {}", err(&bad_wseh));
    }
}

/// `log_tree_commit()` hands the diff machinery the run's own `o->use_color`, so a
/// patch body under `--color=always` is painted exactly as `git diff` paints it.
/// Measured against stock 2.55.0, whose `git log -p --color=always` colours the
/// `diff --git` header bold and the added line green.
#[test]
fn log_colours_the_patch_body_not_just_the_header() {
    let (repo, home) = fixture("colour");
    let o = cmd(&repo, &home, &["log", "--no-decorate", "-p", "-1", "--color=always"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("\u{1b}[1mdiff --git a/docs/readme.txt"), "header unpainted: {text:?}");
    assert!(text.contains("\u{1b}[31m-doc line one\u{1b}[m"), "removed line unpainted: {text:?}");
    assert!(text.contains("\u{1b}[32m+\u{1b}[m\u{1b}[32mdoc line one"), "added line unpainted: {text:?}");
}

/// `--diff-merges=<v>` on `diff-tree` maps onto the very setup functions `-m`,
/// `--dd`, `-c` and `--cc` call (diff-merges.c:68-97). Before this was ported the
/// flag was accepted and ignored, which on a merge commit meant exit 0 with no
/// output where stock printed a combined diff.
#[test]
fn diff_tree_diff_merges_aliases_the_merge_knob() {
    let (repo, home) = merge_fixture("dtmerge");
    let head = ["diff-tree", "-r", "HEAD"];
    let take = |extra: &str| out(&cmd(&repo, &home, &[&head[..], &[extra][..]].concat()));

    // A merge is diffless by default, and `off`/`--no-diff-merges` keep it that way.
    let bare = out(&cmd(&repo, &home, &head));
    assert_eq!(take("--diff-merges=off"), bare);
    assert_eq!(take("--no-diff-merges"), bare);

    // The four modes that do render, each byte-identical to its short spelling.
    for (value, short) in [
        ("--diff-merges=on", "-m"),
        ("--diff-merges=separate", "-m"),
        ("--diff-merges=first-parent", "--dd"),
        ("--diff-merges=combined", "-c"),
        ("--diff-merges=dense-combined", "--cc"),
    ] {
        let long = take(value);
        assert_ne!(long, bare, "{value} was accepted and ignored");
        assert_eq!(long, take(short), "{value} and {short} disagree");
    }

    // `-c` is the combined *raw* record (one colon per parent), `--cc` the patch.
    assert!(take("-c").contains("::100644 100644 100644 "), "-c: {}", take("-c"));
    assert!(take("--cc").contains("diff --cc f.txt\n"), "--cc: {}", take("--cc"));

    // `remerge` has no engine here, so it refuses on a merge rather than silently
    // falling back to another mode's bytes — and stays inert where no merge is
    // reached, which is what stock does for a two-tree invocation.
    let refused = cmd(&repo, &home, &["diff-tree", "-r", "HEAD", "--remerge-diff"]);
    assert!(!refused.status.success(), "remerge silently produced: {}", out(&refused));
    let two_tree = cmd(&repo, &home, &["diff-tree", "-r", "HEAD~1", "HEAD", "--remerge-diff"]);
    assert!(two_tree.status.success(), "{}", err(&two_tree));
    assert_eq!(out(&two_tree), out(&cmd(&repo, &home, &["diff-tree", "-r", "HEAD~1", "HEAD"])));

    // A value `func_by_opt()` does not know is `die()`, exit 128.
    let bad = cmd(&repo, &home, &["diff-tree", "-r", "HEAD", "--diff-merges=nonsuch"]);
    assert_eq!(bad.status.code(), Some(128), "{}", err(&bad));
    assert_eq!(err(&bad), "fatal: invalid value for '--diff-merges': 'nonsuch'\n");
}

/// `format-patch --cherry-pick --right-only`, the selector `git rebase --apply`
/// drives (builtin/rebase.c:668-672), plus `--pretty=mboxrd`, which is the same
/// escape `format.mboxrd` performs but as a pretty format and therefore not gated
/// on `--stdout`.
#[test]
fn format_patch_cherry_pick_drops_equal_patch_ids() {
    let (repo, home) = scratch("cherry");
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "base\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "base"]);
    run(&repo, &home, &["checkout", "-q", "-b", "topic"]);
    // The same diff on both branches, under two different messages: equal patch
    // ids, different commit ids, which is exactly what `--cherry-pick` keys on.
    std::fs::write(repo.join("f.txt"), "base\ncommon\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "topic spelling"]);
    std::fs::write(repo.join("f.txt"), "base\ncommon\ntopic-only\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "topic-only"]);
    run(&repo, &home, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f.txt"), "base\ncommon\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "upstream spelling"]);
    std::fs::write(repo.join("f.txt"), "base\ncommon\nupstream-only\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "upstream-only"]);

    let subjects = |args: &[&str]| -> Vec<String> {
        let o = cmd(&repo, &home, args);
        assert!(o.status.success(), "{args:?}: {}", err(&o));
        out(&o)
            .lines()
            .filter_map(|l| l.strip_prefix("Subject: [PATCH").map(|r| {
                r.split_once("] ").map(|(_, s)| s.to_string()).unwrap_or_default()
            }))
            .collect()
    };

    // The whole symmetric difference, both sides.
    assert_eq!(
        subjects(&["format-patch", "--stdout", "main...topic"]),
        ["topic spelling", "upstream spelling", "topic-only", "upstream-only"]
    );
    // `--cherry-pick` drops the equal-patch-id pair from *both* sides.
    assert_eq!(
        subjects(&["format-patch", "--stdout", "--cherry-pick", "main...topic"]),
        ["topic-only", "upstream-only"]
    );
    // `--right-only` then keeps only the topic side — the rebase selector.
    assert_eq!(
        subjects(&["format-patch", "--stdout", "--cherry-pick", "--right-only", "main...topic"]),
        ["topic-only"]
    );
    assert_eq!(
        subjects(&["format-patch", "--stdout", "--cherry-pick", "--left-only", "main...topic"]),
        ["upstream-only"]
    );
    // An asymmetric range has no left side, so `cherry_pick_list()` returns at once.
    assert_eq!(
        subjects(&["format-patch", "--stdout", "--cherry-pick", "main..topic"]),
        ["topic spelling", "topic-only"]
    );
    // `--cherry-mark` marks rather than drops, and `format-patch` renders no mark.
    assert_eq!(
        subjects(&["format-patch", "--stdout", "--cherry-mark", "--right-only", "main...topic"]),
        subjects(&["format-patch", "--stdout", "--right-only", "main...topic"])
    );

    // The two mutually exclusive pairs.
    let clash = cmd(&repo, &home, &["format-patch", "--stdout", "--cherry-pick", "--cherry-mark", "main...topic"]);
    assert_eq!(clash.status.code(), Some(128), "{}", err(&clash));
    assert_eq!(
        err(&clash),
        "fatal: options '--cherry-mark' and '--cherry-pick' cannot be used together\n"
    );
    let sides = cmd(&repo, &home, &["format-patch", "--stdout", "--left-only", "--right-only", "main...topic"]);
    assert_eq!(sides.status.code(), Some(128), "{}", err(&sides));

    // The exact `git rebase --apply` command line (builtin/rebase.c:668-672).
    let rebase = cmd(
        &repo,
        &home,
        &[
            "format-patch", "-k", "--stdout", "--full-index", "--cherry-pick", "--right-only",
            "--default-prefix", "--no-renames", "--no-cover-letter", "--pretty=mboxrd",
            "--topo-order", "--no-base", "main...topic",
        ],
    );
    assert!(rebase.status.success(), "{}", err(&rebase));
    let text = out(&rebase);
    assert!(text.contains("Subject: topic-only\n"), "-k keeps the bare subject: {text}");
    assert!(!text.contains("upstream-only"), "the left side survived: {text}");
    assert!(text.contains("index "), "--full-index lost: {text}");
}

/// `--pretty=mboxrd` is `CMIT_FMT_MBOXRD`, whose `pp_remainder()` (pretty.c:2286)
/// escapes a `/^>*From /` body line with one more `>`. Unlike `format.mboxrd`
/// (builtin/log.c:2253) it is not gated on `--stdout`, and `is_mboxrd_from()`'s
/// `len > 4` guard leaves a bare `From` alone.
#[test]
fn pretty_mboxrd_escapes_from_lines_without_stdout() {
    let (repo, home) = scratch("mboxrd");
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "a\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "base"]);
    std::fs::write(repo.join("f.txt"), "b\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    let msg = "subject here\n\nbody line\nFrom nowhere in particular\n>From already quoted\nFrom \nend";
    run(&repo, &home, &["commit", "-q", "-m", msg]);

    let plain = out(&cmd(&repo, &home, &["format-patch", "--stdout", "-1", "--pretty=email"]));
    assert!(plain.contains("\nFrom nowhere in particular\n"), "{plain}");

    let escaped = out(&cmd(&repo, &home, &["format-patch", "--stdout", "-1", "--pretty=mboxrd"]));
    assert!(escaped.contains("\n>From nowhere in particular\n"), "{escaped}");
    assert!(escaped.contains("\n>>From already quoted\n"), "{escaped}");
    // `is_mboxrd_from()` requires more than the four bytes of a bare `From`.
    assert!(escaped.contains("\nFrom\nend\n"), "a bare From was escaped: {escaped}");
    assert_ne!(plain, escaped, "--pretty=mboxrd was accepted and dropped");
}

/// `%gd` goes through `refs_shorten_unambiguous_ref(store, ref, 0)`
/// (reflog-walk.c:252), not a category strip, and `read_complete_reflog()`
/// (reflog-walk.c:68-103) looks for the log under four spellings of the argument —
/// which is what makes `git log -g <ambiguous-name>` find a branch's reflog at all.
#[test]
fn reflog_selector_shortens_unambiguously_and_finds_ambiguous_names() {
    let (repo, home) = scratch("reflog");
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "a\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "one"]);
    std::fs::write(repo.join("f.txt"), "b\n").unwrap();
    run(&repo, &home, &["add", "-A"]);
    run(&repo, &home, &["commit", "-q", "-m", "two"]);
    // A branch and a tag sharing one name: `dup` alone is ambiguous, and
    // `ref_rev_parse_rules` resolves it to the *tag*, which has no reflog.
    run(&repo, &home, &["branch", "dup"]);
    run(&repo, &home, &["tag", "dup", "HEAD~1"]);
    run(&repo, &home, &["update-ref", "-m", "move dup", "refs/heads/dup", "HEAD"]);

    // The plain name still lists the branch's reflog, through the
    // `refs/heads/<name>` spelling, and reports the name as typed.
    let o = cmd(&repo, &home, &["log", "-g", "dup", "--format=%gd"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "dup@{0}\n");

    // A full name is shortened only as far as stays unambiguous: plain `dup` would
    // name the tag too, so `heads/dup` is where the shortening stops.
    let full = cmd(&repo, &home, &["log", "-g", "refs/heads/dup", "--format=%gd"]);
    assert!(full.status.success(), "{}", err(&full));
    assert_eq!(out(&full), "heads/dup@{0}\n");

    // `%gD` never shortens.
    let long = cmd(&repo, &home, &["log", "-g", "refs/heads/dup", "--format=%gD"]);
    assert_eq!(out(&long), "refs/heads/dup@{0}\n");

    // An unambiguous branch shortens all the way.
    let main = cmd(&repo, &home, &["log", "-g", "main", "--format=%gd"]);
    assert_eq!(out(&main), "main@{0}\nmain@{1}\n");
}
