//! `--ignore-submodules=<when>` on `format-patch` qualifies how a submodule appears in the
//! diffs. `dirty` and `untracked` only decide how a submodule's *work tree* is judged, and the
//! diff of a commit against its parent has none; `all` hides the submodule entries themselves, so
//! where no commit of the series has a gitlink on either side there is nothing for it to hide.
//!
//! zvcs deferred all three spellings as unported and refused the command
//! (`unsupported flag "--ignore-submodules=all"`) whether or not a submodule was anywhere near.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

#[test]
fn every_spelling_is_accepted_where_no_commit_has_a_gitlink() {
    let Some(t) = Twin::new("fp-ignore-submodules") else { return };
    t.prepare(&["fetch", "-q", "origin"]);
    for when in ["none", "all", "dirty", "untracked"] {
        let flag = format!("--ignore-submodules={when}");
        t.same(&["format-patch", "--stdout", "-1", "origin/main", &flag]);
        t.same(&["format-patch", "--stdout", "--root", "origin/main", &flag]);
    }
}

#[test]
fn a_misspelt_when_is_still_the_die() {
    let Some(t) = Twin::new("fp-ignore-submodules-bad") else { return };
    t.same(&["format-patch", "--stdout", "-1", "--ignore-submodules=bogus"]);
}

#[test]
fn it_composes_with_the_other_options_of_the_original_report() {
    let Some(t) = Twin::new("fp-ignore-submodules-compose") else { return };
    t.prepare(&["fetch", "-q", "origin"]);
    t.same(&[
        "format-patch", "--stdout", "--in-reply-to=<msg-id@example.com>", "-1", "origin/main", "--no-notes",
        "--ignore-all-space", "-v", "2", "--ignore-submodules=all",
    ]);
}
