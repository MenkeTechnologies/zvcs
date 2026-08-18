//! The sequencer's bridge into `git commit`: `should_edit()` and the argument
//! vector `run_git_commit()` builds for it.
//!
//! `revert` and `cherry-pick` write their own commit objects for the common case,
//! which is what `try_to_commit()` does in git. But `do_commit()` only takes that
//! in-process path when neither `EDIT_MSG` nor `VERIFY_MSG` is set
//! (sequencer.c:1728); with an editor requested it falls through to
//! `run_git_commit()`, which spawns a real `git commit` (sequencer.c:1750-1754).
//! That is not an implementation detail — it is *observable*, and in more than one
//! way:
//!
//!   * the editor runs at all, on `.git/COMMIT_EDITMSG` seeded from `MERGE_MSG`,
//!     so what the user types is what gets committed;
//!   * a failing editor, or one that empties the message, aborts the pick;
//!   * the summary comes from `print_commit_summary()` inside `git commit` rather
//!     than the sequencer's, so a revert loses the ` Date:` line the sequencer's
//!     `SUMMARY_SHOW_AUTHOR_DATE` always adds (a cherry-pick keeps it — its
//!     `CHERRY_PICK_HEAD` makes `author_date_is_interesting()` true);
//!   * `git commit` tears down the operation state it finds, so `AUTO_MERGE` does
//!     not survive an edited revert the way it survives a plain one;
//!   * the reflog still reads `revert:`/`cherry-pick:` rather than `commit:`,
//!     because the child inherits `GIT_REFLOG_ACTION` (sequencer.c:1141).
//!
//! So this module reproduces the delegation rather than approximating it: the same
//! argument vector, into this crate's own ported `commit` driver.

use anyhow::Result;
use std::process::ExitCode;

/// Which of the two verbs is replaying — git's `opts->action`, which
/// `action_name()` renders as the reflog action and `should_edit()` consults for
/// the default.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Revert,
    Pick,
}

impl Action {
    /// `action_name()` (builtin/revert.c:38-41).
    pub(crate) fn name(self) -> &'static str {
        match self {
            Action::Revert => "revert",
            Action::Pick => "cherry-pick",
        }
    }
}

/// ```c
/// static int should_edit(struct replay_opts *opts) {
///         if (opts->edit < 0)
///                 return (opts->action == REPLAY_REVERT && isatty(0)) ? 1 : 0;
///         return opts->edit;
/// }
/// ```
///
/// (sequencer.c:2203-2212.) `opts->edit` is the tri-state `-1` until `-e` or
/// `--no-edit` sets it, which is what `edit` models here. The defaults differ
/// between the verbs: an unqualified `git revert` opens an editor at a terminal
/// and does not when stdin is redirected, while `git cherry-pick` never does.
pub(crate) fn should_edit(edit: Option<bool>, action: Action) -> bool {
    use std::io::IsTerminal as _;
    match edit {
        Some(v) => v,
        None => action == Action::Revert && std::io::stdin().is_terminal(),
    }
}

/// `sequencer_reflog_action()` (sequencer.c:2230-2240): an inherited
/// `GIT_REFLOG_ACTION` names the operation when there is one — that is how a
/// rebase's picks land in the reflog as `rebase` — otherwise the verb's own name.
fn reflog_action(action: Action) -> String {
    std::env::var("GIT_REFLOG_ACTION")
        .ok()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| action.name().to_string())
}

/// `run_git_commit(NULL, reflog_action, opts, EDIT_MSG [| ALLOW_EMPTY])`
/// (sequencer.c:1122-1184) for the one shape the pick verbs reach it in: a
/// non-rebase replay whose message is to be edited.
///
/// ```c
/// strvec_push(&cmd.args, "commit");
/// if (!(flags & VERIFY_MSG))                     strvec_push(&cmd.args, "-n");
/// if ((flags & AMEND_MSG))                       strvec_push(&cmd.args, "--amend");
/// if (opts->gpg_sign) strvec_pushf(&cmd.args, "-S%s", opts->gpg_sign);
/// else                strvec_push(&cmd.args, "--no-gpg-sign");
/// if (defmsg)              strvec_pushl(&cmd.args, "-F", defmsg, NULL);
/// else if (!(flags & EDIT_MSG)) strvec_pushl(&cmd.args, "-C", "HEAD", NULL);
/// if ((flags & CLEANUP_MSG))     strvec_push(&cmd.args, "--cleanup=strip");
/// if ((flags & EDIT_MSG))        strvec_push(&cmd.args, "-e");
/// ...
/// if ((flags & ALLOW_EMPTY))     strvec_push(&cmd.args, "--allow-empty");
/// if (!(flags & EDIT_MSG))       strvec_push(&cmd.args, "--allow-empty-message");
/// ```
///
/// `EDIT_MSG` decides most of it: no `-F`, no `-C HEAD` (so `git commit` picks
/// the message up from `MERGE_MSG`, which `do_pick_commit` wrote before the
/// merge), `-e`, and no `--allow-empty-message` — an editor that empties the
/// buffer is meant to abort. `VERIFY_MSG` is never set for a plain pick, hence
/// the unconditional `-n`. `--cleanup` is *not* forwarded: git passes
/// `--cleanup=strip` only for `CLEANUP_MSG`, which a plain pick never sets.
///
/// The child is this binary's own `commit`, so the delegation is a call rather
/// than a fork; `GIT_REFLOG_ACTION` is exported because that is how the wording
/// reaches `builtin/commit.c`'s `reflog_msg` (builtin/commit.c:1850).
pub(crate) fn run_git_commit(
    action: Action,
    gpg_sign: Option<&str>,
    allow_empty: bool,
) -> Result<ExitCode> {
    let mut args: Vec<String> = vec!["-n".to_string()];
    match gpg_sign {
        Some(key) => args.push(format!("-S{key}")),
        None => args.push("--no-gpg-sign".to_string()),
    }
    args.push("-e".to_string());
    if allow_empty {
        args.push("--allow-empty".to_string());
    }
    std::env::set_var("GIT_REFLOG_ACTION", reflog_action(action));
    super::commit::commit(&args)
}
