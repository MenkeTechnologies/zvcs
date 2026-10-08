//! A `--upload-pack` / `--receive-pack` program is always run through the shell.
//!
//! `git_connect()` builds `"<prog> '<path>'"` as one argument and sets `conn->use_shell`, so
//! `prepare_shell_cmd()` runs `sh -c "<prog> '<path>'" "<prog> '<path>'"` even when `<prog>` is a
//! bare name. A program that does not exist is therefore the shell's own
//! `<prog> '<path>': <prog>: command not found` and an empty advertisement, which
//! `die_initial_contact()` reports as `Could not read from remote repository.` at 128. zvcs ran a
//! bare name directly, so a missing program was a spawn failure of its own wording
//! (`Failed to invoke program "<prog>"`, exit 1).
//!
//! The remote is a bare repository so that the path the program is handed is the one typed.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

fn world(label: &str) -> Option<Twin> {
    let t = Twin::new(label)?;
    t.prepare(&["clone", "-q", "--bare", ".", "../bare.git"]);
    Some(t)
}

#[test]
fn a_missing_upload_pack_program_is_the_shells_not_found() {
    let Some(t) = world("override-missing") else { return };
    let (stock, zvcs) = t.run_in("work", &["ls-remote", "--upload-pack=no-such-prog", "../bare.git"]);
    assert_eq!(stock.code, 128, "{stock:?}");
    assert!(stock.stderr.contains("no-such-prog '../bare.git': no-such-prog: command not found"), "{stock:?}");
    assert_eq!(zvcs, stock);
    t.same(&["fetch", "--upload-pack=no-such-prog", "../bare.git"]);
    t.same(&["fetch", "-u", "no-such-prog", "../bare.git"]);
}

#[test]
fn clone_names_the_source_absolute_to_the_missing_program() {
    let Some(t) = world("override-clone") else { return };
    t.same_in(".", &["clone", "--upload-pack=no-such-prog", "bare.git", "copy"]);
}

#[test]
fn a_missing_receive_pack_program_is_the_shells_not_found() {
    let Some(t) = world("override-receive") else { return };
    t.same(&["push", "--receive-pack=no-such-prog", "../bare.git", "main"]);
}

#[test]
fn a_program_with_arguments_still_runs_as_a_command_line() {
    let Some(t) = world("override-args") else { return };
    t.same(&["ls-remote", "--upload-pack=false --flag", "../bare.git"]);
    t.same(&["ls-remote", "--upload-pack=exit 3 #", "../bare.git"]);
}

#[test]
fn an_overridden_program_that_works_still_serves_the_repository() {
    let Some(t) = world("override-works") else { return };
    t.same(&["ls-remote", "--upload-pack=git-upload-pack", "../bare.git"]);
    t.same(&["fetch", "--upload-pack=git upload-pack", "../bare.git"]);
}
