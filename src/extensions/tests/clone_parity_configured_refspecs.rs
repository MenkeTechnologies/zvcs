//! A clone fetches through the `remote.<name>.fetch` refspecs the configuration already holds.
//!
//! `remote_get_early(remote_name)` reads them — from a `-c` pair `write_config()` stored, a template's
//! `config`, the global file or `git -c` — and only then is the clone's own refspec appended
//! (builtin/clone.c:1316-1321). `wanted_peer_refs()` maps the refs through each refspec in turn
//! (:465-466), so an extra refspec creates the refs it maps, an exact one naming no advertised ref dies
//! `couldn't find remote ref` (remote.c:2144-2157), and one that maps a ref the clone's own refspec maps
//! too queues it twice in `write_remote_refs()`, which dies `multiple updates for ref` (refs.c:2574-2593).
//! A single-branch clone applies them to its one ref only (:451-463), and `--revision` checks out the first
//! ref they map (:1547-1548). A template's `core.logAllRefUpdates = false` holds for the refs the clone
//! writes, and its `[remote "<name>"]` section is where `url` and `fetch` go.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `src` has `main` (two commits), `side` and the tags `v1` (light) and `v2` (annotated) on its first
    /// commit; `home` holds a global config with one extra refspec; `tpl-log` and `tpl-remote` are templates.
    fn new(stock: &'static str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-clone-configured-refspecs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root, stock };
        for dir in ["src", "home", "tpl-log", "tpl-remote"] {
            std::fs::create_dir_all(f.root.join(dir)).unwrap();
        }
        std::fs::write(f.root.join("home/.gitconfig"), "[remote \"origin\"]\n\tfetch = +refs/tags/v1:refs/g/v1\n").unwrap();
        std::fs::write(f.root.join("tpl-log/config"), "[core]\n\tlogallrefupdates = false\n").unwrap();
        std::fs::write(
            f.root.join("tpl-remote/config"),
            "[remote \"origin\"]\n\turl = x\n\tfetch = +refs/tags/*:refs/remotes/origin/tags/*\n",
        )
        .unwrap();
        let src = f.root.join("src");
        let git = |args: &[&str]| {
            let (_, err, code) = f.run(stock, &src, &["-c", "maintenance.auto=false"], args);
            assert_eq!(code, 0, "{args:?}: {err}");
        };
        git(&["init", "-q", "-b", "main", "."]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&["branch", "side"]);
        git(&["tag", "v1"]);
        git(&["tag", "-a", "-m", "t", "v2"]);
        git(&["commit", "-q", "--allow-empty", "-m", "two"]);
        f
    }

    fn run(&self, bin: &str, cwd: &Path, pre: &[&str], args: &[&str]) -> (String, String, i32) {
        let out = Command::new(bin)
            .args(pre)
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.root.join("home"))
            .env_remove("GIT_CONFIG_GLOBAL")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_TEMPLATE_DIR")
            .env("GIT_EDITOR", "true")
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
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// Run `git <pre> clone <args>` with `bin` into a fresh directory and describe what it left: the
    /// output, the exit code, and — read by stock git — the refs, the config and the reflogs.
    fn clone(&self, bin: &str, pre: &[&str], args: &[&str]) -> String {
        let work = self.root.join(if bin == ZVCS { "z" } else { "s" });
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let mut full = vec!["clone"];
        full.extend_from_slice(args);
        let (out, err, code) = self.run(bin, &work, pre, &full);
        let mut report = format!("{out}{err}exit {code}\n");
        let dst = work.join("dst");
        if dst.exists() {
            let inspect = |args: &[&str]| self.run(self.stock, &dst, &[], args).0;
            let git_dir = inspect(&["rev-parse", "--git-dir"]);
            report += &inspect(&["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]);
            report += &std::fs::read_to_string(dst.join(git_dir.trim()).join("config")).unwrap();
            report += &inspect(&["reflog", "list"]);
        }
        report.replace(self.root.to_str().unwrap(), "$R")
    }

    fn assert_same(&self, pre: &[&str], args: &[&str]) {
        assert_eq!(self.clone(ZVCS, pre, args), self.clone(self.stock, pre, args), "git {pre:?} clone {args:?}");
    }
}

#[test]
fn configured_fetch_refspecs_take_part_in_the_clone() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "refspecs");
    let src = f.root.join("src");
    let src = src.to_str().unwrap();
    let url = format!("file://{src}");
    let cases: [(&[&str], Vec<&str>); 14] = [
        // The extra refspec maps the tags a second time; the global one maps `v1`.
        (&[], vec!["-c", "remote.origin.fetch=+refs/tags/*:refs/remotes/origin/tags/*", src, "dst"]),
        (&["-c", "remote.origin.fetch=+refs/heads/side:refs/c/side"], vec!["--bare", src, "dst"]),
        (&[], vec!["-o", "up", "--no-tags", "-c", "remote.up.fetch=+refs/heads/*:refs/u/*", &url, "dst"]),
        (&[], vec!["--mirror", "-c", "remote.origin.fetch=+refs/heads/main:refs/x/main", src, "dst"]),
        // The same refspec as the clone's own, or the tags `TAG_REFSPEC` maps again.
        (&[], vec!["-c", "remote.origin.fetch=+refs/heads/*:refs/remotes/origin/*", src, "dst"]),
        (&[], vec!["-c", "remote.origin.fetch=+refs/tags/*:refs/tags/*", &url, "dst"]),
        // Two different remote refs onto one local ref.
        (&[], vec!["-c", "remote.origin.fetch=+refs/heads/side:refs/remotes/origin/main", src, "dst"]),
        // An exact refspec naming no advertised ref, an invalid one, and a partial destination.
        (&[], vec!["-c", "remote.origin.fetch=+refs/heads/nope:refs/n", src, "dst"]),
        (&[], vec!["-c", "remote.origin.fetch=refs/heads/*:refs/x", src, "dst"]),
        (&[], vec!["-c", "remote.origin.fetch=side:foo", src, "dst"]),
        // A single-branch clone sees only its one ref: the wildcard maps `main` alone, and `side` is not there.
        (&[], vec!["--single-branch", "-c", "remote.origin.fetch=+refs/heads/*:refs/w/*", &url, "dst"]),
        (&[], vec!["--single-branch", "-b", "side", "-c", "remote.origin.fetch=+refs/heads/main:refs/m", src, "dst"]),
        // `--revision` checks out the first ref mapped, which the configured refspec mapped.
        (&[], vec!["--revision", "refs/heads/side", "-c", "remote.origin.fetch=+refs/heads/*:refs/w/*", src, "dst"]),
        (&[], vec!["--revision", "refs/heads/nope", "-c", "remote.origin.fetch=+refs/heads/*:refs/w/*", src, "dst"]),
    ];
    for (pre, args) in &cases {
        f.assert_same(pre, args);
    }
}

#[test]
fn a_template_config_holds_for_the_whole_clone() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "template");
    let src = f.root.join("src");
    let src = src.to_str().unwrap();
    let log = format!("--template={}", f.root.join("tpl-log").display());
    let remote = format!("--template={}", f.root.join("tpl-remote").display());
    // No reflog for any ref the clone writes.
    f.assert_same(&[], &[&log, src, "dst"]);
    f.assert_same(&[], &["--mirror", &log, src, "dst"]);
    // `url` replaces the template's, `fetch` joins its refspec, which mapped the tags.
    f.assert_same(&[], &[&remote, src, "dst"]);
    f.assert_same(&[], &["--bare", &remote, "-c", "a.b=c", src, "dst"]);
}
