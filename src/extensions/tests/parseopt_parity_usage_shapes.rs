//! The *shape* of an option-parsing failure, across verbs.
//!
//! parse-options has several refusals and they do not look alike. Two of them
//! decide everything below (parse-options.c:1198-1224):
//!
//! ```c
//!         case PARSE_OPT_ERROR:
//!                 exit(129);
//!         case PARSE_OPT_UNKNOWN:
//!                 if (ctx.argv[0][1] == '-')
//!                         error(_("unknown option `%s'"), ctx.argv[0] + 2);
//!                 else if (isascii(*ctx.opt))
//!                         error(_("unknown switch `%c'"), *ctx.opt);
//!                 else
//!                         error(_("unknown non-ascii option in string: `%s'"),
//!                               ctx.argv[0]);
//!                 usage_with_options(usagestr, options);
//! ```
//!
//! Only `PARSE_OPT_UNKNOWN` prints the usage block. A `PARSE_OPT_ERROR` — which
//! is what `get_arg()` (parse-options.c:60) and `do_get_value()`
//! (parse-options.c:133-143) return — prints one `error:` line and nothing else,
//! and both exit 129. This port kept routing the second shape into the first,
//! which is a visible difference on every command line that gets an option
//! slightly wrong: one line versus forty.
//!
//! Three rules are pinned here, each measured against stock git 2.55.0.
//!
//! 1. **`--<option>=<value>` the entry cannot take** is `do_get_value()`'s
//!    `takes no value`: bare line, no block, 129. It applies to *any* option in
//!    the `--no-` sense, whatever that option does with values, and to a
//!    `PARSE_OPT_NOARG` entry in either sense.
//! 2. **The name in that line is `optname()`'s** (parse-options.c:30-45), which
//!    reads `opt->long_name` — the **table's** spelling, not the user's. So an
//!    abbreviation is reported expanded, and an entry the table spells
//!    `no-verify` is reported `no-no-verify` when it is reached unset.
//! 3. **An unknown short option is named by one character**, `*ctx->opt`,
//!    however many were clustered behind it — and by the whole token only when
//!    that character is not ASCII.
//!
//! Separately, git.c:474-476 demotes a command's `RUN_SETUP` for a lone `-h`:
//!
//! ```c
//!         if (argc == 2 && !strcmp(argv[1], "-h"))
//!                 /* demote to GENTLY to allow 'git cmd -h' outside repo */
//!                 p->option &= ~RUN_SETUP;
//! ```
//!
//! so `git <cmd> -h` answers with the usage block **outside** a repository too,
//! on stdout at 129. Verbs in this port that opened the repository first
//! answered `fatal: not a git repository` at 128 instead.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One invocation under a pinned, isolated environment: no system or global
/// config, a fixed locale, and `HOME` inside the fixture so nothing on the
/// developer's machine can reach the parse.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap()
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-optshape-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// An initialized repository with one commit, so every verb below gets as far
/// as its own option parse rather than stopping on an empty history.
fn repo(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    for args in [
        &["init", "-q", "."][..],
        &["config", "user.name", "A U Thor"],
        &["config", "user.email", "author@example.com"],
    ] {
        let out = run(&dir, args);
        assert!(out.status.success(), "setup `git {args:?}` failed");
    }
    std::fs::write(dir.join("f.txt"), "one\n").unwrap();
    for args in [&["add", "f.txt"][..], &["commit", "-qm", "one"]] {
        let out = run(&dir, args);
        assert!(out.status.success(), "setup `git {args:?}` failed");
    }
    dir
}

