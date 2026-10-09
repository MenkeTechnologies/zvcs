//! Each `git reflog` subcommand installs its own config callback, so a value one
//! of them refuses is no concern of the others.
//!
//! `cmd_reflog()` sends `show` (and the implied `show`) to `git_log_config`,
//! `expire` to `reflog_expire_config` (the two `gc.reflogExpire*` keys, then
//! `git_default_config`) and `write` to `git_ident_config` alone; `list`,
//! `exists`, `delete` and `drop` read no callback (builtin/reflog.c:154, :216,
//! :421). The settings block is only prepared once a command touches objects or
//! resolves a ref: `show`, `expire --all`/`<ref>`, `delete <ref>@{n}` and
//! `drop <ref>`. zvcs ran `git_default_config` over every subcommand and the
//! settings block over all of them, so `-c color.advice.reset=bogus reflog list`
//! died where git lists the reflogs.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

const NULL_OID: &str = "0000000000000000000000000000000000000000";

fn same_with(t: &Twin, key: &str, args: &[&str]) {
    let mut full = vec!["-c", key, "reflog"];
    full.extend_from_slice(args);
    t.same(&full);
}

/// Keys only `git_default_config` (or the settings block) reads; none of them is
/// read by `list`, `exists`, `delete` or `drop`.
const DEFAULT_ONLY_KEYS: &[&str] = &[
    "core.ignorecase=bogus",
    "core.abbrev=bogus",
    "color.advice.reset=99999999999999999999999999",
    "advice.statusHints=bogus",
    "push.default=bogus",
];

#[test]
fn list_exists_delete_drop_and_write_ignore_default_config_keys() {
    let Some(t) = Twin::new("reflog-cb-none") else { return };
    for key in DEFAULT_ONLY_KEYS {
        same_with(&t, key, &["list"]);
        same_with(&t, key, &["list", "extra"]);
        same_with(&t, key, &["exists", "HEAD"]);
        same_with(&t, key, &["exists"]);
        same_with(&t, key, &["delete"]);
        same_with(&t, key, &["drop", "--all"]);
        same_with(&t, key, &["write"]);
        same_with(&t, key, &["write", "refs/heads/main", NULL_OID, NULL_OID, "msg"]);
    }
}

#[test]
fn show_and_expire_still_refuse_default_config_keys() {
    let Some(t) = Twin::new("reflog-cb-default") else { return };
    for key in DEFAULT_ONLY_KEYS {
        same_with(&t, key, &[]);
        same_with(&t, key, &["show"]);
        same_with(&t, key, &["expire", "--all"]);
    }
}

#[test]
fn write_runs_the_ident_callback_alone() {
    let Some(t) = Twin::new("reflog-cb-ident") else { return };
    for key in ["user.useConfigOnly=bogus", "user.name"] {
        same_with(&t, key, &["write", "refs/heads/main", NULL_OID, NULL_OID, "msg"]);
        same_with(&t, key, &["list"]);
        same_with(&t, key, &["exists", "HEAD"]);
    }
}

#[test]
fn expire_refuses_a_bad_gc_expiry_before_expiring_anything() {
    let Some(t) = Twin::new("reflog-cb-expire") else { return };
    for key in [
        "gc.reflogexpire=bogus",
        "gc.reflogExpireUnreachable=bogus",
        "gc.refs/heads/*.reflogexpire=bogus",
        "gc.reflogexpire",
    ] {
        same_with(&t, key, &["expire", "--all"]);
        same_with(&t, key, &["expire"]);
        same_with(&t, key, &["list"]);
    }
}

#[test]
fn settings_block_is_prepared_only_where_git_prepares_it() {
    let Some(t) = Twin::new("reflog-cb-settings") else { return };
    for key in ["core.packedGitLimit=bogus", "index.version=bogus"] {
        same_with(&t, key, &["list"]);
        same_with(&t, key, &["exists", "HEAD"]);
        same_with(&t, key, &["expire"]);
        same_with(&t, key, &["delete"]);
        same_with(&t, key, &["drop", "--all"]);
        same_with(&t, key, &["write", "refs/heads/main", NULL_OID, NULL_OID, "msg"]);
        same_with(&t, key, &["show"]);
        same_with(&t, key, &["expire", "--all"]);
        same_with(&t, key, &["delete", "HEAD@{0}"]);
    }
}
