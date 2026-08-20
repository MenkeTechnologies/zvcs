//! `git config`'s colour and fallback reads: `--get-color`, `--type=color`,
//! `--default=<value>` and `--get-colorbool`.
//!
//! Every expectation is bytes captured from stock git 2.55.0. The three that are
//! easy to get subtly wrong, and that this file exists to pin:
//!
//!   * `--get-color` writes the escape sequence with **no trailing newline**
//!     (`fputs`), and an unset slot with no default writes nothing at all — a
//!     shell `color=$(git config --get-color …)` depends on both.
//!   * `color_parse()` emits attributes in ascending SGR order rather than the
//!     order they were typed, and `reset` contributes no code of its own — it only
//!     puts a leading `;` in front of whatever follows, so a lone `reset` is
//!     `ESC [ m`.
//!   * `--get-colorbool` falls back through `diff.color` (for the slot spelled
//!     `color.diff`) and then `color.ui`, so `color.ui = never` turns every unset
//!     slot off even on a terminal.
//!
//! Reads are pinned to a `--file` this test writes, so nothing depends on the
//! developer's global or system config.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .output()
        .expect("run zvcs git")
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

/// A directory holding a config file named `f` with `text` in it.
fn workdir(tag: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cfgcolor-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("f"), text).expect("write config");
    dir
}

/// Read one colour spec through `--type=color`, which is the same
/// `color_parse()` `--get-color` runs.
fn color_of(dir: &Path, spec: &str) -> Output {
    std::fs::write(dir.join("f"), format!("[test]\n\tc = {spec}\n")).expect("write");
    run(dir, &["config", "-f", "f", "--type=color", "--get", "test.c"])
}

#[test]
fn get_color_writes_the_escape_without_a_newline() {
    let dir = workdir("getcolor", "[color \"diff\"]\n\tmeta = bold red blue\n");
    let o = run(&dir, &["config", "-f", "f", "--get-color", "color.diff.meta"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "\x1b[1;31;44m", "no trailing newline");

    // An unset slot with no default writes nothing and still succeeds.
    let o = run(&dir, &["config", "-f", "f", "--get-color", "color.no.such"]);
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "");

    // The optional second operand is the default, parsed the same way.
    let o = run(&dir, &["config", "-f", "f", "--get-color", "color.no.such", "blue bold"]);
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "\x1b[1;34m");
}

#[test]
fn get_color_reports_a_bad_stored_value_and_a_bad_default_differently() {
    let dir = workdir("getcolor-bad", "[color \"bad\"]\n\tv = notacolor\n");
    // A stored value the parser rejects aborts the config walk: the callback's
    // `error()` line, then the config machinery's own fatal, at 128.
    let o = run(&dir, &["config", "-f", "f", "--get-color", "color.bad.v"]);
    assert_eq!(code(&o), 128);
    assert!(err(&o).starts_with("error: invalid color value: notacolor\n"), "{:?}", err(&o));
    assert!(err(&o).contains("fatal: bad config line 2 in file"), "{:?}", err(&o));

    // A *default* the parser rejects is `error()`'s -1, which reaches the shell as
    // 255 rather than as git's usual 128.
    let o = run(&dir, &["config", "-f", "f", "--get-color", "color.no.such", "bogusvalue"]);
    assert_eq!(code(&o), 255);
    assert_eq!(
        err(&o),
        "error: invalid color value: bogusvalue\nerror: unable to parse default color value\n"
    );
}

#[test]
fn get_color_argument_count_and_type_conflict() {
    let dir = workdir("getcolor-usage", "");
    for args in [
        vec!["config", "-f", "f", "--get-color"],
        vec!["config", "-f", "f", "--get-color", "a.b", "one", "two"],
    ] {
        let o = run(&dir, &args);
        assert_eq!(code(&o), 129, "{args:?}");
        assert_eq!(err(&o), "error: wrong number of arguments, should be from 1 to 2\n");
    }

    // `if ((actions & (ACTION_GET_COLOR|ACTION_GET_COLORBOOL)) && display_opts.type)`
    // runs before every other check and carries no usage block.
    let o = run(&dir, &["config", "-f", "f", "--get-color", "--type=int", "a.b"]);
    assert_eq!(code(&o), 129);
    assert_eq!(err(&o), "error: --get-color and variable type are incoherent\n");
}

