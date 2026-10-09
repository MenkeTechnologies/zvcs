//! A geometric run hands `pack-objects` `--stdin-packs` beside the `--filter` it forwards, and the
//! child refuses the pair before reading a pack name; `repack` returns its 128. `--geometric` is an
//! `OPT_INTEGER`, so its value skips leading whitespace like `git_parse_int()`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_geometric_run_refuses_a_filter_like_its_pack_objects_child() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("repack-geometric-filter", stock);
    for args in [
        &["repack", "--geometric=2", "--filter=blob:none"][..],
        &["repack", "--geometric=2", "-d", "-q", "--filter=tree:0"],
        &["repack", "--geometric=2", "--filter=blob:none", "--filter-to=x"],
        // A bad spec is rejected while the options are parsed, before the child exists.
        &["repack", "--geometric=2", "--filter=nonsense"],
        // `OPT_INTEGER` skips leading whitespace, so this is a geometric run with a factor of 1.
        &["repack", "--geometric= 1"],
        &["repack", "--geometric= 1", "--filter=blob:none"],
    ] {
        let want = s.git(args);
        assert_eq!(z.git(args), want, "{args:?}");
    }
    let want = s.git(&["repack", "--geometric=2", "--filter=blob:none"]);
    assert_eq!(
        (want.code, want.stderr.as_str()),
        (128, "fatal: options '--stdin-packs' and '--filter' cannot be used together\n")
    );
}
