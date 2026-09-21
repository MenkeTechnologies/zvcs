//! `want_color()` parity: which verbs let `color.ui` / `color.<cmd>` turn color
//! on, and what `--color=<when>` accepts.
//!
//! Three rules from git's C, each of which this port got wrong somewhere:
//!
//! * `git_config_colorbool` (color.c:383-403) matches `never`/`always`/`auto`
//!   with `strcasecmp`, so `--color=ALWAYS` is `always`. Called from
//!   `parse_opt_color_flag_cb` (parse-options-cb.c:50-63) with a NULL variable
//!   name it knows *only* those three, so the boolean spellings a config value
//!   may use — `true`, `false`, `on`, `0`, the empty string — are usage errors:
//!   ``error: option `color' expects "always", "auto", or "never"``, exit 129.
//! * `--color=auto` is not the same as no `--color` at all. It writes
//!   `GIT_COLOR_AUTO`, which `want_color_fd` answers from the terminal alone
//!   (color.c:435-439); only the sentinel an absent switch leaves behind reaches
//!   `git_use_color_default` (color.c:432-434), the one path `color.ui` takes.
//! * The plumbing diff commands load `git_diff_basic_config`
//!   (`builtin/diff-tree.c:127`, `diff-index.c:31`, `diff-files.c:34`, each
//!   commented `/* no "diff" UI options */`), which has no `color.diff` arm and
//!   never calls `git_color_config` — so neither `color.diff` nor `color.ui` can
//!   colorize them, while the `color.diff.<slot>` palette that same callback does
//!   read (diff.c:491-499) still applies.
//!
//! Every assertion is absolute rather than a comparison against the installed
//! git, and every command's stdout is a pipe, so nothing here depends on a TTY
//! or on a second git being present.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The escape byte that starts every SGR sequence git emits.
const ESC: u8 = 0x1b;

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", repo)
        .env("ZVCS_HOME", repo)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        // A non-`dumb` TERM so the `auto` arm is decided by the pipe, not by
        // `is_terminal_dumb()`.
        .env("TERM", "xterm")
        .env_remove("GIT_PAGER_IN_USE")
        .output()
        .unwrap()
}

