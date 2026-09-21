//! What a hook is handed and where its output goes.
//!
//! Three properties of the hook runner that every hook shares, all measured
//! against git 2.55.0 rather than inferred:
//!
//! * **Streams.** `RUN_HOOKS_OPT_INIT` sets `.stdout_to_stderr = 1`
//!   (`hook.h:171-176`), which `pick_next_hook()` copies onto the child
//!   (`hook.c:607`), so a hook's own `echo` can never land on the stdout of the
//!   command that ran it. `pre-push` is the single caller that clears the flag,
//!   for backwards compatibility (`transport.c:1405-1411`).
//! * **`GIT_PREFIX`.** `setup_git_directory_gently()` exports it unconditionally
//!   at the end of setup — the work-tree-relative directory the command was typed
//!   in with a trailing `/`, and the empty string at the top (`setup.c:2069-2076`).
//!   Empty and unset are different things to a hook: `${GIT_PREFIX-fallback}`
//!   answers differently.
//! * **`GIT_DIR`.** Only `set_git_dir()` exports it (`setup.c:1070-1074`), and
//!   `setup_discovered_git_dir()` calls it only when the discovered directory is
//!   not the default `.git` (`setup.c:1240-1241`). An ordinary repository
//!   therefore runs its hooks with `GIT_DIR` *unset*; an explicit `--git-dir`
//!   goes through `setup_explicit_git_dir()`, which always exports it.
//!
//! Plus the one lookup consequence that is easy to get backwards: `find_hook()`
//! tests `access(path, X_OK)` and nothing else (`hook.c:38`), so a *directory*
//! named like a hook is found — 0755 carries the execute bits — and then fails to
//! exec, which git reports rather than silently skipping.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-hookenv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// A `git` invocation sealed off from the developer's own configuration — a
/// `core.hooksPath` in `~/.gitconfig` would otherwise decide which hooks run.
fn git(dir: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("no-such-global"))
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0200")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0200");
    cmd
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    git(dir, home).args(args).output().unwrap()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
}

fn init(dir: &Path, home: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    ok(dir, home, &["init", "-q", "-b", "main"]);
}

#[cfg(unix)]
fn write_hook(work: &Path, event: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let hooks = work.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let path = hooks.join(event);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A hook that records whether each named variable is set, and to what, in the
/// shell's own `set`/`unset` vocabulary — `${v+set}` is the only way to tell an
/// exported empty string from an absent variable.
fn recorder(rec: &Path, vars: &[&str]) -> String {
    let mut body = format!("rec={}\n: > \"$rec\"\n", shell_quote(rec));
    for v in vars {
        body.push_str(&format!(
            "if [ -n \"${{{v}+x}}\" ]; then printf '%s=[%s]\\n' {v} \"${v}\" >> \"$rec\"; \
             else printf '%s=<unset>\\n' {v} >> \"$rec\"; fi\n"
        ));
    }
    body
}

fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

fn commit_file(work: &Path, home: &Path, name: &str, body: &str, msg: &str) -> Output {
    std::fs::write(work.join(name), body).unwrap();
    ok(work, home, &["add", name]);
    run(work, home, &["commit", "-m", msg])
}

/// A hook writing to *its* stdout must reach the user on stderr, leaving the
/// command's stdout carrying only the command's own report. A `git commit | …`
/// pipeline that suddenly gained a `pre-commit` hook must not change shape.
#[test]
#[cfg(unix)]
fn a_hooks_stdout_is_folded_into_stderr() {
    let home = root("streams");
    let work = home.join("w");
    init(&work, &home);
    write_hook(&work, "pre-commit", "echo PRECOMMIT-SAYS");
    write_hook(&work, "post-commit", "echo POSTCOMMIT-SAYS");

    let out = commit_file(&work, &home, "f.txt", "a\n", "one");
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stdout.contains("PRECOMMIT-SAYS") && !stdout.contains("POSTCOMMIT-SAYS"),
        "hook chatter reached the command's stdout: {stdout:?}"
    );
    assert!(
        stderr.contains("PRECOMMIT-SAYS") && stderr.contains("POSTCOMMIT-SAYS"),
        "hook chatter did not reach stderr: {stderr:?}"
    );
    // The commit's own line still belongs to stdout, so the redirection is of the
    // child's stream and not of the whole command's.
    assert!(stdout.contains("one"), "commit report left stdout: {stdout:?}");
}

