//! `git cherry-pick` against stock git: `save_opts()` records `--rerere-autoupdate` /
//! `--no-rerere-autoupdate` as `options.allow-rerere-auto` in `sequencer/opts`, which a later
//! `--continue` restores.
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

#[test]
fn cherry_pick_saves_rerere_autoupdate_in_the_sequencer_opts() {
    let Some((s, z)) = world("pick-rerere-opts") else { return };
    for flag in ["--rerere-autoupdate", "--no-rerere-autoupdate"] {
        for side in [&s, &z] {
            side.git(&["cherry-pick", "--quit"]);
            side.git(&["reset", "-q", "--hard", "main"]);
        }
        let args = ["cherry-pick", flag, "main~2", "side"];
        assert_eq!(z.git(&args), s.git(&args), "{flag}");
        assert_eq!(z.read(".git/sequencer/opts"), s.read(".git/sequencer/opts"), "{flag}");
    }
}
