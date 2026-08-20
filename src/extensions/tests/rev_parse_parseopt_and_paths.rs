//! `git rev-parse`'s non-revision modes: `--parseopt`, `--sq-quote`, the
//! repository-path queries (`--git-path`, `--resolve-git-dir`, `--local-env-vars`,
//! `--shared-index-path`, `--path-format=`), `--disambiguate=` and the four
//! `--since`/`--until` date rewrites.
//!
//! Every expectation below is bytes captured from stock git 2.55.0 on a fixture
//! this file builds, so nothing depends on the developer's config: the runs pin
//! `GIT_CONFIG_NOSYSTEM`, point the global and system files at `/dev/null`, and
//! drop `GIT_DIR`/`GIT_WORK_TREE`.
//!
//! `--parseopt` is what every `git-*.sh`-style script uses to parse its own
//! options, so the shapes asserted here are the ones a caller actually evaluates:
//! the `set --` line, the `cat <<\EOF` heredoc `-h` prints, and the `error:` +
//! usage block an unknown option prints on stderr at 129.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The option spec `--parseopt` reads on stdin. Every construct the spec grammar
/// has is in it: a short+long pair, a bare long, a required argument, a named
/// argument hint, an optional argument, a group heading and a short-only option.
const SPEC: &str = "some-command [<options>] <args>...

some-command does foo and bar!
--
h,help    show the help

foo       some nifty option --foo
bar=      some cool option --bar with an argument
baz=arg   another cool option --baz with a named argument
qux?path  qux may take a path argument but has meaning by itself

  An option group Header
C?        option C with an optional argument";

fn run(dir: &Path, args: &[&str]) -> Output {
    run_with_stdin(dir, args, "")
}

fn run_with_stdin(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_GRAFT_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn zvcs git");
    {
        use std::io::Write;
        child.stdin.as_mut().expect("stdin").write_all(stdin.as_bytes()).expect("write spec");
    }
    child.wait_with_output().expect("run zvcs git")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// A repository with three commits on `main`, so `--disambiguate=` has objects to
/// list and the date rewrites have a repository to open.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-rpopt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir fixture");
    let repo = root.canonicalize().expect("canonicalize").join("repo");
    std::fs::create_dir_all(repo.join("sub")).expect("mkdir repo");
    assert!(run(&repo, &["init", "-q", "-b", "main"]).status.success(), "init");
    std::fs::write(repo.join("a.txt"), "hello\n").expect("write");
    assert!(run(&repo, &["add", "a.txt"]).status.success(), "add");
    let commit = Command::new(BIN)
        .args(["commit", "-q", "-m", "one"])
        .current_dir(&repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .output()
        .expect("commit");
    assert!(commit.status.success(), "commit: {}", String::from_utf8_lossy(&commit.stderr));
    repo
}

// ---------------------------------------------------------------------------
// --parseopt
// ---------------------------------------------------------------------------

#[test]
fn parseopt_emits_the_set_line_stock_git_emits() {
    let dir = fixture("po-set");
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "--foo", "--bar=b", "arg"], SPEC);
    assert_eq!(code(&o), 0, "{}", err(&o));
    // A long option with a value is dumped as ` --bar 'b'` — separated, because
    // `--stuck-long` was not given — and the operands follow a literal ` --`.
    assert_eq!(out(&o), "set -- --foo --bar 'b' -- 'arg'\n");
}

#[test]
fn parseopt_stuck_long_glues_the_value_to_the_long_name() {
    let dir = fixture("po-stuck");
    let o = run_with_stdin(
        &dir,
        &["rev-parse", "--parseopt", "--stuck-long", "--", "--foo", "--bar=b", "--qux", "arg"],
        SPEC,
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "set -- --foo --bar='b' --qux -- 'arg'\n");
}

#[test]
fn parseopt_short_cluster_and_optional_argument() {
    let dir = fixture("po-short");
    // `-C` on its own takes no value (it is `PARSE_OPT_OPTARG`), while `-Cval`
    // sticks one to it. Both dump under the short name.
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "-C", "-Cval"], SPEC);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "set -- -C -C 'val' --\n");
}

#[test]
fn parseopt_keep_dashdash_and_stop_at_non_option() {
    let dir = fixture("po-flags");
    let o = run_with_stdin(
        &dir,
        &["rev-parse", "--parseopt", "--keep-dashdash", "--", "--foo", "--", "arg"],
        SPEC,
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "set -- --foo -- '--' 'arg'\n");

    let o = run_with_stdin(
        &dir,
        &["rev-parse", "--parseopt", "--stop-at-non-option", "--", "--foo", "arg", "--bar=b"],
        SPEC,
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "set -- --foo -- 'arg' '--bar=b'\n");
}

