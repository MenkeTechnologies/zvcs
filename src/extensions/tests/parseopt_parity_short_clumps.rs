//! Clumped short options: `-qm msg` is `-q -m msg`, and `-mmsg` is too.
//!
//! git never reads a short option as a word. `parse_options_step()` points
//! `ctx->opt` at the second byte of the argument and keeps calling
//! `parse_short_opt()` until the word runs out:
//!
//! ```c
//!         if (arg[1] != '-') {
//!                 ctx->opt = arg + 1;
//!                 switch (parse_short_opt(ctx, options)) {
//!                 ...
//!                 while (ctx->opt) {
//!                         switch (parse_short_opt(ctx, options)) {
//!                         case PARSE_OPT_UNKNOWN:
//!                                 if (internal_help && *ctx->opt == 'h')
//!                                         goto show_usage;
//!                                 ctx->argv[0] = xstrdup(ctx->opt - 1);
//!                                 *(char *)ctx->argv[0] = '-';
//!                                 goto unknown;
//! ```
//!
//! (parse-options.c:1061-1107.) How much of the word one character eats is the
//! option's type, through `get_arg()` (parse-options.c:47-62): a
//! `PARSE_OPT_NOARG` entry eats nothing and parsing carries on at the next
//! character, an entry with a required value eats the whole remainder of the
//! word — or the next word when the option ends this one — and a
//! `PARSE_OPT_OPTARG` entry takes an attached value and never a detached one.
//!
//! This port's verbs matched whole tokens (`"-q" | "--quiet"`), so every clump
//! was an unknown switch, and the handful of verbs that did split clumps each
//! had their own copy of the loop with its own gaps. `crate::parseopt`'s
//! `expand_short()` is the one port of `parse_short_opt()`; these tests pin the
//! behaviour it has to produce at the command line.
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/usr/local/bin/git`) in a repository built exactly like [`Fixture::new`],
//! and is quoted at the test that asserts it. Nothing here shells out to stock
//! git, reads ambient config or touches the network, so it runs headless.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

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

/// One run's full result: a clump that is rejected says so on stderr, and one
/// that is answered with `-h` says so on stdout, so both streams matter.
struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Fixture {
    /// One commit holding a three-line `f`, plus an unstaged edit to its last
    /// line — enough for a diff, a blame and a stash to have something to say.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zvcs-short-clumps-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\nb\nc\n").unwrap();
        f.ok(&["add", "f"]);
        f.ok(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("f"), "a\nb\nd\n").unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> Out {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        Out {
            code: out.status.code().expect("exited via a signal"),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(out.code, 0, "`git {args:?}` failed: {}", out.stderr);
        out.stdout
    }
}

/// `git stash push -qm <msg>` and `git checkout -qb <branch>`: a flag followed
/// by an option that takes the next word. Stock creates the entry and the
/// branch; this port answered ``error: unknown switch `qm'`` and ``unknown
/// switch `q'``.
#[test]
fn a_flag_and_a_value_option_share_one_word() {
    let f = Fixture::new("flag-then-value");

    f.ok(&["stash", "push", "-qm", "from a clump"]);
    let list = f.ok(&["stash", "list"]);
    assert!(list.contains("from a clump"), "stash list: {list:?}");

    f.ok(&["checkout", "-qb", "nb"]);
    assert_eq!(f.ok(&["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "nb");
}

/// `get_arg()` takes the rest of the word when there is one
/// (parse-options.c:48-50), so `-qmmsg` needs no second word: stock answers
/// `git commit -amsg` with `[main 8dafd7d] sg` — the message is `sg`, not
/// `msg`, because `m` is the option and `sg` is its value.
#[test]
fn a_value_option_takes_the_rest_of_its_own_word() {
    let f = Fixture::new("attached-value");

    f.ok(&["commit", "-amsg"]);
    assert_eq!(f.ok(&["log", "-1", "--format=%s"]).trim(), "sg");

    f.ok(&["checkout", "-qbnb"]);
    assert_eq!(f.ok(&["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "nb");
}

/// The diff table's clumps, which reach every verb that ends in
/// `setup_revisions()`: `git diff -pw` is `-p -w` and `git show -sp` is
/// `-s -p`, whose `OPT_BITOP` puts the patch back on. Stock prints a patch for
/// both; this port answered `unsupported option "-pw"` and
/// `unsupported option -sp`.
#[test]
fn the_diff_table_splits_its_own_clumps() {
    let f = Fixture::new("diff-table");

    let diff = f.ok(&["diff", "-pw"]);
    assert!(diff.starts_with("diff --git a/f b/f"), "diff -pw: {diff:?}");

    let show = f.ok(&["show", "-sp"]);
    assert!(show.contains("diff --git a/f b/f"), "show -sp: {show:?}");

    // `diff-files` reaches the same table through its own leftover loop.
    let files = f.ok(&["diff-files", "-Rp"]);
    assert!(files.starts_with("diff --git b/f a/f"), "diff-files -Rp: {files:?}");
}

/// `blame -ln f`: `-l` (long object names) then `-n` (show the original line
/// number), which stock renders as `^<40 hex> 1 (A U Thor … 1) a`. This port
/// answered `unsupported option: -ln`.
#[test]
fn blame_splits_its_output_flags() {
    let f = Fixture::new("blame");
    let out = f.ok(&["blame", "-ln", "f"]);
    let first = out.lines().next().expect("blame printed nothing");
    let id = first.split(' ').next().unwrap();
    // A boundary commit spends the first column on `^`, so `-l`'s full name is
    // 39 hex behind it — 40 characters in all, against `-l`-less blame's 8.
    assert_eq!(id.len(), 40, "-l asks for the full object name: {first:?}");
    assert!(id.starts_with('^'), "the root commit is a boundary: {first:?}");
    // `-n`'s column: the line's number in the original commit, before the
    // rendered `1)` of the current file.
    assert!(first.contains(" 1 (A U Thor"), "-n's original line number: {first:?}");
}

/// A clump whose second character is unknown: the characters in front of it are
/// still consumed, and `parse_options()` names the character it stopped at —
/// ``error: unknown switch `Z'`` — not the word. Stock exits 129 with the usage
/// block on stderr.
#[test]
fn an_unknown_character_mid_clump_names_itself() {
    let f = Fixture::new("unknown-mid-clump");

    let out = f.run(&["add", "-nZ", "f"]);
    assert_eq!(out.code, 129, "stderr: {}", out.stderr);
    assert!(
        out.stderr.starts_with("error: unknown switch `Z'\n"),
        "the character parsing stopped at, not `n' and not `nZ': {:?}",
        out.stderr
    );
    assert!(out.stderr.contains("usage: git add"), "the block follows: {:?}", out.stderr);

    // The same for a verb whose refusal comes from its own table.
    let out = f.run(&["reset", "-qZ"]);
    assert_eq!(out.code, 129, "stderr: {}", out.stderr);
    assert!(
        out.stderr.starts_with("error: unknown switch `Z'\n"),
        "reset -qZ: {:?}",
        out.stderr
    );
}

/// `-h` inside a clump: `if (internal_help && *ctx->opt == 'h') goto
/// show_usage` (parse-options.c:1087-1088) is reached after everything in
/// front of it has been applied, so `git add -vh` prints the block on *stdout*
/// at 129 — a help request is not a rejection — while `git add -Zh` never gets
/// there and refuses `Z`.
#[test]
fn h_mid_clump_asks_for_help_but_only_once_it_is_reached() {
    let f = Fixture::new("help-mid-clump");

    let out = f.run(&["add", "-vh"]);
    assert_eq!(out.code, 129);
    assert!(out.stdout.starts_with("usage: git add"), "stdout: {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "stderr must stay empty: {:?}", out.stderr);

    let out = f.run(&["add", "-Zh"]);
    assert_eq!(out.code, 129);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert!(
        out.stderr.starts_with("error: unknown switch `Z'\n"),
        "the unknown character comes first: {:?}",
        out.stderr
    );
}

/// A value-taking character that ends the word takes the *next* word, and
/// refuses when there is none: `get_arg()`'s `error(_("%s requires a value"))`
/// (parse-options.c:59-60) names the switch by its character and prints no
/// usage block. Stock: ``error: switch `m' requires a value``, exit 129.
#[test]
fn a_value_option_ending_a_clump_refuses_an_absent_value() {
    let f = Fixture::new("missing-value");
    let out = f.run(&["commit", "-qm"]);
    assert_eq!(out.code, 129, "stdout: {:?}", out.stdout);
    assert_eq!(out.stderr, "error: switch `m' requires a value\n");
}

/// `PARSE_OPT_OPTARG` takes an attached value and never a detached one, so its
/// word cannot be split at all: stock answers `git diff -Mp` with `error:
/// invalid argument to find-renames` — `p` is `-M`'s value — and exits 129,
/// where splitting it would have produced a patch.
#[test]
fn an_optarg_character_keeps_the_rest_of_the_word_as_its_value() {
    let f = Fixture::new("optarg");
    let out = f.run(&["diff", "-Mp"]);
    assert_eq!(out.code, 129, "stdout: {:?}", out.stdout);
    assert_eq!(out.stderr, "error: invalid argument to find-renames\n");

    // `checkout`'s `--track` is the same shape behind a flag: stock answers
    // `git checkout -qt2` with ``error: option `--track' expects "direct" or
    // "inherit"`` — `-q` applied, `2` read as `-t`'s attached mode.
    let out = f.run(&["checkout", "-qt2"]);
    assert_eq!(out.code, 129, "stdout: {:?}", out.stdout);
    assert_eq!(out.stderr, "error: option `--track' expects \"direct\" or \"inherit\"\n");
}

/// A required value swallows the rest of the word whatever it looks like, so
/// `-U3p` is `--unified=3p` and not `-U 3` plus `-p`: stock is `error:
/// --unified expects a numerical value`, exit 129.
#[test]
fn a_required_value_swallows_the_rest_of_the_word_whole() {
    let f = Fixture::new("required-value");
    let out = f.run(&["diff", "-U3p"]);
    assert_eq!(out.code, 129, "stdout: {:?}", out.stdout);
    assert_eq!(out.stderr, "error: --unified expects a numerical value\n");
}

/// `OPTION_NUMBER` (parse-options.c:441-452): a run of digits is one option and
/// the loop carries on behind it. `git archive`'s number entry is the
/// compression level, which the format backend refuses — stock answers `git
/// archive -v0 HEAD` with `fatal: Argument not supported for format 'tar': -0`
/// at 128, proving both that `-v` was consumed and that `-0` reached the
/// backend whole.
#[test]
fn a_digit_run_is_one_option_of_its_own() {
    let f = Fixture::new("number-option");

    let out = f.run(&["archive", "-v0", "HEAD"]);
    assert_eq!(out.code, 128, "stdout len {}", out.stdout.len());
    assert_eq!(out.stderr, "fatal: Argument not supported for format 'tar': -0\n");

    // …and the plain two-flag clump, which used to be ``unknown switch `l'``.
    let formats = f.ok(&["archive", "-lv"]);
    assert!(formats.lines().any(|l| l == "tar"), "archive -lv: {formats:?}");
}

/// `PARSE_OPT_STOP_AT_NON_OPTION` ends option parsing at the first operand
/// (parse-options.c:1023-1030), and every word behind it is data however it is
/// spelled. `git config` is the case: stock stores the literal `-ab`, so the
/// rewrite must stop where the parse does.
#[test]
fn the_rewrite_stops_where_option_parsing_stops() {
    let f = Fixture::new("stop-at-non-option");

    f.ok(&["config", "user.x", "-ab"]);
    assert_eq!(f.ok(&["config", "--get", "user.x"]).trim(), "-ab");

    // Ahead of the first operand the same characters are two options: `-l`
    // lists, `-z` terminates each entry with NUL.
    let listed = f.ok(&["config", "-lz"]);
    assert!(listed.contains("user.x\n-ab\0"), "config -lz: {listed:?}");
}

/// The clump loop must not change which options a verb *has*: an option that
/// takes a value behind a flag is still that option, so `git add -AU 3` reaches
/// `--unified`'s own refusal rather than ``unknown switch `U'``. Stock: `fatal:
/// the option '--unified' requires '--interactive/--patch'`, exit 128.
#[test]
fn a_clump_reaches_the_value_options_the_table_owns() {
    let f = Fixture::new("value-option-in-clump");

    let out = f.run(&["add", "-AU", "3", "f"]);
    assert_eq!(out.code, 128, "stdout: {:?}", out.stdout);
    assert_eq!(
        out.stderr,
        "fatal: the option '--unified' requires '--interactive/--patch'\n"
    );

    let out = f.run(&["reset", "-qU", "3"]);
    assert_eq!(out.code, 128, "stdout: {:?}", out.stdout);
    assert_eq!(out.stderr, "fatal: the option '--unified' requires '--patch'\n");
}

/// Nothing behind `--` is an option, whatever it starts with: the separator
/// ends the rewrite exactly as it ends `parse_options_step()`'s loop
/// (parse-options.c:1110-1115). Stock: `fatal: pathspec \'-nZ\' did not match
/// any files`, exit 128 — the word is one pathspec, not `-n` and a refusal of
/// `Z`.
#[test]
fn a_clump_behind_the_separator_is_a_pathspec() {
    let f = Fixture::new("dashdash");
    let out = f.run(&["add", "--", "-nZ"]);
    assert_eq!(out.code, 128, "stdout: {:?}", out.stdout);
    assert_eq!(out.stderr, "fatal: pathspec \'-nZ\' did not match any files\n");
}
