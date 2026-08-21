//! `parse-options.c`'s fixed error vocabulary, across every verb routed through
//! the shared port in `src/parseopt.rs`.
//!
//! git has exactly four things to say about a malformed option, and it says them
//! in exactly two shapes. Which shape appears is not a style choice: it is
//! decided by which `enum parse_opt_result` reached `parse_options()`.
//!
//! ```c
//!         switch (parse_options_step(&ctx, options, usagestr)) {
//!         case PARSE_OPT_HELP:
//!         case PARSE_OPT_ERROR:
//!                 exit(129);
//!         ...
//!         case PARSE_OPT_UNKNOWN:
//!                 if (ctx.argv[0][1] == '-') {
//!                         error(_("unknown option `%s'"), ctx.argv[0] + 2);
//!                 } else if (isascii(*ctx.opt)) {
//!                         error(_("unknown switch `%c'"), *ctx.opt);
//!                 } else {
//!                         error(_("unknown non-ascii option in string: `%s'"),
//!                               ctx.argv[0]);
//!                 }
//!                 usage_with_options(usagestr, options);
//!         }
//! ```
//! (parse-options.c:1198-1224)
//!
//!   * `PARSE_OPT_ERROR` — what `get_arg()` returns for a missing value — has
//!     already printed its own `error:` line, so `parse_options()` does nothing
//!     but `exit(129)`. **No usage block.**
//!   * `PARSE_OPT_UNKNOWN` prints the `error:` line *and* the usage block, both on
//!     stderr, and exits 129.
//!
//! So `git commit -m` is one line of stderr and `git commit -b` is eighty, and a
//! port that appends the block to both — or to neither — diverges in a way
//! scripted callers see. Three further rules are pinned here because each one was
//! wrong somewhere before the shared port existed:
//!
//!   * `optname()` (parse-options.c:30-45) names the option **as typed**:
//!     ``switch `m'`` for `-m` and ``option `message'`` for `--message`, even
//!     though both reach the same table entry.
//!   * a value-taking option given no value must never be treated as having an
//!     empty one — `git merge --cleanup` is a missing value, not an invalid
//!     cleanup mode, and never reaches the command's own logic.
//!   * a short cluster is named by the character parsing **stopped at**, against
//!     the synthetic `-<rest>` token `parse_options_step()` builds at
//!     parse-options.c:1095, so `git tag -aé` reports `-é`.
//!
//! Every expectation below is the observed stderr and exit status of stock git
//! 2.55.0, captured by running it — not transcribed from documentation. The
//! usage blocks are long and per-verb, so a case that expects one asserts the
//! `error:` line exactly and the block by its first line; a case that expects
//! *no* block asserts the whole of stderr byte-for-byte, which is the half that
//! tells the two shapes apart.
//!
//! No network, no stock git binary, no ambient config: every case runs the built
//! `git` in a private temp repository with `HOME` pinned inside it.
#![cfg(unix)]

use std::path::{Path, PathBuf};
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

