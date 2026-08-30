//! The fleet verbs that *change* things must obey the selector that narrows
//! them, because the cost of ignoring it is not a wrong listing but a wrong
//! write across every indexed repository.
//!
//! Four of them read the shared grammar's bare pattern and threw it away, using
//! the selector's leftovers only for their own argument. Measured on a two-repo
//! fixture before the fix:
//!
//!     git zclean -f alpha        deleted untracked files in alpha AND beta
//!     git zcommitall alpha -m m  committed in both
//!     git ztagall v9 alpha       tagged both
//!     git zcheckout main beta    checked out in both
//!
//! Each reported "2 ok ... (2 repos)" while the person running it had named one.
//!
//! `zclean`'s other guarantee is pinned here too: it runs `git clean -fd`, never
//! `-x`, so ignored files — build outputs, local env files — survive. That one
//! is a single character away from deleting a fleet's worth of untracked work
//! that was deliberately ignored.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(home: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn ok(out: &Output, what: &str) -> String {
    assert!(out.status.success(), "{what} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn both(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

fn subject(home: &Path, repo: &Path) -> String {
    ok(&run(home, repo, &["log", "-1", "--format=%s"]), "log").trim().to_string()
}

/// Two indexed repos, each with a tracked file, an ignored build output, an
/// untracked file and an untracked directory.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fmscope-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    for name in ["alpha", "beta"] {
        let r = root.join(name);
        std::fs::create_dir_all(r.join("build")).unwrap();
        std::fs::create_dir_all(r.join("junkdir")).unwrap();
        run(&home, &r, &["init", "-q", "-b", "main"]);
        run(&home, &r, &["config", "user.email", "t@example"]);
        run(&home, &r, &["config", "user.name", "T"]);
        std::fs::write(r.join(".gitignore"), b"build/\n").unwrap();
        std::fs::write(r.join("f.txt"), b"v\n").unwrap();
        run(&home, &r, &["add", ".gitignore", "f.txt"]);
        run(&home, &r, &["commit", "-q", "-m", "c0"]);
        std::fs::write(r.join("junk.txt"), b"junk\n").unwrap();
        std::fs::write(r.join("junkdir/x"), b"x\n").unwrap();
        std::fs::write(r.join("build/out.o"), b"artifact\n").unwrap();
    }
    run(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    let (a, b) = (root.join("alpha"), root.join("beta"));
    (root, home, a, b)
}

#[test]
fn zclean_deletes_only_in_the_repos_named() {
    let (root, home, alpha, beta) = fixture("clean");

    let out = both(&run(&home, &root, &["zclean", "-f", "alpha"]));
    assert!(out.contains("(1 repos)"), "the pattern must narrow the run:\n{out}");

    assert!(!alpha.join("junk.txt").exists(), "the named repo must be cleaned");
    assert!(!alpha.join("junkdir").exists(), "`clean -fd` must take untracked directories too");
    assert!(beta.join("junk.txt").exists(), "an unnamed repo must be left alone — this deletes files");
    assert!(beta.join("junkdir").exists(), "an unnamed repo must be left alone");

    // No `-x`: ignored files survive in the repo that WAS cleaned.
    assert!(alpha.join("build/out.o").exists(), "ignored files must survive `zclean -f` (no -x)");
    assert!(alpha.join("f.txt").exists(), "tracked files must survive");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zclean_still_refuses_without_the_force_flag() {
    let (root, home, alpha, _beta) = fixture("force");
    let out = run(&home, &root, &["zclean", "alpha"]);
    assert!(!out.status.success(), "zclean must refuse without -f");
    assert!(both(&out).contains("pass -f"), "the refusal must say what is missing:\n{}", both(&out));
    assert!(alpha.join("junk.txt").exists(), "nothing may be deleted by a refused run");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zcommitall_commits_only_in_the_repos_named() {
    let (root, home, alpha, beta) = fixture("commit");
    for r in [&alpha, &beta] {
        std::fs::write(r.join("f.txt"), b"changed\n").unwrap();
    }

    let out = both(&run(&home, &root, &["zcommitall", "alpha", "-m", "scoped"]));
    assert!(out.contains("(1 repos)"), "the pattern must narrow the run:\n{out}");
    assert_eq!(subject(&home, &alpha), "scoped", "the named repo must be committed");
    assert_eq!(subject(&home, &beta), "c0", "an unnamed repo must not be committed");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ztagall_and_zcheckout_take_their_positional_and_still_narrow() {
    let (root, home, alpha, beta) = fixture("tag");

    // `<tag>` first, then a pattern: the tag is the verb's own argument and the
    // rest of the bare tokens narrow the repo set.
    let out = both(&run(&home, &root, &["ztagall", "v9", "beta"]));
    assert!(out.contains("(1 repos)"), "ztagall must narrow on the trailing pattern:\n{out}");
    assert!(ok(&run(&home, &beta, &["tag"]), "tag").contains("v9"), "the named repo must be tagged");
    assert!(!ok(&run(&home, &alpha, &["tag"]), "tag").contains("v9"), "an unnamed repo must not be tagged");

    // Same shape for `zcheckout <branch> [pattern]`. `main` exists in both, so a
    // run that ignored the pattern would report two repos.
    let co = both(&run(&home, &root, &["zcheckout", "main", "alpha"]));
    assert!(co.contains("(1 repos)"), "zcheckout must narrow on the trailing pattern:\n{co}");

    let _ = std::fs::remove_dir_all(&root);
}
