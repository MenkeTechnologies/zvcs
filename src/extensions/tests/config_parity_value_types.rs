//! How a configured string becomes a typed value, and what git says when it
//! cannot — across every origin git names differently.
//!
//! Three things are pinned here, all of them bytes captured from stock git
//! 2.55.0 and all of them previously wrong in the port:
//!
//!   * **The integer bound is a C `int`, not an `int64_t`.** `git_parse_int()`
//!     (parse.c:92-99) passes `maximum_signed_value_of_type(int)` down to
//!     `git_parse_signed()`, so `gc.auto = 3000000000` and
//!     `diff.renameLimit = 5g` are `out of range` rather than values. Only
//!     `git config --type=int` is wider — that one arm is
//!     `git_config_int64()` (builtin/config.c:270-283).
//!   * **`die_bad_number()` names the origin.** Only `CONFIG_ORIGIN_CMDLINE`
//!     has a NULL filename and therefore the short form (config.c:1201-1202);
//!     `git config --file -` is `CONFIG_ORIGIN_STDIN`, whose name is the empty
//!     string and not NULL (config.c:1404-1409), so it reaches the switch and
//!     prints ` in standard input`. A `--blob` prints ` in blob <spec>`.
//!   * **`parse_expiry_date()` works in an unsigned `timestamp_t`.** `now` and
//!     `all` are `TIME_MAX`, printed by `PRItime` as `18446744073709551615` —
//!     not the `i64::MAX` an `i64`-shaped reader saturates at.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(dir: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR");
    c
}

fn run(dir: &Path, args: &[&str]) -> Output {
    cmd(dir).args(args).output().expect("run zvcs git")
}

