//! `git grep -p`/`-W` runs off `match_funcname()` (grep.c:1329), which prefers
//! the path's diff-driver funcname pattern over the built-in "first byte starts
//! an identifier" test. This file pins that wiring — the port used to refuse a
//! path whose `diff` attribute named a driver with a funcname pattern — together
//! with two rules the same renderer gets wrong if `show_line()` is not followed
//! literally:
//!
//!   * grep.c:1752, a peek that runs off the end of the buffer ends the function
//!     body just as a signature line does, so trailing blank lines print nothing;
//!   * grep.c:1212 + 1290, `opt->last_shown` is written only by
//!     `show_line_header()`, which under `-o` is reached only from inside the
//!     per-match loop — so a shown-but-empty context line leaves `last_shown`
//!     behind and the next line looks gapped, emitting a `--`;
//!
//! and grep.c:1251/1255, where that `--` is painted with `color.grep.separator`.
//!
//! All expectations are measured against git 2.55.0 with `--threads 1`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-grepfc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    // Each of these three headings is invisible to the built-in test: `    def`
    // and `  needle` start with a space, and `func Hello() {` would be claimed by
    // the *preceding* `package main` line without golang's pattern.
    std::fs::write(
        repo.join("t.py"),
        "class Foo:\n    def bar(self):\n        return needle\n\n    def baz(self):\n        pass\n",
    )
    .unwrap();
    std::fs::write(repo.join("t.rb"), "def rb_meth\n  needle\nend\n").unwrap();
    std::fs::write(repo.join("t.go"), "package main\n\nfunc Hello() {\n\tneedle\n}\n").unwrap();
    std::fs::write(
        repo.join(".gitattributes"),
        "*.py diff=python\n*.rb diff=ruby\n*.go diff=golang\n",
    )
    .unwrap();
    // A match whose "function" is followed by nothing but a blank line.
    std::fs::write(repo.join("blanks"), "\n\nneedle\n\n").unwrap();
    // Two C functions, the second holding two matches: enough gaps for the `-o`
    // hunk-mark bookkeeping to show.
    std::fs::write(
        repo.join("c.c"),
        "int f(void)\n{\n\tneedle;\n}\n\nint g(void)\n{\n\tneedle;\n\tneedle;\n}\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

fn grep(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["grep", "--threads", "1"];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn python_driver_funcname_beats_the_builtin_test() {
    let (repo, home) = fixture("python");
    // The built-in test would never accept `    def bar(self):` (leading space),
    // and `-W` would fall back to `class Foo:` or print nothing.
    let d = stdout(&grep(&repo, &home, &["-W", "needle", "--", "t.py"]));
    assert_eq!(
        d, "t.py=    def bar(self):\nt.py:        return needle\n",
        "diff=python's funcname pattern should pick the `def` line:\n{d}"
    );
    let d = stdout(&grep(&repo, &home, &["-p", "needle", "--", "t.py"]));
    assert_eq!(d, "t.py=    def bar(self):\nt.py:        return needle\n", "-p too:\n{d}");
}

#[test]
fn ruby_and_golang_drivers_shape_their_own_bodies() {
    let (repo, home) = fixture("rbgo");
    let d = stdout(&grep(&repo, &home, &["-W", "needle", "--", "t.rb"]));
    assert_eq!(d, "t.rb=def rb_meth\nt.rb:  needle\nt.rb-end\n", "ruby body:\n{d}");
    // golang's pattern rejects `package main`, so the body starts at `func`.
    let d = stdout(&grep(&repo, &home, &["-W", "needle", "--", "t.go"]));
    assert_eq!(d, "t.go=func Hello() {\nt.go:\tneedle\nt.go-}\n", "golang body:\n{d}");
}

#[test]
fn function_body_stops_at_end_of_buffer_not_after_a_trailing_blank() {
    let (repo, home) = fixture("trailblank");
    let d = stdout(&grep(&repo, &home, &["-W", "-n", "needle", "--", "blanks"]));
    assert_eq!(
        d, "blanks:3:needle\n",
        "peeking past the trailing blank hits EOF, which ends the body:\n{d}"
    );
}

#[test]
fn only_matching_leaves_last_shown_on_lines_that_print_nothing() {
    let (repo, home) = fixture("omw");
    let d = stdout(&grep(&repo, &home, &["-o", "-W", "needle", "--", "c.c"]));
    // Lines 4, 6, 7 and 8 are shown but print no substring, so each of them is a
    // fresh gap over `last_shown`, which stays at 3 until line 8 matches.
    assert_eq!(
        d, "c.c:needle\n--\n--\n--\nc.c:needle\nc.c:needle\n",
        "-o -W hunk marks:\n{d}"
    );
}

#[test]
fn only_matching_prints_the_heading_on_the_first_match() {
    let (repo, home) = fixture("omheading");
    let d = stdout(&grep(&repo, &home, &["-o", "-W", "--heading", "needle", "--", "c.c"]));
    assert_eq!(
        d, "c.c\nneedle\n--\n--\n--\nneedle\nneedle\n",
        "the heading rides on show_line_header(), which -o reaches per match:\n{d}"
    );
}

#[test]
fn hunk_mark_is_painted_with_the_separator_color() {
    let (repo, home) = fixture("sepcolor");
    let d = stdout(&grep(&repo, &home, &["--color=always", "-A1", "needle", "--", "c.c"]));
    assert!(
        d.contains("\u{1b}[36m--\u{1b}[m\n"),
        "grep.c:1255 wraps `--` in color.grep.separator:\n{d:?}"
    );
    assert!(!d.contains("\n--\n"), "no bare `--` should survive:\n{d:?}");
}