impl Fixture {
    /// A one-commit repository on `main`, so the happy-path cases have something
    /// to act on and the refusals have a repository to be refused inside.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zvcs-parseopt-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        std::fs::write(f.work.join("a.txt"), "base\n").unwrap();
        f.ok(&["init", "-q", "-b", "main", "."]);
        f.ok(&["config", "user.email", "t@example.com"]);
        f.ok(&["config", "user.name", "T"]);
        f.ok(&["add", "a.txt"]);
        f.ok(&["commit", "-q", "-m", "base"]);
        f
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Run {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        Run {
            code: out.status.code().expect("git exited via a signal"),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_in(&self.work, args)
    }

    fn ok(&self, args: &[&str]) {
        let r = self.run(args);
        assert_eq!(r.code, 0, "setup `git {args:?}` failed: {}", r.stderr);
    }
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// `PARSE_OPT_ERROR`: the `error:` line **alone** on stderr, nothing on stdout,
/// exit 129.
///
/// stderr is compared in full. That is the assertion that separates this shape
/// from [`assert_unknown`]: appending the usage block here would still match a
/// `starts_with`, and the two shapes would stop being distinguishable.
fn assert_value_error(f: &Fixture, args: &[&str], line: &str) {
    let r = f.run(args);
    assert_eq!(r.code, 129, "`git {args:?}` exit code (stderr: {})", r.stderr);
    assert_eq!(r.stderr, format!("{line}\n"), "`git {args:?}` stderr");
    assert_eq!(r.stdout, "", "`git {args:?}` stdout");
}

/// `PARSE_OPT_UNKNOWN`: the `error:` line **and** the usage block, both on
/// stderr, nothing on stdout, exit 129.
///
/// `usage_head` is the first line of the verb's own `usage_with_options()`
/// block; asserting it pins that the block really followed rather than copying
/// eighty lines of option table into every case.
fn assert_unknown(f: &Fixture, args: &[&str], line: &str, usage_head: &str) {
    let r = f.run(args);
    assert_eq!(r.code, 129, "`git {args:?}` exit code (stderr: {})", r.stderr);
    assert_eq!(r.stdout, "", "`git {args:?}` stdout");
    assert_eq!(
        r.stderr.lines().next(),
        Some(line),
        "`git {args:?}` first stderr line"
    );
    assert_eq!(
        r.stderr.lines().nth(1),
        Some(usage_head),
        "`git {args:?}` did not print the usage block after the error"
    );
}

// ---------------------------------------------------------------------------
// optname(): the spelling that was typed decides the wording
// ---------------------------------------------------------------------------

/// ``switch `<c>'`` for a short option and ``option `<name>'`` for a long one,
/// for the same table entry. Measured on stock git 2.55.0: `git commit -m`,
/// `git commit --message`, `git tag -m`, `git tag --message`, `git merge -s`,
/// `git merge --strategy`.
#[test]
fn a_missing_value_is_named_by_the_spelling_that_was_typed() {
    let f = Fixture::new("typed");
    for (args, line) in [
        (&["commit", "-m"][..], "error: switch `m' requires a value"),
        (&["commit", "--message"], "error: option `message' requires a value"),
        (&["commit", "-F"], "error: switch `F' requires a value"),
        (&["commit", "--file"], "error: option `file' requires a value"),
        (&["commit", "-C"], "error: switch `C' requires a value"),
        (&["commit", "-c"], "error: switch `c' requires a value"),
        (&["commit", "-t"], "error: switch `t' requires a value"),
        (&["commit", "--template"], "error: option `template' requires a value"),
        (&["tag", "-m"], "error: switch `m' requires a value"),
        (&["tag", "--message"], "error: option `message' requires a value"),
        (&["tag", "-u"], "error: switch `u' requires a value"),
        (&["merge", "-s"], "error: switch `s' requires a value"),
        (&["merge", "--strategy"], "error: option `strategy' requires a value"),
        (&["merge", "-X"], "error: switch `X' requires a value"),
        (&["merge", "-m"], "error: switch `m' requires a value"),
        (&["merge", "--message"], "error: option `message' requires a value"),
        (&["init", "-b"], "error: switch `b' requires a value"),
        (&["init", "--initial-branch"], "error: option `initial-branch' requires a value"),
        (&["init-db", "-b"], "error: switch `b' requires a value"),
        (&["archive", "-o"], "error: switch `o' requires a value"),
        (&["archive", "--output"], "error: option `output' requires a value"),
        (&["checkout", "-b"], "error: switch `b' requires a value"),
        (&["checkout", "-B"], "error: switch `B' requires a value"),
        (&["cherry-pick", "-m"], "error: switch `m' requires a value"),
        (&["cherry-pick", "--mainline"], "error: option `mainline' requires a value"),
        (&["revert", "-X"], "error: switch `X' requires a value"),
        (&["revert", "--strategy-option"], "error: option `strategy-option' requires a value"),
        (&["am", "-C"], "error: switch `C' requires a value"),
        (&["am", "-p"], "error: switch `p' requires a value"),
        (&["fmt-merge-msg", "-m"], "error: switch `m' requires a value"),
        (&["fmt-merge-msg", "--message"], "error: option `message' requires a value"),
        (&["symbolic-ref", "-m"], "error: switch `m' requires a value"),
        (&["blame", "-L"], "error: switch `L' requires a value"),
        (&["blame", "--contents"], "error: option `contents' requires a value"),
        (&["clone", "-o"], "error: switch `o' requires a value"),
        (&["clone", "--origin"], "error: option `origin' requires a value"),
        (&["clone", "-j"], "error: switch `j' requires a value"),
        (&["push", "-o"], "error: switch `o' requires a value"),
        (&["push", "--push-option"], "error: option `push-option' requires a value"),
        (&["verify-pack", "--object-format"], "error: option `object-format' requires a value"),
    ] {
        assert_value_error(&f, args, line);
    }
}

/// `git merge`'s `-F`/`--file` is the one option in these tables that does *not*
/// follow `optname()`. It is an `OPTION_LOWLEVEL_CALLBACK`, which
/// `do_get_value()` dispatches to without calling `get_arg()`
/// (parse-options.c:146-147), so the callback fetches its own value and words
/// its own refusal against `opt->long_name`:
///
/// ```c
///         } else
///                 return error(_("option `%s' requires a value"),
///                              opt->long_name);
/// ```
/// (builtin/merge.c:156-157)
///
/// Stock therefore answers `git merge -F` with ``option `file'`` and not
/// ``switch `F'``, which is the opposite of `git commit -F` two tests up.
#[test]
fn merges_dash_f_is_named_by_its_long_name_in_both_spellings() {
    let f = Fixture::new("mergef");
    assert_value_error(&f, &["merge", "-F"], "error: option `file' requires a value");
    assert_value_error(&f, &["merge", "--file"], "error: option `file' requires a value");
}

// ---------------------------------------------------------------------------
// PARSE_OPT_ERROR vs PARSE_OPT_UNKNOWN: with and without the usage block
// ---------------------------------------------------------------------------

/// An unknown option prints the block; a missing value does not. Asserted on the
/// same verb so the difference cannot be a per-verb accident.
#[test]
fn only_the_unknown_option_shape_prints_the_usage_block() {
    let f = Fixture::new("shapes");
    assert_value_error(&f, &["commit", "-m"], "error: switch `m' requires a value");
    assert_unknown(
        &f,
        &["commit", "-b"],
        "error: unknown switch `b'",
        "usage: git commit [-a | --interactive | --patch] [-s] [-v] [-u[<mode>]] [--amend]",
    );
    assert_unknown(
        &f,
        &["commit", "--zzbogus"],
        "error: unknown option `zzbogus'",
        "usage: git commit [-a | --interactive | --patch] [-s] [-v] [-u[<mode>]] [--amend]",
    );
}

/// The unknown-option refusal, per converted verb: an unknown short switch and
/// an unknown long option, both with the block behind them.
#[test]
fn an_unknown_option_is_refused_in_gits_two_wordings() {
    let f = Fixture::new("unknown");
    for (args, line, head) in [
        (
            &["tag", "-o"][..],
            "error: unknown switch `o'",
            "usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]",
        ),
        (
            &["tag", "--zzbogus"],
            "error: unknown option `zzbogus'",
            "usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]",
        ),
        (
            &["merge", "-o"],
            "error: unknown switch `o'",
            "usage: git merge [<options>] [<commit>...]",
        ),
        (
            &["merge", "--zzbogus"],
            "error: unknown option `zzbogus'",
            "usage: git merge [<options>] [<commit>...]",
        ),
        (
            &["init", "-a"],
            "error: unknown switch `a'",
            "usage: git init [-q | --quiet] [--bare] [--template=<template-directory>]",
        ),
        (
            &["init-db", "-a"],
            "error: unknown switch `a'",
            "usage: git init [-q | --quiet] [--bare] [--template=<template-directory>]",
        ),
        (
            &["mv", "-a"],
            "error: unknown switch `a'",
            "usage: git mv [-v] [-f] [-n] [-k] <source> <destination>",
        ),
        (
            &["symbolic-ref", "-a"],
            "error: unknown switch `a'",
            "usage: git symbolic-ref [-m <reason>] <name> <ref>",
        ),
        (
            &["push", "-a"],
            "error: unknown switch `a'",
            "usage: git push [<options>] [<repository> [<refspec>...]]",
        ),
        (
            &["clone", "-a"],
            "error: unknown switch `a'",
            "usage: git clone [<options>] [--] <repo> [<dir>]",
        ),
    ] {
        assert_unknown(&f, args, line, head);
    }
}

// ---------------------------------------------------------------------------
// The short-cluster walk
// ---------------------------------------------------------------------------

/// `parse_options_step()` rewrites `argv[0]` before reporting an unknown
/// character that is not the first of its token:
///
/// ```c
///         ctx->argv[0] = xstrdup(ctx->opt - 1);
///         *(char *)ctx->argv[0] = '-';
///         goto unknown;
/// ```
/// (parse-options.c:1095-1097)
///
/// so the character named is the one parsing *stopped at*. `git merge -nZ` is
/// `Z`, not `n` — and the whole token has to be walked to know that, which is
/// what these verbs used to skip.
#[test]
fn a_cluster_is_named_by_the_character_parsing_stopped_at() {
    let f = Fixture::new("cluster");
    for (args, line, head) in [
        (
            &["tag", "-aZ"][..],
            "error: unknown switch `Z'",
            "usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]",
        ),
        (
            &["merge", "-nZ"],
            "error: unknown switch `Z'",
            "usage: git merge [<options>] [<commit>...]",
        ),
        (
            &["commit", "-qZ"],
            "error: unknown switch `Z'",
            "usage: git commit [-a | --interactive | --patch] [-s] [-v] [-u[<mode>]] [--amend]",
        ),
        (
            &["init", "-qa"],
            "error: unknown switch `a'",
            "usage: git init [-q | --quiet] [--bare] [--template=<template-directory>]",
        ),
        (
            &["mv", "-fa"],
            "error: unknown switch `a'",
            "usage: git mv [-v] [-f] [-n] [-k] <source> <destination>",
        ),
        (
            &["symbolic-ref", "-qa"],
            "error: unknown switch `a'",
            "usage: git symbolic-ref [-m <reason>] <name> <ref>",
        ),
        (
            &["push", "-nZ"],
            "error: unknown switch `Z'",
            "usage: git push [<options>] [<repository> [<refspec>...]]",
        ),
        (
            &["clone", "-qZ"],
            "error: unknown switch `Z'",
            "usage: git clone [<options>] [--] <repo> [<dir>]",
        ),
    ] {
        assert_unknown(&f, args, line, head);
    }
}

/// `isascii(*ctx.opt)` decides the third wording, and it is tested on the first
/// *byte* — so a multi-byte character reports the whole (synthetic) token rather
/// than a character. The `-aé` case additionally proves the token was rebuilt:
/// stock names `-é`, not `-aé`.
///
/// This also pins that walking the token does not split a codepoint. The gate it
/// replaced sliced `a[1..2]`, which panicked outright on `git tag -é`.
#[test]
fn a_non_ascii_option_names_the_whole_token() {
    let f = Fixture::new("nonascii");
    for (args, head) in [
        (
            &["tag", "-é"][..],
            "usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]",
        ),
        (
            &["tag", "-aé"],
            "usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]",
        ),
        (&["mv", "-é"], "usage: git mv [-v] [-f] [-n] [-k] <source> <destination>"),
        (
            &["symbolic-ref", "-é"],
            "usage: git symbolic-ref [-m <reason>] <name> <ref>",
        ),
        (
            &["commit", "-é"],
            "usage: git commit [-a | --interactive | --patch] [-s] [-v] [-u[<mode>]] [--amend]",
        ),
    ] {
        assert_unknown(&f, args, "error: unknown non-ascii option in string: `-é'", head);
    }
}

/// A value-taking character that ends a cluster still reaches for the next argv
/// element, and is named by *its* character rather than by the one the token
/// began with: `git tag -fm` is ``switch `m'``, not ``switch `f'``.
#[test]
fn a_value_taking_character_at_the_end_of_a_cluster_names_itself() {
    let f = Fixture::new("clusterval");
    assert_value_error(&f, &["tag", "-fm"], "error: switch `m' requires a value");
    assert_value_error(&f, &["commit", "-am"], "error: switch `m' requires a value");
    assert_value_error(&f, &["commit", "-qF"], "error: switch `F' requires a value");
    assert_value_error(&f, &["init", "-qb"], "error: switch `b' requires a value");
    assert_value_error(&f, &["merge", "-nm"], "error: switch `m' requires a value");
    assert_value_error(&f, &["merge", "-nX"], "error: switch `X' requires a value");
}

/// `internal_help` is tested inside the short-option loop
/// (parse-options.c:1069, :1087), so a cluster asks for help exactly when the
/// first character the table does *not* define is `h` — and a help request is
/// not a rejection, so the block goes to **stdout** with no `error:` line.
#[test]
fn h_inside_a_cluster_is_help_and_not_a_rejection() {
    let f = Fixture::new("clusterhelp");
    for (args, head) in [
        (
            &["tag", "-fh"][..],
            "usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]",
        ),
        (
            &["tag", "-lh"],
            "usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]",
        ),
        (
            &["init", "-qh"],
            "usage: git init [-q | --quiet] [--bare] [--template=<template-directory>]",
        ),
        (&["merge", "-nh"], "usage: git merge [<options>] [<commit>...]"),
    ] {
        let r = f.run(args);
        assert_eq!(r.code, 129, "`git {args:?}` exit code");
        assert_eq!(r.stderr, "", "`git {args:?}` wrote to stderr; help is not an error");
        assert_eq!(
            r.stdout.lines().next(),
            Some(head),
            "`git {args:?}` stdout is not the usage block"
        );
    }
}

/// The other side of the same test: an unknown character *before* the `h` wins,
/// because `parse_short_opt()` stops at it and `PARSE_OPT_UNKNOWN` is answered
/// before the help test for the next character is ever reached.
#[test]
fn an_unknown_character_ahead_of_h_is_still_a_rejection() {
    let f = Fixture::new("clusterZh");
    assert_unknown(
        &f,
        &["tag", "-Zh"],
        "error: unknown switch `Z'",
        "usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]",
    );
}

// ---------------------------------------------------------------------------
// The correctness bug: an absent value is not an empty value
// ---------------------------------------------------------------------------

/// `get_arg()` refuses before the command's own logic ever runs, so the
/// diagnostic is about the *missing value* and not about what an empty value
/// would have meant.
///
/// Each of these used to reach the command with `""` in hand and report the
/// consequence instead: `git merge --cleanup` said `fatal: Invalid cleanup mode `
/// at 128, `git add --chmod` said `fatal: --chmod param '' must be either -x or
/// +x` at 128, and `git add --pathspec-from-file` tried to open `''`. All three
/// are 129 and a `requires a value` line in stock git.
#[test]
fn an_absent_value_is_refused_before_the_command_can_misread_it() {
    let f = Fixture::new("absent");
    assert_value_error(&f, &["merge", "--cleanup"], "error: option `cleanup' requires a value");
    assert_value_error(&f, &["add", "--chmod"], "error: option `chmod' requires a value");
    assert_value_error(
        &f,
        &["add", "--pathspec-from-file"],
        "error: option `pathspec-from-file' requires a value",
    );
    assert_value_error(
        &f,
        &["checkout", "--pathspec-from-file"],
        "error: option `pathspec-from-file' requires a value",
    );
    assert_value_error(
        &f,
        &["checkout", "--conflict"],
        "error: option `conflict' requires a value",
    );
    // The repository is untouched: the refusal happened during parsing.
    let r = f.run(&["status", "--porcelain"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "", "a refused command line changed the worktree");
}

/// `--revision` is an `OPT_STRING` in `builtin_clone_options[]`, so
/// parse-options fetches its value before `cmd_clone()` runs at all. Cloning at
/// a bare revision is not ported — but that is a *different* refusal, and it
/// must not pre-empt the one parse-options owns.
#[test]
fn a_value_is_fetched_before_a_port_gap_is_reported() {
    let f = Fixture::new("cloneRev");
    assert_value_error(&f, &["clone", "--revision"], "error: option `revision' requires a value");
}

/// `git blame --date` is the counter-example that keeps the two "requires a
/// value" tables honest: `--date` belongs to `revision.c`'s hand-rolled matcher,
/// not to blame's own `options[]`, so `handle_revision_opt()` words it
/// `fatal: Option '--<name>' requires a value` and exits **128**. Routing it
/// through parse-options' wording would be wrong in both text and status.
#[test]
fn a_revision_option_keeps_revision_cs_wording_and_128() {
    let f = Fixture::new("blamedate");
    let r = f.run(&["blame", "--date"]);
    assert_eq!(r.code, 128, "stderr: {}", r.stderr);
    assert_eq!(r.stderr, "fatal: Option '--date' requires a value\n");
}

/// `-n[<num>]` is `OPTION_INTEGER` with `PARSE_OPT_OPTARG`, so it never reaches
/// for the next argv element: nothing attached is `!p->opt` and takes `defval`.
/// `git tag -ln` therefore *lists* rather than refusing, while an attached value
/// that is not a number is `git_parse_signed()`'s complaint — named for the
/// switch, and with no usage block behind it.
#[test]
fn an_optarg_integer_defaults_instead_of_demanding_a_value() {
    let f = Fixture::new("tagn");
    f.ok(&["tag", "-a", "-m", "annotated", "v1"]);

    let r = f.run(&["tag", "-ln"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(r.stdout, "v1              annotated\n");

    let r = f.run(&["tag", "-ln5"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(r.stdout, "v1              annotated\n");

    assert_value_error(
        &f,
        &["tag", "-nx"],
        "error: switch `n' expects an integer value with an optional k/m/g suffix",
    );
    assert_value_error(
        &f,
        &["tag", "-lnx"],
        "error: switch `n' expects an integer value with an optional k/m/g suffix",
    );
}

// ---------------------------------------------------------------------------
// The happy path, per converted verb
// ---------------------------------------------------------------------------

/// Every verb whose option walk was rewritten still parses a well-formed command
/// line. Single flags, clusters and attached values are all exercised, because
/// the rewrites moved the argv cursor and a cursor that is off by one is
/// invisible until an option's value is eaten as an operand.
#[test]
fn the_happy_path_survives_the_rewritten_walks() {
    let f = Fixture::new("happy");

    // tag: a cluster (`-a` + `-m <msg>`), then the listing options.
    f.ok(&["tag", "-am", "first annotated", "v1"]);
    let r = f.run(&["tag", "-l", "v*"]);
    assert_eq!((r.code, r.stdout.as_str()), (0, "v1\n"));
    let r = f.run(&["tag", "-n1", "v1"]);
    assert_eq!((r.code, r.stdout.as_str()), (0, "v1              first annotated\n"));

    // commit: `-a` and `-m` in one cluster, with the value in the next argv slot.
    std::fs::write(f.work.join("a.txt"), "second\n").unwrap();
    f.ok(&["commit", "-qam", "second"]);
    let r = f.run(&["log", "--format=%s", "-n", "1"]);
    assert_eq!((r.code, r.stdout.as_str()), (0, "second\n"));

    // commit: the separated long spellings, and `--branch`, whose `-b` short
    // form this port used to invent.
    std::fs::write(f.work.join("a.txt"), "third\n").unwrap();
    f.ok(&["commit", "-q", "--message", "third", "--author", "A U Thor <a@example.com>", "-a"]);
    let r = f.run(&["log", "--format=%an", "-n", "1"]);
    assert_eq!((r.code, r.stdout.as_str()), (0, "A U Thor\n"));

    // merge: `-m` and `-s` with separated values, on an already-merged branch so
    // the run is a no-op that still had to parse.
    let r = f.run(&["merge", "-s", "ort", "-m", "merge message", "HEAD"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(r.stdout, "Already up to date.\n");

    // merge: the `-nq` cluster, which used to be `unsupported flag`.
    let r = f.run(&["merge", "-nq", "HEAD"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);

    // init: `-q` and `-b <name>` as a cluster, into a fresh directory.
    let nested = f.work.join("fresh");
    std::fs::create_dir_all(&nested).unwrap();
    let r = f.run_in(&nested, &["init", "-qb", "trunk", "."]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let r = f.run_in(&nested, &["symbolic-ref", "HEAD"]);
    assert_eq!((r.code, r.stdout.as_str()), (0, "refs/heads/trunk\n"));

    // symbolic-ref: `-m <reason>` writes the reflog message it was given.
    let r = f.run_in(&nested, &["symbolic-ref", "-m", "why", "HEAD", "refs/heads/other"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let r = f.run_in(&nested, &["symbolic-ref", "HEAD"]);
    assert_eq!((r.code, r.stdout.as_str()), (0, "refs/heads/other\n"));

    // archive: `-o <file>` with a separated value.
    let out = f.work.join("out.tar");
    let r = f.run(&["archive", "-o", out.to_str().unwrap(), "--format=tar", "HEAD"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(out.exists(), "git archive -o wrote nothing");

    // checkout: `-b <name>` creates the branch and switches to it.
    let r = f.run(&["checkout", "-q", "-b", "topic"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let r = f.run(&["symbolic-ref", "--short", "HEAD"]);
    assert_eq!((r.code, r.stdout.as_str()), (0, "topic\n"));

    // mv: the `-f`/`-v` flags this verb's rewritten cluster walk owns.
    let r = f.run(&["mv", "-fv", "a.txt", "b.txt"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(f.work.join("b.txt").exists(), "git mv -fv moved nothing");
    f.ok(&["commit", "-qm", "rename"]);

    // add: `--chmod` with a separated value, which the missing-value fix rewrote.
    let r = f.run(&["add", "--chmod", "+x", "b.txt"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let r = f.run(&["ls-files", "-s", "b.txt"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.starts_with("100755 "), "--chmod +x did not stage the mode: {}", r.stdout);
}

// ---------------------------------------------------------------------------
// usage() vs usage_with_options(): the block with no error line in front of it
// ---------------------------------------------------------------------------

/// `usage(<usagestr>)` — the block on stderr, exit 129, and **no `error:` line**.
///
/// This is the third shape, and the one `assert_unknown` would happily accept if
/// it only checked that a usage block appeared. `usage_with_options()` prints
/// `error: unknown option `<name>'` first; plain `usage()` does not print
/// anything before the block, which is what `cmd_diff_tree` calls
/// (builtin/diff-tree.c:163). Asserting the *absence* of the error line is the
/// whole assertion.
fn assert_bare_usage(f: &Fixture, args: &[&str], usage_head: &str) {
    let r = f.run(args);
    assert_eq!(r.code, 129, "`git {args:?}` exit code (stderr: {})", r.stderr);
    assert_eq!(r.stdout, "", "`git {args:?}` stdout");
    assert_eq!(
        r.stderr.lines().next(),
        Some(usage_head),
        "`git {args:?}` first stderr line"
    );
    assert!(
        !r.stderr.starts_with("error:"),
        "`git {args:?}` printed an error line ahead of a plain usage() block: {}",
        r.stderr
    );
}

/// The first line of `diff_tree_usage` (builtin/diff-tree.c:83).
const DIFF_TREE_USAGE_HEAD: &str =
    "usage: git diff-tree [--stdin] [-m] [-s] [-v] [--no-commit-id] [--pretty]";

/// The first line of `replay_usage` (builtin/replay.c:85-89).
const REPLAY_USAGE_HEAD: &str = "usage: (EXPERIMENTAL!) git replay ([--contained] \
                                 --onto=<newbase> | --advance=<branch> | --revert=<branch>)";

// ---------------------------------------------------------------------------
// format-patch: every option in builtin_format_patch_options[]
// ---------------------------------------------------------------------------

/// `cmd_format_patch` runs `parse_options()` over its own table
/// (builtin/log.c:2006-2095) to completion before `setup_revisions()` reads a
/// single argument, so a missing value for any of these is `get_arg()`'s
/// `PARSE_OPT_ERROR`: one `error:` line, no usage block, exit 129.
///
/// Every long spelling in that table that takes a value is listed, because the
/// port routes all of them through one helper and a helper is exactly the thing
/// that can be right for the option it was written against and wrong for the
/// other eighteen. `-o` and `-v` are here too: they reach the same helper and
/// must come back out as ``switch `o'`` / ``switch `v'``, not as their long
/// names.
#[test]
fn format_patchs_own_table_reports_a_missing_value_in_gits_words() {
    let f = Fixture::new("fpvalue");
    for (args, line) in [
        (&["format-patch", "--add-header"][..], "error: option `add-header' requires a value"),
        (&["format-patch", "--base"], "error: option `base' requires a value"),
        (&["format-patch", "--cc"], "error: option `cc' requires a value"),
        (
            &["format-patch", "--commit-list-format"],
            "error: option `commit-list-format' requires a value",
        ),
        (
            &["format-patch", "--cover-from-description"],
            "error: option `cover-from-description' requires a value",
        ),
        (
            &["format-patch", "--creation-factor"],
            "error: option `creation-factor' requires a value",
        ),
        (
            &["format-patch", "--description-file"],
            "error: option `description-file' requires a value",
        ),
        (
            &["format-patch", "--filename-max-length"],
            "error: option `filename-max-length' requires a value",
        ),
        (&["format-patch", "--in-reply-to"], "error: option `in-reply-to' requires a value"),
        (&["format-patch", "--interdiff"], "error: option `interdiff' requires a value"),
        (
            &["format-patch", "--output-directory"],
            "error: option `output-directory' requires a value",
        ),
        (&["format-patch", "--range-diff"], "error: option `range-diff' requires a value"),
        (&["format-patch", "--reroll-count"], "error: option `reroll-count' requires a value"),
        (&["format-patch", "--signature"], "error: option `signature' requires a value"),
        (&["format-patch", "--signature-file"], "error: option `signature-file' requires a value"),
        (&["format-patch", "--start-number"], "error: option `start-number' requires a value"),
        (&["format-patch", "--subject-prefix"], "error: option `subject-prefix' requires a value"),
        (&["format-patch", "--suffix"], "error: option `suffix' requires a value"),
        (&["format-patch", "--to"], "error: option `to' requires a value"),
        (&["format-patch", "-o"], "error: switch `o' requires a value"),
        (&["format-patch", "-v"], "error: switch `v' requires a value"),
    ] {
        assert_value_error(&f, args, line);
    }
}

/// Stage 1 outranks stage 2 whatever the argv order: a revision that does not
/// resolve is `setup_revisions()`' to complain about, and `setup_revisions()`
/// does not run until `parse_options()` has swept the whole line
/// (builtin/log.c:2196). So the missing value wins even written last.
#[test]
fn format_patchs_missing_value_outranks_a_bad_revision_written_first() {
    let f = Fixture::new("fporder");
    assert_value_error(
        &f,
        &["format-patch", "zznosuchrev", "--subject-prefix"],
        "error: option `subject-prefix' requires a value",
    );
}

// ---------------------------------------------------------------------------
// fast-export: every option in cmd_fast_export's table
// ---------------------------------------------------------------------------

/// The same shape for `cmd_fast_export`'s table
/// (builtin/fast-export.c:1323-1367). Every entry there is long-only, so
/// `optname()` never produces the `switch` wording for this verb.
///
/// This verb used to answer all ten with its *leftover-argument* usage block
/// instead — `usage_with_options(fast_export_usage, options)` at
/// builtin/fast-export.c:1384 — because the separated `--<opt> <value>` spelling
/// was not recognised at all and fell through `PARSE_OPT_KEEP_UNKNOWN_OPT`'s
/// bucket into `setup_revisions()`. Asserting the whole of stderr is what tells
/// the two apart.
#[test]
fn fast_exports_own_table_reports_a_missing_value_in_gits_words() {
    let f = Fixture::new("fevalue");
    for (args, line) in [
        (&["fast-export", "--anonymize-map"][..], "error: option `anonymize-map' requires a value"),
        (&["fast-export", "--export-marks"], "error: option `export-marks' requires a value"),
        (&["fast-export", "--import-marks"], "error: option `import-marks' requires a value"),
        (
            &["fast-export", "--import-marks-if-exists"],
            "error: option `import-marks-if-exists' requires a value",
        ),
        (&["fast-export", "--progress"], "error: option `progress' requires a value"),
        (&["fast-export", "--reencode"], "error: option `reencode' requires a value"),
        (&["fast-export", "--refspec"], "error: option `refspec' requires a value"),
        (&["fast-export", "--signed-commits"], "error: option `signed-commits' requires a value"),
        (&["fast-export", "--signed-tags"], "error: option `signed-tags' requires a value"),
        (
            &["fast-export", "--tag-of-filtered-object"],
            "error: option `tag-of-filtered-object' requires a value",
        ),
    ] {
        assert_value_error(&f, args, line);
    }
}

/// The half of `get_arg()` that is not an error: `*arg = *++p->argv` reads the
/// next argv slot whatever it contains, so the separated spelling has to work
/// for all ten — and its value must not be left behind to be misread as a
/// revision.
///
/// Before the walk was rewritten `git fast-export --progress 1 HEAD` died
/// `fatal: ambiguous argument '1'` and `--export-marks <file> HEAD` died
/// `fatal: HEAD: no such path in the working tree.`, because the option token
/// went into the unknown bucket and its value became an operand.
#[test]
fn fast_export_takes_a_separated_value_for_every_option_that_has_one() {
    let f = Fixture::new("fesep");
    let marks = f.work.join("marks.txt");
    let marks = marks.to_str().unwrap();

    let r = f.run(&[
        "fast-export",
        "--progress",
        "1",
        "--reencode",
        "yes",
        "--signed-tags",
        "strip",
        "--signed-commits",
        "strip",
        "--tag-of-filtered-object",
        "drop",
        "--refspec",
        "refs/heads/main:refs/heads/main",
        "--export-marks",
        marks,
        "HEAD",
    ]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(
        r.stdout.contains("commit refs/heads/main\n"),
        "no commit stanza in the stream: {}",
        r.stdout
    );
    let written = std::fs::read_to_string(marks).expect("--export-marks wrote no marks file");
    assert!(written.contains(':'), "marks file has no mark lines: {written}");

    // The marks just written import again, through the other two spellings.
    let r = f.run(&["fast-export", "--import-marks", marks, "HEAD"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let r = f.run(&["fast-export", "--import-marks-if-exists", marks, "HEAD"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
}

/// `OPT_INTEGER(0, "progress", …)` has three distinguishable rejections
/// (parse-options.c:260-288) and the port used to give the third wording for all
/// of them. Each is `opterror()`'s single `error:` line and exit 129 — no usage
/// block — in both spellings, since `p->opt` and `*++p->argv` reach the same
/// check.
#[test]
fn fast_exports_progress_reports_gits_three_integer_rejections() {
    let f = Fixture::new("feint");
    assert_value_error(
        &f,
        &["fast-export", "--progress=", "HEAD"],
        "error: option `progress' expects a numerical value",
    );
    assert_value_error(
        &f,
        &["fast-export", "--progress", "", "HEAD"],
        "error: option `progress' expects a numerical value",
    );
    assert_value_error(
        &f,
        &["fast-export", "--progress=bogus", "HEAD"],
        "error: option `progress' expects an integer value with an optional k/m/g suffix",
    );
    assert_value_error(
        &f,
        &["fast-export", "--progress=2147483648", "HEAD"],
        "error: value 2147483648 for option `progress' not in range [-2147483648,2147483647]",
    );
    // The grammar itself is `git_parse_signed()`'s: base 0, so `0x10` is
    // sixteen and `010` is eight, and a k/m/g suffix scales by 1024.
    for v in ["0x10", "010", "1k", "+7", "-2147483648", "2147483647"] {
        let r = f.run(&["fast-export", &format!("--progress={v}"), "HEAD"]);
        assert_eq!(r.code, 0, "`--progress={v}` was rejected: {}", r.stderr);
    }
}

// ---------------------------------------------------------------------------
// diff-tree: the leftover loop, which calls usage() and not usage_with_options()
// ---------------------------------------------------------------------------

/// `cmd_diff_tree` has no option table of its own: `setup_revisions()` takes
/// what it recognises and everything else survives into
///
/// ```c
///         while (--argc > 0) {
///                 const char *arg = *++argv;
///
///                 if (!strcmp(arg, "--stdin")) { read_stdin = 1; continue; }
///                 if (!strcmp(arg, "--merge-base")) { merge_base = 1; continue; }
///                 usage(diff_tree_usage);
///         }
/// ```
/// (builtin/diff-tree.c:152-164)
///
/// `usage()` is not `usage_with_options()`: there is no `error: unknown option`
/// line, only the block, and the status is 129 rather than the `die()` 128 this
/// port used to give. A `-h` that had company is the same argument by the same
/// route.
#[test]
fn diff_tree_answers_an_unknown_argument_with_the_bare_usage_block() {
    let f = Fixture::new("dtunknown");
    for args in [
        &["diff-tree", "--zzbogus"][..],
        &["diff-tree", "-Z"],
        &["diff-tree", "--zzbogus", "HEAD"],
        &["diff-tree", "HEAD", "--zzbogus"],
        &["diff-tree", "-h", "HEAD"],
    ] {
        assert_bare_usage(&f, args, DIFF_TREE_USAGE_HEAD);
    }
}

/// The leftover loop runs *after* `setup_revisions()`, so an operand that does
/// not resolve is diagnosed first however the two are ordered — and *before* the
/// two `die()`s underneath it, so an unknown argument outranks both the
/// `--stdin`/`--merge-base` incompatibility (builtin/diff-tree.c:166-167) and
/// the merge-base operand count (:168-169).
#[test]
fn diff_tree_resolves_revisions_before_it_rejects_an_unknown_argument() {
    let f = Fixture::new("dtorder");
    for args in [
        &["diff-tree", "--zzbogus", "zznosuchrev"][..],
        &["diff-tree", "zznosuchrev", "--zzbogus"],
        &["diff-tree", "-h", "zznosuchrev"],
    ] {
        let r = f.run(args);
        assert_eq!(r.code, 128, "`git {args:?}` exit code (stderr: {})", r.stderr);
        assert_eq!(
            r.stderr.lines().next(),
            Some(
                "fatal: ambiguous argument 'zznosuchrev': unknown revision or path \
                 not in the working tree."
            ),
            "`git {args:?}` stderr"
        );
    }
    for args in [
        &["diff-tree", "--stdin", "--merge-base", "--zzbogus"][..],
        &["diff-tree", "--merge-base", "HEAD", "--zzbogus"],
    ] {
        assert_bare_usage(&f, args, DIFF_TREE_USAGE_HEAD);
    }
}

// ---------------------------------------------------------------------------
// replay: PARSE_OPT_KEEP_UNKNOWN_OPT means the operand check comes first
// ---------------------------------------------------------------------------

/// `cmd_replay` passes `PARSE_OPT_KEEP_UNKNOWN_OPT` (builtin/replay.c:117-118),
/// so an option outside its table is not rejected by `parse_options()` at all —
/// it is copied through for the `setup_revisions()` call at :163. The mode
/// requirement at :121 sits between the two:
///
/// ```c
///         if (!opts.onto && !opts.advance && !opts.revert) {
///                 error(_("exactly one of --onto, --advance, or --revert is required"));
///                 usage_with_options(replay_usage, replay_options);
///         }
/// ```
///
/// So `git replay --zzbogus` is answered by the *missing mode*, with the option
/// never reported at all. This port used to raise its own unported-flag refusal
/// during the sweep, which put it in front — the one thing
/// `PARSE_OPT_KEEP_UNKNOWN_OPT` guarantees cannot happen.
#[test]
fn replay_requires_a_mode_before_it_looks_at_an_unknown_option() {
    let f = Fixture::new("replayorder");
    for args in [
        &["replay"][..],
        &["replay", "--zzbogus"],
        &["replay", "--contained", "--zzbogus"],
        &["replay", "--"],
        &["replay", "--max-count=1", "HEAD"],
    ] {
        assert_unknown(
            &f,
            args,
            "error: exactly one of --onto, --advance, or --revert is required",
            REPLAY_USAGE_HEAD,
        );
    }
}

/// The checks git runs between the mode requirement and `setup_revisions()` keep
/// their place too: `die_for_incompatible_opt3` (builtin/replay.c:126-128) and
/// `get_ref_action_mode` (:137) both come first, and both `die()` at 128.
#[test]
fn replays_incompatible_modes_and_ref_action_still_precede_the_walk() {
    let f = Fixture::new("replaymode");
    let r = f.run(&["replay", "--advance", "main", "--revert", "main", "--zzbogus"]);
    assert_eq!(r.code, 128, "stderr: {}", r.stderr);
    assert_eq!(
        r.stderr,
        "fatal: options '--advance' and '--revert' cannot be used together\n"
    );

    let r = f.run(&["replay", "--onto", "main", "--ref-action=bogus", "--zzbogus"]);
    assert_eq!(r.code, 128, "stderr: {}", r.stderr);
    assert_eq!(r.stderr, "fatal: invalid --ref-action value: 'bogus'\n");
}
