//! `rebase -i -r` `merge` instructions against stock git: `do_merge()`
//! (sequencer.c:4167-4477) for a `merge` with no original commit, `merge -c`, an
//! octopus, a recreated merge under `--signoff`, and a conflicted stop with its
//! `--skip`; plus `todo_list_filter_update_refs()` after the sheet is edited.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::{Out, Side};

/// Both worlds with the same merge-shaped history on top of the shared one:
///
/// ```text
/// up:    base - up1                      (f line 5 = U)
/// main:  base - main1 - merge side       (side, reset to base: f line 3 = S)
/// side2: base - side2c                   (t)
/// side3: main1 - side3c                  (f line 5 = X)
/// ```
fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        let git = |args: &[&str]| {
            let out = side.git(args);
            assert_eq!(out.code, 0, "{args:?}: {out:?}");
        };
        side.write("f", "1\n2\n3\n4\n5\n");
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["checkout", "-q", "-b", "side2"]);
        side.write("t", "t\n");
        git(&["add", "t"]);
        git(&["commit", "-q", "-m", "side2c"]);
        git(&["checkout", "-q", "-B", "side", "main"]);
        side.write("f", "1\n2\nS\n4\n5\n");
        side.write("s", "s\n");
        git(&["add", "f", "s"]);
        git(&["commit", "-q", "-m", "side1"]);
        git(&["checkout", "-q", "main"]);
        side.write("m", "m\n");
        git(&["add", "m"]);
        git(&["commit", "-q", "-m", "main1"]);
        git(&["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        git(&["checkout", "-q", "-b", "side3", "main~1"]);
        side.write("f", "1\n2\n3\n4\nX\n");
        git(&["commit", "-q", "-am", "side3c"]);
        git(&["checkout", "-q", "-b", "up", "main~2"]);
        side.write("f", "1\n2\n3\n4\nU\n");
        side.write("u", "u\n");
        git(&["add", "f", "u"]);
        git(&["commit", "-q", "-m", "up1"]);
        git(&["checkout", "-q", "main"]);
    }
    Some((s, z))
}

/// `git rebase <args>` with `GIT_SEQUENCE_EDITOR` replacing the sheet by `todo`.
fn rebase(side: &Side, todo: &str, args: &[&str]) -> Out {
    let path = side.root.join("todo");
    std::fs::write(&path, todo).unwrap();
    let editor = format!("cp '{}'", path.display());
    let mut argv = vec!["rebase"];
    argv.extend_from_slice(args);
    side.git_env(&[("GIT_SEQUENCE_EDITOR", editor.as_str())], &argv)
}

/// What a finished or stopped rebase left: refs, history, status and state files.
fn post_state(side: &Side) -> Vec<String> {
    let mut out = Vec::new();
    for args in [
        &["for-each-ref"][..],
        &["log", "--format=%H %P%n%an %ae %ad%n%cn %ce %cd%n%B--", "-6", "HEAD"],
        &["status", "--porcelain"],
        &["reflog", "-6", "HEAD"],
    ] {
        out.push(format!("{:?}", side.git(args)));
    }
    for f in [
        "f",
        ".git/MERGE_HEAD",
        ".git/MERGE_MODE",
        ".git/MERGE_MSG",
        ".git/AUTO_MERGE",
        ".git/REBASE_HEAD",
        ".git/COMMIT_EDITMSG",
        ".git/rebase-merge/message",
        ".git/rebase-merge/author-script",
        ".git/rebase-merge/stopped-sha",
        ".git/rebase-merge/patch",
        ".git/rebase-merge/done",
        ".git/rebase-merge/git-rebase-todo",
        ".git/rebase-merge/rewritten-list",
        ".git/rebase-merge/update-refs",
    ] {
        out.push(format!("{f}: {:?}", side.read(f).map(|b| String::from_utf8_lossy(&b).into_owned())));
    }
    out
}

fn same(s: &Side, z: &Side, todo: &str, args: &[&str]) {
    assert_eq!(rebase(z, todo, args), rebase(s, todo, args));
    assert_eq!(post_state(&z), post_state(&s));
}

#[test]
fn merge_without_c_takes_its_message_from_the_heads_and_amends_in_the_editor() {
    let Some((s, z)) = world("rebase-merge-bare") else { return };
    let todo = "label onto\nreset onto\npick side\nlabel s\nreset onto\npick main~1\nmerge s\n";
    same(&s, &z, todo, &["-i", "-r", "up"]);
}

#[test]
fn merge_oneline_becomes_the_message() {
    let Some((s, z)) = world("rebase-merge-oneline") else { return };
    let todo =
        "label onto\nreset onto\npick side\nlabel s\nreset onto\npick main~1\nmerge s # my merge message\n";
    same(&s, &z, todo, &["-i", "-r", "up"]);
}

#[test]
fn octopus_merge_runs_the_octopus_child() {
    let Some((s, z)) = world("rebase-merge-octopus") else { return };
    let todo = "label onto\nreset onto\npick side2\nlabel t\nreset onto\npick side\nlabel s\n\
                reset onto\npick main~1\nmerge s t\n";
    same(&s, &z, todo, &["-i", "-r", "up"]);
}

#[test]
fn merge_c_rewords_a_recreated_merge() {
    let Some((s, z)) = world("rebase-merge-c") else { return };
    let todo = "label onto\nreset onto\npick main~1\nmerge -c main side\n";
    same(&s, &z, todo, &["-i", "-r", "up"]);
}

#[test]
fn signoff_does_not_sign_a_recreated_merge() {
    let Some((s, z)) = world("rebase-merge-signoff") else { return };
    let todo =
        "label onto\nreset onto\npick side\nlabel s\nreset onto\npick main~1\nmerge -C main s # merge side\n";
    same(&s, &z, todo, &["-i", "-r", "--signoff", "up"]);
    assert_eq!(z.git(&["log", "-1", "--format=%B"]).stdout, "merge side\n\n");
}

#[test]
fn conflicted_merge_c_stop_then_skip() {
    let Some((s, z)) = world("rebase-merge-conflict") else { return };
    let todo = "label onto\nreset onto\npick main~1\nmerge -C main side3 # merge side\n";
    same(&s, &z, todo, &["-i", "-r", "up"]);
    assert_eq!(z.git(&["rebase", "--skip"]), s.git(&["rebase", "--skip"]));
    assert_eq!(post_state(&z), post_state(&s));
}

#[test]
fn conflicted_bare_merge_stop_then_abort() {
    let Some((s, z)) = world("rebase-merge-conflict-bare") else { return };
    let todo = "label onto\nreset onto\npick main~1\nmerge side3\n";
    same(&s, &z, todo, &["-i", "-r", "up"]);
    assert_eq!(z.git(&["rebase", "--abort"]), s.git(&["rebase", "--abort"]));
    assert_eq!(post_state(&z), post_state(&s));
}

#[test]
fn update_refs_drops_records_the_edited_sheet_no_longer_names() {
    let Some((s, z)) = world("rebase-merge-update-refs") else { return };
    same(&s, &z, "pick main~1\n", &["-i", "--update-refs", "up"]);
    let todo = "pick main~1\nupdate-ref refs/heads/zz\nupdate-ref refs/heads/side\nbreak\n";
    let Some((s2, z2)) = world("rebase-merge-update-refs-add") else { return };
    same(&s2, &z2, todo, &["-i", "up"]);
}
