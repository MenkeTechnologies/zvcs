//! `--object-dir=` goes through `real_pathdup(arg, 1)` the moment the option is read, and
//! `strbuf_realpath()` dies on the empty string: `fatal: The empty string is not a valid path`
//! (exit 128) - before the missing-subcommand usage error, and for every subcommand.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn an_empty_object_dir_dies_while_the_option_is_parsed() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("midx-empty-object-dir", stock);
    for args in [
        &["multi-pack-index", "--object-dir="][..],
        &["multi-pack-index", "--object-dir=", "write"],
        &["multi-pack-index", "--object-dir=", "verify"],
        &["multi-pack-index", "write", "--object-dir="],
        &["multi-pack-index", "--object-dir", ""],
        &["multi-pack-index", "--object-dir=x"],
    ] {
        let want = s.git(args);
        assert_eq!(z.git(args), want, "{args:?}");
    }
    let want = s.git(&["multi-pack-index", "--object-dir="]);
    assert_eq!((want.code, want.stderr.as_str()), (128, "fatal: The empty string is not a valid path\n"), "{want:?}");
}
