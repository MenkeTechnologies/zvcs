//! A submodule's default remote is the one carrying its `.gitmodules` url, with
//! `url.<base>.insteadOf` applied to that url before the comparison.
//!
//! `get_default_remote_submodule()` (builtin/submodule--helper.c:77-119,
//! v2.56.0) looks the url up with `repo_remote_from_url()` first and only falls
//! back to `repo_get_default_remote()` (the current branch's remote, else
//! `origin`) when no remote matches. 2.56 made `repo_remote_from_url()` run the
//! url through `alias_url()` (remote.c:1840-1861), so a `.gitmodules` url
//! spelled with an alias now finds the remote configured with the expansion.
//! zvcs skipped the lookup altogether, so a submodule on a detached HEAD whose
//! remote is not called `origin` reported — and `submodule sync` wrote —
//! `origin`.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `super` with `sub` added from `up`; inside `sub`, HEAD is detached,
    /// `origin` is renamed to `upstream` and an unrelated `aaa` comes first.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-sm-default-remote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root: std::fs::canonicalize(&root).unwrap() };
        f.git(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.git(&f.root.join("up"), &["commit", "-q", "--allow-empty", "-m", "u"]);
        f.git(&f.root, &["init", "-q", "-b", "main", "super"]);
        let up = f.root.join("up");
        f.git(
            &f.sup(),
            &["-c", "protocol.file.allow=always", "submodule", "add", "-q", up.to_str().unwrap(), "sub"],
        );
        f.git(&f.sup(), &["commit", "-q", "-m", "s"]);
        let sub = f.sup().join("sub");
        f.git(&sub, &["checkout", "-q", "--detach"]);
        f.git(&sub, &["remote", "rename", "origin", "upstream"]);
        f.git(&sub, &["remote", "add", "aaa", f.root.join("other").to_str().unwrap()]);
        f
    }

    fn sup(&self) -> PathBuf {
        self.root.join("super")
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn default_remote(&self) -> String {
        self.git(&self.sup(), &["submodule--helper", "get-default-remote", "sub"])
    }
}

#[test]
fn the_remote_carrying_the_gitmodules_url_wins_over_origin() {
    let f = Fixture::new("url");
    assert_eq!(f.default_remote(), "upstream\n");

    // No remote carries the url: back to `repo_get_default_remote()`, which is
    // `origin` on a detached HEAD.
    let elsewhere = f.root.join("elsewhere");
    f.git(
        &f.sup(),
        &["config", "-f", ".gitmodules", "submodule.sub.url", elsewhere.to_str().unwrap()],
    );
    assert_eq!(f.default_remote(), "origin\n");
}

#[test]
fn an_aliased_gitmodules_url_is_expanded_before_the_lookup() {
    let f = Fixture::new("alias");
    f.git(&f.sup(), &["config", "-f", ".gitmodules", "submodule.sub.url", "short:up"]);
    let base = format!("{}/", f.root.display());
    f.git(&f.sup().join("sub"), &["config", &format!("url.{base}.insteadOf"), "short:"]);
    assert_eq!(f.default_remote(), "upstream\n");

    // `submodule sync` rewrites that remote's url, not `origin`'s.
    assert_eq!(
        f.git(&f.sup(), &["submodule", "sync"]),
        "Synchronizing submodule url for 'sub'\n"
    );
    assert_eq!(
        f.git(&f.sup().join("sub"), &["config", "--get-regexp", r"^remote\..*\.url"]),
        format!("remote.upstream.url short:up\nremote.aaa.url {}/other\n", f.root.display())
    );
}