/// A directory that is not inside any repository, for the `-h` demotion.
fn bare_dir(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    assert!(
        !dir.join(".git").exists(),
        "fixture must not be a repository"
    );
    dir
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The `PARSE_OPT_ERROR` shape in full: the exact line, nothing else on either
/// stream, and 129.
#[track_caller]
fn assert_bare_error(out: &Output, line: &str, what: &str) {
    assert_eq!(stderr(out), format!("{line}\n"), "stderr for {what}");
    assert_eq!(stdout(out), "", "stdout for {what}");
    assert_eq!(out.status.code(), Some(129), "exit for {what}");
}

/// `--<noarg>=<value>`, across the verbs whose refusal used to be their own
/// `unknown option` arm — which prints the block and quotes the `=value` too.
///
/// Every expectation is stock git 2.55.0's own stderr for the same command line
/// in the same fixture.
#[test]
fn noarg_long_given_a_value_is_one_line_and_no_block() {
    let dir = repo("noarg");
    for (args, name) in [
        (&["add", "--dry-run=value"][..], "dry-run"),
        (&["status", "--verbose=value"][..], "verbose"),
        (&["commit", "--quiet=value"][..], "quiet"),
        (&["tag", "--list=value"][..], "list"),
        (&["checkout", "--guess=value"][..], "guess"),
        (&["restore", "--staged=value"][..], "staged"),
        (&["remote", "--verbose=value"][..], "verbose"),
        (&["ls-files", "--deleted=value"][..], "deleted"),
        (&["prune", "--dry-run=value"][..], "dry-run"),
        (&["show-branch", "--all=value"][..], "all"),
        (&["for-each-repo", "--keep-going=value"][..], "keep-going"),
        (&["describe", "--contains=value"][..], "contains"),
        (&["init", "--bare=value"][..], "bare"),
        (&["mktree", "--missing=value"][..], "missing"),
        (&["check-mailmap", "--stdin=value"][..], "stdin"),
    ] {
        let out = run(&dir, args);
        assert_bare_error(
            &out,
            &format!("error: option `{name}' takes no value"),
            &format!("git {}", args.join(" ")),
        );
    }
}

/// The first of `do_get_value()`'s two refusals (parse-options.c:133-134):
///
/// ```c
///         if (unset && p->opt)
///                 return error(_("%s takes no value"), optname(opt, flags));
/// ```
///
/// `unset` alone decides it, so an option that *does* take a value still
/// refuses one in its `--no-` spelling. A port that tested only "is this entry
/// `PARSE_OPT_NOARG`" let these through to the command, which then acted on a
/// value git never delivered.
#[test]
fn negated_spelling_refuses_a_value_even_when_the_option_takes_one() {
    let dir = repo("negated");
    for (args, name) in [
        (
            &["add", "--no-pathspec-from-file=x"][..],
            "no-pathspec-from-file",
        ),
        (&["ls-files", "--no-with-tree=x"][..], "no-with-tree"),
        (&["commit", "--no-gpg-sign=x"][..], "no-gpg-sign"),
        (&["restore", "--no-source=x"][..], "no-source"),
    ] {
        let out = run(&dir, args);
        assert_bare_error(
            &out,
            &format!("error: option `{name}' takes no value"),
            &format!("git {}", args.join(" ")),
        );
    }
}

/// `optname()` reads `opt->long_name`, so the name in the line is the table's
/// and not the user's — twice over.
///
/// An abbreviation comes back expanded, and an entry git spells `no-verify`
/// gains a *second* `no-` when it is reached in the unset sense, because the
/// `skip_prefix(long_name, "no-", &long_name)` at parse-options.c:243 advanced
/// only a local while `optname()` went back to the struct field. Both lines
/// below are stock git 2.55.0's.
#[test]
fn the_name_reported_is_the_tables_spelling_not_the_typed_one() {
    let dir = repo("optname");

    let out = run(&dir, &["add", "--dry=x"]);
    assert_bare_error(&out, "error: option `dry-run' takes no value", "git add --dry=x");

    let out = run(&dir, &["commit", "--no-verify=x"]);
    assert_bare_error(
        &out,
        "error: option `no-verify' takes no value",
        "git commit --no-verify=x",
    );

    let out = run(&dir, &["commit", "--verify=x"]);
    assert_bare_error(
        &out,
        "error: option `no-no-verify' takes no value",
        "git commit --verify=x",
    );
}

/// `PARSE_OPT_NONEG` is the boundary of the rule above: `parse_long_opt()` skips
/// such an entry outright when the token was negated
/// (parse-options.c:248-249, `if (((flags ^ opt_flags) & OPT_UNSET) && !allow_unset) continue;`),
/// so `--no-<opt>=x` there is an *unknown option* — block and all — and not a
/// `takes no value`. `git grep`'s `--max-depth` is `OPT_INTEGER`, whose macro
/// sets `PARSE_OPT_NONEG`. Measured on stock git 2.55.0.
#[test]
fn a_noneg_entry_negated_is_unknown_rather_than_valueless() {
    let dir = repo("noneg");
    let out = run(&dir, &["grep", "--no-max-depth=x"]);
    assert_eq!(out.status.code(), Some(129));
    let err = stderr(&out);
    assert!(
        err.starts_with("error: unknown option `no-max-depth=x'\n"),
        "stderr was {err:?}"
    );
    assert!(
        err.contains("usage: git grep [<options>] [-e] <pattern> [<rev>...] [[--] <path>...]"),
        "stderr was {err:?}"
    );
}

/// A name no entry claims keeps the *other* shape, so this is a discrimination
/// test and not a duplicate: `unknown option` quotes the `=value` too and is
/// followed by the usage block, on stderr, at 129.
#[test]
fn an_unknown_long_option_still_prints_the_block() {
    let dir = repo("unknown-long");
    let out = run(&dir, &["add", "--zz-bogus=value"]);
    assert_eq!(out.status.code(), Some(129));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(
        err.starts_with("error: unknown option `zz-bogus=value'\n"),
        "stderr was {err:?}"
    );
    assert!(
        err.contains("usage: git add [<options>] [--] <pathspec>..."),
        "the usage block is part of this shape; stderr was {err:?}"
    );
}

/// `error(_("unknown switch `%c'"), *ctx.opt)` — one character, whatever is
/// clustered behind it. Three verbs printed the whole remainder of the token.
#[test]
fn an_unknown_short_switch_is_named_by_one_character() {
    let dir = repo("short-cluster");
    for verb in ["bundle", "stash", "verify-tag"] {
        let out = run(&dir, &[verb, "-7q"]);
        assert_eq!(out.status.code(), Some(129), "exit for git {verb} -7q");
        let err = stderr(&out);
        assert!(
            err.starts_with("error: unknown switch `7'\n"),
            "git {verb} -7q named more than `7': {err:?}"
        );
        assert!(
            err.contains("usage: git "),
            "`unknown switch' is the shape that keeps the block; stderr was {err:?}"
        );
    }
}

/// The third arm of the same `switch`: a non-ASCII first character is reported
/// as the **whole token**, dashes included, and never as a character.
#[test]
fn a_non_ascii_short_option_is_named_by_the_whole_token() {
    let dir = repo("non-ascii");
    let out = run(&dir, &["bundle", "-é"]);
    assert_eq!(out.status.code(), Some(129));
    assert!(
        stderr(&out).starts_with("error: unknown non-ascii option in string: `-é'\n"),
        "stderr was {:?}",
        stderr(&out)
    );
}

/// git.c:474-476's demotion: a lone `-h` is answered before the repository is
/// ever looked for, so it works outside one — usage block on **stdout**, no
/// `error:` line, 129.
///
/// The first line of each block is stock git 2.55.0's, and `git <verb> -h`
/// prints the same block inside and outside a repository for every verb here
/// (measured on 2.55.0), which is why the two are not asserted separately.
#[test]
fn a_lone_dash_h_is_answered_outside_a_repository() {
    let dir = bare_dir("help-outside");
    for (verb, first_line) in [
        ("add", "usage: git add [<options>] [--] <pathspec>..."),
        ("stage", "usage: git add [<options>] [--] <pathspec>..."),
        ("checkout", "usage: git checkout [<options>] <branch>"),
        ("cherry", "usage: git cherry [-v] [<upstream> [<head> [<limit>]]]"),
        (
            "for-each-ref",
            "usage: git for-each-ref [--count=<count>] [--shell|--perl|--python|--tcl]",
        ),
        (
            "format-patch",
            "usage: git format-patch [<options>] [<since> | <revision-range>]",
        ),
        ("log", "usage: git log [<options>] [<revision-range>] [[--] <path>...]"),
        (
            "whatchanged",
            "usage: git log [<options>] [<revision-range>] [[--] <path>...]",
        ),
        ("rebase", "usage: git rebase [-i] [options] [--exec <cmd>]"),
        ("reset", "usage: git reset [--mixed | --soft | --hard | --merge | --keep] [-q] [<commit>]"),
        (
            "show-branch",
            "usage: git show-branch [-a | --all] [-r | --remotes] [--topo-order | --date-order]",
        ),
        ("update-index", "usage: git update-index [<options>] [--] [<file>...]"),
    ] {
        let out = run(&dir, &[verb, "-h"]);
        assert_eq!(out.status.code(), Some(129), "exit for git {verb} -h");
        assert_eq!(
            stderr(&out),
            "",
            "asking for help is not an error, so nothing goes to stderr (git {verb} -h)"
        );
        let first = stdout(&out).lines().next().unwrap_or_default().to_string();
        assert!(
            first.starts_with(first_line),
            "git {verb} -h outside a repository printed {first:?}"
        );
    }
}

/// The demotion is `argc == 2` only: `-h` with anything beside it is an ordinary
/// argv that still needs a repository, and outside one the verb dies in setup.
/// Without this the gate above would be a licence to answer help for any command
/// line that happens to contain `-h`.
#[test]
fn the_demotion_does_not_extend_past_a_lone_dash_h() {
    let dir = bare_dir("help-not-alone");
    let out = run(&dir, &["update-index", "--refresh", "-h"]);
    assert_eq!(out.status.code(), Some(128), "exit for git update-index --refresh -h");
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).starts_with("fatal: not a git repository"),
        "stderr was {:?}",
        stderr(&out)
    );
}