fn ok(repo: &Path, args: &[&str]) -> Output {
    let o = run(repo, args);
    assert!(
        o.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn colored(o: &Output) -> bool {
    o.stdout.contains(&ESC)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Two commits, a staged change and an unstaged one, plus a tag and a stash — so
/// every verb under test has something to print.
fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-colorparity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();

    ok(&repo, &["init", "-q", "-b", "main"]);
    ok(&repo, &["config", "user.email", "author@example.com"]);
    ok(&repo, &["config", "user.name", "Author"]);
    std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    ok(&repo, &["add", "f.txt"]);
    ok(&repo, &["commit", "-q", "-m", "first"]);
    std::fs::write(repo.join("f.txt"), "one\nTWO\nthree\n").unwrap();
    ok(&repo, &["add", "f.txt"]);
    ok(&repo, &["commit", "-q", "-m", "second"]);
    ok(&repo, &["tag", "v1"]);
    // Staged and unstaged changes, so `diff` and `diff --cached` both print.
    std::fs::write(repo.join("g.txt"), "alpha\n").unwrap();
    ok(&repo, &["add", "g.txt"]);
    std::fs::write(repo.join("f.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    repo
}

/// The plumbing diff commands: `color.diff`, its `diff.color` alias and
/// `color.ui` must all be inert, in every spelling of "on".
#[test]
fn plumbing_diff_commands_ignore_the_enablement_config() {
    let repo = fixture("plumbing");
    let cmds: [&[&str]; 3] = [
        &["diff-tree", "-p", "HEAD"],
        &["diff-index", "-p", "HEAD"],
        &["diff-files", "-p"],
    ];
    for cmd in cmds {
        for key in ["color.ui", "color.diff", "diff.color"] {
            for value in ["always", "true", "1", "yes", "on"] {
                let setting = format!("{key}={value}");
                let mut argv: Vec<&str> = vec!["-c", &setting];
                argv.extend(cmd.iter().copied());
                let o = ok(&repo, &argv);
                assert!(
                    !colored(&o),
                    "{cmd:?} colorized under {key}={value}; `git_diff_basic_config` has no such arm"
                );
                // `--color=auto` is the terminal test, and stdout here is a pipe.
                let mut with_auto = argv.clone();
                with_auto.push("--color=auto");
                assert!(
                    !colored(&ok(&repo, &with_auto)),
                    "{cmd:?} colorized under --color=auto with {key}={value}"
                );
            }
        }
        // The switch itself still works, and the slot palette still applies:
        // `git_diff_basic_config` reads `color.diff.<slot>` (diff.c:491-499).
        let mut always: Vec<&str> = vec!["-c", "color.diff.meta=red"];
        always.extend(cmd.iter().copied());
        always.push("--color=always");
        let o = ok(&repo, &always);
        assert!(colored(&o), "{cmd:?} --color=always emitted no color");
        assert!(
            String::from_utf8_lossy(&o.stdout).contains("\x1b[31mdiff --git"),
            "{cmd:?} ignored color.diff.meta=red: {:?}",
            String::from_utf8_lossy(&o.stdout)
        );
    }
}

/// The porcelain side of the same split, so the plumbing fix cannot be "turn
/// color off everywhere": `git diff` and `git log` do read the enablement keys.
#[test]
fn porcelain_diff_commands_still_read_the_enablement_config() {
    let repo = fixture("porcelain");
    for key in ["color.ui", "color.diff", "diff.color"] {
        let on = format!("{key}=always");
        for cmd in [
            vec!["-c", &*on, "diff"],
            vec!["-c", &*on, "diff", "--cached"],
            vec!["-c", &*on, "log", "-p", "-n1"],
        ] {
            assert!(
                colored(&ok(&repo, &cmd)),
                "{cmd:?} lost its color under {key}=always"
            );
        }
    }
}

/// `--color=auto` writes `GIT_COLOR_AUTO`, which never consults the config; an
/// absent switch does. Both halves are asserted so neither can be satisfied by
/// always-on or always-off.
#[test]
fn explicit_color_auto_overrides_the_enablement_config() {
    let repo = fixture("auto");
    let cases: [(&str, &[&str]); 4] = [
        ("color.ui", &["diff"]),
        ("color.ui", &["log", "--oneline", "-n1"]),
        ("color.ui", &["branch"]),
        ("color.branch", &["branch"]),
    ];
    for (key, cmd) in cases {
        let on = format!("{key}=always");
        let mut unset: Vec<&str> = vec!["-c", &on];
        unset.extend(cmd.iter().copied());
        assert!(
            colored(&ok(&repo, &unset)),
            "{cmd:?} ignored {key}=always with no --color switch"
        );
        let mut auto = unset.clone();
        auto.push("--color=auto");
        assert!(
            !colored(&ok(&repo, &auto)),
            "{cmd:?} let {key}=always override an explicit --color=auto"
        );
    }
}

/// `git_config_colorbool` compares with `strcasecmp`, so the value grammar is
/// case-insensitive for every verb that declares `OPT__COLOR`.
#[test]
fn color_when_values_are_case_insensitive() {
    let repo = fixture("case");
    ok(&repo, &["stash", "-q"]);
    let cmds: [&[&str]; 6] = [
        &["log", "--oneline", "-n1"],
        &["log", "--format=%C(red)%h%C(reset)", "-n1"],
        &["branch"],
        &["tag"],
        &["for-each-ref", "--format=%(color:red)%(refname)"],
        &["reflog", "-n1"],
    ];
    for cmd in cmds {
        for (upper, lower) in [("--color=ALWAYS", "--color=always"), ("--color=NEVER", "--color=never")] {
            let mut u: Vec<&str> = cmd.to_vec();
            u.push(upper);
            let mut l: Vec<&str> = cmd.to_vec();
            l.push(lower);
            // Compared without asserting success: the pair must agree on stdout,
            // stderr and status, which holds equally for a verb that colors and
            // for one whose `always` path this port has not finished — what must
            // never happen is the two spellings taking different paths.
            let (uo, lo) = (run(&repo, &u), run(&repo, &l));
            assert_eq!(
                (uo.stdout, uo.stderr, uo.status.code()),
                (lo.stdout, lo.stderr, lo.status.code()),
                "{cmd:?}: {upper} and {lower} disagreed"
            );
        }
    }
    // `list_stash()` forwards its arguments to a `git log -g` child, so the
    // reflog-as-log argument scanner is a second place the value grammar lives.
    // `--color=always` there is a separate, still-unported path, so the pair
    // compared here is the one both spellings do reach.
    for (upper, lower) in [("--color=NEVER", "--color=never"), ("--color=AUTO", "--color=auto")] {
        let u = ok(&repo, &["stash", "list", upper]);
        let l = ok(&repo, &["stash", "list", lower]);
        assert_eq!(u.stdout, l.stdout, "stash list: {upper} and {lower} disagreed");
        assert!(!u.stdout.is_empty(), "stash list {upper} listed nothing");
    }
}

/// `parse_opt_color_flag_cb` passes a NULL variable name, so `git_config_colorbool`
/// never reaches its `git_config_bool` fallback: the boolean spellings, and an
/// empty value, are usage errors at exit 129 with one line on stderr.
#[test]
fn color_when_rejects_config_boolean_spellings() {
    let repo = fixture("grammar");
    const MSG: &str = "error: option `color' expects \"always\", \"auto\", or \"never\"\n";
    let cmds: [&[&str]; 7] = [
        &["log", "--oneline", "-n1"],
        &["branch"],
        &["tag"],
        &["for-each-ref"],
        &["reflog", "-n1"],
        &["grep", "two"],
        &["diff"],
    ];
    for cmd in cmds {
        for bad in ["--color=true", "--color=false", "--color=on", "--color=0", "--color=bogus", "--color="] {
            // Right after the subcommand: `git grep` refuses any option that
            // follows its pattern, which would mask the diagnostic under test.
            let mut args: Vec<&str> = vec![cmd[0], bad];
            args.extend(cmd[1..].iter().copied());
            let o = run(&repo, &args);
            assert_eq!(
                o.status.code(),
                Some(129),
                "{cmd:?} {bad}: expected exit 129, stderr {:?}",
                stderr(&o)
            );
            assert_eq!(stderr(&o), MSG, "{cmd:?} {bad}: wrong diagnostic");
            assert!(o.stdout.is_empty(), "{cmd:?} {bad} printed output anyway");
        }
    }
}

/// `git_diff_ui_config` accepts `diff.color` as a second name for the one
/// `color.diff` setting (diff.c:363), and `git log`'s coloring goes through that
/// same switch — the alias must reach the log renderer too, not just `git diff`.
#[test]
fn log_honors_the_diff_color_alias() {
    let repo = fixture("alias");
    assert!(
        colored(&ok(&repo, &["-c", "diff.color=always", "log", "-p", "-n1"])),
        "log ignored diff.color=always"
    );
    assert!(
        !colored(&ok(&repo, &["-c", "diff.color=never", "-c", "color.ui=always", "log", "-p", "-n1"])),
        "log let color.ui outrank the more specific diff.color=never"
    );
}

/// `list_stash()` runs a `git log` child, so `git stash list` takes the whole
/// `git_log_config` refusal surface — but `cmd_stash` returns `!!fn(...)`
/// (builtin/stash.c:2496), so the status the user sees is 1, not 128.
/// `show_stash()` reaches the diff UI layer instead and dies at 128; every other
/// subcommand stops at `git_diff_basic_config` and does not die at all.
#[test]
fn stash_subcommands_take_the_right_color_config_refusal() {
    let repo = fixture("stash");
    ok(&repo, &["stash", "-q"]);
    for key in ["color.ui", "color.diff"] {
        let o = run(&repo, &["-c", &format!("{key}=bogus"), "stash", "list"]);
        assert_eq!(o.status.code(), Some(1), "stash list {key}=bogus: wrong exit");
        assert_eq!(
            stderr(&o),
            format!("fatal: bad boolean config value 'bogus' for '{key}'\n"),
            "stash list {key}=bogus: wrong diagnostic"
        );
        assert!(o.stdout.is_empty(), "stash list {key}=bogus listed anyway");

        let o = run(&repo, &["-c", &format!("{key}=bogus"), "stash", "show"]);
        assert_eq!(o.status.code(), Some(128), "stash show {key}=bogus: wrong exit");
        assert_eq!(
            stderr(&o),
            format!("fatal: bad boolean config value 'bogus' for '{key}'\n"),
            "stash show {key}=bogus: wrong diagnostic"
        );
    }
    // `color.status` is in neither callback, so no stash subcommand refuses it.
    let o = run(&repo, &["-c", "color.status=bogus", "stash", "list"]);
    assert!(
        o.status.success(),
        "stash list refused color.status=bogus: {}",
        stderr(&o)
    );
}
