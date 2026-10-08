//! The sequencer against stock git under `core.logAllRefUpdates=always`: the merge's `AUTO_MERGE`
//! and a rebase's `REBASE_HEAD` (like `CHERRY_PICK_HEAD`) are set through ref transactions, so
//! each leaves a reflog; the log goes with the ref when the stop is over.
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
fn log_all_ref_updates_always_logs_the_merge_state_refs_and_drops_them_with_the_ref() {
    let Some((s, z)) = world("always-state-refs") else { return };
    for side in [&s, &z] {
        side.git(&["checkout", "-q", "side"]);
        side.write("a", "clash\n");
        side.git(&["commit", "-q", "-am", "clash"]);
    }
    // Reflog lines carry the commit time of the machine running the test, so compare
    // everything but the timestamp.
    let logs = |side: &Side| -> Vec<(String, String)> {
        let dir = side.repo().join(".git/logs");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let body = std::fs::read_to_string(dir.join(&n)).unwrap();
                let flat: Vec<&str> = body
                    .lines()
                    .map(|l| l.split_once('\t').map_or("", |(_, msg)| msg))
                    .collect();
                (n, flat.join("|"))
            })
            .collect()
    };
    for args in [
        &["-c", "core.logAllRefUpdates=always", "rebase", "-i", "main"][..],
        &["-c", "core.logAllRefUpdates=always", "cherry-pick", "main"],
    ] {
        for side in [&s, &z] {
            side.git(&["rebase", "--abort"]);
            side.git(&["cherry-pick", "--quit"]);
            side.git(&["reset", "-q", "--hard", "side"]);
        }
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        assert_eq!(logs(&z), logs(&s), "{args:?}");
        for side in [&s, &z] {
            side.git(&["rebase", "--abort"]);
            side.git(&["cherry-pick", "--abort"]);
        }
        assert_eq!(logs(&z), logs(&s), "after the stop, {args:?}");
    }
}
