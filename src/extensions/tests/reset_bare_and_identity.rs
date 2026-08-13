//! Two `git reset` refusals and one non-refusal, pinned against stock git 2.55.0.
//!
//! * **Bare repositories.** `cmd_reset()` (builtin/reset.c:470-478) runs
//!   `setup_work_tree()` for every mode but `--soft` — and for the default
//!   `--mixed` only when a work tree exists — so in a bare repository `--hard`,
//!   `--merge` and `--keep` die with the generic work-tree message while
//!   `--mixed` (and the pathspec form, which defaults to it) dies naming the
//!   mode. `--soft` is the one mode that works. Both refusals come before the
//!   `-N` check, so `reset -N --hard` in a bare repository reports the work tree.
//!
//! * **Reflogs with no identity.** The reflog writer
//!   (`log_ref_write_fd()`, refs/files-backend.c:1940-41) fills a missing
//!   committer with `git_committer_info(0)` — flag 0, meaning `fmt_ident()` runs
//!   non-strict and synthesizes the ident from the account and host instead of
//!   dying. Only object-writing commands pass `IDENT_STRICT`. A `reset` on a
//!   machine with no `user.name`/`user.email` therefore succeeds and writes a
//!   reflog line.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the binary with an identity configured, in a private HOME.
fn git(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .expect("run binary")
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let o = git(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    o
}

fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-resetbare-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("work")).unwrap();
    let root = root.canonicalize().unwrap();
    (root.join("home"), root.join("work"))
}

/// A worktree repo with two commits, and a bare clone of it.
fn bare_clone(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (home, work) = scratch(tag);
    ok(&work, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    ok(&work, &home, &["add", "a.txt"]);
    ok(&work, &home, &["commit", "-q", "-m", "one"]);
    std::fs::write(work.join("a.txt"), "b\n").unwrap();
    ok(&work, &home, &["commit", "-q", "-a", "-m", "two"]);
    let bare = work.parent().unwrap().join("bare.git");
    ok(work.parent().unwrap(), &home, &["clone", "-q", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    (home, work, bare)
}

fn head(dir: &Path, home: &Path) -> String {
    String::from_utf8_lossy(&ok(dir, home, &["rev-parse", "HEAD"]).stdout).trim().to_string()
}

#[test]
fn reset_in_a_bare_repository_refuses_like_git() {
    let (home, _work, bare) = bare_clone("refuse");
    let before = head(&bare, &home);

    // The mode-specific refusals, in git's wording, all exit 128 (`die()`).
    for (args, want) in [
        (&[][..], "fatal: mixed reset is not allowed in a bare repository\n"),
        (&["--mixed"][..], "fatal: mixed reset is not allowed in a bare repository\n"),
        (&["-N"][..], "fatal: mixed reset is not allowed in a bare repository\n"),
        // The pathspec form defaults to MIXED before the bare check runs.
        (&["HEAD", "--", "a.txt"][..], "fatal: mixed reset is not allowed in a bare repository\n"),
        (&["--hard"][..], "fatal: this operation must be run in a work tree\n"),
        (&["--merge"][..], "fatal: this operation must be run in a work tree\n"),
        (&["--keep"][..], "fatal: this operation must be run in a work tree\n"),
        // The work-tree check precedes the `-N` one, so this is not "-N requires --mixed".
        (&["-N", "--hard"][..], "fatal: this operation must be run in a work tree\n"),
    ] {
        let mut argv = vec!["reset"];
        argv.extend_from_slice(args);
        let o = git(&bare, &home, &argv);
        assert_eq!(o.status.code(), Some(128), "git {argv:?} exit: {o:?}");
        assert_eq!(String::from_utf8_lossy(&o.stderr), want, "git {argv:?} stderr");
        assert_eq!(head(&bare, &home), before, "git {argv:?} moved HEAD");
    }

    // `--soft` needs neither a work tree nor the index, and is accepted.
    let o = git(&bare, &home, &["reset", "--soft", "HEAD"]);
    assert_eq!(o.status.code(), Some(0), "soft reset: {o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    assert_eq!(head(&bare, &home), before);

    // `-N` still reports the option when the mode itself is allowed here.
    let o = git(&bare, &home, &["reset", "-N", "--soft"]);
    assert_eq!(o.status.code(), Some(128), "{o:?}");
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "fatal: the option '-N' requires '--mixed'\n"
    );

    let _ = std::fs::remove_dir_all(bare.parent().unwrap());
}

#[test]
fn reset_writes_a_reflog_without_a_configured_identity() {
    let (home, work) = scratch("noident");
    ok(&work, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    ok(&work, &home, &["add", "a.txt"]);
    ok(&work, &home, &["commit", "-q", "-m", "one"]);

    // No `-c user.*` and no environment identity: only the reflog is written, so
    // git synthesizes one rather than refusing.
    let o = Command::new(BIN)
        .args(["reset"])
        .current_dir(&work)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .env_remove("EMAIL")
        .output()
        .expect("run binary");
    assert_eq!(o.status.code(), Some(0), "reset without identity: {o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), "", "reset printed a diagnostic");

    // The reflog line landed, with the synthesized `<name> <email> <time> <zone>`
    // shape git's `fmt_ident()` builds.
    let log = std::fs::read_to_string(work.join(".git/logs/HEAD")).expect("reflog written");
    let last = log.lines().next_back().expect("a reflog line");
    assert!(last.ends_with("\treset: moving to HEAD"), "reflog line: {last}");
    let ident = last.split('\t').next().unwrap();
    assert!(ident.contains(" <") && ident.contains("> "), "no synthesized ident: {last}");

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}
