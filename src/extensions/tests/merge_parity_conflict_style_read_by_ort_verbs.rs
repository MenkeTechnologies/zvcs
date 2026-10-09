//! `merge.conflictStyle` is read by `merge_recursive_config()` through
//! `repo_config(opt->repo, git_xmerge_config, NULL)` (merge-ort.c:5491), i.e. whenever a verb
//! builds its merge-ort options — `init_ui_merge_options()` for `cherry-pick` and `revert`,
//! `init_basic_merge_options()` for `merge-tree` and `replay`. A value `git_xmerge_config()`
//! refuses is fatal there (`error: unknown style '<v>' given for 'merge.conflictstyle'`, then
//! `fatal: unable to parse … from command-line config`, 128), after `diff.algorithm` on the
//! ui path and whether or not the merge turns out to have anything to conflict about.
//!
//! zvcs read the value for `merge` only, and ran the other four to completion.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

fn fetched(label: &str) -> Option<Twin> {
    let t = Twin::new(label)?;
    t.prepare(&["fetch", "-q", "origin"]);
    Some(t)
}

const BAD: &[&str] = &["merge.conflictStyle=bogus", "merge.conflictStyle="];

#[test]
fn cherry_pick_and_revert_refuse_a_bad_style() {
    let Some(t) = fetched("conflict-style-pick") else { return };
    for key in BAD {
        t.same(&["-c", key, "cherry-pick", "origin/main"]);
        t.same(&["-c", key, "cherry-pick", "--allow-empty", "HEAD"]);
        t.same(&["-c", key, "revert", "HEAD"]);
    }
}

#[test]
fn merge_tree_and_replay_refuse_a_bad_style() {
    let Some(t) = fetched("conflict-style-tree") else { return };
    for key in BAD {
        t.same(&["-c", key, "merge-tree", "--write-tree", "HEAD", "origin/main"]);
        t.same(&["-c", key, "merge-tree", "HEAD", "origin/main"]);
        t.same(&["-c", key, "replay", "--onto", "HEAD", "HEAD..origin/main"]);
    }
}

#[test]
fn a_valid_style_and_the_verbs_that_never_merge_are_untouched() {
    let Some(t) = fetched("conflict-style-valid") else { return };
    for style in ["merge", "diff3", "zdiff3"] {
        let key = format!("merge.conflictStyle={style}");
        t.same(&["-c", &key, "merge-tree", "--write-tree", "HEAD", "origin/main"]);
        t.same(&["-c", &key, "cherry-pick", "origin/main"]);
    }
    t.same(&["-c", "merge.conflictStyle=bogus", "merge-base", "HEAD", "origin/main"]);
    t.same(&["-c", "merge.conflictStyle=bogus", "log", "--oneline"]);
}

#[test]
fn diff_algorithm_is_judged_before_the_style_on_the_ui_path() {
    let Some(t) = fetched("conflict-style-order") else { return };
    t.same(&["-c", "diff.algorithm=nonsense", "-c", "merge.conflictStyle=bogus", "cherry-pick", "origin/main"]);
    t.same(&["-c", "merge.conflictStyle=bogus", "-c", "merge.renameLimit=bogus", "cherry-pick", "origin/main"]);
}
