//! Differential parity tests for `git grep`'s boolean grammar
//! (`--and`/`--or`/`--not`/`( … )`) and its function-context renderers
//! (`-W`/`--function-context`, `-p`/`--show-function`): every case runs the same
//! arguments through stock `git` and through this port in the same repository and
//! asserts byte-identical stdout and the same exit code. Regression guard for the
//! expression tree, the flat highlight/column split, and the ported
//! `grep_source_1`/`show_pre_context` line shaping.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo with a prose file for the boolean grammar and a C-like file (whose
/// signature lines begin an identifier and whose bodies are indented) for the
/// function-context renderers.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-grepexpr-{tag}-{}", std::process::id()));
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

    std::fs::write(
        repo.join("words.txt"),
        "alpha one\nbeta two\ngamma three\nalpha beta\ndelta four\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("code.c"),
        "int a(void)\n{\n\tKEY;\n}\n\nint b(void)\n{\n\tKEY;\n\tmore;\n}\n",
    )
    .unwrap();
    git(&repo, &["add", "words.txt", "code.c"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

/// The `(stdout, exit_code)` of `git grep <args>` run under a neutral environment,
/// once for stock git and once for the port, so any config that could perturb grep
/// is held identical for both.
fn run(bin: &str, cwd: &Path, home: &Path, args: &[&str]) -> (Vec<u8>, Option<i32>) {
    let mut full = vec!["grep"];
    full.extend_from_slice(args);
    let out = Command::new(bin)
        .args(&full)
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap();
    (out.stdout, out.status.code())
}

/// Assert stock git and the port agree on stdout and exit code for `args`.
fn parity(repo: &Path, home: &Path, args: &[&str]) {
    let (g_out, g_code) = run("git", repo, home, args);
    let (z_out, z_code) = run(BIN, repo, home, args);
    assert_eq!(
        g_code, z_code,
        "exit code differs for {args:?}: git={g_code:?} zvcs={z_code:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&g_out),
        String::from_utf8_lossy(&z_out),
        "stdout differs for {args:?}"
    );
}

#[test]
fn boolean_grammar_matches_git() {
    let (repo, home) = fixture("bool");
    // --and: only lines carrying both patterns.
    parity(&repo, &home, &["-e", "alpha", "--and", "-e", "beta", "--", "words.txt"]);
    // explicit --or equals the implicit OR of two -e patterns.
    parity(&repo, &home, &["-e", "alpha", "--or", "-e", "delta", "--", "words.txt"]);
    parity(&repo, &home, &["-e", "alpha", "-e", "delta", "--", "words.txt"]);
    // --not negates the following atom.
    parity(&repo, &home, &["-e", "alpha", "--and", "--not", "-e", "one", "--", "words.txt"]);
    // Precedence: --and binds tighter than the implicit/explicit OR.
    parity(&repo, &home, &["-e", "alpha", "--and", "-e", "beta", "--or", "-e", "delta", "--", "words.txt"]);
    // Parentheses regroup, with --column exercising the flat-list column.
    parity(
        &repo,
        &home,
        &["--column", "(", "-e", "alpha", "--or", "-e", "gamma", ")", "--and", "--not", "-e", "beta", "--", "words.txt"],
    );
    // Counting and only-matching go through the same decision but different output.
    parity(&repo, &home, &["-c", "-e", "alpha", "--and", "-e", "beta", "--", "words.txt"]);
    parity(&repo, &home, &["-o", "-e", "alpha", "--and", "-e", "beta", "--", "words.txt"]);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn boolean_grammar_errors_match_git() {
    let (repo, home) = fixture("boolerr");
    // A dangling operator is a fatal (exit 128) in both.
    let (_, g) = run("git", &repo, &home, &["-e", "alpha", "--and", "--", "words.txt"]);
    let (_, z) = run(BIN, &repo, &home, &["-e", "alpha", "--and", "--", "words.txt"]);
    assert_eq!(g, Some(128));
    assert_eq!(z, Some(128));
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn function_context_matches_git() {
    let (repo, home) = fixture("func");
    // -p / --show-function: the enclosing signature line, no hunk marks.
    parity(&repo, &home, &["-p", "KEY", "--", "code.c"]);
    parity(&repo, &home, &["-n", "-p", "KEY", "--", "code.c"]);
    // -W / --function-context: the whole body, with `--` between functions.
    parity(&repo, &home, &["-W", "KEY", "--", "code.c"]);
    parity(&repo, &home, &["-n", "-W", "KEY", "--", "code.c"]);
    // A match in only the second function still finds its own signature.
    parity(&repo, &home, &["-n", "-W", "more", "--", "code.c"]);
    parity(&repo, &home, &["-n", "-p", "more", "--", "code.c"]);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
