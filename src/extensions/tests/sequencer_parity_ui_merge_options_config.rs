//! The sequencer verbs against stock git: `init_ui_merge_options()` reads `merge.verbosity`,
//! `diff.renameLimit`, `merge.renameLimit` and `diff.algorithm`, and dies on a value it cannot
//! take — on every pick, clean or conflicted, for `cherry-pick`, `revert` and `rebase` alike. A
//! rebase has already written `rebase-merge/author-script` by then
//! (`write_author_script()` runs ahead of the merge, sequencer.c:2298).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    Some(twin_repo::pair(label, stock))
}

fn config_file(side: &Side, body: &str) -> String {
    let path = side.root.join("extra.cfg");
    std::fs::write(&path, body).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn an_unreadable_merge_option_ends_every_ui_merge() {
    let Some((s, z)) = world("ui-merge-config") else { return };
    let cases = [
        ("[diff]\n\talgorithm = 99999999999999999999999999\n", "diff.algorithm"),
        ("[merge]\n\tverbosity = 99999999999999999999999999\n", "merge.verbosity"),
        ("[diff]\n\trenameLimit = 1x\n", "diff.renameLimit"),
    ];
    let scripts: &[&[&str]] = &[
        &["cherry-pick", "side"],
        &["cherry-pick", "-Xpatience", "side"],
        &["revert", "--no-edit", "main~1"],
        &["revert", "--no-edit", "HEAD"],
    ];
    for (body, key) in cases {
        for args in scripts {
            for side in [&s, &z] {
                side.git(&["reset", "-q", "--hard", "main"]);
                let _ = std::fs::remove_dir_all(side.repo().join(".git/sequencer"));
                let _ = std::fs::remove_file(side.repo().join(".git/CHERRY_PICK_HEAD"));
                let _ = std::fs::remove_file(side.repo().join(".git/REVERT_HEAD"));
            }
            let run = |side: &Side| {
                let cfg = config_file(side, body);
                side.git_env(&[("GIT_CONFIG_GLOBAL", cfg.as_str())], args)
            };
            assert_eq!(run(&z), run(&s), "{key} {args:?}");
        }
    }
}

#[test]
fn rebase_dies_on_an_unreadable_merge_option_after_writing_the_author_script() {
    let Some((s, z)) = world("rebase-ui-merge-config") else { return };
    for side in [&s, &z] {
        side.git(&["checkout", "-q", "side"]);
        side.write("a", "clash\n");
        side.git(&["commit", "-q", "-am", "clash"]);
    }
    let run = |side: &Side| {
        let cfg = config_file(side, "[merge]\n\tverbosity = 99999999999999999999999999\n");
        let out = side.git_env(&[("GIT_CONFIG_GLOBAL", cfg.as_str())], &["rebase", "-i", "main"]);
        (out, side.read(".git/rebase-merge/author-script"))
    };
    let (want, want_script) = run(&s);
    assert!(want_script.is_some(), "stock leaves the author script: {want:?}");
    assert_eq!(run(&z), (want, want_script));
}