#[test]
fn color_parse_orders_attributes_and_handles_reset() {
    let dir = workdir("colorspec", "");
    // Attributes come out in ascending SGR order, not in the order they were typed.
    assert_eq!(out(&color_of(&dir, "ul bold")), "\x1b[1;4m\n");
    assert_eq!(out(&color_of(&dir, "italic strike blink dim reverse")), "\x1b[2;3;5;7;9m\n");
    // `reset` writes no code of its own.
    assert_eq!(out(&color_of(&dir, "reset")), "\x1b[m\n");
    assert_eq!(out(&color_of(&dir, "reset green")), "\x1b[;32m\n");
    assert_eq!(out(&color_of(&dir, "reset bold")), "\x1b[;1m\n");
    // `-1` is git's documented alias for `normal`, which selects nothing at all.
    assert_eq!(out(&color_of(&dir, "-1")), "\n");
    assert_eq!(out(&color_of(&dir, "normal")), "\n");
    // Colour names are matched case-insensitively…
    assert_eq!(out(&color_of(&dir, "RED")), "\x1b[31m\n");
    // …but attribute names are compared with `memcmp`, so `Bold` is a spec error.
    let o = color_of(&dir, "Bold Red");
    assert_eq!(code(&o), 128);
    assert!(err(&o).starts_with("error: invalid color value: Bold Red\n"), "{:?}", err(&o));
    // 0-7 are the portable ANSI codes, 8-15 the aixterm brights, 16-255 the palette.
    assert_eq!(out(&color_of(&dir, "0")), "\x1b[30m\n");
    assert_eq!(out(&color_of(&dir, "8")), "\x1b[90m\n");
    assert_eq!(out(&color_of(&dir, "216")), "\x1b[38;5;216m\n");
    assert_eq!(out(&color_of(&dir, "brightblue")), "\x1b[94m\n");
    assert_eq!(out(&color_of(&dir, "default")), "\x1b[39m\n");
    // A 12-bit `#rgb` doubles each nibble, so it is the same colour as `#rrggbb`.
    assert_eq!(out(&color_of(&dir, "\"#f00\"")), "\x1b[38;2;255;0;0m\n");
    assert_eq!(out(&color_of(&dir, "\"#ff0000\"")), "\x1b[38;2;255;0;0m\n");
    // A third colour, and a number past the palette, are both spec errors.
    for bad in ["red blue green", "256", "-2", "bogus"] {
        assert_eq!(code(&color_of(&dir, bad)), 128, "{bad}");
    }
}

