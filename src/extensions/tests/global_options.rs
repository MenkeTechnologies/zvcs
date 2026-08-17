//! git's top-level `handle_options()` (git.c), which runs before any subcommand
//! is dispatched — so a divergence here is a divergence in *every* command.
//!
//! Two failure shapes are asserted, both verified against stock git 2.55.0:
//!
//!   * `-C <path>` that cannot be entered is `die_errno("cannot change to
//!     '%s'", …)` — `fatal: cannot change to '<path>': <strerror>`, exit 128.
//!     This port used to print `zvcs: -C: cannot chdir to <path>` and exit 1,
//!     which is both the wrong text and the wrong code for a scripted caller.
//!     An *empty* `-C` argument is a deliberate no-op in the C (the chdir is
//!     guarded by `if ((*argv)[1][0])`), and used to be reported as a failure.
//!   * a global option given with no value prints its own complaint, then
//!     `usage(git_usage_string)`, and exits 129. These used to fall out of the
//!     option loop and be mistaken for the subcommand name.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The first line of git's `usage(git_usage_string)` block, so the assertions
/// pin that the usage really followed the complaint without copying all six
/// lines of it into every case.
const USAGE_HEAD: &str = "usage: git [-v | --version] [-h | --help] [-C <path>] [-c <name>=<value>]";

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_NAMESPACE")
        .output()
        .unwrap()
}

