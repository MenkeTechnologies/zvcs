//! `cmd_diff_pairs()` reads `git_diff_basic_config` before it parses an option or checks `-z`,
//! so a bad `git_default_config()` value (`advice.*`, `core.precomposeUnicode`, `core.abbrev`)
//! is the 128 that wins over the usage errors; only `-h` is answered ahead of it. zvcs gave
//! `diff-pairs` no config walk at all.
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
fn a_bad_default_config_value_precedes_diff_pairs_usage_errors() {
    let Some((s, z)) = world("diff-pairs-config") else { return };
    for kv in [
        "advice.statusHints=none",
        "core.precomposeUnicode=%H",
        "core.abbrev=bogus",
        // Not read by the basic callback.
        "diff.renames=bogus",
        "color.diff=bogus",
    ] {
        for args in [&["diff-pairs"][..], &["diff-pairs", "-z"], &["diff-pairs", "-h"], &["diff-pairs", "--bogus"]] {
            let mut argv = vec!["-c", kv];
            argv.extend_from_slice(args);
            let want = s.git(&argv);
            assert_eq!(z.git(&argv), want, "{argv:?}");
        }
    }
    let want = s.git(&["-c", "advice.statusHints=none", "diff-pairs"]);
    assert_eq!(
        (want.code, want.stderr.as_str()),
        (128, "fatal: bad boolean config value 'none' for 'advice.statushints'\n")
    );
}
