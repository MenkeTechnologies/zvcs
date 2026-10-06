//! A push to a URL or path updates the tracking refs of the remote that URL
//! belongs to.
//!
//! `transport_push()` updates tracking refs through
//! `repo_remote_for_push_tracking()` (transport.c:1601-1607, remote.c:1916-1945):
//! for an unconfigured (anonymous) remote it looks for the one configured remote
//! whose push URL list holds exactly the push URL it was given — after the
//! `insteadOf`/`pushInsteadOf` rewriting both sides get — and uses that remote's
//! fetch refspecs; two such remotes, and it keeps the anonymous one, which maps
//! nothing. zvcs always used the anonymous remote, so `git push ../r.git :b` left
//! `refs/remotes/origin/b` behind and `git push --mirror <url>` left every
//! remote-tracking ref of a branch it deleted. Expectations measured from stock
//! git 2.56.0 under the same environment.

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
    /// `work` on `main`, `origin` = `../r.git`, and `a1`..`a4` pushed there and tracked.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-url-tracking-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "--bare", "../r.git"]);
        f.ok(&["init", "-q", "-b", "main", "."]);
        f.ok(&["commit", "-q", "--allow-empty", "-m", "a"]);
        f.ok(&["remote", "add", "origin", "../r.git"]);
        f.ok(&["push", "-q", "origin", "main", "main:a1", "main:a2", "main:a3", "main:a4"]);
        f
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn tracking(&self) -> String {
        self.ok(&["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin"])
    }
}

#[test]
fn a_push_to_the_remotes_url_updates_its_tracking_refs() {
    let f = Fixture::new("plain");
    f.ok(&["push", "-q", "../r.git", ":refs/heads/a1", "main:refs/heads/a5"]);
    assert_eq!(f.tracking(), "origin/a2\norigin/a3\norigin/a4\norigin/a5\norigin/main\n");

    // Two remotes with that push URL: no tracking remote at all.
    f.ok(&["remote", "add", "twin", "../r.git"]);
    f.ok(&["push", "-q", "../r.git", ":refs/heads/a2"]);
    assert_eq!(f.tracking(), "origin/a2\norigin/a3\norigin/a4\norigin/a5\norigin/main\n");
    f.ok(&["remote", "remove", "twin"]);

    // The given name is rewritten by `insteadOf` before the comparison.
    f.ok(&["config", "url.../r.insteadOf", "short"]);
    f.ok(&["push", "-q", "short.git", ":refs/heads/a3"]);
    assert_eq!(f.tracking(), "origin/a2\norigin/a4\norigin/a5\norigin/main\n");

    // A `pushurl` is what is compared, not the fetch `url`.
    f.ok(&["config", "remote.origin.pushurl", "../r.git"]);
    f.ok(&["config", "remote.origin.url", "elsewhere"]);
    f.ok(&["push", "-q", "../r.git", ":refs/heads/a4"]);
    assert_eq!(f.tracking(), "origin/a2\norigin/a5\norigin/main\n");
}

#[test]
fn a_mirror_push_to_the_url_drops_the_tracking_refs_of_what_it_deleted() {
    let f = Fixture::new("mirror");
    f.ok(&["push", "-q", "--mirror", "../r.git"]);
    assert_eq!(f.tracking(), "origin/main\n");
}
