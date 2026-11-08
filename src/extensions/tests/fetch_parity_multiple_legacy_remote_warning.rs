//! `fetch --multiple` and `remote update` over remotes defined by `$GIT_DIR/remotes/<name>`
//! and `$GIT_DIR/branches/<name>`, against stock git.
//!
//! `cmd_fetch()` builds its list with `add_remote_or_group()` (builtin/fetch.c:2762-2767),
//! which calls `remote_get(name)` for a name that is not a group (remote.c:2216). That read
//! prints `warn_about_deprecated_remote_type()`'s warning (remote.c:334-347) in the parent,
//! once per process, and each `fetch --append` child it spawns reads the file and warns
//! again. So `remote update leg` warns twice; a group's members are never looked up by the
//! parent and warn once each, in their children. zvcs's parent never read the file.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        let up = side.run_in(&side.root, &[], &["init", "-q", "--bare", "-b", "main", "up.git"]);
        assert_eq!(up.code, 0, "{up:?}");
        let push = side.git(&["push", "-q", "../up.git", "main"]);
        assert_eq!(push.code, 0, "{push:?}");
        side.write(".git/remotes/leg", "URL: ../up.git\nPull: refs/heads/main:refs/remotes/leg/main\n");
        side.write(".git/remotes/leg2", "URL: ../up.git\nPull: refs/heads/main:refs/remotes/leg2/main\n");
        side.write(".git/branches/br", "../up.git#main\n");
        side.git(&["config", "remotes.grp", "leg br"]);
        side.git(&["config", "remote.cfg.url", "../up.git"]);
    }
    Some((s, z))
}

#[track_caller]
fn same(s: &Side, z: &Side, args: &[&str]) {
    let mut full = vec!["-c", "maintenance.auto=false"];
    full.extend_from_slice(args);
    assert_eq!(z.git(&full), s.git(&full), "git {args:?}: left is zvcs, right is stock");
    let refs = ["for-each-ref"];
    assert_eq!(z.git(&refs), s.git(&refs), "refs after git {args:?}");
}

#[test]
fn the_parent_reads_each_named_legacy_remote_before_its_child_does() {
    let Some((s, z)) = world("fetch-multiple-legacy") else { return };
    let warnings = s.git(&["-c", "maintenance.auto=false", "remote", "update", "leg"]).stderr;
    assert_eq!(warnings.matches("which is nominated for removal").count(), 2, "{warnings}");
    z.git(&["-c", "maintenance.auto=false", "remote", "update", "leg"]);
    for side in [&s, &z] {
        side.git(&["update-ref", "-d", "refs/remotes/leg/main"]);
    }
    same(&s, &z, &["remote", "update", "leg"]);
    same(&s, &z, &["remote", "update", "leg", "leg2"]);
    same(&s, &z, &["remote", "update", "br"]);
    same(&s, &z, &["fetch", "--multiple", "leg"]);
    same(&s, &z, &["fetch", "--multiple", "cfg", "leg", "leg2"]);
    same(&s, &z, &["fetch", "--multiple", "cfg", "leg", "nope"]);
}

#[test]
fn a_group_and_a_single_fetch_warn_once_per_remote() {
    let Some((s, z)) = world("fetch-group-legacy") else { return };
    same(&s, &z, &["remote", "update", "grp"]);
    same(&s, &z, &["fetch", "grp"]);
    same(&s, &z, &["fetch", "leg"]);
    same(&s, &z, &["push", "--dry-run", "leg", "HEAD:refs/heads/x"]);
}
