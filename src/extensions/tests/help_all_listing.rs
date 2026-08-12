//! `git help --all --no-verbose` — git's `list_commands()` form.
//!
//! This is the one `git help` listing built at runtime rather than printed from
//! a transcribed table, so it has three independent things to get wrong and one
//! honest divergence to pin down:
//!
//!   1. **The set of names.** Stock lists its builtin table plus the `git-*`
//!      files in `libexec/git-core`; this port lists the git-compat dispatch
//!      table plus whatever `git-*` sits in its own exec-path. The dispatch
//!      table is the source of truth and nothing may pad or trim it.
//!   2. **The layout.** `pretty_print_cmdnames()` is `print_columns()` with a
//!      two-space indent and two spaces of padding, columns forced on. It must
//!      go through the `git column` engine, not a second column formatter — so
//!      the block is compared against `git column` fed the same names.
//!   3. **The fall-through.** The non-verbose `--all` arm `break`s instead of
//!      returning, so `cmd_help` runs on into its no-topic tail: the real output
//!      carries two synopses and two trailers. Easy to "tidy away" by mistake.
//!
//! The heading names this installation's exec-path, which is by construction not
//! stock's `libexec/git-core`; the test pins it to `git --exec-path` so the two
//! answers can never drift apart, which is the only honest guarantee available.
//!
//! Every case runs with `PATH` pointed at an empty directory, so the `$PATH`
//! scan half of `load_command_list()` contributes nothing and the listing is the
//! dispatch table exactly — otherwise the result would depend on whatever `git-*`
//! helpers the build machine happens to have installed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Terminal width every case is rendered at, so the column count is fixed
/// regardless of the terminal the suite runs in.
const COLUMNS: &str = "80";

/// An isolated HOME plus an empty directory to use as `PATH`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-helpall-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let bin = root.join("emptybin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    (home, bin)
}

fn run(home: &Path, bin: &Path, args: &[&str]) -> Output {
    run_in(home, home, bin, args)
}

fn run_in(dir: &Path, home: &Path, bin: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("PATH", bin)
        .env("COLUMNS", COLUMNS)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("TERM", "dumb")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "exit {:?}", out.status.code());
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// The listing block: everything between the blank line after the heading and
/// the blank line that closes the table.
fn listing_block(text: &str) -> String {
    let after_heading = text
        .split_once("available git commands in '")
        .expect("heading present")
        .1;
    let after_blank = after_heading.split_once("'\n\n").expect("blank line after heading").1;
    let (block, _) = after_blank.split_once("\n\n").expect("blank line after table");
    format!("{block}\n")
}

fn names_in(block: &str) -> BTreeSet<String> {
    block.split_whitespace().map(str::to_string).collect()
}

/// The heading must name whatever `git --exec-path` reports. Stock uses one
/// `git_exec_path()` for both; a port that answered differently in the two
/// places would be describing an installation that does not exist.
#[test]
fn the_heading_names_the_exec_path_the_binary_reports() {
    let (home, bin) = fixture("execpath");
    let exec_path = stdout(&run(&home, &bin, &["--exec-path"]));
    let text = stdout(&run(&home, &bin, &["help", "--all", "--no-verbose"]));

    let expected = format!("available git commands in '{}'\n", exec_path.trim_end());
    assert!(
        text.contains(&expected),
        "heading missing or disagrees with --exec-path ({exec_path:?})"
    );
}

/// With nothing on `PATH`, the listing is `load_builtin_commands()` alone — the
/// git-compat dispatch table, every entry once. This is the assertion that
/// forbids padding the list to look more like stock's, or trimming it to hide a
/// verb the binary really does serve.
#[test]
fn the_listing_is_exactly_the_dispatch_table() {
    let (home, bin) = fixture("set");
    let text = stdout(&run(&home, &bin, &["help", "--all", "--no-verbose"]));
    let block = listing_block(&text);

    let listed = names_in(&block);
    let dispatched: BTreeSet<String> = zvcs::dispatch::PORCELAIN_VERBS
        .iter()
        .map(|v| (*v).to_string())
        .collect();
    assert_eq!(listed, dispatched);

    // `uniq()` in git; a name printed twice would still pass the set compare.
    assert_eq!(block.split_whitespace().count(), dispatched.len());
}

/// `pretty_print_cmdnames()` is `print_columns()` with `indent = "  "` and
/// `padding = 2`. Rendering the same names through `git column` with those
/// options must produce the identical block — if it does not, the listing grew
/// its own column formatter, which is precisely what this port must not do.
#[test]
fn the_table_is_laid_out_by_the_column_engine() {
    let (home, bin) = fixture("layout");
    let block = listing_block(&stdout(&run(&home, &bin, &["help", "--all", "--no-verbose"])));

    let mut names: Vec<&str> = zvcs::dispatch::PORCELAIN_VERBS.to_vec();
    names.sort_unstable();
    let feed = format!("{}\n", names.join("\n"));

    let mut child = Command::new(BIN)
        .args(["column", "--mode=column", "--indent=  ", "--padding=2"])
        .current_dir(&home)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("PATH", &bin)
        .env("COLUMNS", COLUMNS)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(feed.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(block, String::from_utf8(out.stdout).unwrap());
}

/// The non-verbose arm `break`s, so `cmd_help` falls into its no-topic tail and
/// prints the plain `git help` output after the listing. Two synopses, two
/// trailers — the shape stock actually emits.
#[test]
fn the_common_help_follows_the_listing() {
    let (home, bin) = fixture("tail");
    let text = stdout(&run(&home, &bin, &["help", "--all", "--no-verbose"]));
    let common = stdout(&run(&home, &bin, &["help"]));

    assert!(text.ends_with(&common), "common help is not the tail of the listing form");
    assert_eq!(text.matches("usage: git [-v | --version]").count(), 2);
    assert_eq!(text.matches("See 'git help git' for an overview of the system.").count(), 2);
    assert!(
        text.starts_with("usage: git [-v | --version]"),
        "the synopsis must open the output"
    );
}

/// `pretty_print_cmdnames()` forces columns on and consults `column.*` only for
/// the layout — its comment says so in as many words. `never` must therefore
/// still produce a table, while `plain` must still switch to one name per line
/// with the same two-space indent.
///
/// The values are written to a repository config, which is the scope
/// `get_colopts()`' `repo_config()` reads.
#[test]
fn column_config_selects_the_layout_but_cannot_switch_the_table_off() {
    let (home, bin) = fixture("colcfg");
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(run_in(&repo, &home, &bin, &["init", "-q", "-b", "main"]).status.success());

    assert!(run_in(&repo, &home, &bin, &["config", "column.help", "never"]).status.success());
    let table = listing_block(&stdout(&run_in(&repo, &home, &bin, &["help", "--all", "--no-verbose"])));
    assert!(
        table.lines().next().unwrap().split_whitespace().count() > 1,
        "column.help=never must not disable the table: {:?}",
        table.lines().next()
    );

    assert!(run_in(&repo, &home, &bin, &["config", "column.help", "plain"]).status.success());
    let plain = listing_block(&stdout(&run_in(&repo, &home, &bin, &["help", "--all", "--no-verbose"])));
    assert_eq!(plain.lines().count(), zvcs::dispatch::PORCELAIN_VERBS.len());
    assert_eq!(plain.lines().next(), Some("  add"));
}
