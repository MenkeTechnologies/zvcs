//! `git clone` honors `clone.defaultRemoteName` from global config: the created
//! remote and its remote-tracking refs use that name instead of `origin`, and
//! the written `.git/config` matches real git byte-for-byte on the keys the
//! config controls (`remote.<name>.fetch`, `branch.<head>.remote`). Regression
//! guard against the config being ignored (remote always `origin`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A fresh tempdir containing an upstream repo (one commit on `main`, plus a
/// `feature` branch) and an empty `home` for the global gitconfig. Returns
/// `(root, up, home)`.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-clonecfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let up = root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "-q", "-b", "main"]);
    git(&up, &["config", "user.email", "t@e.x"]);
    git(&up, &["config", "user.name", "t"]);
    std::fs::write(up.join("f"), "x\n").unwrap();
    git(&up, &["add", "f"]);
    git(&up, &["commit", "-q", "-m", "c0"]);
    git(&up, &["branch", "feature"]);

    (root, up, home)
}

/// Set a key in the global (HOME) gitconfig used by the clone under test.
fn set_global(home: &Path, key: &str, val: &str) {
    let path = home.join(".gitconfig");
    assert!(
        Command::new("git")
            .args(["config", "--file"])
            .arg(&path)
            .args([key, val])
            .status()
            .unwrap()
            .success(),
        "setting global {key} failed"
    );
}

/// Clone `up` into `dst` under `home`'s global config with `GIT_CONFIG_NOSYSTEM`
/// so only the fixture's config participates, using the given clone binary.
fn clone_with(bin: &str, cwd: &Path, home: &Path, up: &Path, dst: &str) -> Output {
    Command::new(bin)
        .args(["clone", "-q", up.to_str().unwrap(), dst])
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

/// Read a single key from a cloned repo's `.git/config`.
fn cfg_get(repo: &Path, key: &str) -> String {
    let out = Command::new("git")
        .args(["config", "--file"])
        .arg(repo.join(".git/config"))
        .args(["--get", key])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// True iff `refs/remotes/<remote>/<branch>` exists in the cloned repo.
fn remote_ref_exists(repo: &Path, remote: &str, branch: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/remotes/{remote}/{branch}")])
        .current_dir(repo)
        .status()
        .unwrap()
        .success()
}

#[test]
fn default_remote_name_from_config_matches_git() {
    let (root, up, home) = fixture("defaultname");
    set_global(&home, "clone.defaultRemoteName", "upstream");

    // zvcs and real git both clone under the same global config.
    let z = clone_with(BIN, &root, &home, &up, "zc");
    assert!(z.status.success(), "zvcs clone failed: {}", String::from_utf8_lossy(&z.stderr));
    let g = clone_with("git", &root, &home, &up, "gc");
    assert!(g.status.success(), "git clone failed: {}", String::from_utf8_lossy(&g.stderr));

    let zc = root.join("zc");
    let gc = root.join("gc");

    // The config-named remote is created (not `origin`).
    assert_eq!(cfg_get(&zc, "branch.main.remote"), "upstream");
    assert_eq!(cfg_get(&zc, "remote.upstream.fetch"), "+refs/heads/*:refs/remotes/upstream/*");
    assert_eq!(cfg_get(&zc, "branch.main.merge"), "refs/heads/main");
    // No stray `origin` section survives.
    assert_eq!(cfg_get(&zc, "remote.origin.url"), "", "origin must not exist when config renames it");

    // Byte-for-byte agreement with real git on the keys clone.defaultRemoteName drives.
    for key in ["branch.main.remote", "remote.upstream.fetch", "branch.main.merge"] {
        assert_eq!(cfg_get(&zc, key), cfg_get(&gc, key), "mismatch on {key}");
    }

    // Remote-tracking refs use the configured name for every upstream branch.
    assert!(remote_ref_exists(&zc, "upstream", "main"), "refs/remotes/upstream/main missing");
    assert!(remote_ref_exists(&zc, "upstream", "feature"), "refs/remotes/upstream/feature missing");
    assert!(!remote_ref_exists(&zc, "origin", "main"), "no origin/* refs may exist");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn absent_config_defaults_to_origin() {
    let (root, up, home) = fixture("origin");
    // No clone.defaultRemoteName set anywhere.

    let z = clone_with(BIN, &root, &home, &up, "zc");
    assert!(z.status.success(), "zvcs clone failed: {}", String::from_utf8_lossy(&z.stderr));
    let zc = root.join("zc");

    // Default remote is `origin`, proving the config is what flips the name above.
    assert_eq!(cfg_get(&zc, "branch.main.remote"), "origin");
    assert_eq!(cfg_get(&zc, "remote.origin.fetch"), "+refs/heads/*:refs/remotes/origin/*");
    assert!(remote_ref_exists(&zc, "origin", "main"), "refs/remotes/origin/main missing");
    assert!(!remote_ref_exists(&zc, "upstream", "main"), "no upstream/* refs without config");

    let _ = std::fs::remove_dir_all(&root);
}
