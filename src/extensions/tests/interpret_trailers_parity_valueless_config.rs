//! A `trailer.*` variable written without `=` is git's `NULL` value, which the
//! trailer callbacks treat apart from the empty string.
//!
//! ```c
//! case TRAILER_KEY:
//!         if (conf->key)
//!                 warning(_("more than one %s"), conf_key);
//!         if (!value)
//!                 return config_error_nonbool(conf_key);
//! ```
//!
//! (`git_trailer_config()`, trailer.c:553-573, and `separators` in
//! `git_trailer_default_config()`, trailer.c:510-513.) The refusal dies as every
//! `repo_config()` callback error does: `error: missing value for '<key>'`, then
//! `fatal: unable to parse '<key>' from command-line config` (or the file and
//! line). The three enum variables take `NULL` as "back to the default"
//! (`trailer_set_where()`, trailer.c:389-404), so they warn about nothing.
//! zvcs read every valueless entry as `''`: the strings were set to the empty
//! string and the enums warned `unknown value ''`.
//!
//! `commit` and `tag` read the configuration once `--trailer` is given
//! (`trailer_config_init()`, builtin/commit.c:1824-1825, builtin/tag.c:571-572),
//! so they die the same way, before writing `COMMIT_EDITMSG`, not with `unable to
//! pass trailers to --trailers`.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical
//! repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

const MSG: &str = "subject\n\nbody\n\nSigned-off-by: A <a@x>\nAcked-by: B <b@x>\n";

#[test]
fn valueless_trailer_variables_match_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("trailer-valueless", stock);
    for side in [&s, &z] {
        side.write("msg", MSG);
    }
    let cases: &[&[&str]] = &[
        &["-c", "trailer.x.key", "interpret-trailers", "msg"],
        &["-c", "trailer.x.cmd", "interpret-trailers", "msg"],
        &["-c", "trailer.x.command", "interpret-trailers", "msg"],
        &["-c", "trailer.x.key=a", "-c", "trailer.x.key", "interpret-trailers", "msg"],
        &["-c", "trailer.separators", "interpret-trailers", "msg"],
        &["-c", "trailer.x.where", "interpret-trailers", "--trailer", "x=1", "msg"],
        &["-c", "trailer.where", "interpret-trailers", "--trailer", "a=b", "msg"],
        &["-c", "trailer.x.key", "commit", "--allow-empty", "-m", "x", "--trailer", "a=b"],
        &["-c", "trailer.x.key", "tag", "-a", "-m", "x", "tt", "--trailer", "a=b"],
    ];
    for args in cases {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        assert_eq!(z.read(".git/COMMIT_EDITMSG"), s.read(".git/COMMIT_EDITMSG"), "{args:?}");
    }
}

#[test]
fn a_valueless_key_in_a_file_names_the_file_and_line() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("trailer-valueless-file", stock);
    for side in [&s, &z] {
        let config = side.repo().join(".git/config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("[trailer \"x\"]\n\tkey\n");
        std::fs::write(config, text).unwrap();
    }
    let args = ["commit", "--allow-empty", "-m", "x", "--trailer", "a=b"];
    let want = s.git(&args);
    assert_eq!(want.code, 128, "{want:?}");
    assert_eq!(z.git(&args), want);
}

#[test]
fn an_in_place_refusal_names_the_operand_as_given() {
    use std::os::unix::fs::PermissionsExt;
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("trailer-inplace-name", stock);
    for side in [&s, &z] {
        side.write("m/msg", MSG);
        let path = side.repo().join("m/msg");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let args = ["interpret-trailers", "--in-place", "--trailer", "x=y", "m/msg"];
    let want = s.git(&args);
    assert_eq!(want.stderr, "error: file m/msg is not writable by user\n");
    assert_eq!(z.git(&args), want);
}
