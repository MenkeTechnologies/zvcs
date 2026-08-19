//! The `diffcore_std()` passes and the alternate output formats `format-patch`
//! reaches through `setup_revisions()`.
//!
//! One fixture carries every shape these options discriminate on: a rename with a
//! small edit, a copy whose source is itself modified (which `-C` can pair), a
//! copy whose source is untouched (which only `--find-copies-harder` can pair),
//! and a file large enough for `diffcore_break()`'s `MINIMUM_BREAK_SIZE` that is
//! then completely rewritten. The commit message carries tabs so
//! `--expand-tabs` has something to de-tabify.
//!
//! Every expectation below was measured against stock git 2.55.0 over exactly
//! this fixture, and each is paired with an `assert_ne!` against the same command
//! without the option — an option that renders identically to the default has not
//! been implemented, it has been swallowed.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fpdc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);

        f.write("old.txt", &lines("moved line", 30));
        f.write("source.txt", &lines("shared body line", 30));
        f.write("still.txt", "untouched one\nuntouched two\n");
        // Over `MINIMUM_BREAK_SIZE` (400 bytes), so `should_break()` engages.
        f.write(
            "rewrite.txt",
            &lines("original line", 40).replace('\n', " with padding text to pass the break floor\n"),
        );
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "seed"]);

        f.git(&["mv", "old.txt", "new.txt"]);
        f.append("new.txt", "moved line 31\n");
        // A copy whose source is modified: `-C` alone can use it.
        f.write("source_copy.txt", &lines("shared body line", 30));
        f.append("source.txt", "extra tail\n");
        // A copy whose source is untouched: only `--find-copies-harder` sees it.
        f.write("still_copy.txt", "untouched one\nuntouched two\n");
        f.write(
            "rewrite.txt",
            &lines("REPLACED line", 40).replace('\n', " with entirely other wording in place\n"),
        );
        f.git(&["add", "-A"]);
        f.commit_with_message("second\n\nbody\twith\ttabs\n");
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn append(&self, name: &str, body: &str) {
        let path = self.work.join(name);
        let mut have = std::fs::read_to_string(&path).unwrap();
        have.push_str(body);
        std::fs::write(path, have).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
            .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn commit_with_message(&self, message: &str) {
        let path = self.root.join("msg");
        std::fs::write(&path, message).unwrap();
        self.git(&["commit", "-q", "-F", path.to_str().unwrap()]);
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// The patch series for the tip commit, with `extra` appended to the command.
    fn patch(&self, extra: &[&str]) -> String {
        let mut args = vec!["format-patch", "--stdout", "-1", "HEAD"];
        args.extend_from_slice(extra);
        let out = self.run(&args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Only the lines of `patch()` that `keep` accepts, joined back together.
    fn picked(&self, extra: &[&str], keep: impl Fn(&str) -> bool) -> String {
        self.patch(extra)
            .lines()
            .filter(|l| keep(l))
            .map(|l| format!("{l}\n"))
            .collect()
    }
}

fn lines(prefix: &str, n: usize) -> String {
    (1..=n).map(|i| format!("{prefix} {i}\n")).collect()
}

/// Header lines a rename/copy pass adds or removes, which is what every rename
/// assertion below compares on.
fn is_pair_header(l: &str) -> bool {
    l.starts_with("diff --git ")
        || l.starts_with("similarity index ")
        || l.starts_with("dissimilarity index ")
        || l.starts_with("rename ")
        || l.starts_with("copy ")
        || l.starts_with("new file mode ")
        || l.starts_with("deleted file mode ")
}

/// `-M`'s threshold is a real score, not a switch: the 96%-similar rename this
/// fixture carries survives the default and `-M90%` but not `-M100%`, where it
/// falls back to the delete plus create it was built from. `--no-renames`
/// (diff.c:6180) reaches the same fallback from the other direction, and the
/// short and long spellings agree.
#[test]
fn find_renames_threshold_and_no_renames() {
    let f = Fixture::new("renames");
    let detected = f.picked(&[], is_pair_header);
    assert!(
        detected.contains("similarity index 96%\nrename from old.txt\nrename to new.txt\n"),
        "default detection: {detected}"
    );

    // A threshold the pair clears leaves the default alone …
    assert_eq!(f.picked(&["-M90%"], is_pair_header), detected);
    assert_eq!(f.picked(&["--find-renames=90%"], is_pair_header), detected);

    // … and one it cannot changes the answer.
    let split = f.picked(&["-M100%"], is_pair_header);
    assert_ne!(split, detected, "-M100% must not render as the default");
    assert_eq!(
        split,
        "diff --git a/new.txt b/new.txt\nnew file mode 100644\n\
         diff --git a/old.txt b/old.txt\ndeleted file mode 100644\n\
         diff --git a/rewrite.txt b/rewrite.txt\n\
         diff --git a/source.txt b/source.txt\n\
         diff --git a/source_copy.txt b/source_copy.txt\nnew file mode 100644\n\
         diff --git a/still_copy.txt b/still_copy.txt\nnew file mode 100644\n"
    );
    assert_eq!(f.picked(&["--find-renames=100%"], is_pair_header), split);
    assert_eq!(f.picked(&["--no-renames"], is_pair_header), split);
}

/// `-C` pairs a new file against a source the same commit *also* touched;
/// `--find-copies-harder` (and its `-C -C` spelling, diff.c:5734-5737) additionally
/// admits sources the commit left alone, which is a strictly larger answer.
#[test]
fn find_copies_and_find_copies_harder_are_three_distinct_answers() {
    let f = Fixture::new("copies");
    let default = f.picked(&[], is_pair_header);
    let copies = f.picked(&["-C"], is_pair_header);
    let harder = f.picked(&["--find-copies-harder"], is_pair_header);

    assert_ne!(copies, default, "-C must not render as the default");
    assert_ne!(harder, copies, "--find-copies-harder must not render as -C");

    // The default reports both new files as additions.
    assert!(default.contains("diff --git a/source_copy.txt b/source_copy.txt\nnew file mode 100644\n"));
    assert!(default.contains("diff --git a/still_copy.txt b/still_copy.txt\nnew file mode 100644\n"));

    // `-C` pairs the one whose source is modified, and only that one.
    assert!(copies.contains(
        "diff --git a/source.txt b/source_copy.txt\nsimilarity index 100%\n\
         copy from source.txt\ncopy to source_copy.txt\n"
    ));
    assert!(copies.contains("diff --git a/still_copy.txt b/still_copy.txt\nnew file mode 100644\n"));

    // `--find-copies-harder` pairs the untouched source too.
    assert!(harder.contains(
        "diff --git a/still.txt b/still_copy.txt\nsimilarity index 100%\n\
         copy from still.txt\ncopy to still_copy.txt\n"
    ));
    assert_eq!(f.picked(&["-C", "-C"], is_pair_header), harder);
    assert_eq!(f.picked(&["--find-copies", "--find-copies"], is_pair_header), harder);
}

/// `-B` breaks the complete rewrite, and `diffcore_merge_broken()` glues it back
/// into one modification carrying a score. That score is three separate lines
/// nothing else prints: `dissimilarity index` in the patch (diff.c:4897-4903),
/// ` rewrite <path> (<n>%)` in the summary (diff.c:6819-6827), and the `M100`
/// suffix in the raw format (diff.c:6481-6486).
#[test]
fn break_rewrites_scores_the_pair_in_three_places() {
    let f = Fixture::new("break");
    let default = f.patch(&[]);
    let broken = f.patch(&["-B"]);
    assert_ne!(broken, default, "-B must not render as the default");

    assert!(!default.contains("dissimilarity index"));
    assert!(broken.contains(
        "diff --git a/rewrite.txt b/rewrite.txt\ndissimilarity index 100%\n"
    ));
    assert!(broken.contains(" rewrite rewrite.txt (100%)\n"));
    assert_eq!(f.patch(&["-B50%"]), broken);
    assert_eq!(f.patch(&["--break-rewrites"]), broken);

    let raw = f.picked(&["--raw", "-B", "-C"], |l| l.starts_with(':'));
    assert_eq!(
        raw,
        ":100644 100644 2cbbadb 026dfdb R096\told.txt\tnew.txt\n\
         :100644 100644 b3b95c6 cfa3dcd M100\trewrite.txt\n\
         :100644 100644 0368f9b 8495c58 M\tsource.txt\n\
         :100644 100644 0368f9b 0368f9b C100\tsource.txt\tsource_copy.txt\n\
         :000000 100644 0000000 d3da260 A\tstill_copy.txt\n"
    );
}

/// `--abbrev=<n>` is a floor on the `index` line's object names, clamped up to
/// `MINIMUM_ABBREV` and down to the hash width (revision.c:2643-2648).
/// `--full-index` overrides it there but *not* in the raw format, which reads
/// `opt->abbrev` straight (diff.c:6477 against diff.c:4917).
#[test]
fn abbrev_widths_and_the_full_index_split() {
    let f = Fixture::new("abbrev");
    let first_index = |extra: &[&str]| -> String {
        f.patch(extra)
            .lines()
            .find(|l| l.starts_with("index "))
            .expect("a patch with an index line")
            .to_owned()
    };
    let default = first_index(&[]);
    assert_eq!(default, "index 2cbbadb..026dfdb 100644");
    assert_ne!(first_index(&["--abbrev=8"]), default);
    assert_eq!(first_index(&["--abbrev=8"]), "index 2cbbadb7..026dfdb4 100644");
    // Below `MINIMUM_ABBREV`, and a value `strtoul` reads as zero, both clamp to 4.
    assert_eq!(first_index(&["--abbrev=1"]), "index 2cbb..026d 100644");
    assert_eq!(first_index(&["--abbrev=x"]), "index 2cbb..026d 100644");
    // Bare `--abbrev` and `--no-abbrev` are `DEFAULT_ABBREV`, i.e. the default.
    assert_eq!(first_index(&["--abbrev"]), default);
    assert_eq!(first_index(&["--no-abbrev"]), default);

    let raw_line = |extra: &[&str]| f.picked(extra, |l| l.starts_with(':'));
    let raw = raw_line(&["--raw"]);
    assert!(raw.starts_with(":100644 100644 2cbbadb 026dfdb R096\told.txt\tnew.txt\n"));
    // `--full-index` widens the `index` line to forty and leaves the raw line alone.
    assert_eq!(raw_line(&["--raw", "--full-index"]), raw);
    assert_eq!(
        f.patch(&["--raw", "--full-index"])
            .lines()
            .find(|l| l.starts_with("index "))
            .expect("a patch with an index line"),
        "index 2cbbadb75d57ad0b7f858f834d6081ef4c758f58..\
         026dfdb468a514e2a7fa0350e06c59ac02f502e3 100644"
    );
    assert_ne!(raw_line(&["--raw", "--abbrev=12"]), raw);
}

/// The `OPT_BITOP` output formats (diff.c:6056-6066). `--raw` adds a raw block
/// ahead of the patch, `--patch-with-raw` is its `-p` synonym, and
/// `--patch-with-stat` is `-p --stat` — which, by making `output_format`
/// non-zero, also drops format-patch's default `--summary` block.
#[test]
fn raw_and_patch_with_formats() {
    let f = Fixture::new("formats");
    let default = f.patch(&[]);
    let raw = f.patch(&["--raw"]);
    assert_ne!(raw, default, "--raw must not render as the default");
    assert_eq!(f.patch(&["--patch-with-raw"]), raw);

    assert_eq!(
        f.picked(&["--raw"], |l| l.starts_with(':')),
        ":100644 100644 2cbbadb 026dfdb R096\told.txt\tnew.txt\n\
         :100644 100644 b3b95c6 cfa3dcd M\trewrite.txt\n\
         :100644 100644 0368f9b 8495c58 M\tsource.txt\n\
         :000000 100644 0000000 0368f9b A\tsource_copy.txt\n\
         :000000 100644 0000000 d3da260 A\tstill_copy.txt\n"
    );
    // A raw block replaces the stat, so the `---` marker the stat brings is gone.
    assert!(!raw.contains("\n---\n"));
    assert!(default.contains("\n---\n"));
    // Both still carry the patch itself.
    assert!(raw.contains("diff --git a/rewrite.txt b/rewrite.txt\n"));

    let with_stat = f.patch(&["--patch-with-stat"]);
    assert_ne!(with_stat, default);
    assert_eq!(default.matches("\n create mode ").count(), 2);
    assert_eq!(with_stat.matches("\n create mode ").count(), 0);
    assert!(with_stat.contains("\n 5 files changed, 74 insertions(+), 40 deletions(-)\n"));
}

/// `--compact-summary` annotates each `--stat` row (`get_compact_summary()`,
/// diff.c:4156-4180) and, because it also sets `DIFF_FORMAT_DIFFSTAT`, suppresses
/// the summary block those `create mode` lines came from. `--numstat` prints the
/// raw name and is untouched.
#[test]
fn compact_summary_annotates_only_the_stat_rows() {
    let f = Fixture::new("compact");
    let default = f.patch(&[]);
    let compact = f.patch(&["--compact-summary"]);
    assert_ne!(compact, default, "--compact-summary must not render as the default");

    assert_eq!(
        f.picked(&["--compact-summary"], |l| l.contains('|') || l.contains("files changed")),
        " old.txt => new.txt    |  1 +\n\
         \x20rewrite.txt           | 80 +++++++++++++++++++++----------------------\n\
         \x20source.txt            |  1 +\n\
         \x20source_copy.txt (new) | 30 ++++++++++++++++\n\
         \x20still_copy.txt (new)  |  2 ++\n\
         \x205 files changed, 74 insertions(+), 40 deletions(-)\n"
    );
    assert_eq!(compact.matches("\n create mode ").count(), 0);
    assert_eq!(
        f.picked(&["--compact-summary", "--numstat"], |l| l.starts_with("2\t0\t")),
        "2\t0\tstill_copy.txt\n"
    );
    // `--no-compact-summary` clears the annotation flag but not the
    // `DIFF_FORMAT_DIFFSTAT` its positive form set, so the pair leaves a plain
    // stat with no summary block — neither the default nor `--compact-summary`.
    let cleared = f.patch(&["--compact-summary", "--no-compact-summary"]);
    assert_ne!(cleared, compact);
    assert_ne!(cleared, default);
    assert!(!cleared.contains(" (new) "));
    assert_eq!(cleared.matches("\n create mode ").count(), 0);
    assert!(cleared.contains("\n source_copy.txt    | 30 +++++++++++++++++\n"));
}

/// `--output-indicator-{new,old,context}` replaces the byte in front of a hunk's
/// body lines and nothing else — the `---`/`+++` headers and the `--stat` graph
/// keep their own characters. A value wider than one byte is `diff_opt_char()`'s
/// 129.
#[test]
fn output_indicators_only_move_the_hunk_body() {
    let f = Fixture::new("indicators");
    let default = f.patch(&[]);
    let marked = f.patch(&["--output-indicator-new=@", "--output-indicator-context=~"]);
    assert_ne!(marked, default);

    assert!(default.contains("\n shared body line 30\n+extra tail\n"));
    assert!(marked.contains("\n~shared body line 30\n@extra tail\n"));
    // Untouched: the file headers and the diffstat graph.
    assert!(marked.contains("\n--- a/source.txt\n+++ b/source.txt\n"));
    assert!(marked.contains("| 30 ++++++++++++++++"));

    let out = f.run(&["format-patch", "--stdout", "-1", "HEAD", "--output-indicator-new=ab"]);
    assert_eq!(out.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: output-indicator-new expects a character, got 'ab'\n"
    );
}

/// `--expand-tabs[=<n>]` de-tabifies the *log message*, measuring what precedes
/// each tab in display columns (`strbuf_add_tabexpand()`, pretty.c:2183-2221).
/// format-patch's `expand_tabs_in_log_default` is 0 (builtin/log.c:2109), so the
/// bare option means 8 and `--no-expand-tabs` is the default.
#[test]
fn expand_tabs_rewrites_the_log_message_only() {
    let f = Fixture::new("tabs");
    let default = f.patch(&[]);
    assert!(default.contains("\nbody\twith\ttabs\n"));

    let expanded = f.patch(&["--expand-tabs"]);
    assert_ne!(expanded, default);
    assert!(expanded.contains("\nbody    with    tabs\n"));
    assert_eq!(f.patch(&["--expand-tabs=8"]), expanded);
    // "body" is four columns wide, so a three-column tab stop lands two spaces on.
    assert!(f.patch(&["--expand-tabs=3"]).contains("\nbody  with  tabs\n"));
    assert_eq!(f.patch(&["--no-expand-tabs"]), default);
    assert_eq!(f.patch(&["--expand-tabs=0"]), default);

    // The patch body's own tabs are diff content, not log text, and stay put.
    assert!(expanded.contains("\n@@ "));

    let out = f.run(&["format-patch", "--stdout", "-1", "HEAD", "--expand-tabs=-1"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: '-1': not a non-negative integer\n"
    );
}

/// format-patch always ORs `DIFF_FORMAT_PATCH` into the output format, so the four
/// options that ask for a *different* one die instead (builtin/log.c:2220-2227) —
/// and two of them together are rejected one step earlier, by `diff_setup_done()`
/// (diff.c:5259-5261). A bad revision is resolved first and preempts all of them.
#[test]
fn the_output_formats_format_patch_cannot_have() {
    let f = Fixture::new("refusals");
    for (flag, named) in [
        ("--name-only", "--name-only"),
        ("--name-status", "--name-status"),
        ("--check", "--check"),
        ("--remerge-diff", "--remerge-diff"),
        ("--diff-merges=remerge", "--remerge-diff"),
    ] {
        let out = f.run(&["format-patch", "--stdout", "-1", "HEAD", flag]);
        assert_eq!(out.status.code(), Some(128), "{flag}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("fatal: {named} does not make sense\n"),
            "{flag}"
        );
        assert!(out.stdout.is_empty(), "{flag} printed a patch");
    }

    let both = f.run(&["format-patch", "--stdout", "-1", "HEAD", "--check", "--name-only"]);
    assert_eq!(both.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&both.stderr),
        "fatal: options '--name-only', '--name-status', '--check', and '-s' \
         cannot be used together\n"
    );

    // `setup_revisions()` runs before the four checks, so its failure wins.
    let bad = f.run(&["format-patch", "--stdout", "--name-only", "nosuchrev"]);
    assert_eq!(bad.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&bad.stderr).starts_with("fatal: ambiguous argument 'nosuchrev'"),
        "{:?}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

/// The value errors the rename options report. All three are `error()` returns
/// from a parse-options callback, so they exit 129 with one line and no usage
/// block, and `-l`'s is phrased as a *short* switch.
#[test]
fn rename_option_value_errors() {
    let f = Fixture::new("valueerr");
    for (args, message) in [
        (vec!["--find-renames=zz"], "error: invalid argument to find-renames\n"),
        (vec!["-Mzz"], "error: invalid argument to find-renames\n"),
        (vec!["--find-copies=q"], "error: invalid argument to find-copies\n"),
        (vec!["-Bx"], "error: break-rewrites expects <n>/<m> form\n"),
        (
            vec!["--break-rewrites=1/2/3"],
            "error: break-rewrites expects <n>/<m> form\n",
        ),
        (
            vec!["-l", "x"],
            "error: switch `l' expects an integer value with an optional k/m/g suffix\n",
        ),
        (vec!["-l"], "error: switch `l' requires a value\n"),
    ] {
        let mut argv = vec!["format-patch", "--stdout", "-1", "HEAD"];
        argv.extend_from_slice(&args);
        let out = f.run(&argv);
        assert_eq!(out.status.code(), Some(129), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), message, "{args:?}");
    }

    // `-l <n>` with a usable value is not an error, and a limit too small to run
    // the pass at all leaves the rename undetected.
    assert!(f.patch(&["-l", "1000"]).contains("rename from old.txt\n"));
    assert_ne!(f.patch(&["-l", "1"]), f.patch(&["-l", "1000"]));
}
