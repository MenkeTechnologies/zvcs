//! `git tag` always builds a reflog message for the object it tags
//! (`create_reflog_msg()`, builtin/tag.c:386-432: `tag: tagging <abbrev> (<subject>,
//! <date>)`) and hands it to the transaction, which writes it wherever a reflog is
//! written for the ref: forced under `--create-reflog`, and otherwise for a tag
//! whose reflog already exists or when `core.logAllRefUpdates=always` asks for one.
//!
//! zvcs passed the message only under `--create-reflog`, so a tag logged because of
//! `core.logAllRefUpdates=always` carried an empty message.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

#[test]
fn always_logs_tags_with_the_tagging_message() {
    let Some(t) = Twin::new("tag-reflog-always") else { return };
    t.same(&["-c", "core.logAllRefUpdates=always", "tag", "light", "HEAD"]);
    t.same(&["reflog", "show", "refs/tags/light"]);
    t.same(&["-c", "core.logAllRefUpdates=always", "tag", "-a", "-m", "annotated", "ann", "HEAD"]);
    t.same(&["reflog", "show", "refs/tags/ann"]);
    t.same(&["-c", "core.logAllRefUpdates=always", "tag", "-f", "light", "HEAD~1"]);
    t.same(&["reflog", "show", "refs/tags/light"]);
}

#[test]
fn the_message_names_the_kind_of_a_non_commit_target() {
    let Some(t) = Twin::new("tag-reflog-kinds") else { return };
    t.same(&["-c", "core.logAllRefUpdates=always", "tag", "tree-tag", "HEAD^{tree}"]);
    t.same(&["reflog", "show", "refs/tags/tree-tag"]);
    t.same(&["-c", "core.logAllRefUpdates=always", "tag", "-a", "-m", "m", "outer", "HEAD"]);
    t.same(&["-c", "core.logAllRefUpdates=always", "tag", "-f", "again", "outer"]);
    t.same(&["reflog", "show", "refs/tags/again"]);
}

#[test]
fn create_reflog_and_the_defaults_are_unchanged() {
    let Some(t) = Twin::new("tag-reflog-default") else { return };
    t.same(&["tag", "plain", "HEAD"]);
    t.same(&["reflog", "exists", "refs/tags/plain"]);
    t.same(&["tag", "--create-reflog", "logged", "HEAD"]);
    t.same(&["reflog", "show", "refs/tags/logged"]);
    t.same(&["-c", "core.logAllRefUpdates=false", "tag", "--create-reflog", "forced", "HEAD"]);
    t.same(&["reflog", "show", "refs/tags/forced"]);
    t.same(&["-c", "core.logAllRefUpdates=false", "tag", "unlogged", "HEAD"]);
    t.same(&["reflog", "exists", "refs/tags/unlogged"]);
}

#[test]
fn a_tag_whose_reflog_exists_keeps_appending() {
    let Some(t) = Twin::new("tag-reflog-existing") else { return };
    t.same(&["tag", "--create-reflog", "grow", "HEAD"]);
    t.same(&["tag", "-f", "grow", "HEAD~1"]);
    t.same(&["reflog", "show", "refs/tags/grow"]);
}