#[test]
fn parseopt_negation_and_unique_abbreviation() {
    let dir = fixture("po-abbrev");
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "--no-foo"], SPEC);
    assert_eq!(out(&o), "set -- --no-foo --\n", "{}", err(&o));

    // `--f` resolves to `--foo` because nothing else starts with it.
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "--f"], SPEC);
    assert_eq!(out(&o), "set -- --foo --\n", "{}", err(&o));

    // `--b` is `--bar` or `--baz`, and the ambiguity is reported on stderr while
    // the usage block still goes to stdout inside its heredoc.
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "--b"], SPEC);
    assert_eq!(code(&o), 129);
    assert_eq!(err(&o), "error: ambiguous option: b (could be --bar or --baz)\n");
    assert!(out(&o).starts_with("cat <<\\EOF\n"), "{:?}", out(&o));
}

#[test]
fn parseopt_help_prints_the_spec_usage_in_a_heredoc() {
    let dir = fixture("po-help");
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "-h"], SPEC);
    assert_eq!(code(&o), 129);
    assert_eq!(err(&o), "");
    // Byte for byte from stock 2.55.0: the `cat <<\EOF` wrapper `PARSE_OPT_SHELL_EVAL`
    // adds, the `usage: ` prefix, the four-space indent an empty usage line switches
    // on, and the option rows padded to column 26.
    assert_eq!(
        out(&o),
        "cat <<\\EOF\n\
         usage: some-command [<options>] <args>...\n\
         \n\
         \x20   some-command does foo and bar!\n\
         \n\
         \x20   -h, --[no-]help       show the help\n\
         \x20   --[no-]foo            some nifty option --foo\n\
         \x20   --[no-]bar ...        some cool option --bar with an argument\n\
         \x20   --[no-]baz <arg>      another cool option --baz with a named argument\n\
         \x20   --[no-]qux[=<path>]   qux may take a path argument but has meaning by itself\n\
         \n\
         An option group Header\n\
         \x20   -C[...]               option C with an optional argument\n\
         \n\
         EOF\n"
    );
}

#[test]
fn parseopt_unknown_option_reports_on_stderr_without_the_heredoc() {
    let dir = fixture("po-unknown");
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "--unknown"], SPEC);
    assert_eq!(code(&o), 129);
    assert_eq!(out(&o), "", "the error path never writes the heredoc");
    assert!(err(&o).starts_with("error: unknown option `unknown'\nusage: some-command"), "{:?}", err(&o));
}

#[test]
fn parseopt_missing_value_is_an_error_line_alone() {
    let dir = fixture("po-noval");
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "--bar"], SPEC);
    assert_eq!(code(&o), 129);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "error: option `bar' requires a value\n");
}