/// The same run with `text` on stdin, which is what `--file -` reads.
fn run_stdin(dir: &Path, args: &[&str], text: &str) -> Output {
    use std::io::Write as _;
    let mut child = cmd(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn zvcs git");
    child.stdin.as_mut().expect("stdin").write_all(text.as_bytes()).expect("write stdin");
    child.wait_with_output().expect("wait")
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

/// An empty directory to run in. Nothing here needs a repository except the
/// blob test, which makes its own.
fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cfgtypes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// `git_parse_int()`'s bound is a C `int`. Every reader that is not
/// `--type=int` goes through it, so a value between `INT_MAX` and `i64::MAX`
/// is `out of range` — it is not a large number.
///
/// `gc.auto` and `diff.renameLimit` are two different call sites that reach the
/// same reader; both are pinned so a fix applied to one key only would fail.
#[test]
fn an_int_key_is_bounded_by_a_c_int_and_not_an_int64() {
    let dir = workdir("intbound");
    assert_eq!(code(&run(&dir, &["init", "-q", "."])), 0);

    for (key, args) in [
        ("gc.auto", ["gc", "--auto"]),
        ("diff.renameLimit", ["diff", "--stat"]),
    ] {
        for value in ["3000000000", "-3000000000", "5g", "0x80000000"] {
            let o = run(&dir, &["-c", &format!("{key}={value}"), args[0], args[1]]);
            assert_eq!(
                code(&o),
                128,
                "{key}={value} should be out of range, got {:?}",
                out(&o)
            );
            assert_eq!(
                err(&o),
                format!(
                    "fatal: bad numeric config value '{value}' for '{}': out of range\n",
                    key.to_lowercase()
                ),
            );
        }

        // The other side of the bound: `INT_MAX` itself is a value, and the
        // `k`/`m`/`g` grammar still scales below it.
        let o = run(&dir, &["-c", &format!("{key}=2147483647"), args[0], args[1]]);
        assert_eq!(code(&o), 0, "{}", err(&o));
    }
}

/// `grep.threads` used to be read by a private decimal parser with an invented
/// `t` suffix. It is `git_config_int()` like every other integer key: base 0,
/// so `0x10` is sixteen, and bounded by a C `int`.
#[test]
fn grep_threads_reads_the_shared_base_zero_integer_grammar() {
    let dir = workdir("grepthreads");
    let o = run(&dir, &["init", "-q", "."]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    std::fs::write(dir.join("f.txt"), "hello\n").expect("write");
    assert_eq!(code(&run(&dir, &["add", "f.txt"])), 0);

    // Base 0: hexadecimal is a thread count, not junk.
    let o = run(&dir, &["-c", "grep.threads=0x10", "grep", "-c", "hello", "--", "f.txt"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "f.txt:1\n");

    // Bounded by a C `int`, with git's `out of range` wording rather than the
    // `invalid unit` a shape-sniffing reporter would guess.
    for value in ["3000000000", "5g"] {
        let o = run(
            &dir,
            &["-c", &format!("grep.threads={value}"), "grep", "-c", "hello", "--", "f.txt"],
        );
        assert_eq!(code(&o), 128, "grep.threads={value}: {:?}", out(&o));
        assert_eq!(
            err(&o),
            format!("fatal: bad numeric config value '{value}' for 'grep.threads': out of range\n"),
        );
    }

    // `t` is not one of git's units.
    let o = run(&dir, &["-c", "grep.threads=1t", "grep", "-c", "hello", "--", "f.txt"]);
    assert_eq!(code(&o), 128);
    assert_eq!(
        err(&o),
        "fatal: bad numeric config value '1t' for 'grep.threads': invalid unit\n"
    );
}

/// `die_bad_number()` (config.c:1204-1224) switches on the origin type. A
/// `-c` value has a NULL filename and takes the short form; a value read from
/// stdin does not, so it is named ` in standard input`.
#[test]
fn a_bad_number_names_standard_input_but_not_the_command_line() {
    let dir = workdir("badnum-origin");

    let o = run(&dir, &["-c", "zz.probe=bogus", "config", "--type=int", "zz.probe"]);
    assert_eq!(code(&o), 128);
    assert_eq!(
        err(&o),
        "fatal: bad numeric config value 'bogus' for 'zz.probe': invalid unit\n"
    );

    let o = run_stdin(
        &dir,
        &["config", "--file", "-", "--type=int", "zz.probe"],
        "[zz]\n\tprobe = bogus\n",
    );
    assert_eq!(code(&o), 128);
    assert_eq!(
        err(&o),
        "fatal: bad numeric config value 'bogus' for 'zz.probe' in standard input: invalid unit\n"
    );

    // The overflow wording carries the same clause.
    let o = run_stdin(
        &dir,
        &["config", "--file", "-", "--type=int", "zz.probe"],
        "[zz]\n\tprobe = 9999999999999999999999\n",
    );
    assert_eq!(code(&o), 128);
    assert_eq!(
        err(&o),
        "fatal: bad numeric config value '9999999999999999999999' for 'zz.probe' \
         in standard input: out of range\n"
    );
}

/// The callback types (`color`, `expiry-date`) `error()` and hand -1 back, and
/// the config machinery adds its own second line naming the source and the
/// physical line. Stdin's is `bad config line <n> in standard input`, which the
/// port used to render as the command line's `unable to parse` instead.
#[test]
fn a_callback_failure_from_stdin_names_the_line_in_standard_input() {
    let dir = workdir("callback-stdin");

    let o = run_stdin(
        &dir,
        &["config", "--file", "-", "--type=color", "zz.probe"],
        "[zz]\n\tprobe = nosuchcolor\n",
    );
    assert_eq!(code(&o), 128);
    assert_eq!(
        err(&o),
        "error: invalid color value: nosuchcolor\nfatal: bad config line 2 in standard input\n"
    );

    // The line number is the entry's own, not a constant.
    let o = run_stdin(
        &dir,
        &["config", "--file", "-", "--type=expiry-date", "zz.probe"],
        "[zz]\n\tother = 1\n\n\tprobe = bogus\n",
    );
    assert_eq!(code(&o), 128);
    assert_eq!(
        err(&o),
        "error: 'bogus' for 'zz.probe' is not a valid timestamp\n\
         fatal: bad config line 4 in standard input\n"
    );
}

/// A blob is read by `git_config_from_mem()`, whose `default_error_action` is
/// `CONFIG_ERROR_ERROR` and not `CONFIG_ERROR_DIE` (config.c:1448). So the
/// number still `die()`s — `die_bad_number()` is unconditional — but a callback
/// failure only `error()`s, the parse returns -1, and `get_value()` reaches
/// `ret = !values.nr` with the item `format_config()` had already grown still
/// counted: exit 0, with the bare `opts->term` it appended on the failing path.
#[test]
fn a_blob_names_itself_and_keeps_the_error_non_fatal() {
    let dir = workdir("blob-origin");
    assert_eq!(code(&run(&dir, &["init", "-q", "."])), 0);
    std::fs::write(dir.join("cfg"), "[zz]\n\tprobe = bogus\n").expect("write");
    let o = run(&dir, &["hash-object", "-w", "cfg"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let oid = out(&o).trim().to_owned();

    // A number: `die_bad_number()` with the blob clause, fatal at 128.
    let o = run(&dir, &["config", &format!("--blob={oid}"), "--type=int", "zz.probe"]);
    assert_eq!(code(&o), 128, "{:?}", out(&o));
    assert_eq!(
        err(&o),
        format!("fatal: bad numeric config value 'bogus' for 'zz.probe' in blob {oid}: invalid unit\n"),
    );

    // A callback type: two `error()` lines, exit 0, and the lone terminator.
    let o = run(&dir, &["config", &format!("--blob={oid}"), "--type=color", "zz.probe"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(err(&o), format!("error: invalid color value: bogus\nerror: bad config line 2 in blob {oid}\n"));
    assert_eq!(out(&o), "\n");
}

/// `parse_expiry_date()` (date.c:957) stores `TIME_MAX` for `all` and `now`,
/// and `timestamp_t` is unsigned — so the printed sentinel is `u64::MAX` and
/// not the `i64::MAX` an `i64`-shaped reader saturates at. `never` and `false`
/// are the other sentinel, zero.
#[test]
fn the_expiry_sentinels_are_an_unsigned_timestamp() {
    let dir = workdir("expiry");
    for spelling in ["now", "all"] {
        let o = run(
            &dir,
            &["-c", &format!("zz.probe={spelling}"), "config", "--type=expiry-date", "zz.probe"],
        );
        assert_eq!(code(&o), 0, "{}", err(&o));
        assert_eq!(out(&o), "18446744073709551615\n", "{spelling}");
    }
    for spelling in ["never", "false"] {
        let o = run(
            &dir,
            &["-c", &format!("zz.probe={spelling}"), "config", "--type=expiry-date", "zz.probe"],
        );
        assert_eq!(code(&o), 0, "{}", err(&o));
        assert_eq!(out(&o), "0\n", "{spelling}");
    }
}

/// The four pathspec switches are `git_env_bool()` (parse.c:197-208), which is
/// `git_parse_maybe_bool()` plus a `die()` — not a lenient coercion. Two things
/// follow that a `Boolean::try_from(..).unwrap_or(false)` gets wrong: the
/// base-0 integer fallback makes `0x10` true, and a value that is neither a
/// word nor an integer is fatal instead of false.
#[test]
fn the_pathspec_environment_booleans_use_gits_grammar_and_die_on_junk() {
    let dir = workdir("pathspec-env");
    assert_eq!(code(&run(&dir, &["init", "-q", "."])), 0);
    std::fs::write(dir.join("f.txt"), "hello\n").expect("write");
    assert_eq!(code(&run(&dir, &["add", "f.txt"])), 0);

    // `0x10` is sixteen, which is true — so it conflicts with `glob`.
    let o = cmd(&dir)
        .args(["ls-files", "--", "f.txt"])
        .env("GIT_NOGLOB_PATHSPECS", "0x10")
        .env("GIT_GLOB_PATHSPECS", "1")
        .output()
        .expect("run");
    assert_eq!(code(&o), 128, "{:?}", out(&o));
    assert_eq!(
        err(&o),
        "fatal: global 'glob' and 'noglob' pathspec settings are incompatible\n"
    );

    // Junk is `die()`, naming the variable and the value it was given.
    let o = cmd(&dir)
        .args(["ls-files", "--", "f.txt"])
        .env("GIT_NOGLOB_PATHSPECS", "bogus")
        .output()
        .expect("run");
    assert_eq!(code(&o), 128, "{:?}", out(&o));
    assert_eq!(
        err(&o),
        "fatal: bad boolean environment value 'bogus' for 'GIT_NOGLOB_PATHSPECS'\n"
    );

    // `git_parse_maybe_bool()`'s integer half is bounded by a C `int`, so a
    // value past it is not a boolean at all.
    let o = cmd(&dir)
        .args(["ls-files", "--", "f.txt"])
        .env("GIT_NOGLOB_PATHSPECS", "3000000000")
        .output()
        .expect("run");
    assert_eq!(code(&o), 128, "{:?}", out(&o));
    assert_eq!(
        err(&o),
        "fatal: bad boolean environment value '3000000000' for 'GIT_NOGLOB_PATHSPECS'\n"
    );

    // An off value is off, and nothing is said about it.
    for value in ["0", "false", "off", ""] {
        let o = cmd(&dir)
            .args(["ls-files", "--", "f.txt"])
            .env("GIT_NOGLOB_PATHSPECS", value)
            .env("GIT_GLOB_PATHSPECS", "1")
            .output()
            .expect("run");
        assert_eq!(code(&o), 0, "GIT_NOGLOB_PATHSPECS={value:?}: {}", err(&o));
        assert_eq!(out(&o), "f.txt\n");
    }
}

/// `GIT_ATTR_NOSYSTEM` is `git_env_bool()` too (attr.c:893), and `git var
/// GIT_ATTR_SYSTEM` is where its verdict shows. Both halves of the grammar are
/// pinned: the base-0 integer fallback makes `0x10` true (so the variable has
/// no value and exits 1), and junk is fatal rather than a silent false.
#[test]
fn git_var_reads_attr_nosystem_with_gits_boolean_grammar() {
    let dir = workdir("var-attr-nosystem");

    let o = cmd(&dir)
        .args(["var", "GIT_ATTR_SYSTEM"])
        .env("GIT_ATTR_NOSYSTEM", "0x10")
        .output()
        .expect("run");
    assert_eq!(code(&o), 1, "0x10 is sixteen, which is true: {:?}", out(&o));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "");

    for value in ["bogus", "3000000000"] {
        let o = cmd(&dir)
            .args(["var", "GIT_ATTR_SYSTEM"])
            .env("GIT_ATTR_NOSYSTEM", value)
            .output()
            .expect("run");
        assert_eq!(code(&o), 128, "GIT_ATTR_NOSYSTEM={value}: {:?}", out(&o));
        assert_eq!(
            err(&o),
            format!("fatal: bad boolean environment value '{value}' for 'GIT_ATTR_NOSYSTEM'\n"),
        );
    }
}
