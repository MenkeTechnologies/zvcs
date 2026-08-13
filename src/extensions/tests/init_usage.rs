//! `git init` / `git init-db` argument-handling contract: which refusal wins,
//! what it prints, and what it leaves on disk.
//!
//! `builtin/init-db.c` reaches its refusals in a fixed order, and they are not
//! all the same kind of refusal. Getting the *order* wrong or the *kind* wrong
//! both show up as a wrong exit code, which callers branch on:
//!
//! ```text
//!   1. --separate-git-dir with --bare   die()    -> 128, `fatal: ...`
//!   2. more than one <directory>        usage()  -> 129, usage string alone
//!   3. unknown --object-format          die()    -> 128, `fatal: ...`
//!   4. unknown --ref-format             die()    -> 128, `fatal: ...`
//! ```
//!
//! and, ahead of all four, `parse_options` itself rejects an unknown option with
//! `usage_with_options()` -> 129 and the *reflowed* usage plus the option list.
//! So `git init` has two distinct 129 outputs, and which one you get says which
//! code path ran. Every expectation below is the observed stdout/stderr/exit of
//! stock git 2.55.0, so a drift in either direction fails here.
//!
//! No network, no stock git binary and no ambient config: each case runs the
//! built `git` in a fresh temp dir with `HOME` pinned inside it.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `init_db_usage[0]` from `builtin/init-db.c` behind the `"usage: "` prefix
/// `usage_builtin()` adds — what plain `usage()` prints, verbatim. Note the
/// nine-space continuation indent: this is the C string literal as written, not
/// the version `usage_with_options()` reflows.
const PLAIN_USAGE: &str = "\
usage: git init [-q | --quiet] [--bare] [--template=<template-directory>]
         [--separate-git-dir <git-dir>] [--object-format=<format>]
         [--ref-format=<format>]
         [-b <branch-name> | --initial-branch=<branch-name>]
         [--shared[=<permissions>]] [<directory>]
";

/// A finished `git` run: everything the parity contract compares.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    /// Every path created under the working directory, sorted, `/`-joined.
    tree: Vec<String>,
}

