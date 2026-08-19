//! The diff renderers `format-patch` reaches once a patch is being emitted:
//! `--word-diff`, `--color-moved`, `--ws-error-highlight`, `--inter-hunk-context`,
//! `--ignore-blank-lines`, `--diff-filter`, `--relative`, `--submodule`,
//! `--diff-merges` and `--line-prefix`.
//!
//! Every expectation below was measured against stock git 2.55.0 over the fixture
//! this file builds, and each case is pinned twice: the bytes the flag produces, and
//! an `assert_ne!` against the same command without it, so a flag that silently
//! became a no-op fails here rather than passing on a plausible-looking patch.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A history with everything the renderers below need: a file whose two edits
    /// are far enough apart to make two hunks, a change that only adds a blank line,
    /// a four-line block moved from one file into another, a trailing-whitespace
    /// addition, a rename that crosses a directory boundary, a merge, and a gitlink
    /// that moves.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fpdr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sub = root.join("sub");
        let work = root.join("work");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        // The submodule's own history, built before the fixture guard exists so its
        // `Drop` cannot take the directory out from under the superproject setup.
        let at = |dir: &std::path::Path, args: &[&str]| {
            let out = git_at(&root, dir, args).output().unwrap();
            assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
        };
        at(&sub, &["init", "-q", "-b", "main", "."]);
        at(&sub, &["config", "user.email", "t@e.co"]);
        at(&sub, &["config", "user.name", "t"]);
        write(&sub.join("s.txt"), "one\n");
        at(&sub, &["add", "-A"]);
        at(&sub, &["commit", "-q", "-m", "sub one"]);
        write(&sub.join("s.txt"), "one\ntwo\n");
        at(&sub, &["commit", "-q", "-am", "sub two"]);

        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::create_dir_all(f.work.join("old")).unwrap();
        std::fs::create_dir_all(f.work.join("src")).unwrap();
        write(&f.work.join("src/gaps.txt"), "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n");
        write(&f.work.join("src/blanks.txt"), "top\n\n\nbottom\n");
        write(&f.work.join("src/block.txt"), "head\nMOVED1\nMOVED2\nMOVED3\nMOVED4\ntail\n");
        write(&f.work.join("src/dest.txt"), "only\n");
        write(&f.work.join("src/keep.txt"), "keep\n");
        write(&f.work.join("old/moved.txt"), "alpha\nbeta\ngamma\ndelta\nepsilon\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["tag", "base"]);

        write(&f.work.join("src/gaps.txt"), "L1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nL10\n");
        write(&f.work.join("src/blanks.txt"), "top\n\n\n\nbottom\n");
        write(&f.work.join("src/block.txt"), "head\ntail\n");
        write(&f.work.join("src/dest.txt"), "only\nMOVED1\nMOVED2\nMOVED3\nMOVED4\n");
        write(&f.work.join("src/keep.txt"), "keep\ntrail \n");
        f.git(&["mv", "old/moved.txt", "src/moved.txt"]);
        write(&f.work.join("src/moved.txt"), "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "edits"]);
        f.git(&["tag", "edits"]);

        f.git(&["checkout", "-q", "-b", "side", "base"]);
        write(&f.work.join("src/side.txt"), "other\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "side"]);
        f.git(&["checkout", "-q", "main"]);
        f.git(&["merge", "-q", "--no-ff", "-m", "merged", "side"]);
        f.git(&["tag", "merged"]);

        let url = sub.to_str().unwrap();
        f.git(&["-c", "protocol.file.allow=always", "submodule", "add", "-q", url, "sub"]);
        subgit(&f, &["checkout", "-q", "main~1"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "add submodule"]);
        subgit(&f, &["checkout", "-q", "main"]);
        f.git(&["add", "sub"]);
        f.git(&["commit", "-q", "-m", "bump submodule"]);
        f.git(&["tag", "bumped"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TERM", "dumb")
            .env("COLUMNS", "80")
            .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
            .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// One `format-patch --stdout -1 <rev>` run, with `flags` appended.
    fn patch(&self, rev: &str, flags: &[&str]) -> String {
        let mut args = vec!["format-patch", "--stdout", "-1", rev];
        args.extend_from_slice(flags);
        let out = self.run(&args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn write(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

/// A `git` run in `dir` with the fixture's pinned environment.
fn git_at(home: &std::path::Path, dir: &std::path::Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.co")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.co");
    c
}

fn subgit(f: &Fixture, args: &[&str]) {
    let out = git_at(&f.root, &f.work.join("sub"), args).output().unwrap();
    assert!(out.status.success(), "setup `git -C sub {args:?}` failed: {out:?}");
}

/// `--word-diff`'s three textual modes. `plain` brackets each side inline,
/// `porcelain` keeps the `-`/`+`/` ` sign columns and terminates every record with a
/// `~`, and `color` is `plain`'s layout with `options->use_color = GIT_COLOR_ALWAYS`
/// forced on — so it paints even to a pipe, where nothing else here would.
#[test]
fn word_diff_modes_rewrite_the_hunk_body() {
    let f = Fixture::new("worddiff");
    let plain_patch = f.patch("edits", &[]);
    assert!(plain_patch.contains("-l1\n+L1\n"), "{plain_patch}");

    let plain = f.patch("edits", &["--word-diff"]);
    assert!(plain.contains("[-l1-]{+L1+}\n"), "{plain}");
    assert_ne!(plain, plain_patch);
    assert_eq!(plain, f.patch("edits", &["--word-diff=plain"]));

    let porcelain = f.patch("edits", &["--word-diff=porcelain"]);
    assert!(porcelain.contains("-l1\n+L1\n~\n l2\n~\n"), "{porcelain}");
    assert_ne!(porcelain, plain);

    // `diff_opt_word_diff()`'s `color` arm forces colour on, so the escape sequences
    // are there even though stdout is a pipe and no `--color` was given.
    let color = f.patch("edits", &["--word-diff=color"]);
    assert!(color.contains("\x1b[31ml1\x1b[m\x1b[32mL1\x1b[m"), "{color:?}");
    assert_ne!(color, plain);
    assert_eq!(color, f.patch("edits", &["--color-words"]));

    // `--word-diff-regex` promotes a diff that is not already a word diff to
    // `plain`, and changes what counts as a word.
    let re = f.patch("edits", &["--word-diff-regex=[A-Za-z]+"]);
    assert!(re.contains("[-l-]{+L+}1"), "{re}");
    assert_ne!(re, plain);
}

/// `--color-moved` needs colour to be on at all — `o->emitted_symbols` is only
/// allocated when it is — and then paints a block that left one file and arrived in
/// another with the zebra pair, not the plain add/remove colours.
#[test]
fn color_moved_repaints_a_block_that_changed_file() {
    let f = Fixture::new("colormoved");
    let plain = f.patch("edits", &["--color=always"]);
    assert!(plain.contains("\x1b[31m-MOVED1\x1b[m"), "{plain:?}");

    let zebra = f.patch("edits", &["--color=always", "--color-moved"]);
    assert!(zebra.contains("\x1b[1;35m-MOVED1\x1b[m"), "{zebra:?}");
    assert!(zebra.contains("\x1b[1;36m+\x1b[m\x1b[1;36mMOVED1\x1b[m"), "{zebra:?}");
    assert_ne!(zebra, plain);
    // The argument-less spelling is `COLOR_MOVED_DEFAULT`, which is `zebra`.
    assert_eq!(zebra, f.patch("edits", &["--color=always", "--color-moved=zebra"]));

    // `plain` shares zebra's first alternation, so with a single moved block the two
    // agree byte for byte; `no` puts the ordinary add/remove colours back.
    assert_eq!(zebra, f.patch("edits", &["--color=always", "--color-moved=plain"]));
    assert_eq!(plain, f.patch("edits", &["--color=always", "--color-moved=no"]));

    // With colour off the detector never runs, so the flag changes nothing.
    assert_eq!(f.patch("edits", &["--color-moved"]), f.patch("edits", &[]));
}

/// `--ws-error-highlight` picks which sides `ws_check_emit()` marks. The added
/// trailing space is highlighted by default (`WSEH_NEW`); `all` extends the check to
/// the context line above it, and `none` turns it off entirely.
#[test]
fn ws_error_highlight_selects_the_marked_sides() {
    let f = Fixture::new("wseh");
    let default = f.patch("edits", &["--color=always"]);
    assert!(default.contains("\x1b[32mtrail\x1b[m\x1b[41m \x1b[m"), "{default:?}");
    assert!(default.contains(" keep\x1b[m"), "{default:?}");

    // `all` re-emits the context line through the whitespace checker, which is
    // visible as the extra reset the checker writes after the sign byte.
    let all = f.patch("edits", &["--color=always", "--ws-error-highlight=all"]);
    assert!(all.contains(" \x1b[mkeep\x1b[m"), "{all:?}");
    assert_ne!(all, default);

    let none = f.patch("edits", &["--color=always", "--ws-error-highlight=none"]);
    assert!(!none.contains("\x1b[41m"), "{none:?}");
    assert_ne!(none, default);

    // `parse_ws_error_highlight()` reports the prefix it had already accepted.
    let bad = f.run(&[
        "format-patch",
        "--stdout",
        "-1",
        "edits",
        "--ws-error-highlight=new,bogus",
    ]);
    assert_eq!(bad.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&bad.stderr),
        "error: unknown value after ws-error-highlight=new,\n"
    );
}

/// `--inter-hunk-context=<n>` widens `xdl_get_hunk()`'s `max_common`
/// (`ctxlen + ctxlen + interhunkctxlen`), so two change groups that were six context
/// lines apart end up in one hunk.
#[test]
fn inter_hunk_context_merges_neighbouring_hunks() {
    let f = Fixture::new("interhunk");
    let default = f.patch("edits", &[]);
    assert_eq!(default.matches("\n@@ ").count(), 7, "{default}");
    assert!(default.contains("@@ -1,4 +1,4 @@\n"), "{default}");
    assert!(default.contains("@@ -7,4 +7,4 @@ l6\n"), "{default}");

    let merged = f.patch("edits", &["--inter-hunk-context=3"]);
    assert_eq!(merged.matches("\n@@ ").count(), 6, "{merged}");
    assert!(merged.contains("@@ -1,10 +1,10 @@\n"), "{merged}");
    assert!(!merged.contains("@@ -7,4 +7,4 @@"), "{merged}");
    assert_ne!(merged, default);

    // Zero is the default, so it must not move anything.
    assert_eq!(f.patch("edits", &["--inter-hunk-context=0"]), default);

    // `OPT_UNSIGNED`'s own diagnostic, and `parse_options`' missing-value one.
    let neg = f.run(&["format-patch", "--stdout", "-1", "edits", "--inter-hunk-context=-1"]);
    assert_eq!(neg.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&neg.stderr),
        "error: option `inter-hunk-context' expects a non-negative integer value \
         with an optional k/m/g suffix\n"
    );
    let bare = f.run(&["format-patch", "--stdout", "-1", "edits", "--inter-hunk-context"]);
    assert_eq!(bare.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&bare.stderr),
        "error: option `inter-hunk-context' requires a value\n"
    );
}

/// `--ignore-blank-lines` marks a change whose every record is blank as ignorable,
/// and `xdl_get_hunk()` then drops the hunk no real change is holding in place — the
/// whole file section disappears, diffstat row included.
#[test]
fn ignore_blank_lines_drops_a_blank_only_change() {
    let f = Fixture::new("blanklines");
    let default = f.patch("edits", &[]);
    assert!(default.contains("diff --git a/src/blanks.txt b/src/blanks.txt"), "{default}");
    assert!(default.contains(" src/blanks.txt         | 1 +\n"), "{default}");

    let ignored = f.patch("edits", &["--ignore-blank-lines"]);
    assert!(!ignored.contains("blanks.txt"), "{ignored}");
    assert_ne!(ignored, default);
    // Only that file leaves: every other section is still there.
    assert!(ignored.contains("diff --git a/src/gaps.txt b/src/gaps.txt"), "{ignored}");
}

/// `--diff-filter=<letters>`: an uppercase letter selects a status, its lowercase
/// excludes it, and an exclusion with no inclusion beside it starts from every
/// status and subtracts.
#[test]
fn diff_filter_selects_by_status_letter() {
    let f = Fixture::new("difffilter");
    let all = f.patch("edits", &[]);

    let renames = f.patch("edits", &["--diff-filter=R"]);
    assert!(renames.contains("rename {old => src}/moved.txt (86%)"), "{renames}");
    assert!(!renames.contains("gaps.txt"), "{renames}");
    assert_ne!(renames, all);

    // The lowercase spelling keeps everything else.
    let no_renames = f.patch("edits", &["--diff-filter=r"]);
    assert!(!no_renames.contains("moved.txt"), "{no_renames}");
    assert!(no_renames.contains("src/gaps.txt"), "{no_renames}");
    assert_ne!(no_renames, all);
    assert_ne!(no_renames, renames);

    // A filter nothing matches leaves the mail headers and nothing else.
    let none = f.patch("edits", &["--diff-filter=U"]);
    assert!(!none.contains("\n---\n"), "{none}");
    assert!(none.contains("Subject: [PATCH] edits\n"), "{none}");

    let bad = f.run(&["format-patch", "--stdout", "-1", "edits", "--diff-filter=Z"]);
    assert_eq!(bad.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&bad.stderr),
        "error: unknown change class 'Z' in --diff-filter=Z\n"
    );
}

/// `--relative=<path>` is two things: `diff_queue()` narrows the pair queue to the
/// prefix *before* `diffcore_rename()` runs, and `strip_prefix()` shortens the names
/// the patch and the diffstat print — but not the ones `diff_summary()` prints.
#[test]
fn relative_narrows_before_rename_detection_and_then_shortens() {
    let f = Fixture::new("relative");
    let all = f.patch("edits", &[]);
    assert!(all.contains("rename from old/moved.txt\nrename to src/moved.txt\n"), "{all}");

    let rel = f.patch("edits", &["--relative=src"]);
    // The deletion side never entered the queue, so there is nothing to pair with:
    // the rename is reported as a plain creation.
    assert!(!rel.contains("rename from"), "{rel}");
    assert!(rel.contains("diff --git a/moved.txt b/moved.txt\nnew file mode 100644\n"), "{rel}");
    assert!(rel.contains("--- a/blanks.txt\n+++ b/blanks.txt\n"), "{rel}");
    assert!(rel.contains(" blanks.txt | 1 +\n"), "{rel}");
    // `diff_summary()` never calls `strip_prefix()`, so its line keeps `src/`.
    assert!(rel.contains(" create mode 100644 src/moved.txt\n"), "{rel}");
    assert_ne!(rel, all);

    // A trailing slash is added if the value lacks one, so both spellings agree.
    assert_eq!(rel, f.patch("edits", &["--relative=src/"]));
    // A prefix nothing lives under empties the queue.
    let empty = f.patch("edits", &["--relative=nowhere"]);
    assert!(!empty.contains("\n---\n"), "{empty}");
}

/// `--submodule=log|diff` replaces a gitlink pair's whole `diff --git` section with
/// `show_submodule_diff_summary()` / `show_submodule_inline_diff()`. The diffstat is
/// built by a pass that has no such branch, so its row is the `short` one either way.
#[test]
fn submodule_formats_replace_the_gitlink_section() {
    let f = Fixture::new("submodule");
    let short = f.patch("bumped", &[]);
    assert!(short.contains("diff --git a/sub b/sub"), "{short}");
    assert!(short.contains("-Subproject commit "), "{short}");
    assert_eq!(short, f.patch("bumped", &["--submodule=short"]));

    let log = f.patch("bumped", &["--submodule=log"]);
    assert!(!log.contains("diff --git"), "{log}");
    assert!(log.contains("  > sub two\n"), "{log}");
    // The diffstat is unchanged by the format.
    assert!(log.contains(" sub | 2 +-\n"), "{log}");
    assert_ne!(log, short);
    // A bare `--submodule` is `log`.
    assert_eq!(log, f.patch("bumped", &["--submodule"]));

    let inline = f.patch("bumped", &["--submodule=diff"]);
    assert!(inline.contains("diff --git a/sub/s.txt b/sub/s.txt\n"), "{inline}");
    assert!(inline.contains("+two\n"), "{inline}");
    assert_ne!(inline, log);

    let bad = f.run(&["format-patch", "--stdout", "-1", "bumped", "--submodule=bogus"]);
    assert_eq!(bad.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&bad.stderr),
        "error: failed to parse --submodule option parameter: 'bogus'\n"
    );
}

/// `--diff-merges` on a merge `--max-parents=2` let into the series. `separate`
/// emits one whole message per parent, `first-parent` stops after the first, and the
/// combined modes replace the three-dash separator with a blank line and hang the
/// combined patch off the *first parent's* diffstat.
#[test]
fn diff_merges_decides_what_a_merge_carries() {
    let f = Fixture::new("diffmerges");
    let merge = &["--max-parents=2"];

    // format-patch's default is `off`: header and message, nothing else.
    let off = f.patch("merged", merge);
    assert!(!off.contains("\n---\n"), "{off}");
    assert!(!off.contains("diff --git"), "{off}");
    assert_eq!(off, f.patch("merged", &["--max-parents=2", "--diff-merges=off"]));

    let separate = f.patch("merged", &["--max-parents=2", "--diff-merges=separate"]);
    assert_eq!(separate.matches("Subject: [PATCH] merged\n").count(), 2, "{separate}");
    assert!(separate.contains("diff --git a/src/side.txt b/src/side.txt"), "{separate}");
    assert!(separate.contains("diff --git a/src/gaps.txt b/src/gaps.txt"), "{separate}");
    assert_ne!(separate, off);
    assert_eq!(separate, f.patch("merged", &["--max-parents=2", "--diff-merges=on"]));
    assert_eq!(separate, f.patch("merged", &["--max-parents=2", "-m"]));

    let first = f.patch("merged", &["--max-parents=2", "--diff-merges=first-parent"]);
    assert_eq!(first.matches("Subject: [PATCH] merged\n").count(), 1, "{first}");
    assert!(first.contains("diff --git a/src/side.txt b/src/side.txt"), "{first}");
    assert!(!first.contains("gaps.txt"), "{first}");
    assert_ne!(first, separate);
    assert_eq!(first, f.patch("merged", &["--max-parents=2", "--dd"]));

    // The combined modes: a blank line where `---` would be, and the first parent's
    // stat. This merge is clean, so no path differs from every parent and the
    // combined patch itself is empty.
    let combined = f.patch("merged", &["--max-parents=2", "--diff-merges=combined"]);
    assert!(combined.contains("Subject: [PATCH] merged\n\n\n src/side.txt | 1 +\n"), "{combined}");
    assert!(!combined.contains("diff --"), "{combined}");
    assert_ne!(combined, off);
    assert_ne!(combined, first);
    assert_eq!(
        combined,
        f.patch("merged", &["--max-parents=2", "--diff-merges=dense-combined"])
    );

    let bad = f.run(&[
        "format-patch",
        "--stdout",
        "-1",
        "merged",
        "--max-parents=2",
        "--diff-merges=bogus",
    ]);
    assert_eq!(bad.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&bad.stderr),
        "fatal: invalid value for '--diff-merges': 'bogus'\n"
    );
}

/// `--line-prefix` is applied per emitter, not to the message as a whole: every line
/// of a patch takes it, the signature `cmd_format_patch` prints itself does not, and
/// a cover letter — which never reaches `show_log()` — takes it on its `From:` header
/// and its diffstat and on nothing else.
#[test]
fn line_prefix_follows_the_emitters_that_write_it() {
    let f = Fixture::new("lineprefix");
    let plain = f.patch("edits", &[]);
    let prefixed = f.patch("edits", &["--line-prefix=>>"]);
    assert_ne!(prefixed, plain);

    assert!(prefixed.starts_with(">>From "), "{prefixed}");
    assert!(prefixed.contains(">>Subject: [PATCH] edits\n>>\n>>---\n>> src/blanks.txt"), "{prefixed}");
    assert!(prefixed.contains(">>diff --git a/src/gaps.txt b/src/gaps.txt\n"), "{prefixed}");
    assert!(prefixed.contains(">>+L1\n"), "{prefixed}");
    // `print_signature()` writes straight to the file with no prefix in front.
    assert!(prefixed.ends_with("\n-- \n2.55.0\n\n"), "{prefixed}");
    // Dropping the prefix from every line is exactly the unprefixed patch.
    let stripped: String = prefixed
        .split_inclusive('\n')
        .map(|l| l.strip_prefix(">>").unwrap_or(l))
        .collect();
    assert_eq!(stripped, plain);

    let cover = {
        let out = f.run(&[
            "format-patch",
            "--stdout",
            "base..edits",
            "--cover-letter",
            "--line-prefix=>>",
        ]);
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    // Only the header line that follows the magic `From` line is prefixed, plus the
    // diffstat rows `show_diffstat()` writes through `diff_flush()`.
    assert!(cover.starts_with("From "), "{cover}");
    assert!(cover.contains("\n>>From: t <t@e.co>\nDate: "), "{cover}");
    assert!(cover.contains("\nSubject: [PATCH 0/1] *** SUBJECT HERE ***\n"), "{cover}");
    assert!(cover.contains("\n>> src/blanks.txt         | 1 +\n"), "{cover}");
}
