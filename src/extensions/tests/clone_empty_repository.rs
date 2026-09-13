//! `git clone` of a repository with no commits, over the local path, the
//! transport (`--no-local`) and a bare destination.
//!
//! git decides a clone is empty from `mapped_refs` — what the remote's own fetch
//! refspecs selected (builtin/clone.c:443-471, :1560-1563). gitoxide adds an implicit
//! `HEAD:refs/remotes/<name>/HEAD` refspec that maps even an unborn `HEAD`, and counting
//! that mapping made every empty clone look non-empty: the warning was never printed,
//! and the checkout then complained that `HEAD` names a nonexistent ref.
//!
//! Every expectation below was measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, bindir: &Path, args: &[&str]) -> Output {
    let path = format!("{}:{}", bindir.display(), std::env::var("PATH").unwrap_or_default());
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("PATH", path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run binary")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn config(repo: &Path, home: &Path, bindir: &Path, key: &str) -> String {
    let out = run(repo, home, bindir, &["config", "--get-all", key]);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fixture {
    root: std::path::PathBuf,
    home: std::path::PathBuf,
    bindir: std::path::PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-clone-empty-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let bindir = root.join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bindir).unwrap();
        for name in ["git", "git-upload-pack", "git-receive-pack"] {
            std::os::unix::fs::symlink(BIN, bindir.join(name)).unwrap();
        }
        Fixture { root, home, bindir }
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        run(cwd, &self.home, &self.bindir, args)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn an_empty_source_warns_and_follows_its_unborn_head() {
    let fx = Fixture::new("unborn");
    // A branch name no default would produce, so HEAD can only come from the
    // remote's `unborn HEAD symref-target:` answer.
    fx.git(&fx.root, &["init", "-q", "-b", "trunk", "src"]);

    // Local: `clone_local()` prints `done.` after the warning (builtin/clone.c:371).
    let out = fx.git(&fx.root, &["clone", "src", "local"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "Cloning into 'local'...\nwarning: You appear to have cloned an empty repository.\ndone.\n"
    );
    let local = fx.root.join("local");
    assert_eq!(std::fs::read_to_string(local.join(".git/HEAD")).unwrap(), "ref: refs/heads/trunk\n");
    assert_eq!(config(&local, &fx.home, &fx.bindir, "branch.trunk.merge"), "refs/heads/trunk");
    // No ref was advertised, so `write_remote_refs()` never ran (builtin/clone.c:554).
    assert!(!local.join(".git/packed-refs").exists());

    // Transport: no `done.`, and nothing about a nonexistent ref.
    let out = fx.git(&fx.root, &["clone", "--no-local", "src", "transport"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "Cloning into 'transport'...\nwarning: You appear to have cloned an empty repository.\n"
    );
    assert!(!fx.root.join("transport/.git/packed-refs").exists());

    // Bare: same detection, no branch config.
    let out = fx.git(&fx.root, &["clone", "--bare", "src", "bare.git"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "Cloning into bare repository 'bare.git'...\nwarning: You appear to have cloned an empty repository.\ndone.\n"
    );
    assert_eq!(std::fs::read_to_string(fx.root.join("bare.git/HEAD")).unwrap(), "ref: refs/heads/trunk\n");

    // `warning()` ignores `-q`.
    let out = fx.git(&fx.root, &["clone", "-q", "src", "quiet"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "warning: You appear to have cloned an empty repository.\n");
}

#[test]
fn a_single_branch_clone_of_a_dangling_head_maps_nothing() {
    let fx = Fixture::new("dangling");
    let src = fx.root.join("src");
    fx.git(&fx.root, &["init", "-q", "-b", "other", "src"]);
    fx.git(&src, &["-c", "user.name=t", "-c", "user.email=t@e.co", "commit", "-q", "--allow-empty", "-m", "x"]);
    fx.git(&src, &["symbolic-ref", "HEAD", "refs/heads/gone"]);

    let out = fx.git(&fx.root, &["clone", "--single-branch", "--no-local", "src", "dst"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "Cloning into 'dst'...\nwarning: You appear to have cloned an empty repository.\n"
    );
    let dst = fx.root.join("dst");
    // `remote_head_points_at` is NULL, so `write_refspec_config()` writes no fetch
    // refspec (builtin/clone.c:810-823) ...
    assert_eq!(config(&dst, &fx.home, &fx.bindir, "remote.origin.fetch"), "");
    // ... but `refs` was not NULL — `refs/heads/other` was advertised — so the ref
    // transaction still created `packed-refs`.
    assert_eq!(
        std::fs::read_to_string(dst.join(".git/packed-refs")).unwrap(),
        "# pack-refs with: peeled fully-peeled sorted \n"
    );
}
