//! `%(push)` and `%(push:remoteref)` for a branch with no `branch.<name>.remote`.
//!
//! `%(push)` is `branch_get_push()` (remote.c:1963-2042): the push remote is
//! `remotes_pushremote_for_branch()`'s, which falls back to the sole configured
//! remote or `origin`, and under `push.default=current` or `matching` the branch
//! maps through that remote's fetch refspecs, so `feature` pushes to
//! `refs/remotes/origin/feature` whether or not that ref exists (`%(push:track)`
//! is then `[gone]`). `%(push:remoteref)` is `remote_ref_for_branch(branch, 1)`
//! (remote.c:737-749) on the same remote. zvcs asked gitoxide, which knows no
//! push remote for such a branch, and printed all of these empty.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
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

#[test]
fn push_atoms_fall_back_to_the_default_push_remote() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-fer-push-default-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let up = root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    let g = |dir: &Path, args: &[&str]| {
        let (_, err, code) = run(stock, dir, &home, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    g(&up, &["init", "-q", "-b", "main", "."]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "one"]);
    g(&root, &["clone", "-q", "up", "work"]);
    let work = root.join("work");
    g(&work, &["commit", "-q", "--allow-empty", "-m", "two"]);
    g(&work, &["branch", "feature"]);
    g(&work, &["branch", "-q", "--track", "tracked", "origin/main"]);

    let format = "--format=%(refname:short)|%(push)|%(push:short)|%(push:remotename)|%(push:remoteref)|%(push:track)|%(push:trackshort)";
    let configs: [&[&str]; 8] = [
        &["-c", "push.default=current"],
        &["-c", "push.default=matching"],
        &["-c", "push.default=simple"],
        &["-c", "push.default=upstream"],
        &["-c", "push.default=nothing"],
        &["-c", "remote.origin.push=refs/heads/*:refs/heads/up/*"],
        &["-c", "remote.pushDefault=origin", "-c", "push.default=current"],
        &["-c", "remote.origin.mirror=true"],
    ];
    for config in configs {
        let mut args = config.to_vec();
        args.extend_from_slice(&["for-each-ref", format, "refs/heads"]);
        assert_eq!(run(BIN, &work, &home, &args), run(stock, &work, &home, &args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
