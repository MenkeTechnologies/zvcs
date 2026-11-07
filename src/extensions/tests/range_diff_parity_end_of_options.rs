//! `git range-diff --end-of-options` with an operand that starts with `-`.
//!
//! parse_options_step() consumes `--end-of-options` (parse-options.c:1119-1124),
//! so a branch named `-x` reaches `cmd_range_diff()` as an operand, resolves as a
//! committish and is spliced into `-x..HEAD` (builtin/range-diff.c:103-129). The
//! inner `git log` that read_patches() runs over that range reads it as an
//! option again, leaves it over, and dies with `unrecognized argument` after its
//! revisions resolve (builtin/log.c:316-320), which range-diff reports as
//! `could not parse log` at 255 (range-diff.c:78-86). is_range_diff_range() pends
//! nothing for such a word (range-diff.c:616-642), so the two-range form refuses
//! it. A forwarded operand behaves the same, and a cluster `diff_opt_parse()`
//! stops part-way through is left over as its rest. zvcs answered
//! `unknown option `end-of-options'` for all of it.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_dash_operand_past_end_of_options_reaches_the_inner_log_as_an_option() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("range-diff-eoo", stock);
    for side in [&s, &z] {
        side.git(&["update-ref", "refs/heads/-x", "main~1"]);
        side.git(&["update-ref", "refs/heads/-p", "main~1"]);
    }
    for args in [
        &["range-diff", "--end-of-options", "-x", "main", "main"][..],
        &["range-diff", "--end-of-options", "-x", "main~2", "main"],
        &["range-diff", "--end-of-options", "-p", "main", "main"],
        &["range-diff", "--end-of-options", "-x...main"],
        &["range-diff", "--end-of-options", "main...-x"],
        &["range-diff", "--end-of-options", "-x..main", "main~1..main"],
        &["range-diff", "--end-of-options", "main~1..main", "-x..main"],
        &["range-diff", "--end-of-options", "main", "-x", "main"],
        &["range-diff", "--end-of-options", "main~2", "main~1", "main"],
        &["range-diff", "--end-of-options", "main~2", "main~1", "main", "-x"],
        &["range-diff", "--end-of-options", "main~2", "main~1", "main", "nosuch", "-x"],
        &["range-diff", "--end-of-options", "main~2", "main~1", "main", "--", "-x"],
        &["range-diff", "--end-of-options", "-x", "main", "main", "--", "a"],
        &["range-diff", "--end-of-options", "--end-of-options", "main", "main"],
        &["range-diff", "--end-of-options", "--", "main~2..main~1", "main~2..main"],
        &["range-diff", "--end-of-options", "-y", "main", "main"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
