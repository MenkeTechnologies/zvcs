//! `git shortlog` with the options `handle_revision_opt()` hands on rather than
//! owns: `-r`, `-t`, `diff_merges_parse_opts()`, `-v`, and last
//! `diff_opt_parse()` over the whole diff option table (revision.c:2583-2593,
//! :2758-2762).
//!
//! `cmd_shortlog()` sends every word its own five-entry table does not claim to
//! `parse_revision_opt()`, and past `--end-of-options` every word to
//! `setup_revisions()` (builtin/shortlog.c:430-459), so stock takes `-p`,
//! `--stat`, `-S <string>` and the rest, checks them as `diff_setup_done()`
//! does, and prints the same tally. A cluster `diff_opt_parse()` stops part-way
//! through is left in argv as `-<rest>` (parse-options.c:1088-1097), which is the
//! word `unknown option` and `unrecognized argument` then name. Three diff
//! options reach the tally: `--output` takes it, `--follow` keeps `revs->prune`
//! off, and `--find-object` and the separate/remerge `--diff-merges` styles clear
//! `revs->simplify_history` (revision.c:3192-3202, diff-merges.c:36-41).
//! zvcs refused every one with `unknown option`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// The twin history plus `refs/heads/-x`, and a merge whose side branch changes
/// `g` and changes it back, so the default walk drops that branch from
/// `-- g` while full history keeps it.
fn worlds(label: &str, stock: &str) -> (twin_repo::Side, twin_repo::Side) {
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        side.write("g", "1\n");
        side.git(&["add", "g"]);
        side.git(&["commit", "-q", "-m", "g"]);
        side.git(&["checkout", "-q", "-b", "flip"]);
        side.write("g", "2\n");
        side.git(&["commit", "-q", "-am", "flip"]);
        side.write("g", "1\n");
        side.git(&["commit", "-q", "-am", "flop"]);
        side.git(&["checkout", "-q", "main"]);
        side.write("a", "1\n2\n3\n4\n");
        side.git(&["commit", "-q", "-am", "four"]);
        let merge = side.git(&["merge", "-q", "--no-edit", "flip"]);
        assert_eq!(merge.code, 0, "{merge:?}");
        side.git(&["update-ref", "refs/heads/-x", "main~1"]);
    }
    (s, z)
}

fn same(s: &twin_repo::Side, z: &twin_repo::Side, args: &[&str]) {
    assert_eq!(z.git(args), s.git(args), "{args:?}");
}

#[test]
fn diff_options_are_accepted_and_checked() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = worlds("shortlog-diffopt-accept", stock);
    for args in [
        &["shortlog", "-p", "HEAD"][..],
        &["shortlog", "--stat", "-s", "HEAD"],
        &["shortlog", "-sp", "HEAD"],
        &["shortlog", "-ps", "HEAD"],
        &["shortlog", "-S", "two", "-n", "HEAD"],
        &["shortlog", "-sS", "two", "HEAD"],
        &["shortlog", "-r", "-t", "-v", "--cc", "-s", "HEAD"],
        &["shortlog", "--diff-merges", "c", "-s", "HEAD"],
        &["shortlog", "--stat=abc", "HEAD"],
        &["shortlog", "--diff-filter=Q", "HEAD"],
        &["shortlog", "--name-only", "--name-status", "HEAD"],
        &["shortlog", "-G", "x", "-S", "y", "HEAD"],
        &["shortlog", "--follow", "HEAD"],
        &["shortlog", "--follow", "--full-diff", "HEAD", "--", "a"],
        &["shortlog", "--find-object=nosuch", "HEAD"],
        &["shortlog", "--combined-all-paths", "HEAD"],
        &["shortlog", "--combined-all-paths", "--cc", "HEAD"],
        &["shortlog", "--diff-merges=x", "HEAD"],
        &["shortlog", "--diff-merges"],
        &["shortlog", "--author", "Thor", "-s", "HEAD"],
        &["shortlog", "--grep", "fl", "HEAD"],
    ] {
        same(&s, &z, args);
    }
}

#[test]
fn a_cluster_is_reported_by_what_the_diff_parser_left() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = worlds("shortlog-diffopt-cluster", stock);
    for args in [
        &["shortlog", "-px", "HEAD"][..],
        &["shortlog", "-sx", "HEAD"],
        &["shortlog", "-s", "-px", "HEAD"],
        &["shortlog", "--end-of-options", "-s", "-x"],
        &["shortlog", "--end-of-options", "-sn", "-x"],
        &["shortlog", "--end-of-options", "-w2", "-x"],
        &["shortlog", "--end-of-options", "-c", "-x"],
        &["shortlog", "--end-of-options", "--summary", "-x"],
        &["shortlog", "--end-of-options", "--numbered", "-x"],
        &["shortlog", "--end-of-options", "-p", "-q", "HEAD"],
        &["shortlog", "--end-of-options", "--stat", "HEAD"],
        &["shortlog", "--end-of-options", "--committer=Mitter", "HEAD"],
        &["shortlog", "--end-of-options", "-x"],
    ] {
        same(&s, &z, args);
    }
}

#[test]
fn the_three_diff_options_a_tally_can_see() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = worlds("shortlog-diffopt-effects", stock);
    for args in [
        &["shortlog", "-s", "HEAD", "--", "g"][..],
        &["shortlog", "-s", "-m", "HEAD", "--", "g"],
        &["shortlog", "-s", "-m", "--no-diff-merges", "HEAD", "--", "g"],
        &["shortlog", "-s", "--remerge-diff", "HEAD", "--", "g"],
        &["shortlog", "-s", "--cc", "HEAD", "--", "g"],
        &["shortlog", "-s", "--follow", "HEAD", "--", "g"],
        &["shortlog", "-s", "--find-object=flip:g", "HEAD", "--", "g"],
    ] {
        same(&s, &z, args);
    }
    let args = ["shortlog", "--output=tally", "HEAD"];
    same(&s, &z, &args);
    assert_eq!(z.read("tally"), s.read("tally"));
    assert!(s.read("tally").is_some_and(|t| !t.is_empty()));
}
