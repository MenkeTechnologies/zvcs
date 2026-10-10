//! `verify-tag` reads the repository settings with the first object it opens.
//!
//! `cmd_verify_tag()` runs `repo_config(git_default_config)` and nothing else up front
//! (builtin/verify-tag.c:38); `core.packedGitLimit` and its siblings belong to the settings block,
//! which is prepared when `gpg_verify_tag()` first reads an object. A name that resolves to no
//! ref is `error: tag '<name>' not found.` at exit 1, and a name that does resolve dies with
//! the numeric-config refusal at 128. zvcs refused every invocation at setup.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn an_unresolvable_name_never_reaches_the_settings_block() {
    let Some(t) = Twin::new("verify-tag-settings-miss") else { return };
    t.prepare(&["config", "core.packedGitLimit", "false"]);
    let (stock, zvcs) = t.run_in("work", &["verify-tag", "--raw", "no-such-tag", "refs/tags/also-missing"]);
    assert_eq!(stock.code, 1, "{stock:?}");
    assert!(stock.stderr.contains("tag 'no-such-tag' not found."), "{stock:?}");
    assert_eq!(zvcs, stock);
}

#[test]
fn a_resolvable_name_is_refused_by_the_settings_block() {
    let Some(t) = Twin::new("verify-tag-settings-hit") else { return };
    t.prepare(&["config", "core.packedGitLimit", "false"]);
    let (stock, zvcs) = t.run_in("work", &["verify-tag", "v0.1.0"]);
    assert_eq!(stock.code, 128, "{stock:?}");
    assert_eq!(zvcs, stock);
}