/// `get_arg()`'s own `PARSE_OPT_ERROR` (parse-options.c:59-60) shares the shape
/// but not the wording, and it must stay distinguishable: a value-taking option
/// with nothing left in argv is `requires a value`, bare, 129.
#[test]
fn a_missing_value_is_the_same_shape_with_the_other_wording() {
    let dir = repo("missing-value");
    for (args, name) in [
        (&["hash-object", "--path"][..], "path"),
        (&["ls-files", "--exclude-from"][..], "exclude-from"),
        (&["add", "--pathspec-from-file"][..], "pathspec-from-file"),
        (&["check-attr", "--source"][..], "source"),
        (&["ls-tree", "--format"][..], "format"),
        (&["replace", "--format"][..], "format"),
        (&["column", "--command"][..], "command"),
        (&["verify-tag", "--format"][..], "format"),
        (
            &["bugreport", "--output-directory"][..],
            "output-directory",
        ),
    ] {
        let out = run(&dir, args);
        assert_bare_error(
            &out,
            &format!("error: option `{name}' requires a value"),
            &format!("git {}", args.join(" ")),
        );
    }
}

/// `parse_options_step()` consumes a lone `--` before any table lookup:
///
/// ```c
///                 if (!arg[2] /* "--" */) {
///                         if (!(ctx->flags & PARSE_OPT_KEEP_DASHDASH)) {
///                                 ctx->argc--;
///                                 ctx->argv++;
///                         }
///                         break;
///                 }
/// ```
///
/// so it ends option parsing and is never looked up. Eight verbs here stripped
/// its two dashes, resolved the empty remainder against their table, found
/// nothing and reported ``error: unknown option `''`` — a name the user cannot
/// have typed. For the four sub-command verbs what is left afterwards is a
/// command line with no sub-command word, which is `PARSE_OPT_SUBCOMMAND`'s own
/// refusal; `remote` has an optional sub-command and simply lists. Every
/// expectation is stock git 2.55.0's.
#[test]
fn a_lone_dashdash_ends_option_parsing_rather_than_being_looked_up() {
    let dir = repo("dashdash");
    for verb in ["refs", "repo", "bundle", "hook", "history"] {
        let out = run(&dir, &[verb, "--"]);
        assert_eq!(out.status.code(), Some(129), "exit for git {verb} --");
        let err = stderr(&out);
        assert!(
            err.starts_with("error: need a subcommand\n"),
            "git {verb} -- said {err:?}"
        );
    }
    // `cmd_remote`'s sub-command is optional, so `--` leaves an ordinary
    // `git remote` — which succeeds and prints nothing in a repository with no
    // remotes.
    let out = run(&dir, &["remote", "--"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
}

/// `register_abbrev()` (parse-options.c:497) keeps the last two candidates, and
/// a prefix that names more than one entry is `ambiguous option:` — the one
/// refusal that splits its halves across the streams. `parse_long_opt()` reports
/// the reason with `error()` on stderr and returns `PARSE_OPT_HELP`, which
/// `parse_options_step()` routes to `usage_with_options_internal(...,
/// USAGE_TO_STDOUT)`, so the block lands on **stdout** at 129.
///
/// Three verbs wrote the block to stderr or dropped it, and `mktree` answered
/// the ambiguity with `unknown option` because its private resolver reported a
/// tie as "no match". Every line below is stock git 2.55.0's.
#[test]
fn an_ambiguous_abbreviation_puts_its_block_on_stdout() {
    let dir = repo("ambiguous");
    for (args, line) in [
        (
            &["check-mailmap", "--m"][..],
            "error: ambiguous option: m (could be --mailmap-file or --mailmap-blob)",
        ),
        (
            &["fsck", "--c"][..],
            "error: ambiguous option: c (could be --cache or --connectivity-only)",
        ),
        (
            &["hash-object", "--s"][..],
            "error: ambiguous option: s (could be --stdin or --stdin-paths)",
        ),
        (
            &["mktree", "--no"][..],
            "error: ambiguous option: no (could be --no-missing or --no-batch)",
        ),
    ] {
        let out = run(&dir, args);
        let what = args.join(" ");
        assert_eq!(out.status.code(), Some(129), "exit for git {what}");
        assert_eq!(stderr(&out), format!("{line}\n"), "stderr for git {what}");
        assert!(
            stdout(&out).starts_with("usage: git "),
            "the block belongs on stdout for git {what}; stdout was {:?}",
            stdout(&out)
        );
    }
}
