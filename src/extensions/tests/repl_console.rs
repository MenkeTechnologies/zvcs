//! `git zrepl` — the console, and the six verbs that only mean anything inside
//! it (`zcd`, `zpwd`, `zenv`, `zunset`, `zecho`, `zbanner`).
//!
//! The console is one process running many lines, so what it must get right is
//! the state that outlives a line: the working directory a later command
//! resolves its repository from, and the environment a later command and its
//! children see. Piped stdin takes the raw reader (no reedline), which is what
//! makes this testable at all.
//!
//! It must also tokenize a line the way `git <line>` does. It used to split on
//! whitespace alone, so every quoted argument arrived in pieces with the quotes
//! still attached — `commit -m "two words"` committed with the subject `"two`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn git(home: &Path, cwd: &Path, args: &[&str]) {
    let out = run(home, cwd, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

/// Feed `lines` to one console session and return stdout+stderr in order of
/// stream, which is how a scripted user reads it.
fn console(home: &Path, cwd: &Path, lines: &[&str], extra_env: &[(&str, &str)]) -> String {
    use std::io::Write;
    let mut cmd = Command::new(BIN);
    cmd.arg("zrepl")
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut script = lines.join("\n");
    script.push_str("\nexit\n");
    child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Two repos with distinguishable HEAD subjects, so a line's working directory
/// is visible in what a later line prints.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-repl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let (a, b) = (root.join("alpha"), root.join("beta"));
    for (dir, subject) in [(&a, "in-alpha"), (&b, "in-beta")] {
        std::fs::create_dir_all(dir).unwrap();
        git(&home, dir, &["init", "-q", "-b", "main"]);
        git(&home, dir, &["config", "user.email", "t@example"]);
        git(&home, dir, &["config", "user.name", "T"]);
        git(&home, dir, &["commit", "-q", "--allow-empty", "-m", subject]);
    }
    (root, home, a, b)
}

#[test]
fn a_console_line_tokenizes_the_way_git_does() {
    let (root, home, a, _b) = fixture("quote");

    // Double quotes group, single quotes group, and a backslash escapes a space
    // — git's own alias word-splitting. Splitting on whitespace committed the
    // subject `"two` and left the rest as stray arguments.
    let out = console(
        &home,
        &a,
        &[
            "commit --allow-empty -m \"two words here\"",
            "log -1 --format=%s",
            "commit --allow-empty -m 'single quoted'",
            "log -1 --format=%s",
            "commit --allow-empty -m back\\ slashed",
            "log -1 --format=%s",
        ],
        &[],
    );
    for want in ["two words here", "single quoted", "back slashed"] {
        assert!(out.lines().any(|l| l == want), "the console mangled a quoted argument (`{want}`):\n{out}");
    }

    // An unclosed quote is git's own error, and the console keeps going rather
    // than dying on a typo mid-session.
    let bad = console(&home, &a, &["zecho \"oops", "zecho still alive"], &[]);
    assert!(bad.contains("unclosed quote"), "an unclosed quote must be reported:\n{bad}");
    assert!(bad.contains("still alive"), "the console must survive a bad line:\n{bad}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zcd_and_zpwd_move_the_session_between_repos() {
    let (root, home, a, b) = fixture("cd");

    // The point of zcd: a later line resolves its repository from the new
    // directory, so the console can walk a tree without leaving the process.
    let out = console(
        &home,
        &a,
        &["log -1 --format=%s", &format!("zcd {}", b.display()), "zpwd", "log -1 --format=%s"],
        &[],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.contains(&"in-alpha"), "the session must start in alpha:\n{out}");
    assert!(lines.contains(&"in-beta"), "after zcd, a later line must run in beta:\n{out}");
    assert!(out.contains(&b.display().to_string()), "zpwd must print the new directory:\n{out}");

    // `zcd -` returns to the previous directory, as $OLDPWD does in a shell.
    let back = console(
        &home,
        &a,
        &[&format!("zcd {}", b.display()), "zcd -", "log -1 --format=%s"],
        &[],
    );
    assert!(back.lines().any(|l| l == "in-alpha"), "`zcd -` must return to the previous directory:\n{back}");

    // Bare `zcd` goes to $HOME.
    let home_dir = root.join("fakehome");
    std::fs::create_dir_all(&home_dir).unwrap();
    let to_home = console(&home, &a, &["zcd", "zpwd"], &[("HOME", home_dir.to_str().unwrap())]);
    assert!(to_home.contains(&home_dir.display().to_string()), "bare `zcd` must go to $HOME:\n{to_home}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zenv_and_zunset_carry_across_lines_and_into_children() {
    let (root, home, a, _b) = fixture("env");
    run(&home, &a, &["zreindex", "--sync", a.to_str().unwrap()]);

    // Set on one line, read on the next, and visible to a process the console
    // spawns — `zforeach` runs the command through a shell, so this proves the
    // variable reached the child's environment, not just the console's own map.
    let out = console(
        &home,
        &a,
        &["zenv MARKER=xyzzy", "zenv MARKER", "zforeach -- sh -c 'echo SAW=$MARKER'"],
        &[],
    );
    assert!(out.lines().any(|l| l == "xyzzy"), "`zenv NAME` must read back what was set:\n{out}");
    assert!(out.contains("SAW=xyzzy"), "a spawned child must inherit the variable:\n{out}");

    // zunset is the complement: the later read comes back empty, and the child
    // sees nothing.
    let cleared = console(
        &home,
        &a,
        &["zenv MARKER=xyzzy", "zunset MARKER", "zforeach -- sh -c 'echo SAW=[$MARKER]'"],
        &[],
    );
    assert!(cleared.contains("SAW=[]"), "zunset must remove the variable for children too:\n{cleared}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zecho_prints_its_arguments_literally() {
    let (root, home, a, _b) = fixture("echo");

    // Documented promise: no variable and no glob expansion. The console now
    // strips quotes the way git does, but what is inside them is never expanded.
    let out = console(&home, &a, &["zenv FOO=bar", "zecho hello $FOO", "zecho *"], &[]);
    assert!(out.lines().any(|l| l == "hello $FOO"), "zecho must not expand variables:\n{out}");
    assert!(out.lines().any(|l| l == "*"), "zecho must not expand globs:\n{out}");

    // `-n` suppresses the newline, so the next line's output continues it.
    let joined = console(&home, &a, &["zecho -n abc", "zecho def"], &[]);
    assert!(joined.contains("abcdef"), "`zecho -n` must suppress the trailing newline:\n{joined}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zbanner_honors_no_color() {
    let (root, home, a, _b) = fixture("banner");

    let plain = run(&home, &a, &["zbanner", "--no-color"]);
    let text = String::from_utf8_lossy(&plain.stdout);
    assert!(!text.is_empty(), "the banner must print something");
    assert!(!text.contains('\u{1b}'), "`--no-color` must emit no escape sequences:\n{text:?}");

    let colored = String::from_utf8_lossy(&run(&home, &a, &["zbanner", "--color"]).stdout).into_owned();
    assert!(colored.contains('\u{1b}'), "`--color` must emit color even when redirected");

    let _ = std::fs::remove_dir_all(&root);
}