#[test]
fn parseopt_own_options_and_malformed_specs() {
    let dir = fixture("po-own");
    // An unknown option *before* the `--` is parsed against `--parseopt`'s own
    // three-entry table, whose usage block names only those three.
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--badopt"], SPEC);
    assert_eq!(code(&o), 129);
    assert_eq!(
        err(&o),
        "error: unknown option `badopt'\n\
         usage: git rev-parse --parseopt [<options>] -- [<args>...]\n\
         \n\
         \x20   --[no-]keep-dashdash  keep the `--` passed as an arg\n\
         \x20   --[no-]stop-at-non-option\n\
         \x20                         stop parsing after the first non-option argument\n\
         \x20   --[no-]stuck-long     output in stuck long form\n\
         \n"
    );

    // No `--` operand at all: the same usage block, with no `error:` line.
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt"], SPEC);
    assert_eq!(code(&o), 129);
    assert!(err(&o).starts_with("usage: git rev-parse --parseopt"), "{:?}", err(&o));

    // Stdin that ends before the `--` separator, and one that has no usage line
    // in front of it.
    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "--x"], "usage");
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: premature end of input\n");

    let o = run_with_stdin(&dir, &["rev-parse", "--parseopt", "--", "--x"], "--");
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: no usage string given before the `--' separator\n");
}

// ---------------------------------------------------------------------------
// --sq-quote
// ---------------------------------------------------------------------------

#[test]
fn sq_quote_wraps_every_argument_and_escapes_quote_and_bang() {
    let dir = fixture("sq");
    let o = run(&dir, &["rev-parse", "--sq-quote", "a", "b"]);
    // Every element is preceded by a space, the first one included.
    assert_eq!(out(&o), " 'a' 'b'\n");

    let o = run(&dir, &["rev-parse", "--sq-quote", "a'b", "c d", "$x", "!"]);
    assert_eq!(out(&o), " 'a'\\''b' 'c d' '$x' ''\\!''\n");

    // No arguments at all is the bare newline `printf("%s\n", buf.buf)` writes.
    let o = run(&dir, &["rev-parse", "--sq-quote"]);
    assert_eq!(out(&o), "\n");
    assert_eq!(code(&o), 0);
}

// ---------------------------------------------------------------------------
// repository path queries
// ---------------------------------------------------------------------------

#[test]
fn local_env_vars_lists_gits_repository_local_environment() {
    let dir = fixture("env");
    let o = run(&dir, &["rev-parse", "--local-env-vars"]);
    assert_eq!(code(&o), 0);
    assert_eq!(
        out(&o),
        "GIT_ALTERNATE_OBJECT_DIRECTORIES\nGIT_CONFIG\nGIT_CONFIG_PARAMETERS\n\
         GIT_CONFIG_COUNT\nGIT_OBJECT_DIRECTORY\nGIT_DIR\nGIT_WORK_TREE\n\
         GIT_IMPLICIT_WORK_TREE\nGIT_GRAFT_FILE\nGIT_INDEX_FILE\n\
         GIT_NO_REPLACE_OBJECTS\nGIT_REPLACE_REF_BASE\nGIT_PREFIX\n\
         GIT_SHALLOW_FILE\nGIT_COMMON_DIR\n"
    );

    // It answers outside a repository too: `cmd_rev_parse()` handles it before
    // `setup_git_directory()` runs.
    let outside = dir.parent().expect("parent");
    let o = run(outside, &["rev-parse", "--local-env-vars"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).starts_with("GIT_ALTERNATE_OBJECT_DIRECTORIES\n"));
}

#[test]
fn git_path_renders_the_stored_git_directory_and_honours_overrides() {
    let dir = fixture("gitpath");
    assert_eq!(out(&run(&dir, &["rev-parse", "--git-path", "objects"])), ".git/objects\n");
    assert_eq!(out(&run(&dir, &["rev-parse", "--git-path", "HEAD"])), ".git/HEAD\n");
    // The name is pasted on verbatim: no normalization, no rejection.
    assert_eq!(out(&run(&dir, &["rev-parse", "--git-path", "foo/../bar"])), ".git/foo/../bar\n");
    // `--git-path=<name>` is a different token entirely — `cmd_rev_parse()` compares
    // the whole string — so it falls through and is echoed as an unknown flag.
    assert_eq!(out(&run(&dir, &["rev-parse", "--git-path=HEAD"])), "--git-path=HEAD\n");

    let o = run(&dir, &["rev-parse", "--git-path"]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: --git-path requires an argument\n");

    // `adjust_git_path()`'s relocations: `$GIT_INDEX_FILE` and `$GIT_GRAFT_FILE`
    // replace the whole path rather than a component of it.
    let o = Command::new(BIN)
        .args(["rev-parse", "--git-path", "index"])
        .current_dir(&dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_INDEX_FILE", "/tmp/zvcs-test.idx")
        .output()
        .expect("run");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "/tmp/zvcs-test.idx\n");

    let o = Command::new(BIN)
        .args(["rev-parse", "--git-path", "info/grafts"])
        .current_dir(&dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_GRAFT_FILE", "/tmp/zvcs-test.grafts")
        .output()
        .expect("run");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "/tmp/zvcs-test.grafts\n");

    // `core.hooksPath` replaces the `hooks` component and keeps the rest.
    let o = run(&dir, &["-c", "core.hooksPath=/tmp/zvcs-hooks", "rev-parse", "--git-path", "hooks/pre-commit"]);
    assert_eq!(out(&o), "/tmp/zvcs-hooks/pre-commit\n");
}

#[test]
fn git_path_from_a_subdirectory_is_relative_to_where_the_command_ran() {
    let dir = fixture("gitpath-sub");
    let sub = dir.join("sub");
    // `print_path(…, DEFAULT_RELATIVE_IF_SHARED)` measures the stored `.git`
    // against the prefix, so one directory down it climbs back out.
    assert_eq!(out(&run(&sub, &["rev-parse", "--git-path", "HEAD"])), "../.git/HEAD\n");
    assert_eq!(out(&run(&sub, &["rev-parse", "--git-common-dir"])), "../.git\n");
}

#[test]
fn resolve_git_dir_answers_for_a_directory_and_dies_for_anything_else() {
    let dir = fixture("resolve");
    // A real git directory answers with the string it was *given*, unchanged.
    assert_eq!(out(&run(&dir, &["rev-parse", "--resolve-git-dir", ".git"])), ".git\n");

    let o = run(&dir, &["rev-parse", "--resolve-git-dir", "nope"]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: not a gitdir 'nope'\n");

    // The work tree itself is not a git directory.
    let o = run(&dir, &["rev-parse", "--resolve-git-dir", "."]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: not a gitdir '.'\n");

    let o = run(&dir, &["rev-parse", "--resolve-git-dir"]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: --resolve-git-dir requires an argument\n");
}

#[test]
fn shared_index_path_prints_nothing_for_an_ordinary_index() {
    let dir = fixture("shared");
    let o = run(&dir, &["rev-parse", "--shared-index-path"]);
    assert_eq!(code(&o), 0);
    // Not an empty line — no bytes at all, because git's `if (split_index)` guards
    // the whole `print_path()` call.
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "");
}

#[test]
fn path_format_overrides_only_the_options_after_it() {
    let dir = fixture("pathfmt");
    let top = dir.canonicalize().expect("canonicalize");

    let o = run(&dir, &["rev-parse", "--path-format=absolute", "--git-dir"]);
    assert_eq!(out(&o), format!("{}\n", top.join(".git").display()));

    let o = run(&dir, &["rev-parse", "--path-format=relative", "--git-dir"]);
    assert_eq!(out(&o), ".git\n");

    // Scan state, so a query written *before* it keeps the default rendering.
    let o = run(&dir, &["rev-parse", "--git-dir", "--path-format=absolute", "--git-dir"]);
    assert_eq!(out(&o), format!(".git\n{}\n", top.join(".git").display()));

    let o = run(&dir, &["rev-parse", "--path-format=bogus", "--git-dir"]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: unknown argument to --path-format: bogus\n");

    // `opt_with_value()` accepts the bare spelling with a NULL value, which is a
    // different complaint from an empty one.
    let o = run(&dir, &["rev-parse", "--path-format"]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: --path-format requires an argument\n");

    let o = run(&dir, &["rev-parse", "--path-format="]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: unknown argument to --path-format: \n");
}

// ---------------------------------------------------------------------------
// --disambiguate= and the date rewrites
// ---------------------------------------------------------------------------

#[test]
fn disambiguate_lists_every_object_with_the_prefix() {
    let dir = fixture("disamb");
    let head = out(&run(&dir, &["rev-parse", "HEAD"])).trim_end().to_string();
    let prefix = &head[..4];

    let o = run(&dir, &["rev-parse", &format!("--disambiguate={prefix}")]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).lines().any(|l| l == head), "{:?} should list {head}", out(&o));

    // A prefix shorter than the two-character fanout selects nothing, and so does
    // one that is not hex at all — neither is an error.
    for arg in ["--disambiguate=", "--disambiguate=1", "--disambiguate=zz", "--disambiguate=GG"] {
        let o = run(&dir, &["rev-parse", arg]);
        assert_eq!(code(&o), 0, "{arg}");
        assert_eq!(out(&o), "", "{arg}");
    }

    // `show_abbrev()` is `show_rev()`, so `--verify`/`--short` count every match as
    // a revision — one match still prints, then the scan dies for want of exactly one.
    let o = run(&dir, &["rev-parse", "--short", &format!("--disambiguate={prefix}")]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: Needed a single revision\n");
    assert!(!out(&o).is_empty(), "the abbreviated name is printed before the die");
}

#[test]
fn since_and_until_rewrite_into_max_and_min_age() {
    let dir = fixture("dates");
    // An `@<epoch>` date is exact, so the rewrite can be pinned to the second.
    assert_eq!(
        out(&run(&dir, &["rev-parse", "--since=@1234567890"])),
        "--max-age=1234567890\n"
    );
    assert_eq!(out(&run(&dir, &["rev-parse", "--after=@1234567890"])), "--max-age=1234567890\n");
    assert_eq!(out(&run(&dir, &["rev-parse", "--until=@1234567890"])), "--min-age=1234567890\n");
    assert_eq!(out(&run(&dir, &["rev-parse", "--before=@1234567890"])), "--min-age=1234567890\n");

    // A rewrite prints at its own position, in among the revisions.
    let head = out(&run(&dir, &["rev-parse", "HEAD"]));
    let o = run(&dir, &["rev-parse", "HEAD", "--since=1970-01-01T00:16:40Z", "--until=@1234567890"]);
    assert_eq!(out(&o), format!("{}--max-age=1000\n--min-age=1234567890\n", head));

    // `approxidate()`, not `approxidate_careful()`: a date it cannot read is "now"
    // rather than an error, so this must not fail and must not print a flag-shaped
    // token other than the rewrite.
    let o = run(&dir, &["rev-parse", "--since=bogusdate"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).starts_with("--max-age="), "{:?}", out(&o));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let printed: i64 = out(&o).trim_end().trim_start_matches("--max-age=").parse().expect("epoch");
    assert!((now - printed).abs() < 120, "{printed} should be about now ({now})");

    // `show_datestring()` needs `DO_FLAGS`, which `--verify` clears — so the rewrite
    // disappears and the scan dies for want of a revision.
    let o = run(&dir, &["rev-parse", "--verify", "--since=@1234567890"]);
    assert_eq!(code(&o), 128);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "fatal: Needed a single revision\n");
}