#[test]
fn default_supplies_a_value_only_for_a_get_that_found_nothing() {
    let dir = workdir("default", "[color \"diff\"]\n\tmeta = bold red blue\n");
    let o = run(&dir, &["config", "-f", "f", "--default=hello", "--get", "some.missing"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "hello\n");

    // A key that *is* set ignores the default entirely.
    let o = run(&dir, &["config", "-f", "f", "--default=hello", "--get", "color.diff.meta"]);
    assert_eq!(out(&o), "bold red blue\n");

    // The separated spelling, and the bare one-operand read (which git resolves to
    // ACTION_GET before the `--default` check runs).
    assert_eq!(
        out(&run(&dir, &["config", "-f", "f", "--default", "hello", "--get", "some.missing"])),
        "hello\n"
    );
    assert_eq!(out(&run(&dir, &["config", "-f", "f", "--default=hello", "some.missing"])), "hello\n");

    // `--no-default` NULLs the slot again, so the read is a plain miss at exit 1.
    let o = run(&dir, &["config", "-f", "f", "--default=x", "--no-default", "--get", "some.missing"]);
    assert_eq!(code(&o), 1);
    assert_eq!(out(&o), "");
}

#[test]
fn default_goes_through_the_type_conversion() {
    let dir = workdir("default-typed", "");
    assert_eq!(
        out(&run(&dir, &["config", "-f", "f", "--default=17", "--type=int", "--get", "x.y"])),
        "17\n"
    );
    assert_eq!(
        out(&run(&dir, &["config", "-f", "f", "--default=bold red", "--type=color", "--get", "x.y"])),
        "\x1b[1;31m\n"
    );

    // A default the type rejects is fatal, and the number arm keeps its own wording
    // — with no file of origin, because the value came from the command line.
    let o = run(&dir, &["config", "-f", "f", "--default=notanint", "--type=int", "--get", "x.y"]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: bad numeric config value 'notanint' for 'x.y': invalid unit\n");

    // The colour arm reports through `format_config()`'s own `die()` instead.
    let o = run(&dir, &["config", "-f", "f", "--default=bogus", "--type=color", "--get", "x.y"]);
    assert_eq!(code(&o), 128);
    assert_eq!(
        err(&o),
        "error: invalid color value: bogus\nfatal: failed to format default config value: bogus\n"
    );
}

#[test]
fn default_is_refused_for_every_action_but_get() {
    let dir = workdir("default-usage", "");
    for action in ["--get-all", "--get-regexp", "--list"] {
        let o = run(&dir, &["config", "-f", "f", "--default=x", action, "some.missing"]);
        assert_eq!(code(&o), 129, "{action}");
        assert_eq!(err(&o), "error: --default is only applicable to --get\n", "{action}");
    }
    // `OPT_STRING` with no value left on the line.
    let o = run(&dir, &["config", "-f", "f", "--default"]);
    assert_eq!(code(&o), 129);
    assert_eq!(err(&o), "error: option `default' requires a value\n");
}

#[test]
fn get_colorbool_falls_back_through_diff_color_and_color_ui() {
    // Stdout is a pipe in the test harness, so the `auto` answer is "off" — which
    // is what makes the `always`/`never` overrides visible in the exit code.
    let cases: &[(&str, &str, i32)] = &[
        ("", "color.diff", 1),
        ("[color]\n\tui = always\n", "color.diff", 0),
        ("[color]\n\tui = always\n", "color.branch", 0),
        ("[color]\n\tui = never\n", "color.diff", 1),
        ("[color]\n\tui = auto\n", "color.diff", 1),
        // The historical `diff.color` spelling answers only for `color.diff`.
        ("[diff]\n\tcolor = always\n", "color.diff", 0),
        ("[diff]\n\tcolor = always\n", "color.branch", 1),
        // The slot itself beats both fallbacks.
        ("[color]\n\tui = never\n\tdiff = always\n", "color.diff", 0),
        ("[color]\n\tui = always\n\tdiff = never\n", "color.diff", 1),
    ];
    for (i, (text, slot, want)) in cases.iter().enumerate() {
        let dir = workdir(&format!("cb{i}"), text);
        let o = run(&dir, &["config", "-f", "f", "--get-colorbool", slot]);
        assert_eq!(code(&o), *want, "case {i}: {text:?} {slot}");
        assert_eq!(out(&o), "", "case {i} prints nothing without the tty operand");
    }
}

#[test]
fn get_colorbool_prints_when_told_whether_stdout_is_a_tty() {
    let dir = workdir("cb-print", "[color]\n\tui = never\n");
    // With the operand present the answer is printed and the exit code is 0 either
    // way — and `never` still wins over a stated tty.
    let o = run(&dir, &["config", "-f", "f", "--get-colorbool", "color.diff", "true"]);
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "false\n");

    let dir = workdir("cb-print2", "");
    assert_eq!(out(&run(&dir, &["config", "-f", "f", "--get-colorbool", "color.diff", "true"])), "true\n");
    assert_eq!(out(&run(&dir, &["config", "-f", "f", "--get-colorbool", "color.diff", "false"])), "false\n");

    // `git_config_bool("command line", …)` dies on a word that is not a boolean.
    let o = run(&dir, &["config", "-f", "f", "--get-colorbool", "color.diff", "bogus"]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: bad boolean config value 'bogus' for 'command line'\n");
}

#[test]
fn get_colorbool_dies_on_a_slot_that_is_neither_a_word_nor_a_boolean() {
    let dir = workdir("cb-bad", "[color]\n\tdiff = red\n");
    let o = run(&dir, &["config", "-f", "f", "--get-colorbool", "color.diff"]);
    assert_eq!(code(&o), 128);
    assert_eq!(err(&o), "fatal: bad boolean config value 'red' for 'color.diff'\n");
}
