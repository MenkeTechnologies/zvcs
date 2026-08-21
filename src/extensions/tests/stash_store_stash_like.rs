//! `git stash store` validates its argument before it writes `refs/stash`.
//!
//! `do_store_stash()` opens with the assertion, not with the ref update:
//!
//! ```c
//! oid_to_hex_r(revision, w_commit);
//! assert_stash_like(&info, revision);
//! ```
//!
//! (`builtin/stash.c:1132-1134`), and the assertion is four resolutions:
//!
//! ```c
//! if (get_oidf(&info->b_commit, "%s^1", revision) ||
//!     get_oidf(&info->w_tree, "%s:", revision) ||
//!     get_oidf(&info->b_tree, "%s^1:", revision) ||
//!     get_oidf(&info->i_tree, "%s^2:", revision))
//!         die(_("'%s' is not a stash-like commit"), revision);
//! ```
//!
//! (`builtin/stash.c:216-223`.) `%s^2` is the one that bites: only the two- or
//! three-parent merge `git stash create` builds satisfies it, so an ordinary
//! commit — `git stash store -m x HEAD` — is a fatal and exit 128 with no ref
//! written. Accepting it left `refs/stash` pointing at something the reflog
//! walker cannot make an entry out of, and for a tree or blob argument at
//! something that is not a commit at all.
//!
//! Three further details, all captured from stock git 2.55.0:
//!
//! * The `%s` is the **hex object id**, never the spelling the caller used:
//!   `revision` comes from `oid_to_hex_r(w_commit)`. So `store HEAD` names the
//!   commit's id.
//! * A non-commit argument prints **two** lines — `lookup_commit_reference()`'s
//!   `error: object <oid> is a <kind>, not a commit` ahead of the `die`.
//! * `-q` suppresses neither. It gates `store_stash()`'s own `Cannot update`
//!   line (`builtin/stash.c:1181-1183`) and nothing else; `die()` is not quiet.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

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
    /// Two commits, and one real stash entry dropped from `refs/stash` again so
    /// its commit is still readable while the ref is free.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stashstore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work };

        fx.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(fx.work.join("a.txt"), "one\n").unwrap();
        fx.ok(&["add", "a.txt"]);
        fx.ok(&["commit", "-q", "-m", "one"]);
        std::fs::write(fx.work.join("a.txt"), "two\n").unwrap();
        fx.ok(&["commit", "-q", "-a", "-m", "two"]);
        fx
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(["-c", "user.email=t@e.co", "-c", "user.name=t"])
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run binary")
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert!(out.status.success(), "setup git {args:?}: {out:?}");
        out
    }

    fn rev(&self, spec: &str) -> String {
        String::from_utf8_lossy(&self.ok(&["rev-parse", spec]).stdout).trim_end().to_string()
    }

    /// Build a genuine stash commit and take `refs/stash` back off it, leaving
    /// a stash-like object with the ref free for `store` to claim.
    fn orphan_stash_commit(&self) -> String {
        std::fs::write(self.work.join("a.txt"), "dirty\n").unwrap();
        self.ok(&["stash", "push", "-q", "-m", "seed"]);
        let id = self.rev("refs/stash");
        self.ok(&["update-ref", "-d", "refs/stash"]);
        id
    }

    fn stash_ref(&self) -> Option<String> {
        let out = self.run(&["rev-parse", "--verify", "--quiet", "refs/stash"]);
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn an_ordinary_commit_is_refused_and_writes_nothing() {
    let fx = Fixture::new("commit");
    let head = fx.rev("HEAD");
    let root = fx.rev("HEAD~1");

    // `HEAD` (one parent) and the root commit (none) both fail on `%s^2`, and
    // both report the hex id rather than the spelling that was typed.
    for (spec, oid) in [("HEAD", &head), (head.as_str(), &head), (root.as_str(), &root)] {
        let out = fx.run(&["stash", "store", "-m", "x", spec]);
        assert_eq!(out.status.code(), Some(128), "git stash store {spec}: {out:?}");
        assert_eq!(
            stderr(&out),
            format!("fatal: '{oid}' is not a stash-like commit\n"),
            "git stash store {spec} stderr"
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "git stash store {spec} stdout");
        assert_eq!(fx.stash_ref(), None, "git stash store {spec} wrote refs/stash anyway");
    }
}

#[test]
fn a_non_commit_object_reports_its_kind_first() {
    let fx = Fixture::new("kinds");
    let tree = fx.rev("HEAD^{tree}");
    let blob = fx.rev("HEAD:a.txt");

    for (oid, kind) in [(&tree, "tree"), (&blob, "blob")] {
        let out = fx.run(&["stash", "store", "-m", "x", oid]);
        assert_eq!(out.status.code(), Some(128), "git stash store {oid}: {out:?}");
        assert_eq!(
            stderr(&out),
            format!(
                "error: object {oid} is a {kind}, not a commit\nfatal: '{oid}' is not a stash-like commit\n"
            ),
            "git stash store {oid} stderr"
        );
        assert_eq!(fx.stash_ref(), None, "git stash store {oid} wrote refs/stash anyway");
    }
}

#[test]
fn a_well_formed_but_absent_id_reaches_the_assertion() {
    let fx = Fixture::new("absent");

    // `repo_get_oid()` takes a full-length hex name as the id without consulting
    // the object database, so this gets past `store_stash()`'s resolution and
    // fails the stash-like assertion — 128 and the `die`, not the `Cannot
    // update` line and 1. Rejecting it during resolution reported the wrong one.
    let absent = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let out = fx.run(&["stash", "store", "-m", "x", absent]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(stderr(&out), format!("fatal: '{absent}' is not a stash-like commit\n"));
    assert_eq!(fx.stash_ref(), None);

    // A name that resolves to nothing at all *is* the `Cannot update` line, on
    // stderr, with exit 1 — the other half of `store_stash()`'s branch.
    let out = fx.run(&["stash", "store", "-m", "x", "no-such-ref"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(stderr(&out), "Cannot update refs/stash with no-such-ref\n");
    assert_eq!(fx.stash_ref(), None);
}

#[test]
fn quiet_does_not_soften_the_assertion() {
    let fx = Fixture::new("quiet");
    let head = fx.rev("HEAD");

    // `-q` gates the `Cannot update` line only; the `die` is unconditional.
    for args in [
        &["stash", "store", "-q", "-m", "x", head.as_str()][..],
        &["stash", "store", "--quiet", "-m", "x", head.as_str()][..],
    ] {
        let out = fx.run(args);
        assert_eq!(out.status.code(), Some(128), "git {args:?}: {out:?}");
        assert_eq!(stderr(&out), format!("fatal: '{head}' is not a stash-like commit\n"));
    }

    // The same flag *does* silence the resolution failure.
    let out = fx.run(&["stash", "store", "-q", "-m", "x", "no-such-ref"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(stderr(&out), "", "{out:?}");
}

#[test]
fn a_real_stash_commit_is_still_stored() {
    let fx = Fixture::new("accept");
    let stash = fx.orphan_stash_commit();
    assert_eq!(fx.stash_ref(), None, "fixture did not free refs/stash");

    // The assertion must not have become "refuse everything": the commit
    // `git stash push` built has the two parents and the three trees, so it
    // passes and lands with the message the caller gave.
    let out = fx.run(&["stash", "store", "-m", "restored", &stash]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stderr(&out), "", "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{out:?}");
    assert_eq!(fx.stash_ref().as_deref(), Some(stash.as_str()));

    let listed = fx.ok(&["stash", "list"]);
    assert_eq!(String::from_utf8_lossy(&listed.stdout), "stash@{0}: restored\n");
}