/// `pre-push` is the exception `transport.c:1411` carves out: its stdout stays
/// stdout. A hook that prints a report for a pipeline must keep working.
#[test]
#[cfg(unix)]
fn pre_push_alone_keeps_its_stdout() {
    let home = root("prepush");
    let work = home.join("w");
    init(&work, &home);
    let bare = home.join("bare.git");
    ok(&home, &home, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    commit_file(&work, &home, "f.txt", "a\n", "one");
    write_hook(&work, "pre-push", "cat > /dev/null; echo PREPUSH-SAYS");

    let out = run(&work, &home, &["push", bare.to_str().unwrap(), "main:main"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PREPUSH-SAYS"),
        "pre-push stdout was redirected to stderr: stdout={stdout:?} stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `GIT_PREFIX` is exported for every hook and carries the directory the command
/// was typed in. It is the empty string at the top of the work tree — exported,
/// not absent — and `sub/deep/` two levels down, trailing slash included.
#[test]
#[cfg(unix)]
fn git_prefix_names_the_directory_the_command_was_typed_in() {
    let home = root("prefix");
    let work = home.join("w");
    init(&work, &home);
    let rec = home.join("prefix.rec");
    write_hook(&work, "pre-commit", &recorder(&rec, &["GIT_PREFIX"]));

    commit_file(&work, &home, "f.txt", "a\n", "top");
    assert_eq!(std::fs::read_to_string(&rec).unwrap(), "GIT_PREFIX=[]\n");

    let deep = work.join("sub/deep");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("g.txt"), "b\n").unwrap();
    ok(&deep, &home, &["add", "g.txt"]);
    let out = run(&deep, &home, &["commit", "-m", "deep"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read_to_string(&rec).unwrap(), "GIT_PREFIX=[sub/deep/]\n");
}

/// A repository found by walking up from the current directory never exports
/// `GIT_DIR`; the hook is left to rediscover it from the work tree root it was
/// started in. Asking for the directory explicitly takes the other setup path,
/// which does export it — so the absence is a decision, not an omission.
#[test]
#[cfg(unix)]
fn git_dir_is_exported_only_when_it_was_asked_for_explicitly() {
    let home = root("gitdir");
    let work = home.join("w");
    init(&work, &home);
    let rec = home.join("gitdir.rec");
    write_hook(&work, "pre-commit", &recorder(&rec, &["GIT_DIR"]));

    commit_file(&work, &home, "f.txt", "a\n", "discovered");
    assert_eq!(
        std::fs::read_to_string(&rec).unwrap(),
        "GIT_DIR=<unset>\n",
        "a discovered repository must not export GIT_DIR"
    );

    let git_dir = work.join(".git");
    std::fs::write(work.join("f.txt"), "b\n").unwrap();
    let out = git(&home, &home)
        .args([
            "--git-dir",
            git_dir.to_str().unwrap(),
            "--work-tree",
            work.to_str().unwrap(),
        ])
        .args(["add", work.join("f.txt").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let out = git(&home, &home)
        .args([
            "--git-dir",
            git_dir.to_str().unwrap(),
            "--work-tree",
            work.to_str().unwrap(),
        ])
        .args(["commit", "-m", "explicit"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(&rec).unwrap(),
        format!("GIT_DIR=[{}]\n", git_dir.display())
    );
}

/// `find_hook()`'s `access(X_OK)` accepts a directory, so `mkdir
/// .git/hooks/pre-commit` is a *found* hook that cannot be executed. git reports
/// the exec failure and the commit is refused; treating the directory as "no
/// hook" would let a repository whose hooks are mis-installed commit unchecked.
///
/// The path in the message is `cmd->args.v[0]` — the path `find_hook()` returned,
/// relative to the work tree root git stands in, never an absolutized rewrite.
#[test]
#[cfg(unix)]
fn a_directory_named_like_a_hook_fails_the_exec_instead_of_being_skipped() {
    let home = root("dirhook");
    let work = home.join("w");
    init(&work, &home);
    commit_file(&work, &home, "f.txt", "a\n", "one");
    std::fs::create_dir_all(work.join(".git/hooks/pre-commit")).unwrap();

    std::fs::write(work.join("f.txt"), "b\n").unwrap();
    ok(&work, &home, &["add", "f.txt"]);
    let out = run(&work, &home, &["commit", "-m", "two"]);

    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: cannot exec '.git/hooks/pre-commit': Permission denied\n"
    );
    let log = run(&work, &home, &["log", "--format=%s"]);
    assert_eq!(
        String::from_utf8_lossy(&log.stdout),
        "one\n",
        "the commit went through despite the unrunnable hook"
    );
}
