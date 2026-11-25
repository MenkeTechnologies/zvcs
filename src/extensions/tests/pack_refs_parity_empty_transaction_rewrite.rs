//! `pack-refs` with nothing to pack still rewrites `packed-refs`, and `refs optimize`
//! reports its usage errors under its own name.
//!
//! `packed_transaction_prepare()` does not skip a transaction with zero updates
//! (refs/packed-backend.c:1724-1730): `write_with_updates()` writes the current header
//! and passes every packed record through in sorted order, each with the `^` line it
//! already carried and no other (:1416-1418, :1532-1535). So a `packed-refs` whose
//! header lacked `fully-peeled` or `sorted`, or whose records were out of order, comes
//! out canonical even when no loose ref was selected. zvcs left such a file untouched.
//!
//! `cmd_refs_optimize()` is `pack_refs_core()` with `refs_optimize_usage`
//! (builtin/refs.c:178-188), so an unknown option or a positional prints the
//! `git refs optimize` usage block; zvcs printed `git pack-refs`'s.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::{pair, Side};

/// Replace the loose `main`, `side` and `v1` by records in a `packed-refs` that has no
/// traits in its header and lists them in reverse order; `extra` stays loose.
fn unsorted_packed_refs(side: &Side) {
    assert_eq!(side.git(&["tag", "-a", "-m", "ann", "v1", "main~1"]).code, 0);
    assert_eq!(side.git(&["branch", "extra"]).code, 0);
    let oid = |rev: &str| side.git(&["rev-parse", rev]).stdout.trim().to_owned();
    let body = format!(
        "# pack-refs with: \n{} refs/tags/v1\n{} refs/heads/side\n{} refs/heads/main\n",
        oid("v1"),
        oid("side"),
        oid("main"),
    );
    side.write(".git/packed-refs", &body);
    for loose in ["refs/heads/main", "refs/heads/side", "refs/tags/v1"] {
        std::fs::remove_file(side.repo().join(".git").join(loose)).unwrap();
    }
}

#[test]
fn pack_refs_with_no_selected_loose_ref_rewrites_packed_refs() {
    let Some(stock) = stock_git::stock_git() else { return };
    for verb in [&["pack-refs"][..], &["refs", "optimize", "--include", "refs/notes/*"]] {
        let (s, z) = pair("pack-refs-empty-rewrite", stock);
        unsorted_packed_refs(&s);
        unsorted_packed_refs(&z);
        assert_eq!(s.git(verb), z.git(verb), "{verb:?}");
        let packed = s.read(".git/packed-refs").unwrap();
        assert!(packed.starts_with(b"# pack-refs with: peeled fully-peeled sorted \n"), "{verb:?}");
        assert_eq!(packed, z.read(".git/packed-refs").unwrap(), "{verb:?}: packed-refs bytes");
        assert_eq!(z.read(".git/refs/heads/extra").is_some(), s.read(".git/refs/heads/extra").is_some());
    }
}

/// The usage cases share one fixture: none of them changes a ref.
#[test]
fn usage_and_option_parsing() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = pair("pack-refs-usage", stock);
    refs_optimize_usage_errors_name_refs_optimize(&s, &z);
    options_after_a_positional_are_still_parsed(&s, &z);
}

fn refs_optimize_usage_errors_name_refs_optimize(s: &Side, z: &Side) {
    for args in [
        &["refs", "optimize", "--bogus"][..],
        &["refs", "optimize", "extra"],
        &["refs", "optimize", "--", "-h"],
        &["refs", "optimize", "-x"],
    ] {
        let (so, zo) = (s.git(args), z.git(args));
        assert!(so.stderr.contains("usage: git refs optimize"), "{args:?}: {so:?}");
        assert_eq!(so, zo, "{args:?}");
    }
}

/// parse_options() collects a non-option and keeps parsing (parse-options.c:1011-1018), and
/// pack_refs_core() refuses the collected arguments only afterwards (pack-refs.c:38-39). So
/// an option after a positional still acts, and `--end-of-options` ends the options like
/// `--` does. zvcs gave the usage block at the first positional and called
/// `--end-of-options` an unknown option.
fn options_after_a_positional_are_still_parsed(s: &Side, z: &Side) {
    for verb in [&["pack-refs"][..], &["refs", "optimize"]] {
        for tail in [
            &["extra", "-h"][..],
            &["extra", "--bogus"],
            &["extra", "--include"],
            &["--end-of-options", "--all"],
            &["--all", "--end-of-options"],
        ] {
            let args: Vec<&str> = verb.iter().chain(tail).copied().collect();
            assert_eq!(s.git(&args), z.git(&args), "{args:?}");
        }
    }
    assert_eq!(s.read(".git/packed-refs"), z.read(".git/packed-refs"));
}