/// Run the built `git` with `args` in a private temp dir whose `HOME` is pinned
/// inside it, so no user or system config can reach the command.
fn run(tag: &str, args: &[&str]) -> Run {
    let root = std::env::temp_dir().join(format!(
        "zvcs-initusage-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let work = root.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    let out = Command::new(BIN)
        .args(args)
        .current_dir(&work)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // `git init --bare` with no operand initializes into the cwd; make sure
        // nothing outside `work` can be picked up as the repository instead.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();

    let run = Run {
        code: out.status.code().expect("git exited via signal"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        tree: tree(&work),
    };
    let _ = std::fs::remove_dir_all(&root);
    run
}

/// Every entry under `dir` (files and directories), relative and sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            out.push(
                path.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Assert a run refused with plain `usage()`: exit 129, the unreflowed usage
/// string alone on stderr (no option list, no `error:` line), nothing on stdout,
/// and — because the refusal precedes any directory creation — nothing on disk.
fn assert_plain_usage(tag: &str, args: &[&str]) {
    let r = run(tag, args);
    assert_eq!(r.code, 129, "{args:?} exit code");
    assert_eq!(r.stdout, "", "{args:?} stdout");
    assert_eq!(r.stderr, PLAIN_USAGE, "{args:?} stderr");
    assert!(r.tree.is_empty(), "{args:?} created {:?}", r.tree);
}

#[test]
fn two_directories_print_plain_usage_and_exit_129() {
    // `} else if (0 < argc) { usage(init_db_usage[0]); }` — the second operand
    // is a usage error, not a `die()`, so 129 and no `fatal:` prefix.
    assert_plain_usage("two-dirs", &["init", "a", "b"]);
}

#[test]
fn three_directories_print_plain_usage_and_exit_129() {
    assert_plain_usage("three-dirs", &["init", "a", "b", "c"]);
}

#[test]
fn operands_after_double_dash_still_count() {
    // `--` ends option parsing; everything after it is an operand, so this is
    // three directories, not one directory and two flags.
    assert_plain_usage("dashdash", &["init", "--", "newrepo", "a", "b"]);
}

#[test]
fn repeated_flags_do_not_excuse_extra_operands() {
    // Repeated and interleaved flags are all consumed by parse_options; what is
    // left is two operands, which is still one too many.
    assert_plain_usage(
        "repeated-flags",
        &["init", "--quiet", "-q", "-q", "--bare", "newrepo", "nested/dir"],
    );
}

#[test]
fn extra_operands_outrank_an_unknown_object_format() {
    // The operand count is judged before `if (object_format)`, so the usage
    // error wins over `fatal: unknown hash algorithm 'bogus'`.
    assert_plain_usage("fmt-then-dirs", &["init", "--object-format=bogus", "a", "b"]);
}

#[test]
fn extra_operands_outrank_an_unknown_ref_format() {
    assert_plain_usage("ref-then-dirs", &["init", "--ref-format=bogus", "a", "b"]);
}

#[test]
fn separate_git_dir_with_bare_outranks_extra_operands() {
    // This check sits above the operand count in `cmd_init_db`, so the answer is
    // `die()` (128), not `usage()` (129) — the one ordering that goes the other
    // way, and the reason the checks cannot simply be sorted by severity.
    let r = run("sgd-bare-dirs", &["init", "--separate-git-dir=x", "--bare", "a", "b"]);
    assert_eq!(r.code, 128, "exit code");
    assert_eq!(r.stdout, "");
    assert_eq!(
        r.stderr,
        "fatal: options '--separate-git-dir' and '--bare' cannot be used together\n"
    );
}

#[test]
fn unknown_option_outranks_extra_operands_and_prints_the_option_list() {
    // parse_options refuses first, and through `usage_with_options()` — the
    // *other* 129 output: an `error:` line, then the reflowed usage (sixteen-space
    // continuation indent) followed by the option list. Distinguishing the two
    // 129s is the point; asserting only the exit code would let them swap.
    for (tag, args) in [
        ("unknown-last", &["init", "a", "b", "--frobnicate"]),
        ("unknown-first", &["init", "--frobnicate", "a", "b"]),
    ] {
        let r = run(tag, args);
        assert_eq!(r.code, 129, "{args:?} exit code");
        assert_eq!(r.stdout, "", "{args:?} stdout");
        assert!(
            r.stderr.starts_with("error: unknown option `frobnicate'\n"),
            "{args:?} stderr began {:?}",
            r.stderr.lines().next()
        );
        assert!(
            r.stderr.contains("\n                [--separate-git-dir <git-dir>]"),
            "{args:?} stderr is not the reflowed usage"
        );
        assert!(
            r.stderr.contains("    --[no-]bare           create a bare repository\n"),
            "{args:?} stderr carries no option list"
        );
    }
}

#[test]
fn init_db_shares_the_contract_and_still_says_git_init() {
    // `init-db` is the same builtin under its historical name, so it must refuse
    // identically — including the usage text naming `git init`, which is what
    // stock prints because both names resolve to `cmd_init_db`.
    assert_plain_usage("initdb-two-dirs", &["init-db", "a", "b"]);
    assert_plain_usage("initdb-dashdash", &["init-db", "--", "newrepo", "a", "b"]);

    let r = run("initdb-unknown-opt", &["init-db", "--frobnicate", "a"]);
    assert_eq!(r.code, 129);
    assert!(r.stderr.starts_with("error: unknown option `frobnicate'\n"));
}

#[test]
fn one_directory_is_accepted() {
    // The guard rejects the *second* operand only; a lone directory, with or
    // without `--`, is the ordinary invocation and must still work.
    for (tag, args, dir) in [
        ("one-dir", vec!["init", "-q", "newrepo"], "newrepo"),
        ("one-dir-dashdash", vec!["init", "-q", "--", "newrepo"], "newrepo"),
        ("one-dir-initdb", vec!["init-db", "-q", "newrepo"], "newrepo"),
    ] {
        let r = run(tag, &args);
        assert_eq!(r.code, 0, "{args:?} exit code, stderr={:?}", r.stderr);
        assert_eq!(r.stdout, "", "{args:?} stdout under -q");
        assert!(
            r.tree.contains(&format!("{dir}/.git/HEAD").replace('/', std::path::MAIN_SEPARATOR_STR)),
            "{args:?} created no repository: {:?}",
            r.tree
        );
    }
}

#[test]
fn a_missing_leading_directory_is_created_for_bare_and_non_bare() {
    // git `chdir()`s into the operand and, on failure, creates it with
    // `safe_create_leading_directories_const()` + `mkdir()` before retrying — so
    // `nested/` need not exist. Both repository kinds go through that same code,
    // which is why `--bare` must not be the one that fails.
    for (tag, args, head) in [
        ("mkdir-nonbare", vec!["init", "-q", "nested/dir"], "nested/dir/.git/HEAD"),
        ("mkdir-bare", vec!["init", "-q", "--bare", "nested/dir"], "nested/dir/HEAD"),
        ("mkdir-deep", vec!["init", "-q", "a/b/c/d"], "a/b/c/d/.git/HEAD"),
    ] {
        let r = run(tag, &args);
        assert_eq!(r.code, 0, "{args:?} exit code, stderr={:?}", r.stderr);
        assert!(
            r.tree.contains(&head.replace('/', std::path::MAIN_SEPARATOR_STR)),
            "{args:?} left no {head}: {:?}",
            r.tree
        );
    }
}

#[test]
fn an_invalid_initial_branch_name_dies_without_writing_head() {
    // `create_reference_database()` (`setup.c`) validates `refs/heads/<name>`
    // with `check_refname_format()` and `die()`s — 128, not this port's own
    // exit 1 — after the skeleton exists but before `HEAD` is symref'd. The
    // absent `HEAD` is the load-bearing half: it is what tells a later `git`
    // that the directory is not a usable repository, and stock leaves it absent.
    for (tag, args, git_dir) in [
        ("badbranch-plain", vec!["init", "-q", "--initial-branch=\t"], ".git"),
        ("badbranch-bare", vec!["init", "-q", "--bare", "--initial-branch=\t"], ""),
        ("badbranch-initdb", vec!["init-db", "-q", "--initial-branch=\t"], ".git"),
    ] {
        let r = run(tag, &args);
        assert_eq!(r.code, 128, "{args:?} exit code");
        assert_eq!(
            r.stderr, "fatal: invalid initial branch name: '\t'\n",
            "{args:?} stderr"
        );

        let prefix: PathBuf = if git_dir.is_empty() { PathBuf::new() } else { PathBuf::from(git_dir) };
        let joined = |name: &str| {
            prefix
                .join(name)
                .to_string_lossy()
                .into_owned()
        };
        assert!(
            !r.tree.contains(&joined("HEAD")),
            "{args:?} wrote a HEAD stock git never writes: {:?}",
            r.tree
        );
        // The skeleton git *does* leave behind is still there — the port must
        // not "clean up" more than git does.
        assert!(
            r.tree.contains(&joined("config")),
            "{args:?} left no config: {:?}",
            r.tree
        );
    }
}

#[test]
fn a_valid_initial_branch_name_still_writes_head() {
    // Guard against the validation above being too strict: the names the fuzzer
    // reaches for around the invalid one are all legal refnames.
    for name in ["trunk", "0", "v1", "false", "999999999", "feature/x"] {
        let r = run("goodbranch", &["init", "-q", &format!("--initial-branch={name}")]);
        assert_eq!(r.code, 0, "--initial-branch={name} exit, stderr={:?}", r.stderr);
        assert!(
            r.tree.contains(&PathBuf::from(".git").join("HEAD").to_string_lossy().into_owned()),
            "--initial-branch={name} wrote no HEAD"
        );
    }
}
