//! `git zverbs` lists every superset (`z*`) verb and its one-line usage, so the
//! user never has to remember the set. The listing is sourced from `z_usage`,
//! which `print_verbs` silently skips for any verb lacking an entry — so the
//! guard that matters is: every verb in `SUPERSET_VERBS` shows up on its own
//! line. A verb added to the dispatch table without a matching usage string
//! would otherwise vanish from the listing unnoticed.

use std::process::Command;
use zvcs::dispatch::SUPERSET_VERBS;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[test]
fn zverbs_lists_every_superset_verb() {
    let out = Command::new(BIN).arg("zverbs").output().unwrap();
    assert!(out.status.success(), "git zverbs failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();

    // Every listed verb name begins a line (the "usage: git " lead-in is stripped,
    // so a line starts with the verb itself). Anchor on "<verb> " / "<verb>\n" to
    // avoid a prefix match (e.g. `zstash` inside `zstashes`).
    for verb in SUPERSET_VERBS {
        let listed = stdout
            .lines()
            .any(|l| l == *verb || l.starts_with(&format!("{verb} ")));
        assert!(listed, "zverbs omitted `{verb}` — missing a z_usage entry?\n{stdout}");
    }

    // The count of listed lines equals the verb count: no stragglers, no dupes.
    assert_eq!(
        stdout.lines().filter(|l| !l.is_empty()).count(),
        SUPERSET_VERBS.len(),
        "zverbs line count should equal the superset verb count:\n{stdout}"
    );
}

#[test]
fn zverbs_dash_h_prints_its_own_usage() {
    let out = Command::new(BIN).args(["zverbs", "-h"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("usage: git zverbs"),
        "zverbs -h should print its usage line:\n{stdout}"
    );
}
