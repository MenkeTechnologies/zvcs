//! A command-line `includeIf.<cond>.path` whose condition holds and whose path
//! is relative.
//!
//! `git_config_include()` (config.c:416-448) sends an `includeif.<cond>.path`
//! through `handle_path_include()` (:142-191) when
//! `include_condition_is_true()` (:396-414) holds, and a command-line value
//! has no file to be relative to: `relative config includes must come from
//! files`, then `unable to parse command-line config` at 128
//! (`do_git_config_sequence()`, :1600-1602). A false condition is never
//! followed. The conditions are `include_by_gitdir()` (:238-295),
//! `include_by_branch()` (:297-320), and `hasconfig:remote.*.url:`, which
//! `populate_remote_urls()` evaluates unconditionally.
//!
//! zvcs followed only `include.path` this way; an `includeIf` with a true
//! condition was dropped by one config reader and surfaced as gitoxide's own
//! error at exit 1 by another.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A repository at `r/` on branch `feat/x` with one commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-config-cmdline-include-if-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("r")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("r");
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "feat/x", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "i"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// `git -c <cond>.path=rel rev-parse --git-dir`.
    fn with_condition(&self, cond: &str) -> (String, String, i32) {
        self.run(&["-c", &format!("{cond}.path=rel"), "rev-parse", "--git-dir"])
    }
}

fn refused() -> (String, String, i32) {
    (
        String::new(),
        "error: relative config includes must come from files\nfatal: unable to parse command-line config\n".into(),
        128,
    )
}

fn followed_nothing() -> (String, String, i32) {
    (".git\n".into(), String::new(), 0)
}

#[test]
fn a_true_condition_refuses_the_relative_path() {
    let f = Fixture::new("true");
    let top = f.work.display().to_string();
    let upper = top.to_uppercase();
    for cond in [
        format!("includeIf.gitdir:{top}/.git"),
        format!("includeIf.gitdir:{top}/"),
        "includeIf.gitdir:r/.git".into(),
        "includeIf.gitdir:r/".into(),
        "includeIf.gitdir:~/r/".into(),
        format!("includeIf.gitdir/i:{upper}/"),
        "includeIf.onbranch:feat/x".into(),
        "includeIf.onbranch:feat/".into(),
        "includeIf.onbranch:feat/*".into(),
        "includeIf.hasconfig:remote.*.url:nope".into(),
    ] {
        assert_eq!(f.with_condition(&cond), refused(), "{cond}");
    }
    // Every verb that reads the configuration, not only one.
    for verb in [&["status", "-s"][..], &["hash-object", "--stdin"], &["config", "--list"]] {
        let mut args = vec!["-c", "includeIf.onbranch:feat/x.path=rel"];
        args.extend_from_slice(verb);
        assert_eq!(f.run(&args), refused(), "{verb:?}");
    }
    // A valueless one is `handle_path_include()`'s nonbool refusal, which names
    // `include.path` whatever the key was.
    assert_eq!(
        f.run(&["-c", "includeIf.onbranch:feat/x.path", "rev-parse", "--git-dir"]),
        (
            String::new(),
            "error: missing value for 'include.path'\nfatal: unable to parse command-line config\n".into(),
            128
        )
    );
}

#[test]
fn a_false_or_unknown_condition_is_never_followed() {
    let f = Fixture::new("false");
    let top = f.work.display().to_string();
    let upper = top.to_uppercase();
    for cond in [
        format!("includeIf.gitdir:{top}"),
        "includeIf.gitdir:nope/".into(),
        format!("includeIf.gitdir:{upper}/"),
        "includeIf.onbranch:feat".into(),
        "includeIf.onbranch:f*".into(),
        "includeIf.hasconfig:other:nope".into(),
        "includeIf.bogus".into(),
    ] {
        assert_eq!(f.with_condition(&cond), followed_nothing(), "{cond}");
    }
    // A detached `HEAD` is on no branch.
    f.run(&["checkout", "-q", "--detach"]);
    assert_eq!(f.with_condition("includeIf.onbranch:feat/x"), followed_nothing());
}
