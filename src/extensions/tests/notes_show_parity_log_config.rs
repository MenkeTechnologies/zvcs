//! `git notes show` runs `git show <blob>` on the note, so it reads the configuration `git show`
//! reads (`git_log_config()`, then `repo_init_revisions()`'s grep pass) — but only when a note
//! exists. `notes list` and a miss never start the child, and an invalid `diff.*` value does not
//! touch them. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::{Twins, run};

#[test]
fn notes_show_runs_git_show_on_the_note_so_it_reads_the_log_config() {
    let Some(t) = Twins::new("notes-show") else { return };
    for side in ["stock", "zvcs"] {
        let o = run(t.stock_bin, &t.root.join(side), &t.root.join("home"), &["notes", "add", "-m", "hello", "HEAD"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
    }
    // `diff.trustExitCode` is read by `git show`, which only starts when a note exists.
    let (stock, _) = t.same_with("diff.trustExitCode", "warn", &["notes", "show", "HEAD"]);
    assert_eq!(stock.code, 128, "{stock:?}");
    // `list` and a missing note never start it.
    t.same_with("diff.trustExitCode", "warn", &["notes", "list"]);
    t.same_with("diff.trustExitCode", "warn", &["notes", "show", "HEAD~1"]);
}