/// A repository with one commit and one subdirectory, so `-C` has both a target
/// that exists and a file that is not a directory to aim at.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-globals-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap().join("repo");
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    std::fs::write(repo.join("f"), "a\n").unwrap();
    assert!(run(&repo, &["init", "-q", "-b", "main"]).status.success());
    repo
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn dash_c_into_a_missing_directory_is_git_die_errno() {
    let repo = fixture("missing");

    let out = run(&repo, &["-C", "nope", "status", "--porcelain"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'nope': No such file or directory\n");
    assert!(out.stdout.is_empty(), "nothing runs after the chdir fails");

    // The errno text is the OS's, not a fixed string: a non-directory is ENOTDIR.
    let out = run(&repo, &["-C", "f", "status", "--porcelain"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'f': Not a directory\n");

    // A later `-C` fails the same way once an earlier one has already moved.
    let out = run(&repo, &["-C", "sub", "-C", "nope", "status", "--porcelain"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'nope': No such file or directory\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn dash_c_with_an_empty_path_is_a_no_op() {
    let repo = fixture("empty");

    // git guards the chdir with `if ((*argv)[1][0])`, so this succeeds and stays
    // put — `--show-prefix` proves the cwd never moved.
    let out = run(&repo, &["-C", "", "rev-parse", "--show-prefix"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\n", "the top level prefix is one empty line");

    // And an empty one does not swallow the failure of a real one after it.
    let out = run(&repo, &["-C", "", "-C", "nope", "status", "--porcelain"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'nope': No such file or directory\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn dash_c_that_succeeds_moves_the_command() {
    let repo = fixture("ok");

    let out = run(&repo, &["-C", "sub", "rev-parse", "--show-prefix"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "sub/\n");

    // Repeated `-C` is cumulative and relative, as consecutive chdir(2) calls are.
    let out = run(&repo, &["-C", "sub", "-C", "..", "rev-parse", "--show-prefix"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\n", "the top level prefix is one empty line");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_global_option_without_its_value_is_usage_129() {
    let repo = fixture("novalue");

    // Each entry is git's own `fprintf(stderr, …)` for that option — no `fatal: `
    // and no `error: ` prefix — followed by the usage block.
    for (arg, complaint) in [
        ("-C", "no directory given for '-C' option"),
        ("-c", "-c expects a configuration string"),
        ("--git-dir", "no directory given for '--git-dir' option"),
        ("--work-tree", "no directory given for '--work-tree' option"),
        ("--namespace", "no namespace given for --namespace"),
    ] {
        let out = run(&repo, &[arg]);
        assert_eq!(code(&out), 129, "{arg}: stderr: {}", stderr(&out));
        let err = stderr(&out);
        let mut lines = err.lines();
        assert_eq!(lines.next(), Some(complaint), "{arg} complaint");
        assert_eq!(lines.next(), Some(USAGE_HEAD), "{arg} usage block");
        assert!(out.stdout.is_empty(), "{arg} prints nothing on stdout");
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn dash_c_inside_an_alias_expansion_fails_the_same_way() {
    let repo = fixture("alias");
    // `handle_options()` runs again on every turn of `run_argv`'s alias loop, so
    // a `-C` an expansion introduces must die exactly as a command-line one does.
    assert!(run(&repo, &["config", "alias.gone", "-C nope status --porcelain"]).status.success());
    assert!(run(&repo, &["config", "alias.stay", "-C \"\" rev-parse --show-prefix"]).status.success());

    let out = run(&repo, &["gone"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: cannot change to 'nope': No such file or directory\n");

    let out = run(&repo, &["stay"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\n", "the top level prefix is one empty line");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn an_unrecognised_leading_option_is_unknown_option_129() {
    let repo = fixture("unknown");

    // `handle_options()` has no "leave it for the dispatcher" path: the final
    // `else` of its chain is `unknown option: %s` + `usage(git_usage_string)`,
    // which `usage()` closes with exit 129. `--super-prefix` is in the list
    // because 2.45 deleted its branch, so on 2.55 it is unknown like the rest.
    for args in [
        &["--frobnicate", "status"][..],
        &["--super-prefix", "x", "status"][..],
        &["--no-icase-pathspecs", "status"][..],
        &["-Z", "status"][..],
        &["--", "status"][..],
    ] {
        let out = run(&repo, args);
        assert_eq!(code(&out), 129, "{args:?}: stderr: {}", stderr(&out));
        let err = stderr(&out);
        let mut lines = err.lines();
        assert_eq!(lines.next(), Some(format!("unknown option: {}", args[0]).as_str()), "{args:?}");
        assert_eq!(lines.next(), Some(USAGE_HEAD), "{args:?} usage block");
        assert!(out.stdout.is_empty(), "{args:?} prints nothing on stdout");
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn exec_path_is_matched_as_a_prefix() {
    let repo = fixture("execpath");

    // `skip_prefix(cmd, "--exec-path", &cmd)` only asks whether the *remainder*
    // starts with `=`. Anything else that begins with `--exec-path` therefore
    // prints the directory and exits 0 with the remainder ignored, rather than
    // reaching the `unknown option` arm.
    let bare = run(&repo, &["--exec-path"]);
    assert_eq!(code(&bare), 0, "stderr: {}", stderr(&bare));
    assert!(!bare.stdout.is_empty(), "the exec directory is printed");

    for spelling in ["--exec-pathZZZ", "--exec-path-"] {
        let out = run(&repo, &[spelling, "status"]);
        assert_eq!(code(&out), 0, "{spelling}: stderr: {}", stderr(&out));
        assert_eq!(out.stdout, bare.stdout, "{spelling} prints the same directory");
    }

    // The `=` form sets the directory instead, and the command after it runs.
    let out = run(&repo, &["--exec-path=/nonexistent-exec", "rev-parse", "--show-prefix"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn no_command_left_after_the_option_layer_is_the_usage_block() {
    let repo = fixture("nocmd");

    // `cmd_main`'s `if (!argc)`: the synopsis, the common command groups and the
    // trailer, on *stdout*, exit 1. `handle_options` can eat the whole command
    // line several ways — `--bare` needs no argument, `-c` takes the next token
    // whatever it is, and `--shallow-file` swallows the token after it — so this
    // is not only the bare `git` case.
    for args in [
        &[][..],
        &["--bare"][..],
        &["-c", "a.b=1"][..],
        &["-c", "status"][..],
        &["--no-optional-locks"][..],
    ] {
        let out = run(&repo, args);
        assert_eq!(code(&out), 1, "{args:?}: stderr: {}", stderr(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.starts_with(USAGE_HEAD), "{args:?}: {stdout}");
        assert!(
            stdout.contains("These are common Git commands used in various situations:"),
            "{args:?}: the common command groups follow the synopsis"
        );
        assert!(stderr(&out).is_empty(), "{args:?}: nothing on stderr");
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_malformed_config_key_is_reported_when_config_is_read() {
    let repo = fixture("badkey");

    // config.c's `config_parse_pair` + `do_parse_config_key`, reported by the
    // reader as `error: <reason>` and then `fatal: unable to parse command-line
    // config` with exit 128 — not at push time, which is why `git -c foo
    // --version` below still works.
    for (key, reason) in [
        ("foo", "key does not contain a section: foo"),
        (".a", "key does not contain a section: .a"),
        ("a.", "key does not contain variable name: a."),
        ("", "empty config key"),
        ("=v", "empty config key"),
        ("a.b c=1", "invalid key: a.b c"),
        ("a.1b=1", "invalid key: a.1b"),
        ("a.b.c d=1", "invalid key: a.b.c d"),
    ] {
        let out = run(&repo, &["-c", key, "status", "--porcelain"]);
        assert_eq!(code(&out), 128, "-c {key}: stderr: {}", stderr(&out));
        assert_eq!(
            stderr(&out),
            format!("error: {reason}\nfatal: unable to parse command-line config\n"),
            "-c {key}"
        );
    }

    // The keys the same routine accepts: a section may start with a digit, only
    // the variable name may not, and case is folded rather than rejected.
    for key in ["1a.b=1", "A.B=1", "a.B.c=1", "a-b.c-d=1"] {
        let out = run(&repo, &["-c", key, "status", "--porcelain"]);
        assert_eq!(code(&out), 0, "-c {key}: stderr: {}", stderr(&out));
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_bad_config_key_is_not_checked_before_the_configuration_is_read() {
    let repo = fixture("lazykey");

    // `git_config_push_parameter()` does no validation at all; the diagnostic
    // comes from whoever reads the configuration first. A command that reads
    // none never notices, which stock 2.55.0 demonstrates with `version` and a
    // bare `help`.
    let out = run(&repo, &["-c", "foo", "version"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version "));

    let out = run(&repo, &["-c", "foo", "--version"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));

    let out = run(&repo, &["-c", "foo", "help"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with(USAGE_HEAD));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn config_env_failures_are_die_at_push_time() {
    let repo = fixture("configenv");

    // Unlike `-c`, all three `git_config_push_env()` failures are `die()`, so
    // they are reported before the command runs and exit 128. The key is quoted
    // as `%.*s` up to the *last* `=`, which is where the split happens.
    for (spec, message) in [
        ("x.y", "fatal: invalid config format: x.y\n"),
        ("x.y=", "fatal: missing environment variable name for configuration 'x.y'\n"),
        (
            "x.y=ZVCS_NO_SUCH_VARIABLE",
            "fatal: missing environment variable 'ZVCS_NO_SUCH_VARIABLE' for configuration 'x.y'\n",
        ),
    ] {
        for args in [&["--config-env", spec, "status"][..], &[&format!("--config-env={spec}"), "status"][..]] {
            let out = run(&repo, args);
            assert_eq!(code(&out), 128, "{args:?}: stderr: {}", stderr(&out));
            assert_eq!(stderr(&out), message, "{args:?}");
        }
    }

    // With no value at all it is the usage complaint, like every other global.
    let out = run(&repo, &["--config-env"]);
    assert_eq!(code(&out), 129, "stderr: {}", stderr(&out));
    let err = stderr(&out);
    let mut lines = err.lines();
    assert_eq!(lines.next(), Some("no config key given for --config-env"));
    assert_eq!(lines.next(), Some(USAGE_HEAD));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn an_alias_that_changes_the_environment_is_refused() {
    let repo = fixture("aliasenv");

    // `handle_alias` passes a non-null `envchanged` to `handle_options`, and
    // every global that reaches a child through the environment sets it. The
    // alias is then refused rather than allowed to leak the setting.
    for (name, body) in [
        ("gd", "--git-dir=.git status"),
        ("wt", "--work-tree=. status"),
        ("bare", "--bare status"),
        ("lit", "--literal-pathspecs status"),
        ("locks", "--no-optional-locks status"),
        ("nopag", "--no-pager status"),
        ("cd", "-C . status"),
    ] {
        assert!(run(&repo, &["config", &format!("alias.{name}"), body]).status.success());
        let out = run(&repo, &[name]);
        assert_eq!(code(&out), 128, "alias.{name}: stderr: {}", stderr(&out));
        assert_eq!(
            stderr(&out),
            format!(
                "fatal: alias '{name}' changes environment variables.\n\
                 You can use '!git' in the alias to do this\n"
            ),
            "alias.{name}"
        );
    }

    // The globals that do *not* set it stay usable inside an alias: `-p` and
    // `-c` are both allowed, and the `-c` still reaches the command.
    assert!(run(&repo, &["config", "alias.paged", "-p rev-parse --show-prefix"]).status.success());
    let out = run(&repo, &["paged"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));

    assert!(run(&repo, &["config", "alias.cfg", "-c core.abbrev=12 rev-parse --show-prefix"]).status.success());
    let out = run(&repo, &["cfg"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));

    // …and a bad key inside an alias is reported exactly like a typed one.
    assert!(run(&repo, &["config", "alias.cfgbad", "-c foo status --porcelain"]).status.success());
    let out = run(&repo, &["cfgbad"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "error: key does not contain a section: foo\nfatal: unable to parse command-line config\n"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn an_alias_loop_prints_the_whole_chain() {
    let repo = fixture("aliasloop");

    for (name, body) in [("l1", "l2"), ("l2", "l3"), ("l3", "l1")] {
        assert!(run(&repo, &["config", &format!("alias.{name}"), body]).status.success());
    }

    // `run_argv` keeps every command name it has looked at and prints the list,
    // marking the repeated entry `<==` and the last one `==>`.
    let out = run(&repo, &["l1"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "fatal: alias loop detected: expansion of 'l1' does not terminate:\n  l1 <==\n  l2\n  l3 ==>\n"
    );

    // Entering the same loop at a different point renames the chain, because the
    // sentence names the *first* command of the pass rather than the alias that
    // closed the circle.
    let out = run(&repo, &["l2"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "fatal: alias loop detected: expansion of 'l2' does not terminate:\n  l2 <==\n  l3\n  l1 ==>\n"
    );

    // A self-referencing alias is the other guard, inside `handle_alias`.
    assert!(run(&repo, &["config", "alias.self", "self"]).status.success());
    let out = run(&repo, &["self"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: recursive alias: self\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn an_alias_that_expands_to_nothing_runnable_names_the_typed_command() {
    let repo = fixture("aliasfail");

    assert!(run(&repo, &["config", "alias.unk", "frobnicate"]).status.success());
    assert!(run(&repo, &["config", "alias.emp", ""]).status.success());
    assert!(run(&repo, &["config", "alias.chain", "unk"]).status.success());

    // `cmd_main`'s `if (was_alias)` branch: the message names the command the
    // user typed and the verb the chain ended on, and `help_unknown_cmd` — the
    // "did you mean" suggestions and `help.autocorrect` alike — is never reached.
    for (typed, ended_on) in [("unk", "frobnicate"), ("emp", ""), ("chain", "frobnicate")] {
        let out = run(&repo, &[typed]);
        assert_eq!(code(&out), 1, "{typed}: stderr: {}", stderr(&out));
        assert_eq!(
            stderr(&out),
            format!("expansion of alias '{typed}' failed; '{ended_on}' is not a git command\n"),
            "{typed}"
        );
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn the_aliasing_notice_is_for_the_bare_dash_h_only() {
    let repo = fixture("aliash");

    assert!(run(&repo, &["config", "alias.st", "status --porcelain"]).status.success());

    // `handle_alias`' guard is `args->nr == 2 && !strcmp(args->v[1], "-h")`, so
    // the notice belongs to `git <alias> -h` and nothing else. With any further
    // argument the `-h` is the expanded command's, not a request to describe the
    // alias, and stock stays silent — the loosened `nr > 1` spelling printed the
    // notice into the middle of `git st -h <path>`'s own output.
    let out = run(&repo, &["st", "-h"]);
    assert_eq!(stderr(&out), "'st' is aliased to 'status --porcelain'\n");

    for args in [&["st", "-h", "f"][..], &["st", "f", "-h"][..], &["st", "-h", "-h"][..]] {
        assert!(
            !stderr(&run(&repo, args)).contains("is aliased to"),
            "{args:?} must not announce the alias"
        );
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_bad_key_pushed_by_an_alias_is_reported_at_the_next_lookup() {
    let repo = fixture("aliaskey");

    // `alias_lookup()` reads the configuration, so a `-c` an expansion pushed is
    // reported by the *next* turn of `run_argv`'s loop — after this turn's
    // recursive / empty / loop guards have had their say. Getting the order
    // wrong replaces git's own diagnostic for the alias with the config one.
    assert!(run(&repo, &["config", "alias.selfx", "-c foo selfx"]).status.success());
    let out = run(&repo, &["selfx"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: recursive alias: selfx\n");

    // One link further along the chain the lookup does happen, so there the key
    // is what fails.
    assert!(run(&repo, &["config", "alias.a2", "-c foo b2"]).status.success());
    assert!(run(&repo, &["config", "alias.b2", "version"]).status.success());
    let out = run(&repo, &["a2"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "error: key does not contain a section: foo\nfatal: unable to parse command-line config\n"
    );

    // A chain that reaches a builtin without another lookup gets the deferred
    // check against that builtin instead: `version` reads no configuration and
    // so never notices, exactly as `git -c foo version` does not.
    assert!(run(&repo, &["config", "alias.badv", "-c foo version"]).status.success());
    let out = run(&repo, &["badv"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version "));

    // …while one that reaches a builtin which does read configuration reports it.
    assert!(run(&repo, &["config", "alias.bads", "-c foo status --porcelain"]).status.success());
    let out = run(&repo, &["bads"]);
    assert_eq!(code(&out), 128, "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "error: key does not contain a section: foo\nfatal: unable to parse command-line config\n"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
