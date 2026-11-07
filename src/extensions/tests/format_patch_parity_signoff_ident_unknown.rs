//! `git format-patch --signoff` with no usable committer identity.
//!
//! show_log() asks fmt_name(WANT_COMMITTER_IDENT) for the trailer, first for
//! need_8bit_cte and then in append_signoff() (log-tree.c:870-892,
//! sequencer.c:5814), which is fmt_ident() under IDENT_STRICT (ident.c:544-562):
//! an identity it cannot work out dies there with the `Committer identity
//! unknown` block and 128, after log_write_email_headers() already wrote the
//! `From <commit>` line, which stdio writes out on exit. A cover letter's own
//! `From:` is git_committer_info(0) and does not die. zvcs said
//! `zvcs: format-patch: unable to auto-detect email address` and exited 1.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn signoff_dies_where_show_log_asks_for_the_identity() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("format-patch-signoff-ident", stock);
    // An explicitly empty name is refused without the advice block.
    let empty_name: &[(&str, &str)] = &[("GIT_COMMITTER_NAME", "")];
    for args in [
        &["format-patch", "--signoff", "--stdout", "-2"][..],
        &["format-patch", "-s", "--stdout", "-2", "--attach"],
        &["format-patch", "-s", "--stdout", "-2", "--line-prefix=> "],
        &["format-patch", "-s", "-2", "-o", "out"],
        &["format-patch", "-s", "--stdout", "-1"],
    ] {
        assert_eq!(z.git_env(empty_name, args), s.git_env(empty_name, args), "{args:?}");
        assert_eq!(z.read("out/0001-two.patch"), s.read("out/0001-two.patch"), "{args:?}");
    }
    // No email anywhere and `user.useConfigOnly`: refused, with the advice block,
    // before any host lookup, so the outcome does not depend on the hostname.
    for side in [&s, &z] {
        side.git(&["config", "user.useConfigOnly", "true"]);
    }
    let args = &["format-patch", "-s", "--stdout", "-2"];
    let unset = |side: &twin_repo::Side| {
        let mut cmd = std::process::Command::new(&side.bin);
        cmd.args(args)
            .current_dir(side.repo())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &side.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("LC_ALL", "C");
        let out = cmd.output().unwrap();
        (out.status.code(), out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
    };
    assert_eq!(unset(&z), unset(&s));
}
