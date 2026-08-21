//! `git rebase` — reapply commits on top of another base tip.
//!
//! ### Shape of the port
//!
//! `builtin/rebase.c` is a long funnel: `parse_options()`, then ~130 lines of
//! backend inference and option-compatibility checks, then `<upstream>` and
//! `<onto>` resolution, then `require_clean_work_tree()`, then
//! `can_fast_forward()`, and only at the very end the actual replay. Almost
//! every invocation git rejects, it rejects *inside the funnel*, long before a
//! commit would be picked. This module reproduces the funnel in git's order and
//! bails only at the point where a commit would actually have to be replayed.
//!
//! That ordering is load-bearing, not cosmetic: `git rebase --strategy-option=ours v0.2.0`
//! against a repo without that tag fails with `fatal: invalid upstream 'v0.2.0'`,
//! not with anything about strategies. Refusing an unimplemented flag at parse
//! time produces the wrong diagnostic for every such combination.
//!
//! ### What is ported
//!
//! * `-X<option>` and `--ignore-whitespace` (which becomes
//!   `-Xignore-space-change` on the merge backend, builtin/rebase.c:1546-1548),
//!   applied to every pick and every recreated `merge` through
//!   [`crate::merge_apply::StrategyOptions`], and recorded in
//!   `$state_dir/strategy` / `$state_dir/strategy_opts` so `--continue` merges
//!   the same way. merge-ort's `Auto-merging`/`CONFLICT` block is shown only
//!   when the merge came out unclean, which is git's
//!   `show_output = !is_rebase_i(opts) || !result.clean` (sequencer.c:783).
//! * The whole option grammar, including the backend inference (`imply_merge()`,
//!   `parse_opt_am()`/`parse_opt_merge()`/`parse_opt_interactive()`), and every
//!   `die()` it can raise — `apply options and merge options cannot be used
//!   together`, `<opt> requires the merge backend`, `--reschedule-failed-exec
//!   requires --exec or --interactive`, `switch \`C' expects a numerical value`,
//!   `Invalid whitespace option`, the `--keep-base`/`--onto`/`--root`/
//!   `--fork-point` pairwise conflicts, and the `usage` (129) paths.
//! * `<upstream>` / `<onto>` resolution, including `a...b` merge-base onto specs
//!   and `--keep-base`, plus `fatal: invalid upstream '<spec>'`,
//!   `fatal: Does not point to a valid commit '<spec>'` and `'<spec>': need
//!   exactly one merge base[ with branch]`.
//! * `error_on_missing_default_upstream()` on **stdout**, exit 1, in both its
//!   forms (on a branch, and `You are not currently on a branch.`).
//! * `require_clean_work_tree()`, byte for byte, including the `<path>: needs
//!   merge` lines `refresh_index()` prints on **stdout** for a conflicted index
//!   and the `additionally, your index contains uncommitted changes.` line.
//! * `can_fast_forward()` — merge-base checks plus `is_linear_history()` — and
//!   both of its outcomes: the silent up-to-date exit, and the
//!   `Current branch <b> is up to date, rebase forced.` variant that falls
//!   through when `REBASE_FORCE` is set.
//! * The two finishes that replay nothing:
//!   - **merge backend, empty todo** — the sequencer's `noop` item. `ORIG_HEAD`,
//!     `rebase (start): checkout <onto>`, the branch update
//!     `rebase (finish): <ref> onto <oid>`, `rebase (finish): returning to <ref>`,
//!     the `Rebasing (1/1)` progress line `pick_commits()` emits for the `noop`
//!     when `--no-ff`/`-f` turned off `allow_ff`, and
//!     `Successfully rebased and updated <ref>.` on stderr.
//!   - **apply backend, `merge-base(onto, head) == head`** — `First, rewinding
//!     head to replay your work on top of it...` then `Fast-forwarded <b> to
//!     <onto>.`, both on stdout, with the same ref/reflog dance and no
//!     `Successfully rebased` line.
//! * The one replay that is a re-commit rather than a merge: **`can_fast_forward()`
//!   holds but `REBASE_FORCE` is set**, i.e. `git rebase -f`/`--no-ff`/
//!   `--ignore-date`/`--committer-date-is-author-date` over a range already
//!   sitting on `<onto>`. Both backends rewrite the range's metadata there —
//!   the committer always, the author date under `--ignore-date` — while every
//!   tree stays byte-identical. See the `exact_replay` comment below for why
//!   that is exact rather than an approximation. This covers `Applying: <first
//!   line>` per commit on stdout for the apply backend, `Rebasing (n/m)` on
//!   stderr for the merge backend, the `rebase (pick)` reflog entries, and the
//!   branch landing on the rewritten tip.
//!
//! ### The sequencer
//!
//! Every other merge-backend replay runs through the instruction sheet, exactly
//! as git does: `run_specific_rebase()` does not branch on `-i`, it forces
//! `GIT_SEQUENCE_EDITOR=:` when `-i` was absent and then runs the sequencer for
//! both. So `git rebase <upstream>` and `git rebase -i <upstream>` share one
//! path here too — [`sequencer_rebase`] builds the sheet
//! ([`super::rebase_todo::make_script`]), applies `--autosquash` and `--exec`,
//! hands it to the sequence editor when `-i` asked for one, and then
//! [`Sequencer::run`] executes it.
//!
//! Implemented instructions: `pick`, `reword`, `edit`, `squash`, `fixup`
//! (including `-C`/`-c`), `exec`, `break`, `drop`, `noop` and comments, with
//! `--continue` / `--skip` / `--abort` / `--edit-todo` / `--show-current-patch`
//! over `.git/rebase-merge`. Each pick is a real three-way merge (the commit's
//! tree against the growing tip over its first parent) via
//! [`crate::merge_apply`]; a conflict stops the rebase with
//! `CONFLICT`/`could not apply` and leaves the conflicted worktree and index in
//! place.
//!
//! `label`, `reset` and `merge` execute too, which is what makes
//! `--rebase-merges` rebuild a branch topology rather than flatten it:
//! [`super::rebase_todo::make_script_with_merges`] writes the sheet,
//! [`Sequencer::do_label`] parks `refs/rewritten/<label>` at the current `HEAD`,
//! [`Sequencer::do_reset`] moves back to one, and [`Sequencer::do_merge`]
//! recreates the merge — fast-forwarding to the original commit when `HEAD` and
//! every merge head are still its parents, and running a real `ort` merge
//! otherwise. The `refs/rewritten/*` scratch refs are deleted when the rebase
//! finishes. Refused within `merge`, with a message naming the reason: octopus
//! merges (git shells out to `git merge -s octopus`), an explicit `-s`, the
//! `merge -c` editor variant, `merge` without an original `-C` commit, more than
//! one merge base, and `reset [new root]`. A refused instruction leaves the
//! rebase resumable, so `--abort` still works.
//!
//! `update-ref` executes too: it records where `HEAD` has reached in
//! `$state_dir/update-refs`, and the refs move when the sheet finishes.
//!
//! ### `--empty`, `--cherry-mark` and `--update-refs`
//!
//! A commit that contributes nothing to the replay is dropped in one of two
//! places, and which one decides what the user sees:
//!
//! * **Before the sheet**, by `revs.cherry_mark` (sequencer.c:6172), for a
//!   commit whose *patch* is already in `<upstream>`. The patch ids come from
//!   [`super::cherry::commit_patch_id`], the port of `commit_patch_id()` that
//!   `git cherry` already needed, applied over the two sides of
//!   `<upstream>...<orig_head>` the way `cherry_pick_list()` does. Such a commit
//!   never reaches a pick: it is announced with
//!   `warning: skipped previously applied commit <abbrev>` and the
//!   `advice.skippedCherryPicks` hint. `--reapply-cherry-picks` turns it off.
//! * **In the pick loop**, by `allow_empty()` (sequencer.c:1787-1818), for a
//!   commit that *became* empty — the merge resolved to the tree already at
//!   `HEAD`. `--empty` decides between the three outcomes: `keep` commits it
//!   anyway, `drop` reports `dropping <oid> <subject> -- patch contents already
//!   upstream`, and `stop` halts. `stop` is not a message this module prints:
//!   `do_commit()` re-enters a real `git commit` without `--allow-empty`
//!   (sequencer.c:1750-1755) and forwards its refusal, which is why the
//!   `The previous cherry-pick is now empty…` advice and the whole `git status`
//!   report arrive on **stderr**. The `--empty` value is recorded as the
//!   `drop_redundant_commits` / `keep_redundant_commits` marker pair.
//!
//! `--update-refs` generates an `update-ref <fullname>` instruction after every
//! pick whose commit carries a `refs/heads/*` decoration other than the branch
//! being rebased ([`super::rebase_todo::add_update_ref_commands`]), records the
//! `(ref, before, after)` triples in `$state_dir/update-refs`, fills each
//! `after` in as the instruction runs, and applies them all at the end with the
//! `rewritten during rebase` reflog message and the `Updated the following refs
//! with --update-refs:` report. A ref another worktree has checked out becomes a
//! `# Ref <name> checked out at '<path>'` comment instead, as git's
//! `add_decorations_to_list()` does.
//!
//! ### What is NOT ported, and why
//!
//! * `-s <strategy>` other than `ort`/`recursive`: the name is honoured as far
//!   as `$state_dir/strategy` and the `-Xsubtree` seeding go, but the merge is
//!   always merge-ort. `git rebase -s resolve` therefore reports merge-ort's
//!   `CONFLICT (content)` and stops resumably where git's `git-merge-resolve`
//!   shell strategy prints `Trying simple merge.` / `fatal: merge program
//!   failed` and dies.
//! * `--rebase-merges=rebase-cousins`, which changes which base
//!   `make_script_with_merges()` resets a branch to; only the default mode is
//!   ported.
//! * **The apply backend's mailbox.** [`run_am`] is ported in full — it pipes
//!   `git format-patch -k --stdout --full-index --cherry-pick --right-only
//!   --default-prefix --no-renames --no-cover-letter --pretty=mboxrd
//!   --topo-order --no-base <upstream>...<orig_head>` into
//!   `git am --rebasing --patch-format=mboxrd`, then `move_to_original_branch()`,
//!   exactly as `builtin/rebase.c:620-733` does — and the consuming half is
//!   complete: [`super::am`] implements `--rebasing` (`parse_mail_rebase`,
//!   `get_commit_info`, `write_commit_patch`, the `rewritten` list, `REBASE_HEAD`
//!   and the three-way fallback), and fed a stock mailbox it reproduces stock's
//!   commit objects, refs, reflogs and state files byte for byte, through a
//!   conflict stop and `--continue`/`--skip`.
//!
//!   What is still missing is one flag on the *producing* half:
//!   [`super::format_patch`] accepts `--cherry-pick` and `--right-only` but not
//!   `--pretty=mboxrd` (it implements the `format.mboxrd` *config* only). Until it
//!   does, a replay that would actually apply a patch stops at the format-patch
//!   step, `run_am()` puts the branch back where it found it, and the refusal
//!   quotes format-patch's own rejection rather than claiming `git am` is
//!   missing. The shapes that write no patches — the fast-forward finish and the
//!   exact replay above — are unaffected and still complete.
//!
//! ### `--root`
//!
//! `--root` replays every commit reachable from `<branch>` rather than the
//! `<upstream>..<branch>` range: `builtin/rebase.c` leaves `options.upstream`
//! NULL and builds its revision range as `<onto>..<orig_head>`. Without `--onto`
//! it first mints a stand-in `<onto>` — an empty-tree commit with no parents,
//! `options.squash_onto` — checks HEAD out at it, and lets the sequencer turn the
//! first pick into a *new root commit* (`CREATE_ROOT_COMMIT`) instead of a child
//! of that stand-in. Both shapes are ported here, including `do_pick_commit()`'s
//! fast-forward arm, which is what makes a plain `git rebase --root` over an
//! already-rooted linear history a no-op that leaves every commit id untouched
//! (reflog `rebase: fast-forward`) while `git rebase --root -f` rewrites them.
//!
//! ### Config
//!
//! Read here as the defaults their command-line options override:
//! `rebase.backend`, `rebase.autoStash`, `rebase.forkPoint` (false ⇒
//! `--no-fork-point`), `rebase.stat` (true ⇒ `--stat`), `rebase.autoSquash`
//! (only under an explicit `-i`, matching `cmd_rebase()`),
//! `rebase.rescheduleFailedExec`, `rebase.rebaseMerges` and `rebase.updateRefs`
//! (the last two raise git's apply-backend incompatibility errors; `updateRefs`
//! is otherwise the default `--update-refs`/`--no-update-refs` overrides).
//!
//! Read by the instruction sheet in [`super::rebase_todo`]:
//! `rebase.instructionFormat` (the oneline on each line),
//! `rebase.abbreviateCommands` (one-letter spellings) and
//! `rebase.missingCommitsCheck` (what happens when the user deletes a line).
//!
//! `rebase.maxLabelLength` sizes the labels `make_script_with_merges()` mints.
//!
//! ### Message rewriting
//!
//! `--signoff` appends the `Signed-off-by` trailer `append_signoff()` builds from
//! the committer identity, and `--trailer <t>` folds its arguments in through
//! `amend_strbuf_with_trailers()` — the whole of `trailer.c`'s
//! `where`/`ifExists`/`ifMissing` machinery, so a trailer the message already
//! ends with is not repeated. Both are applied on the exact-replay and sequencer
//! paths, in git's order (sign-off first), and both are applied *before* the pick
//! merges, so a conflict stop's `$state_dir/message` and `MERGE_MSG` already
//! carry them and `--continue` commits what the stop recorded.
//!
//! Everything this module comments — the `Conflicts:` block a stopped pick
//! appends, the todo list's help, the squash-message headers — is prefixed with
//! `comment_line_str`, resolved once by [`super::rebase_todo::comment_prefix`]
//! from `core.commentChar`/`core.commentString` (one knob; last one set wins;
//! the value is used whole, not truncated to a character; `auto` resolves to `#`
//! outside `git commit`). It has to be the configured string on both sides: the
//! message cleanup strips lines starting with it, so a block written with a
//! literal `#` under any other setting survived into the commit object.
//!
//! `--signoff` is
//! recorded in `$state_dir/signoff` and the `--trailer` arguments in
//! `$state_dir/trailer` (one per line), so a resumed rebase keeps applying both.
//! A `fixup -C`, whose message *replaces* rather than melds, picks them up in
//! `append_squash_message()` instead. `--trailer` arguments are validated up
//! front (`validate_trailer_args()`), which is the only way either option can
//! fail; both merely set `REBASE_FORCE` otherwise, so an up-to-date range takes
//! the noop / fast-forward finish (git rewrites nothing there —
//! `git rebase --signoff HEAD` leaves the tip untouched), and a missing upstream
//! (stdout, exit 1) or an invalid upstream/onto (`fatal:`, exit 128) still
//! reports git's own diagnostic in git's order.
//!
//! ### Hooks
//!
//! `pre-rebase` runs first, with `<upstream>` as spelled (or the literal
//! `--root`) and the optional `[<branch>]` operand — one argument when no
//! operand was given, because `run_hooks_l()` is NULL-terminated. A non-zero
//! exit stops the rebase with `error: The pre-rebase hook refused to rebase.`
//! before anything has moved.
//!
//! A rebase runs three more of them. `post-commit` fires for every commit the
//! sequencer writes, and `post-rewrite amend` for every one that amended an
//! existing commit (`try_to_commit()`, sequencer.c:1697-1699) — this port builds
//! those commit objects directly instead of re-entering `git commit`, so it runs
//! them itself. At the end, `post-rewrite rebase` receives the whole
//! `<old> <new>` map the run accumulated in `$state_dir/rewritten-list`
//! (sequencer.c:5190-5207), which is what makes a fixup chain report every melded
//! commit against the single one that replaced it, and what carries the map
//! across a conflict stop and `--continue`. None of the three can fail a rebase:
//! their exit status is dropped, as git drops it.
//!
//! Not ported: the `git notes copy --for-rewrite=rebase` that git runs just
//! before the final hook. Nothing here rewrites notes, and with no
//! `notes.rewriteRef` configured stock copies nothing either.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use super::{Arg, LongOpt};

/// `cmd_rebase()`'s `struct option builtin_rebase_options[]` (builtin/rebase.c),
/// in table order, as [`super::resolve_long`] reads it.
///
/// `no-verify`, `no-stat` and `no-ff` are entries spelled with their own `no-`,
/// which parse-options reads as the *unset* sense of `verify` / `stat` / `ff`; the
/// `OPT_CMDMODE` entries (`--continue`, `--skip`, `--abort`, `--quit`,
/// `--edit-todo`, `--show-current-patch`, `--apply`, `--merge`, `--interactive`)
/// and `--empty` carry `PARSE_OPT_NONEG`, so none of those has a `--no-` spelling.
pub(super) const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "onto",                        neg: true,  arg: Arg::Required },
    LongOpt { name: "keep-base",                   neg: true,  arg: Arg::None },
    LongOpt { name: "no-verify",                   neg: true,  arg: Arg::None },
    LongOpt { name: "quiet",                       neg: true,  arg: Arg::None },
    LongOpt { name: "verbose",                     neg: true,  arg: Arg::None },
    LongOpt { name: "no-stat",                     neg: true,  arg: Arg::None },
    LongOpt { name: "trailer",                     neg: true,  arg: Arg::Required },
    LongOpt { name: "signoff",                     neg: true,  arg: Arg::None },
    LongOpt { name: "committer-date-is-author-date", neg: true,  arg: Arg::None },
    LongOpt { name: "reset-author-date",           neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-date",                 neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-whitespace",           neg: true,  arg: Arg::None },
    LongOpt { name: "whitespace",                  neg: true,  arg: Arg::Required },
    LongOpt { name: "force-rebase",                neg: true,  arg: Arg::None },
    LongOpt { name: "no-ff",                       neg: true,  arg: Arg::None },
    LongOpt { name: "continue",                    neg: false, arg: Arg::None },
    LongOpt { name: "skip",                        neg: false, arg: Arg::None },
    LongOpt { name: "abort",                       neg: false, arg: Arg::None },
    LongOpt { name: "quit",                        neg: false, arg: Arg::None },
    LongOpt { name: "edit-todo",                   neg: false, arg: Arg::None },
    LongOpt { name: "show-current-patch",          neg: false, arg: Arg::None },
    LongOpt { name: "apply",                       neg: false, arg: Arg::None },
    LongOpt { name: "merge",                       neg: false, arg: Arg::None },
    LongOpt { name: "interactive",                 neg: false, arg: Arg::None },
    LongOpt { name: "preserve-merges",             neg: true,  arg: Arg::None },
    LongOpt { name: "rerere-autoupdate",           neg: true,  arg: Arg::None },
    LongOpt { name: "empty",                       neg: false, arg: Arg::Required },
    LongOpt { name: "keep-empty",                  neg: true,  arg: Arg::None },
    LongOpt { name: "autosquash",                  neg: true,  arg: Arg::None },
    LongOpt { name: "update-refs",                 neg: true,  arg: Arg::None },
    LongOpt { name: "gpg-sign",                    neg: true,  arg: Arg::Optional },
    LongOpt { name: "autostash",                   neg: true,  arg: Arg::None },
    LongOpt { name: "exec",                        neg: true,  arg: Arg::Required },
    LongOpt { name: "allow-empty-message",         neg: true,  arg: Arg::None },
    LongOpt { name: "rebase-merges",               neg: true,  arg: Arg::Optional },
    LongOpt { name: "fork-point",                  neg: true,  arg: Arg::None },
    LongOpt { name: "strategy",                    neg: true,  arg: Arg::Required },
    LongOpt { name: "strategy-option",             neg: true,  arg: Arg::Required },
    LongOpt { name: "root",                        neg: true,  arg: Arg::None },
    LongOpt { name: "reschedule-failed-exec",      neg: true,  arg: Arg::None },
    LongOpt { name: "reapply-cherry-picks",        neg: true,  arg: Arg::None },
];

use super::rebase_todo as todo;

/// git's `builtin/rebase.c` usage block, reproduced verbatim (git 2.55.0) so the
/// `-h` and unknown-option paths are byte-identical.
pub(super) const USAGE: &str = "\
usage: git rebase [-i] [options] [--exec <cmd>] [--onto <newbase> | --keep-base] [<upstream> [<branch>]]
   or: git rebase [-i] [options] [--exec <cmd>] [--onto <newbase>] --root [<branch>]
   or: git rebase --continue | --abort | --skip | --edit-todo

    --[no-]onto <revision>
                          rebase onto given branch instead of upstream
    --[no-]keep-base      use the merge-base of upstream and branch as the current base
    --no-verify           allow pre-rebase hook to run
    --verify              opposite of --no-verify
    -q, --[no-]quiet      be quiet. implies --no-stat
    -v, --[no-]verbose    display a diffstat of what changed upstream
    -n, --no-stat         do not show diffstat of what changed upstream
    --stat                opposite of --no-stat
    --[no-]trailer <trailer>
                          add custom trailer(s)
    --[no-]signoff        add a Signed-off-by trailer to each commit
    --[no-]committer-date-is-author-date
                          make committer date match author date
    --[no-]reset-author-date
                          ignore author date and use current date
    -C <n>                passed to 'git apply'
    --[no-]ignore-whitespace
                          ignore changes in whitespace
    --[no-]whitespace <action>
                          passed to 'git apply'
    -f, --[no-]force-rebase
                          cherry-pick all commits, even if unchanged
    --no-ff               cherry-pick all commits, even if unchanged
    --ff                  opposite of --no-ff
    --continue            continue
    --skip                skip current patch and continue
    --abort               abort and check out the original branch
    --quit                abort but keep HEAD where it is
    --edit-todo           edit the todo list during an interactive rebase
    --show-current-patch  show the patch file being applied or merged
    --apply               use apply strategies to rebase
    -m, --merge           use merging strategies to rebase
    -i, --interactive     let the user edit the list of commits to rebase
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --empty (drop|keep|stop)
                          how to handle commits that become empty
    --[no-]autosquash     move commits that begin with squash!/fixup! under -i
    --[no-]update-refs    update branches that point to commits that are being rebased
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG-sign commits
    --[no-]autostash      automatically stash/stash pop before and after
    -x, --[no-]exec <exec>
                          add exec lines after each commit of the editable list
    -r, --[no-]rebase-merges[=<mode>]
                          try to rebase merges instead of skipping them
    --[no-]fork-point     use 'merge-base --fork-point' to refine upstream
    -s, --[no-]strategy <strategy>
                          use the given merge strategy
    -X, --[no-]strategy-option <option>
                          pass the argument through to the merge strategy
    --[no-]root           rebase all reachable commits up to the root(s)
    --[no-]reschedule-failed-exec
                          automatically re-schedule any `exec` that fails
    --[no-]reapply-cherry-picks
                          apply all changes, even those already present upstream

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]ignore-date`, `--[no-]preserve-merges`, `--[no-]keep-empty`,
/// `--[no-]allow-empty-message`.
/// Captured byte-for-byte from stock git 2.55.0's `git rebase --help-all`.
pub(super) const USAGE_ALL: &str = r#"usage: git rebase [-i] [options] [--exec <cmd>] [--onto <newbase> | --keep-base] [<upstream> [<branch>]]
   or: git rebase [-i] [options] [--exec <cmd>] [--onto <newbase>] --root [<branch>]
   or: git rebase --continue | --abort | --skip | --edit-todo

    --[no-]onto <revision>
                          rebase onto given branch instead of upstream
    --[no-]keep-base      use the merge-base of upstream and branch as the current base
    --no-verify           allow pre-rebase hook to run
    --verify              opposite of --no-verify
    -q, --[no-]quiet      be quiet. implies --no-stat
    -v, --[no-]verbose    display a diffstat of what changed upstream
    -n, --no-stat         do not show diffstat of what changed upstream
    --stat                opposite of --no-stat
    --[no-]trailer <trailer>
                          add custom trailer(s)
    --[no-]signoff        add a Signed-off-by trailer to each commit
    --[no-]committer-date-is-author-date
                          make committer date match author date
    --[no-]reset-author-date
                          ignore author date and use current date
    --[no-]ignore-date    synonym of --reset-author-date
    -C <n>                passed to 'git apply'
    --[no-]ignore-whitespace
                          ignore changes in whitespace
    --[no-]whitespace <action>
                          passed to 'git apply'
    -f, --[no-]force-rebase
                          cherry-pick all commits, even if unchanged
    --no-ff               cherry-pick all commits, even if unchanged
    --ff                  opposite of --no-ff
    --continue            continue
    --skip                skip current patch and continue
    --abort               abort and check out the original branch
    --quit                abort but keep HEAD where it is
    --edit-todo           edit the todo list during an interactive rebase
    --show-current-patch  show the patch file being applied or merged
    --apply               use apply strategies to rebase
    -m, --merge           use merging strategies to rebase
    -i, --interactive     let the user edit the list of commits to rebase
    -p, --[no-]preserve-merges
                          (REMOVED) was: try to recreate merges instead of ignoring them
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --empty (drop|keep|stop)
                          how to handle commits that become empty
    -k, --[no-]keep-empty keep commits which start empty
    --[no-]autosquash     move commits that begin with squash!/fixup! under -i
    --[no-]update-refs    update branches that point to commits that are being rebased
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG-sign commits
    --[no-]autostash      automatically stash/stash pop before and after
    -x, --[no-]exec <exec>
                          add exec lines after each commit of the editable list
    --[no-]allow-empty-message
                          allow rebasing commits with empty messages
    -r, --[no-]rebase-merges[=<mode>]
                          try to rebase merges instead of skipping them
    --[no-]fork-point     use 'merge-base --fork-point' to refine upstream
    -s, --[no-]strategy <strategy>
                          use the given merge strategy
    -X, --[no-]strategy-option <option>
                          pass the argument through to the merge strategy
    --[no-]root           rebase all reachable commits up to the root(s)
    --[no-]reschedule-failed-exec
                          automatically re-schedule any `exec` that fails
    --[no-]reapply-cherry-picks
                          apply all changes, even those already present upstream

"#;

/// `options.flags`, mirroring the `REBASE_*` bits in `builtin/rebase.c`.
const NO_QUIET: u32 = 1 << 0;
const VERBOSE: u32 = 1 << 1;
const DIFFSTAT: u32 = 1 << 2;
const FORCE: u32 = 1 << 3;
const INTERACTIVE_EXPLICIT: u32 = 1 << 4;

/// `enum rebase_type`. The backend is *inferred* from the options, and which
/// one is in force decides several of git's `die()`s, so it is tracked exactly.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Unspecified,
    Apply,
    Merge,
}

/// The mode options of `git rebase`, which replace the normal invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModeOption {
    Continue,
    Skip,
    Abort,
    Quit,
    EditTodo,
    ShowCurrentPatch,
}

impl ModeOption {
    fn flag(self) -> &'static str {
        match self {
            ModeOption::Continue => "--continue",
            ModeOption::Skip => "--skip",
            ModeOption::Abort => "--abort",
            ModeOption::Quit => "--quit",
            ModeOption::EditTodo => "--edit-todo",
            ModeOption::ShowCurrentPatch => "--show-current-patch",
        }
    }
}

/// One commit of an exact replay, resolved up front so a refusal further down
/// still leaves the repository untouched. `parent_tree` is only read to spot the
/// empty commits `git format-patch` would drop.
struct Replay {
    /// The commit being replayed. Only the `post-rewrite` payload reads it: it is
    /// the "old" half of the `<old> <new>` pair `record_in_rewritten()` writes for
    /// every pick.
    oid: ObjectId,
    tree: ObjectId,
    parent_tree: ObjectId,
    message: BString,
    author: gix::actor::Signature,
}

/// git's `imply_merge()`: a merge-only option either selects the merge backend
/// or, if the apply backend was already selected, is fatal.
fn imply_merge(ty: &mut Backend, option: &str) -> Result<(), String> {
    match *ty {
        Backend::Apply => Err(format!("{option} requires the merge backend")),
        Backend::Merge => Ok(()),
        Backend::Unspecified => {
            *ty = Backend::Merge;
            Ok(())
        }
    }
}

pub fn rebase(args: &[String]) -> Result<ExitCode> {
    // `die()`: one line on stderr prefixed `fatal: `, exit 128.
    macro_rules! die {
        ($($t:tt)*) => {{
            eprintln!("fatal: {}", format_args!($($t)*));
            return Ok(ExitCode::from(128));
        }};
    }
    // `usage_with_options()`: the whole usage block on stderr, exit 129.
    macro_rules! usage {
        () => {{
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }};
    }
    // `parse_options` errors: one line, then the usage block, exit 129.
    macro_rules! opterr {
        ($($t:tt)*) => {{
            eprint!("error: {}\n{USAGE}", format_args!($($t)*));
            return Ok(ExitCode::from(129));
        }};
    }
    macro_rules! try_imply {
        ($ty:expr, $opt:expr) => {
            if let Err(m) = imply_merge(&mut $ty, $opt) {
                die!("{m}");
            }
        };
    }

    // `total_argc` is git's pre-parse argc, i.e. "rebase" plus everything after
    // it. The `--continue`-family check below compares against it directly.
    let total_argc = args.len() + 1;

    // --- state ------------------------------------------------------------
    let mut flags: u32 = NO_QUIET;
    let mut ty = Backend::Unspecified;
    let mut action: Option<ModeOption> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut onto_name: Option<String> = None;
    let mut keep_base = false;
    let mut ok_to_skip_pre_rebase = false;
    let mut trailers: Vec<String> = Vec::new();
    let mut signoff = false;
    let mut committer_date_is_author_date = false;
    let mut ignore_date = false;
    let mut git_am_opts: Vec<String> = Vec::new();
    let mut ignore_whitespace = false;
    let mut preserve_merges = false;
    let mut empty_set = false;
    // `options.empty` — `EMPTY_UNSPECIFIED` until the command line or
    // `cmd_rebase()`'s defaulting below picks one of `drop`/`keep`/`stop`.
    let mut empty: Option<Empty> = None;
    // `options.keep_empty`, which `REBASE_OPTIONS_INIT` starts at 1: a commit
    // that changes nothing is listed in the instruction sheet (tagged
    // ` # empty`) unless `--no-keep-empty` drops it.
    let mut keep_empty = true;
    let mut autosquash: i32 = -1;
    let mut update_refs: i32 = -1;
    let mut autostash = false;
    let mut exec: Vec<String> = Vec::new();
    let mut rebase_merges: i32 = -1;
    let mut fork_point: i32 = -1;
    let mut strategy: Option<String> = None;
    let mut strategy_opts: Vec<String> = Vec::new();
    let mut root = false;
    let mut reschedule_failed_exec: i32 = -1;
    let mut reapply_cherry_picks: i32 = -1;
    // `opts.allow_rerere_auto` — `RERERE_AUTOUPDATE` / `RERERE_NOAUTOUPDATE` /
    // unset, which `repo_rerere()` maps to "stage the replay" / "leave it in the
    // worktree" / "ask `rerere.autoupdate`".
    let mut rerere_autoupdate: Option<bool> = None;
    // `options.gpg_sign_opt` — set by `-S`/`--gpg-sign[=<key-id>]`, cleared by
    // `--no-gpg-sign`, and seeded from `commit.gpgSign` below.
    let mut gpg_sign: i32 = -1;

    // git reads the repository (and the in-progress state dirs, which seed the
    // backend) before `parse_options` runs.
    // Every ref this moves carries a reflog line, and git writes those with an
    // identity it synthesizes from the OS when `user.*` is unset — only a
    // `commit` with nothing determinable is refused. Without this a bare runner,
    // a container or a `sudo` shell cannot switch branches at all, and a
    // recursive submodule walk aborts on the first one it reaches.
    let mut repo = gix::discover(".")?;
    crate::ensure_reflog_identity(&mut repo);
    let state_dir = repo.common_dir();
    let apply_in_progress = state_dir.join("rebase-apply").is_dir();
    let merge_in_progress = state_dir.join("rebase-merge").is_dir();
    if apply_in_progress {
        ty = Backend::Apply;
    } else if merge_in_progress {
        ty = Backend::Merge;
    }
    let in_progress = apply_in_progress || merge_in_progress;

    // `rebase_config()` runs before `parse_options`, so `rebase.autoStash`
    // seeds the default and the `--autostash`/`--no-autostash` option (handled
    // in the loop below via `autostash = !unset`) overwrites it — git's
    // `opts->autostash = git_config_bool()` followed by `OPT_AUTOSTASH`, which
    // is why the explicit flag wins over the config both ways.
    if repo.config_snapshot().boolean("rebase.autoStash") == Some(true) {
        autostash = true;
    }
    // `rebase.stat` is the default for `--stat`/`--no-stat`: it sets or clears
    // REBASE_DIFFSTAT and nothing else, so unlike `-v` it never turns on
    // REBASE_VERBOSE and never disables the preemptive fast-forward. The `--stat`
    // / `--no-stat` / `-q` / `-v` arms in the loop below overwrite it.
    match repo.config_snapshot().boolean("rebase.stat") {
        Some(true) => flags |= DIFFSTAT,
        Some(false) => flags &= !DIFFSTAT,
        None => {}
    }
    // `rebase.forkPoint`: `opts->fork_point = git_config_bool(...) ? -1 : 0`, i.e.
    // only the `false` side is a decision — it pins `--no-fork-point` so a rebase
    // against the branch's tracking ref does not walk the upstream reflog. `true`
    // restores the -1 "unset" state, which is already the initial value.
    match repo.config_snapshot().boolean("rebase.forkPoint") {
        Some(true) => fork_point = -1,
        Some(false) => fork_point = 0,
        None => {}
    }

    // --- parse_options ----------------------------------------------------
    let mut i = 0;
    let mut no_more_options = false;
    while i < args.len() {
        let a = args[i].as_str();

        if no_more_options || a == "-" || !a.starts_with('-') || a.len() == 1 {
            positional.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            no_more_options = true;
            i += 1;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): an exact match tested after the `--` break
        // above and ahead of parse_long_opt(), so it wins over the unknown-name
        // refusal just below, never abbreviates, and never takes an `=<value>`.
        // It renders `USAGE_FULL`, which for `rebase` keeps six hidden entries
        // including the `(REMOVED)` `--preserve-merges`.
        if a == "--help-all" {
            return Ok(super::show_usage(USAGE_ALL));
        }

        // A long name no table entry claims loses here, before the `no-` splitting
        // below ever sees it. That order is load-bearing: `--abort`, `--continue`,
        // `--skip`, `--quit`, `--edit-todo`, `--show-current-patch`, `--apply`,
        // `--merge` and `--interactive` are `OPT_CMDMODE`s, which carry
        // `PARSE_OPT_NONEG` and therefore have no `--no-` spelling at all — the
        // hand-rolled splitter would otherwise read `--no-abort` as a negated
        // `--abort` and act on it, where stock answers `unknown option`.
        if let Some(long) = a.strip_prefix("--") {
            if matches!(
                super::resolve_long(LONG_OPTS, long),
                super::Resolved::Unknown
            ) {
                opterr!("unknown option `{long}'");
            }
        }

        // Respell a unique abbreviation as the name it resolves to, so `--autosq`
        // reaches the same arm as `--autosquash`. The `no-` handling below is left
        // exactly as it was: the resolver hands back a full spelling, which that
        // code then splits the same way it always split a full spelling.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };

        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = match long.find('=') {
                Some(p) => (&long[..p], Some(long[p + 1..].to_string())),
                None => (long, None),
            };
            // parse-options accepts `--no-<name>` for every option that is not
            // itself spelled with a leading `no-`; the two `no-*`-named options
            // here (`--no-verify`, `--no-stat`, `--no-ff`) fall out of the same
            // rule with their sense flipped, which is exactly how git's usage
            // block renders them.
            let (name, unset) = match name.strip_prefix("no-") {
                Some(rest) if !rest.is_empty() => (rest, true),
                _ => (name, false),
            };

            // Pull the value of a value-taking option: inline `=v`, else the
            // next argv entry.
            macro_rules! value {
                () => {{
                    match inline.clone() {
                        Some(v) => v,
                        None => {
                            i += 1;
                            match args.get(i) {
                                Some(v) => v.clone(),
                                None => {
                                    // git names the option without its dashes
                                    // and prints no usage block here.
                                    eprintln!("error: option `{name}' requires a value");
                                    return Ok(ExitCode::from(129));
                                }
                            }
                        }
                    }
                }};
            }
            macro_rules! noarg {
                () => {
                    if inline.is_some() {
                        opterr!("option `{name}' takes no value");
                    }
                };
            }

            match name {
                "onto" => {
                    if unset {
                        onto_name = None;
                    } else {
                        onto_name = Some(value!());
                    }
                }
                "keep-base" => {
                    noarg!();
                    keep_base = !unset;
                }
                "verify" => {
                    noarg!();
                    // `--no-verify` *enables* skipping the hook.
                    ok_to_skip_pre_rebase = unset;
                }
                "quiet" => {
                    noarg!();
                    // OPT_NEGBIT: plain `-q`/`--quiet` clears the bits.
                    if unset {
                        flags |= NO_QUIET | VERBOSE | DIFFSTAT;
                    } else {
                        flags &= !(NO_QUIET | VERBOSE | DIFFSTAT);
                    }
                }
                "verbose" => {
                    noarg!();
                    if unset {
                        flags &= !(NO_QUIET | VERBOSE | DIFFSTAT);
                    } else {
                        flags |= NO_QUIET | VERBOSE | DIFFSTAT;
                    }
                }
                // `--no-stat` clears the diffstat bit; `--stat` is its negation.
                "stat" => {
                    noarg!();
                    if unset {
                        flags &= !DIFFSTAT;
                    } else {
                        flags |= DIFFSTAT;
                    }
                }
                "trailer" => {
                    if unset {
                        trailers.clear();
                    } else {
                        trailers.push(value!());
                    }
                }
                "signoff" => {
                    noarg!();
                    signoff = !unset;
                }
                "committer-date-is-author-date" => {
                    noarg!();
                    committer_date_is_author_date = !unset;
                }
                "reset-author-date" | "ignore-date" => {
                    noarg!();
                    ignore_date = !unset;
                }
                "ignore-whitespace" => {
                    noarg!();
                    ignore_whitespace = !unset;
                }
                "whitespace" => {
                    if unset {
                        git_am_opts.retain(|o| !o.starts_with("--whitespace="));
                    } else {
                        let v = value!();
                        git_am_opts.push(format!("--whitespace={v}"));
                    }
                }
                "force-rebase" => {
                    noarg!();
                    if unset {
                        flags &= !FORCE;
                    } else {
                        flags |= FORCE;
                    }
                }
                // `--no-ff` sets REBASE_FORCE; `--ff` is its negation.
                "ff" => {
                    noarg!();
                    if unset {
                        flags |= FORCE;
                    } else {
                        flags &= !FORCE;
                    }
                }
                "continue" => {
                    noarg!();
                    action = Some(ModeOption::Continue);
                }
                "skip" => {
                    noarg!();
                    action = Some(ModeOption::Skip);
                }
                "abort" => {
                    noarg!();
                    action = Some(ModeOption::Abort);
                }
                "quit" => {
                    noarg!();
                    action = Some(ModeOption::Quit);
                }
                "edit-todo" => {
                    noarg!();
                    action = Some(ModeOption::EditTodo);
                }
                "show-current-patch" => {
                    noarg!();
                    action = Some(ModeOption::ShowCurrentPatch);
                }
                "apply" => {
                    noarg!();
                    if ty == Backend::Merge {
                        die!("apply options and merge options cannot be used together");
                    }
                    ty = Backend::Apply;
                }
                "merge" => {
                    noarg!();
                    if ty == Backend::Apply {
                        die!("apply options and merge options cannot be used together");
                    }
                    ty = Backend::Merge;
                }
                "interactive" => {
                    noarg!();
                    if ty == Backend::Apply {
                        die!("apply options and merge options cannot be used together");
                    }
                    ty = Backend::Merge;
                    flags |= INTERACTIVE_EXPLICIT;
                }
                "preserve-merges" => {
                    noarg!();
                    preserve_merges = true;
                }
                // `OPT_RERERE_AUTOUPDATE(&opts.allow_rerere_auto)`: the flag
                // `do_pick_commit()` hands to `repo_rerere(r,
                // opts->allow_rerere_auto)` when a pick conflicts.
                "rerere-autoupdate" => {
                    noarg!();
                    rerere_autoupdate = Some(!unset);
                }
                "empty" => {
                    let v = value!();
                    match v.to_ascii_lowercase().as_str() {
                        "drop" => empty = Some(Empty::Drop),
                        "keep" => empty = Some(Empty::Keep),
                        "stop" => empty = Some(Empty::Stop),
                        "ask" => {
                            empty = Some(Empty::Stop);
                            eprintln!(
                                "warning: --empty=ask is deprecated; use '--empty=stop' instead."
                            );
                        }
                        _ => die!(
                            "unrecognized empty type '{v}'; valid values are \"drop\", \"keep\", and \"stop\"."
                        ),
                    }
                    empty_set = true;
                }
                // `--keep-empty` decides whether a commit that changes nothing
                // gets a line in the instruction sheet.
                "keep-empty" => {
                    noarg!();
                    try_imply!(ty, if unset { "--no-keep-empty" } else { "--keep-empty" });
                    keep_empty = !unset;
                }
                "autosquash" => {
                    noarg!();
                    autosquash = i32::from(!unset);
                }
                "update-refs" => {
                    noarg!();
                    update_refs = i32::from(!unset);
                }
                // `-S`/`--gpg-sign[=<key-id>]` asks for signed replays; `--no-gpg-sign`
                // (and the absence of both) does not. Only the affirmative form is
                // refused, and only where a commit would actually be created — an
                // up-to-date `git rebase -S <upstream>` signs nothing and completes.
                "gpg-sign" => gpg_sign = i32::from(!unset),
                "autostash" => {
                    noarg!();
                    autostash = !unset;
                }
                "exec" => {
                    if unset {
                        exec.clear();
                    } else {
                        exec.push(value!());
                    }
                }
                "allow-empty-message" => noarg!(),
                "rebase-merges" => {
                    if unset {
                        rebase_merges = 0;
                    } else {
                        rebase_merges = 1;
                        match inline.as_deref() {
                            None => {}
                            Some("") => eprintln!(
                                "warning: --rebase-merges with an empty string argument is deprecated and will stop working in a future version of Git. Use --rebase-merges without an argument instead, which does the same thing."
                            ),
                            Some("no-rebase-cousins" | "rebase-cousins") => {}
                            Some(other) => die!("Unknown rebase-merges mode: {other}"),
                        }
                    }
                }
                "fork-point" => {
                    noarg!();
                    fork_point = i32::from(!unset);
                }
                "strategy" => {
                    if unset {
                        strategy = None;
                    } else {
                        strategy = Some(value!());
                    }
                }
                "strategy-option" => {
                    if unset {
                        strategy_opts.clear();
                    } else {
                        strategy_opts.push(value!());
                    }
                }
                "root" => {
                    noarg!();
                    root = !unset;
                }
                "reschedule-failed-exec" => {
                    noarg!();
                    reschedule_failed_exec = i32::from(!unset);
                }
                "reapply-cherry-picks" => {
                    noarg!();
                    reapply_cherry_picks = i32::from(!unset);
                }
                _ => opterr!("unknown option `{}'", &a[2..]),
            }
            i += 1;
            continue;
        }

        // --- short options, including clusters and attached values ---------
        let chars: Vec<char> = a.chars().collect();
        let mut k = 1;
        while k < chars.len() {
            let c = chars[k];
            let rest: String = chars[k + 1..].iter().collect();
            // Value for an option that requires one: the rest of the cluster,
            // else the next argv entry.
            macro_rules! sval {
                () => {{
                    if rest.is_empty() {
                        i += 1;
                        match args.get(i) {
                            Some(v) => v.clone(),
                            None => {
                                eprintln!("error: switch `{c}' requires a value");
                                return Ok(ExitCode::from(129));
                            }
                        }
                    } else {
                        rest.clone()
                    }
                }};
            }
            match c {
                'h' => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                'q' => flags &= !(NO_QUIET | VERBOSE | DIFFSTAT),
                'v' => flags |= NO_QUIET | VERBOSE | DIFFSTAT,
                'n' => flags &= !DIFFSTAT,
                'f' => flags |= FORCE,
                'm' => {
                    if ty == Backend::Apply {
                        die!("apply options and merge options cannot be used together");
                    }
                    ty = Backend::Merge;
                }
                'i' => {
                    if ty == Backend::Apply {
                        die!("apply options and merge options cannot be used together");
                    }
                    ty = Backend::Merge;
                    flags |= INTERACTIVE_EXPLICIT;
                }
                'p' => preserve_merges = true,
                // Same as the long `--keep-empty` above: it only selects the
                // merge backend, and carries no state of its own.
                'k' => try_imply!(ty, "--keep-empty"),
                'C' => {
                    let v = sval!();
                    git_am_opts.push(format!("-C{v}"));
                    break;
                }
                'x' => {
                    exec.push(sval!());
                    break;
                }
                's' => {
                    strategy = Some(sval!());
                    break;
                }
                'X' => {
                    strategy_opts.push(sval!());
                    break;
                }
                // Optional-argument shorts consume only an attached value.
                'S' => {
                    gpg_sign = 1;
                    break;
                }
                'r' => {
                    rebase_merges = 1;
                    if !rest.is_empty() {
                        match rest.as_str() {
                            "no-rebase-cousins" | "rebase-cousins" => {}
                            other => die!("Unknown rebase-merges mode: {other}"),
                        }
                    }
                    break;
                }
                _ => opterr!("unknown switch `{c}'"),
            }
            k += 1;
        }
        i += 1;
    }

    // --- post-parse checks, in builtin/rebase.c order ----------------------
    //
    // ```c
    // if (options.trailer_args.nr) {
    //         if (validate_trailer_args(&options.trailer_args))
    //                 die(NULL);
    //         options.flags |= REBASE_FORCE;
    // }
    // ```
    // (builtin/rebase.c:1299-1303). `die(NULL)` prints nothing of its own — the
    // message is the `error()` `validate_trailer_args()` already wrote — so this
    // exits 128 silently. `REBASE_FORCE` is what makes every commit in the range
    // be re-committed rather than fast-forwarded: without it a pick that already
    // sits on its parent would keep its old message, trailer and all.
    if !trailers.is_empty() {
        if !super::interpret_trailers::validate_trailer_args(&trailers)? {
            return Ok(ExitCode::from(128));
        }
        flags |= FORCE;
    }
    // git sets REBASE_FORCE for `--signoff` alongside trailers, so an already
    // up-to-date range still replays (the noop finish) instead of taking the
    // silent up-to-date exit — `git rebase --signoff HEAD` prints
    // `Current branch <b> is up to date, rebase forced.`, not `... up to date.`.
    if signoff {
        flags |= FORCE;
    }

    if preserve_merges {
        eprintln!(
            "fatal: --preserve-merges was replaced by --rebase-merges\n\
             Note: Your `pull.rebase` configuration may also be set to 'preserve',\n\
             which is no longer supported; use 'merges' instead"
        );
        return Ok(ExitCode::from(128));
    }

    // A mode option must be the *only* argument.
    if action.is_some() && total_argc != 2 {
        usage!();
    }
    if positional.len() > 2 {
        usage!();
    }

    if keep_base {
        if onto_name.is_some() {
            die!("options '--keep-base' and '--onto' cannot be used together");
        }
        if root {
            die!("options '--keep-base' and '--root' cannot be used together");
        }
        if fork_point < 0 {
            fork_point = 0;
        }
    }
    if root && fork_point > 0 {
        die!("options '--root' and '--fork-point' cannot be used together");
    }

    if let Some(m) = action {
        if !in_progress {
            die!("no rebase in progress");
        }
        if m == ModeOption::EditTodo && ty != Backend::Merge {
            die!("The --edit-todo action can only be used during interactive rebase.");
        }
        // `run_am()`'s `ACTION_CONTINUE`/`ACTION_SKIP` arms plus `cmd_rebase()`'s
        // shared `ACTION_ABORT`/`ACTION_QUIT`: a `.git/rebase-apply` session is
        // resumed through `git am`, not through the sequencer.
        if apply_in_progress {
            if matches!(m, ModeOption::EditTodo | ModeOption::ShowCurrentPatch) {
                // `--edit-todo` already died above; `--show-current-patch` is
                // `git am --show-current-patch`, which under `--rebasing` shows
                // the original commit (builtin/am.c:2229).
                return super::am::am(&["--show-current-patch".to_string()]);
            }
            return rebase_apply_resume(&repo, m);
        }
        return match m {
            ModeOption::Abort => rebase_abort(&repo),
            ModeOption::Quit => rebase_quit(&repo),
            ModeOption::Continue => rebase_continue(&repo, false),
            ModeOption::Skip => rebase_continue(&repo, true),
            ModeOption::EditTodo => rebase_edit_todo(&repo),
            ModeOption::ShowCurrentPatch => rebase_show_current_patch(&repo),
        };
    }
    if in_progress {
        let base = if apply_in_progress {
            "rebase-apply"
        } else {
            "rebase-merge"
        };
        let dir = if apply_in_progress {
            state_dir.join("rebase-apply")
        } else {
            state_dir.join("rebase-merge")
        };
        eprintln!(
            "fatal: It seems that there is already a {base} directory, and\n\
             I wonder if you are in the middle of another rebase.  If that is the\n\
             case, please try\n\tgit rebase (--continue | --abort | --skip)\n\
             If that is not the case, please\n\trm -fr \"{}\"\n\
             and run me again.  I am stopping in case you still have something\n\
             valuable there.",
            dir.display()
        );
        return Ok(ExitCode::from(128));
    }

    let mut allow_preemptive_ff = true;
    if flags & INTERACTIVE_EXPLICIT != 0 || !exec.is_empty() || autosquash == 1 {
        allow_preemptive_ff = false;
    }
    if committer_date_is_author_date || ignore_date {
        flags |= FORCE;
    }

    // git's chain is `if fix/strip … else if -C … else if --whitespace= …`, so
    // `fix` and `strip` are consumed by the first arm and never reach the
    // stricter value check below. Reordering these would reject them.
    for opt in &git_am_opts {
        if opt == "--whitespace=fix" || opt == "--whitespace=strip" {
            allow_preemptive_ff = false;
        } else if let Some(p) = opt.strip_prefix("-C") {
            if !p.chars().all(|c| c.is_ascii_digit()) {
                die!("switch `C' expects a numerical value");
            }
        } else if let Some(p) = opt.strip_prefix("--whitespace=") {
            if !p.is_empty() && !matches!(p, "warn" | "nowarn" | "error" | "error-all") {
                die!("Invalid whitespace option: '{p}'");
            }
        }
    }

    for cmd in &exec {
        if cmd.contains('\n') {
            eprintln!("error: exec commands cannot contain newlines");
            return Ok(ExitCode::from(1));
        }
        if cmd.trim_matches([' ', '\t', '\r', '\x0c', '\x0b']).is_empty() {
            eprintln!("error: empty exec command");
            return Ok(ExitCode::from(1));
        }
    }

    if flags & NO_QUIET == 0 {
        git_am_opts.push("-q".to_string());
    }

    if empty_set {
        try_imply!(ty, "--empty");
    }

    if reapply_cherry_picks < 0 {
        reapply_cherry_picks = i32::from(keep_base);
    } else if !keep_base {
        try_imply!(
            ty,
            if reapply_cherry_picks == 1 {
                "--reapply-cherry-picks"
            } else {
                "--no-reapply-cherry-picks"
            }
        );
    }

    if !exec.is_empty() {
        try_imply!(ty, "--exec");
    }

    if ty == Backend::Apply {
        if ignore_whitespace {
            git_am_opts.push("--ignore-whitespace".to_string());
        }
        if committer_date_is_author_date {
            git_am_opts.push("--committer-date-is-author-date".to_string());
        }
        if ignore_date {
            git_am_opts.push("--ignore-date".to_string());
        }
    } else if ignore_whitespace {
        strategy_opts.push("ignore-space-change".to_string());
    }

    if strategy.is_none() && !strategy_opts.is_empty() {
        strategy = Some("ort".to_string());
    }
    if strategy.is_some() {
        try_imply!(ty, "--strategy");
    }

    if root && onto_name.is_none() {
        try_imply!(ty, "--root without --onto");
    }
    if !trailers.is_empty() {
        try_imply!(ty, "--trailer");
    }

    // "all am options except -q are compatible only with --apply". The two
    // config keys below are the merge backend's own defaults, so reaching the
    // apply backend with either of them on — and no command-line `--no-…` to
    // clear it — is a contradiction git refuses rather than silently drops.
    // `rebase.rebaseMerges` is `git_parse_maybe_bool`'d, and a non-boolean value
    // (`rebase-cousins` / `no-rebase-cousins`) counts as on.
    if !git_am_opts.is_empty() || ty == Backend::Apply {
        let has_real_am_opt = git_am_opts.iter().any(|o| o != "-q");
        if has_real_am_opt || ty == Backend::Apply {
            let snap = repo.config_snapshot();
            let cfg_rebase_merges = snap
                .string("rebase.rebaseMerges")
                .map(|_| snap.boolean("rebase.rebaseMerges").unwrap_or(true));
            let cfg_update_refs = snap.boolean("rebase.updateRefs");
            if ty == Backend::Merge {
                die!("apply options and merge options cannot be used together");
            } else if rebase_merges == -1 && cfg_rebase_merges == Some(true) {
                die!(
                    "apply options are incompatible with rebase.rebaseMerges.  \
                     Consider adding --no-rebase-merges"
                );
            } else if update_refs == -1 && cfg_update_refs == Some(true) {
                die!(
                    "apply options are incompatible with rebase.updateRefs.  \
                     Consider adding --no-update-refs"
                );
            }
            ty = Backend::Apply;
        }
    }

    if update_refs == 1 {
        try_imply!(ty, "--update-refs");
    }
    // ```c
    // options.update_refs = (options.update_refs >= 0) ? options.update_refs :
    //                      ((options.config_update_refs >= 0) ? options.config_update_refs : 0);
    // ```
    // (builtin/rebase.c:1589-1590) — `rebase.updateRefs` is the default the flag
    // overrides, and the apply-backend contradiction above already fired.
    if update_refs < 0 {
        update_refs = i32::from(
            repo.config_snapshot().boolean("rebase.updateRefs") == Some(true),
        );
    }
    if rebase_merges == 1 {
        try_imply!(ty, "--rebase-merges");
    }
    // `--autosquash` implies the merge backend; without it on the command line,
    // `rebase.autoSquash` supplies the default — but only under an explicit
    // `-i`, since a non-interactive rebase must not silently reorder commits.
    if autosquash == 1 {
        try_imply!(ty, "--autosquash");
    } else if autosquash == -1 {
        autosquash = i32::from(
            repo.config_snapshot().boolean("rebase.autoSquash") == Some(true)
                && flags & INTERACTIVE_EXPLICIT != 0,
        );
    }

    if ty == Backend::Unspecified {
        // `options.default_backend` starts as "merge" and is overridden by
        // `rebase.backend`.
        let configured = repo
            .config_snapshot()
            .string("rebase.backend")
            .map(|v| v.to_string());
        match configured.as_deref() {
            None | Some("merge") => ty = Backend::Merge,
            Some("apply") => ty = Backend::Apply,
            Some(other) => die!("Unknown rebase backend: {other}"),
        }
    }

    // ```c
    // if (options.empty == EMPTY_UNSPECIFIED) {
    //         if (options.flags & REBASE_INTERACTIVE_EXPLICIT) options.empty = EMPTY_STOP;
    //         else if (options.exec.nr > 0)                    options.empty = EMPTY_KEEP;
    //         else                                             options.empty = EMPTY_DROP;
    // }
    // ```
    // (builtin/rebase.c:1626-1633), right after the backend has been settled.
    let empty = empty.unwrap_or(if flags & INTERACTIVE_EXPLICIT != 0 {
        Empty::Stop
    } else if !exec.is_empty() {
        Empty::Keep
    } else {
        Empty::Drop
    });

    if reschedule_failed_exec > 0 && ty != Backend::Merge {
        die!("--reschedule-failed-exec requires --exec or --interactive");
    }

    // git resolves `<onto>` here. With `--root` and no `--onto`, `builtin/rebase.c`
    // mints a synthesized root commit — an empty-tree commit with no parents and
    // the configured author/committer — to stand in as `<onto>`, and writes it to
    // the object database at *this* point, before the `argc > 1` operand check
    // below. So an invalid `git rebase --root -- a b` still leaves that one loose
    // object behind (exit 129), byte-for-byte what stock git leaves. It happens
    // after the backend `die()`s — `git rebase --root --apply a b` reports
    // `--root without --onto requires the merge backend` (128) and mints nothing —
    // and is skipped entirely when `--onto` supplies the base (`git rebase --root
    // --onto HEAD -- a b` usage-errors with no object written).
    let squash_onto = if root && onto_name.is_none() {
        Some(write_synth_root(&repo)?)
    } else {
        None
    };

    // git resolves `<upstream>` here. With `--root` no upstream token is
    // consumed, so `builtin/rebase.c`'s `--root` arm ends with `if (argc > 1)
    // usage_with_options(...)`: at most a single `[<branch>]` positional is
    // allowed, and a second one is a usage error (129). Without `--root` the
    // first positional is the upstream and the `> 2` case was already rejected
    // above, so this only bites the `--root` path. It sits after every
    // `imply_merge()`/backend `die()` — `git rebase --root --apply a b` reports
    // `--root without --onto requires the merge backend` (128), not this — and
    // before the signoff refusal, matching `git rebase --root --signoff a b`,
    // which git answers with the usage block rather than touching `--signoff`.
    if root && positional.len() > 1 {
        usage!();
    }

    // `--signoff`/`--trailer` do not error here. git resolves `<upstream>`,
    // `<onto>` and the clean-work-tree state first, so a missing upstream
    // (`error_on_missing_default_upstream`, stdout, exit 1), an invalid upstream
    // or onto (`fatal:`, exit 128) or a dirty tree all take precedence over any
    // message-rewrite refusal — the refusal moves down to the exact-replay
    // decision, where it fires only if commits would actually be rewritten.

    // --- HEAD --------------------------------------------------------------
    let head = repo.head()?;
    if head.is_unborn() {
        crate::git_fatal!("cannot rebase an unborn branch");
    }
    let mut head_oid = head
        .id()
        .ok_or_else(|| anyhow!("HEAD does not point to a commit"))?
        .detach();
    let mut branch: Option<FullName> = head.referent_name().map(std::borrow::ToOwned::to_owned);
    drop(head);

    // --- <upstream> --------------------------------------------------------
    // `--root` consumes no `<upstream>` token: `builtin/rebase.c` leaves
    // `options.upstream` NULL and derives the replay range from `<onto>` instead,
    // so the whole resolution below — including the missing-default-upstream
    // report and `--fork-point` — is skipped, and the first positional is
    // `[<branch>]` rather than `<upstream>`.
    let upstream_spec = if root {
        String::new()
    } else {
        match positional.first() {
        Some(s) if s == "-" => "@{-1}".to_string(),
        Some(s) => s.clone(),
        None => {
            let tracking = branch.as_ref().and_then(|b| {
                repo.branch_remote_tracking_ref_name(b.as_ref(), gix::remote::Direction::Fetch)
            });
            // `set_merge()` (remote.c:1780-1790): when the branch's remote is the
            // repository itself (`branch.<name>.remote = .`, what
            // `git branch --set-upstream-to=<local-branch>` writes),
            // `remote_find_tracking()` finds nothing — there are no refspecs to
            // map through — and git falls back to DWIM-resolving
            // `branch.<name>.merge` as a ref name. `branch_get_upstream()` then
            // returns that full ref. gix's tracking-ref lookup only knows the
            // refspec path, so without this a `git rebase` with no operand on a
            // branch tracking another *local* branch reported
            // `There is no tracking information for the current branch.` and
            // exited 1 where stock rebases.
            let tracking = tracking.or_else(|| {
                let name = branch.as_ref()?.shorten().to_string();
                let snap = repo.config_snapshot();
                if snap.string(&format!("branch.{name}.remote"))?.to_string() != "." {
                    return None;
                }
                let merge = snap.string(&format!("branch.{name}.merge"))?.to_string();
                let full = repo
                    .rev_parse_single(merge.as_str())
                    .ok()
                    .and_then(|_| repo.find_reference(merge.as_str()).ok())
                    .map(|r| r.name().as_bstr().to_string())
                    .unwrap_or(merge);
                Some(Ok(full_name(&full).ok()?))
            });
            match tracking {
                Some(Ok(name)) => {
                    if fork_point < 0 {
                        fork_point = 1;
                    }
                    name.shorten().to_string()
                }
                Some(Err(e)) => crate::git_fatal!("{e}"),
                None => {
                    // `error_on_missing_default_upstream()`: stdout, exit 1.
                    match branch.as_ref() {
                        Some(b) => print!(
                            "There is no tracking information for the current branch.\n\
                             Please specify which branch you want to rebase against.\n\
                             See git-rebase(1) for details.\n\
                             \n    git rebase '<branch>'\n\n\
                             If you wish to set tracking information for this branch you can do so with:\n\
                             \n    git branch --set-upstream-to=<remote>/<branch> {}\n\n",
                            b.shorten()
                        ),
                        None => print!(
                            "You are not currently on a branch.\n\
                             Please specify which branch you want to rebase against.\n\
                             See git-rebase(1) for details.\n\
                             \n    git rebase '<branch>'\n\n"
                        ),
                    }
                    return Ok(ExitCode::from(1));
                }
            }
        }
        }
    };
    // `options.upstream_arg` — what the `pre-rebase` hook is handed. It is the
    // `<upstream>` token as the user spelled it, except under `--root`, where
    // `builtin/rebase.c:1684` substitutes the literal string `--root`.
    let upstream_arg = if root { "--root".to_string() } else { upstream_spec.clone() };
    let upstream_oid = if root {
        None
    } else {
        // `options.upstream = lookup_commit_reference_by_name(options.upstream_name)`
        // (`builtin/rebase.c:1663`), which opens with `repo_get_oid_committish()`
        // — one trip through `get_oid_basic()`, so one
        // `warning: refname … is ambiguous.` for the operand as typed.
        crate::objname::warn_ambiguous_refname(&repo, &upstream_spec);
        match peel_to_commit(&repo, &upstream_spec) {
            Some(oid) => Some(oid),
            None => die!("invalid upstream '{upstream_spec}'"),
        }
    };

    // --- <branch> ----------------------------------------------------------
    // `cmd_rebase()`: a `<branch>` that is not the current one is checked out
    // first (`options.switch_to`), silently — the rebase's own messages are the
    // only ones printed. Everything below then works off the new `HEAD`.
    let branch_arg = if root {
        positional.first()
    } else {
        positional.get(1)
    };
    // `options.switch_to`: the branch to check out once the tree is known clean.
    let mut switch_to: Option<String> = None;
    // The subset of `switch_to` that actually moves `HEAD`: a `<branch>` that is not
    // the current one has to be checked out before the replay can work off it.
    let mut eager_switch: Option<String> = None;
    let branch_name = match branch_arg {
        Some(requested) => {
            // ```c
            // strbuf_addf(&buf, "refs/heads/%s", branch_name);
            // if (!refs_read_ref(get_main_ref_store(the_repository), buf.buf, &branch_oid)) {
            //         die_if_checked_out(buf.buf, 1);
            //         options.head_name = xstrdup(buf.buf);
            //         options.orig_head = lookup_commit_object(the_repository, &branch_oid);
            // /* If not is it a valid ref (branch or commit)? */
            // } else {
            //         options.orig_head = lookup_commit_reference_by_name(branch_name);
            //         options.head_name = NULL;
            // }
            // ```
            //
            // The two arms resolve the operand by *different* means, and which one
            // runs decides both what is rebased and whether the operand warns:
            //
            // * `refs_read_ref()` is a ref-store read. It never reaches
            //   `get_oid_basic()`, so a `<branch>` whose name happens to be 40 hex
            //   digits does not warn — and `options.orig_head` is the **branch's
            //   own tip**, not the object those 40 characters decode to. Resolving
            //   this arm through a revspec parser instead picks up
            //   `get_oid_basic()`'s full-hex rule and rebases the wrong commit,
            //   which then fails the `refs/heads/<hex>` update with an old-value
            //   mismatch (the ref holds its tip, the update expects the decoded
            //   id).
            // * `lookup_commit_reference_by_name()` is the ordinary resolution and
            //   does warn, once.
            let branch_ref = repo.try_find_reference(&format!("refs/heads/{requested}")).ok().flatten();
            let is_branch = branch_ref.is_some();
            if !is_branch {
                crate::objname::warn_ambiguous_refname(&repo, requested);
            }
            // `lookup_commit_object()` for the ref arm — the tip as recorded, with
            // no tag peeling and no revspec grammar; `peel_to_commit()` for the
            // other, which is what `lookup_commit_reference_by_name()` does.
            let resolved = match branch_ref {
                Some(mut r) => r
                    .peel_to_id()
                    .ok()
                    .map(gix::Id::detach)
                    .and_then(|id| repo.find_object(id).ok())
                    .filter(|o| o.kind == gix::object::Kind::Commit)
                    .map(|o| o.id),
                None => peel_to_commit(&repo, requested),
            };
            if resolved.is_none() {
                die!("no such branch/commit '{requested}'");
            }
            let current = branch.as_ref().map(|b| b.shorten().to_string());
            // `options.switch_to = argv[0]` (builtin/rebase.c:1698) is set for any
            // `<branch>` argument, the current one included: when the rebase turns out
            // to be a no-op, `checkout_up_to_date()` still records the checkout.
            switch_to = Some(requested.clone());
            if !is_branch {
                // `cmd_rebase()`'s "if not, is it a valid ref (branch or commit)?"
                // arm, right after the `refs/heads/<arg>` lookup misses: an argument
                // that names no local branch but does resolve to a commit is *not*
                // refused — `options.orig_head` becomes that commit and
                // `options.head_name` stays NULL, so the rebase runs detached.
                // `git rebase <upstream> HEAD` is the everyday spelling, and an
                // up-to-date one still detaches `HEAD` before saying so.
                head_oid = resolved.ok_or_else(|| anyhow!("no such branch/commit '{requested}'"))?;
                branch = None;
            } else if current.as_deref() != Some(requested.as_str()) {
                // Everything below rebases `<branch>`, so its tip stands in for
                // `HEAD` right away. The worktree switch itself waits until after
                // `require_clean_work_tree()`, which git runs first — a dirty tree
                // must produce git's refusal, not the checkout's.
                head_oid = resolved.ok_or_else(|| anyhow!("no such branch/commit '{requested}'"))?;
                branch = Some(FullName::try_from(format!("refs/heads/{requested}"))?);
                eager_switch = Some(requested.clone());
            }
            requested.clone()
        }
        None => match branch.as_ref() {
            Some(b) => b.shorten().to_string(),
            None => "HEAD".to_string(),
        },
    };

    // --- <onto> ------------------------------------------------------------
    // `--root` without `--onto` stands the minted root commit in as `<onto>`, by
    // its full hex id — which is what lands in the `rebase (start): checkout <…>`
    // reflog entry, exactly as stock git records it.
    let onto_spec = if keep_base {
        format!("{upstream_spec}...{branch_name}")
    } else if let Some(oid) = squash_onto {
        oid.to_string()
    } else {
        onto_name.clone().unwrap_or_else(|| upstream_spec.clone())
    };
    let _onto_is_merge_base = onto_spec.contains("...");
    let onto_oid = if let Some(p) = onto_spec.find("...") {
        let left = if p == 0 { "HEAD" } else { &onto_spec[..p] };
        let right_raw = &onto_spec[p + 3..];
        let right = if right_raw.is_empty() {
            "HEAD"
        } else {
            right_raw
        };
        // `repo_get_oid_mb()` (`object-name.c:1308`) resolves the two sides in
        // order and gives up between them:
        //
        // ```c
        // st = repo_get_oid_committish(r, sb.buf, &oid_tmp);
        // if (st) return st;
        // one = lookup_commit_reference_gently(r, &oid_tmp, 0);
        // if (!one) return -1;
        // if (repo_get_oid_committish(r, dots[3] ? (dots + 3) : "HEAD", &oid_tmp)) return -1;
        // ```
        //
        // so a left side that does not resolve — or resolves to something that is
        // not commit-ish — means the right one is never looked at and never warns.
        crate::objname::warn_ambiguous_refname(&repo, left);
        let left_commit = peel_to_commit(&repo, left);
        if left_commit.is_some() {
            crate::objname::warn_ambiguous_refname(&repo, right);
        }
        let base = match (left_commit, peel_to_commit(&repo, right)) {
            (Some(l), Some(r)) => merge_base_unique(&repo, l, r)?,
            _ => None,
        };
        match base {
            Some(oid) => oid,
            None if keep_base => {
                die!("'{upstream_spec}': need exactly one merge base with branch")
            }
            None => die!("'{onto_spec}': need exactly one merge base"),
        }
    } else {
        // `options.onto = lookup_commit_reference_by_name(options.onto_name)`
        // (`builtin/rebase.c:1760`). Without `--onto`, `options.onto_name` *is*
        // `options.upstream_name` (`:1747`), so the same operand is resolved a
        // second time and `git rebase <40-hex-ref>` warns twice, not once.
        crate::objname::warn_ambiguous_refname(&repo, &onto_spec);
        match peel_to_commit(&repo, &onto_spec) {
            Some(oid) => oid,
            None => die!("Does not point to a valid commit '{onto_spec}'"),
        }
    };

    // `--keep-base` defaults `--reapply-cherry-picks` on, which git models by
    // moving the upstream to the onto so nothing looks already-applied.
    let upstream_oid = if keep_base && reapply_cherry_picks == 1 {
        Some(onto_oid)
    } else {
        upstream_oid
    };

    // `--fork-point` (git's default when the upstream is the branch's tracking
    // ref): refine the base to where the branch forked from the upstream, read
    // off the upstream's reflog, so commits a rewound upstream dropped are dropped
    // here too. When the reflog yields no unique fork point git falls back to the
    // plain merge-base, i.e. `fork_point_oid` stays `None` and the replay walk
    // below hides only the upstream — identical to `--no-fork-point`.
    let fork_point_oid = if fork_point > 0 {
        get_fork_point(&repo, &upstream_spec, head_oid)?
    } else {
        None
    };

    // --- require_clean_work_tree() -----------------------------------------
    // `refresh_index()` runs first and reports unmerged paths on stdout, even
    // under --quiet, before either error line.
    let (unstaged, staged, conflicts) = dirty_state(&repo)?;
    // `--autostash` over a dirty tree is honored: the actual stash is created
    // later, right before the finish moves HEAD (so every early refusal below
    // still leaves the worktree untouched), and re-applied on completion. A tree
    // with unmerged (conflicted) entries cannot be autostashed and still refuses.
    let autostash_wanted = autostash && (unstaged || staged) && conflicts.is_empty();
    for path in &conflicts {
        println!("{path}: needs merge");
    }
    if !autostash_wanted && (unstaged || staged) {
        if unstaged {
            eprintln!("error: cannot rebase: You have unstaged changes.");
            if staged {
                eprintln!("error: additionally, your index contains uncommitted changes.");
            }
        } else {
            eprintln!("error: cannot rebase: Your index contains uncommitted changes.");
        }
        eprintln!("error: Please commit or stash them.");
        return Ok(ExitCode::from(1));
    }

    // `cmd_rebase()`: with the tree known clean, check out the `<branch>` that is
    // about to be rebased. git does this silently — the rebase's own messages are
    // the only ones printed.
    if let Some(requested) = &eager_switch {
        super::checkout::switch_to_branch(
            &repo,
            requested,
            true,
            false,
            Some(&format!("{}: checkout {requested}", reflog_action())),
        )?;
    }

    // --- can_fast_forward() ------------------------------------------------
    // git calls `can_fast_forward()` with `options.upstream`, which `--root`
    // leaves NULL, so the preemptive fast-forward is never taken under `--root`:
    // `git rebase --root --onto <head>` finishes with `Successfully rebased and
    // updated <ref>.` rather than `Current branch <b> is up to date.`.
    let branch_base = merge_base_unique(&repo, onto_oid, head_oid)?;
    let can_ff = match upstream_oid {
        Some(up) => can_fast_forward(&repo, branch_base, onto_oid, up, head_oid)?,
        None => false,
    };
    if allow_preemptive_ff && can_ff {
        if flags & FORCE == 0 {
            // `checkout_up_to_date()` (builtin/rebase.c:855): the lazy switch to the
            // branch that needs no rebasing. Its `reset_head()` carries `ropts.branch`,
            // so the line lands in the branch's reflog as well as `HEAD`'s — even when
            // the branch is the one already checked out and nothing moves. A switch to
            // a *different* branch already happened above and is not repeated.
            if let Some(requested) = switch_to.as_ref().filter(|_| eager_switch.is_none()) {
                let message = format!("{}: checkout {requested}", reflog_action());
                if let Some(name) = &branch {
                    repo.edit_reference(RefEdit {
                        change: Change::Update {
                            log: LogChange {
                                mode: RefLog::AndReference,
                                force_create_reflog: false,
                                message: message.clone().into(),
                            },
                            expected: PreviousValue::Any,
                            new: Target::Object(head_oid),
                        },
                        name: name.clone(),
                        deref: false,
                    })?;
                    // `update_refs()` (reset.c) does two ref updates when it has a
                    // branch: `refs_update_ref(<branch>)` — which `split_head_update()`
                    // mirrors into `logs/HEAD`, since HEAD points at that branch, and
                    // which the `edit_reference` above already produced — and then
                    // `refs_update_symref("HEAD", <branch>)`, appended here. Both carry
                    // the same message, so an unmoved branch still gains two identical
                    // `logs/HEAD` lines. Verified against stock 2.55.0.
                    super::checkout::append_head_log(
                        &repo,
                        Some(head_oid),
                        Some(head_oid),
                        &message,
                    );
                } else {
                    // Detached (`<branch>` named a commit-ish rather than a local
                    // branch, so `options.head_name` is NULL and `reset_head()` gets
                    // `RESET_HEAD_DETACH`): `update_refs()` takes the single
                    // `refs_update_ref("HEAD", …, REF_NO_DEREF)` branch instead, which
                    // repoints `HEAD` itself at the commit. `reset_head()`'s two-way
                    // merge carries the worktree and index along with it, so a
                    // commit-ish that is not where `HEAD` already stands is a real
                    // checkout, not just a reflog line.
                    let at = repo.head_id().ok().map(|id| id.detach());
                    if at == Some(head_oid) {
                        set_head(&repo, Target::Object(head_oid), &message)?;
                    } else {
                        let old_index = repo.index_or_load_from_head()?.into_owned();
                        set_head(&repo, Target::Object(head_oid), &message)?;
                        update_clean_worktree(
                            &repo,
                            &old_index,
                            head_oid,
                            &AtomicBool::new(false),
                        )?;
                    }
                }
            }
            if flags & NO_QUIET != 0 {
                if branch_name == "HEAD" {
                    println!("HEAD is up to date.");
                } else {
                    println!("Current branch {branch_name} is up to date.");
                }
            }
            // git reaches `finish_rebase()` here too, so the automatic
            // maintenance run fires on the nothing-to-do path as well.
            super::maintenance::run_auto_maintenance(&repo, flags & NO_QUIET == 0)?;
            return Ok(ExitCode::SUCCESS);
        } else if flags & NO_QUIET != 0 {
            if branch_name == "HEAD" {
                println!("HEAD is up to date, rebase forced.");
            } else {
                println!("Current branch {branch_name} is up to date, rebase forced.");
            }
        }
    }

    // `pre-rebase` receives `<upstream> [<branch>]`; a non-zero exit aborts.
    // ```c
    // if (!ok_to_skip_pre_rebase &&
    //     run_hooks_l(the_repository, "pre-rebase", options.upstream_arg,
    //                 argc ? argv[0] : NULL, NULL)) {
    //         ret = error(_("The pre-rebase hook refused to rebase."));
    //         goto cleanup_autostash;
    // }
    // ```
    // (builtin/rebase.c:1832-1838). Two things this got wrong, both visible to
    // any hook that reads `$#`:
    //
    // * The first argument is `options.upstream_arg` — the `<upstream>` **as
    //   spelled**, or the literal `--root` (builtin/rebase.c:1667, 1684) — not
    //   `<onto>`. With `--onto` the two differ.
    // * The second is `argv[0]`, i.e. the optional `[<branch>]` operand, and
    //   `run_hooks_l()` is NULL-terminated: with no operand the hook is called
    //   with **one** argument, not with the current branch as a second. A hook
    //   written against `git rebase <upstream>` saw `$#` of 2 and `$2` naming a
    //   branch git never passes.
    //
    // The refusal also has a diagnostic — `error()` on stderr — and exit 1.
    let mut hook_args: Vec<&str> = vec![upstream_arg.as_str()];
    if let Some(b) = branch_arg {
        hook_args.push(b.as_str());
    }
    if !ok_to_skip_pre_rebase && !crate::hooks::run(&repo, "pre-rebase", &hook_args, None)? {
        eprintln!("error: The pre-rebase hook refused to rebase.");
        return Ok(ExitCode::from(1));
    }

    // `--stat` (and `rebase.stat=true`): the diffstat of what changed upstream,
    // `diff_tree_oid(merge_base, onto)` with DIFF_FORMAT_DIFFSTAT|DIFF_FORMAT_SUMMARY
    // on stdout. A null merge base (unrelated histories) diffs against the empty
    // tree. Rendering is delegated to the `diff` porcelain, which drives the same
    // machinery; the one deviation is git's `detect_rename`, which this port has
    // nowhere, so a rename renders as a delete plus an add.
    //
    // `-v` additionally sets REBASE_VERBOSE, which prefixes the range this diffstat
    // covers — `Changes from <merge base> to <onto>:`, or `Changes to <onto>:` when
    // there is no merge base (`is_null_oid(&branch_base)`, which is every `--root`
    // rebase). It also makes the sequencer emit a second, post-replay diffstat once
    // the branch is back in place; see [`verbose_replay_diffstat`].
    if flags & DIFFSTAT != 0 {
        if flags & VERBOSE != 0 {
            match branch_base {
                Some(base) => println!("Changes from {base} to {onto_oid}:"),
                None => println!("Changes to {onto_oid}:"),
            }
        }
        let base = branch_base.unwrap_or_else(|| ObjectId::empty_tree(repo.object_hash()));
        super::diff::diff(&[
            "--stat".to_string(),
            "--summary".to_string(),
            base.to_string(),
            onto_oid.to_string(),
        ])?;
    }

    // --- decide whether anything would be replayed -------------------------
    // The merge backend picks the right side of `<upstream>...<head>`; the apply
    // backend picks `<upstream>..<head>`. Both are empty exactly when
    // `<upstream>..<head>` is, which is what is measured here.
    // Hide the upstream, plus the fork point when `--fork-point` refined it: any
    // commit reachable from the fork point (a stale tip a rewound upstream
    // dropped) is excluded from the replay. In the common case the fork point is
    // the plain merge-base — an ancestor of the upstream — so hiding it changes
    // nothing.
    // Under `--root` there is no upstream and git's range is `<onto>..<orig_head>`
    // instead, which is why `git rebase --root --onto <base>` replays only what
    // `<base>` does not already contain.
    let mut hidden: Vec<ObjectId> = vec![upstream_oid.unwrap_or(onto_oid)];
    if let Some(fp) = fork_point_oid {
        hidden.push(fp);
    }
    let mut replay_range: Vec<ObjectId> = Vec::new();
    let mut replay_parents: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for info in repo.rev_walk([head_oid]).with_hidden(hidden).all()? {
        let info = info?;
        replay_parents.insert(info.id, info.parent_ids.to_vec());
        replay_range.push(info.id);
    }
    // `sequencer_make_script()` sets `revs.topo_order = 1` with
    // `revs.sort_order = REV_SORT_IN_GRAPH_ORDER`, so the walk's output is
    // re-sorted topologically through a LIFO stack before anything is dropped
    // from it. That sort is what keeps a side branch contiguous, and it decides
    // which of two commits with no ancestry between them is replayed first:
    // `sort_in_topological_order()` drains a merge's parents last-pushed-first,
    // so the *second* parent's side comes out ahead of the first parent's and
    // the reversal below puts the first-parent side back in front.
    //
    // The whole list is sorted, merges included; `revs.max_parents = 1` drops
    // them at output time (below), after the sort has already used them to
    // order what stays.
    let replay_range = super::rev_list::topo_sort(&replay_range, &replay_parents, None);
    // `sequencer_make_script()` walks with `revs.max_parents = 1` unless
    // `--rebase-merges` asked for the merge structure to be recreated: a merge
    // commit has no single patch to replay, and `pick` refuses one outright. git
    // therefore leaves merges out of the instruction sheet, which is what makes
    // `git rebase --onto <base> <upstream>` work on a branch that contains one.
    let todo_range: Vec<ObjectId> = if rebase_merges == 1 {
        // `make_script_with_merges()` keeps the merges and writes
        // `label`/`reset`/`merge` instructions to rebuild the topology.
        replay_range.clone()
    } else {
        replay_range
            .iter()
            .copied()
            .filter(|id| {
                repo.find_commit(*id)
                    .map(|c| c.parent_ids().count() <= 1)
                    .unwrap_or(true)
            })
            .collect()
    };

    // `revs.cherry_mark = !reapply_cherry_picks` (sequencer.c:6172), resolved by
    // `cherry_pick_list()` (revision.c) over the symmetric range the sheet is
    // built from: patch ids are taken for the commits on *each* side of
    // `<upstream>...<orig_head>`, and a right-side commit whose id also appears
    // on the left is flagged `PATCHSAME`. `sequencer_make_script()` then drops it
    // with `warning: skipped previously applied commit <abbrev>`.
    //
    // Both sides must be non-empty — `cherry_pick_list()` returns early
    // otherwise — so a branch that is purely ahead of its upstream marks nothing.
    // Merges have no patch id (`commit_patch_id()` refuses them) and are never
    // marked. The left side is hidden behind the same `^<fork point>` the replay
    // range uses, so `--fork-point` narrows what counts as "already upstream" too.
    //
    // This is what keeps `git rebase -i <upstream>` (whose `--empty` default is
    // `stop`) from halting on a commit the upstream already carries: git removes
    // it from the sheet here, long before `allow_empty()` could see an unchanged
    // index.
    let patchsame: HashSet<ObjectId> = if reapply_cherry_picks == 1 || root || todo_range.is_empty()
    {
        HashSet::new()
    } else {
        let mut left_hidden: Vec<ObjectId> = vec![head_oid];
        if let Some(fp) = fork_point_oid {
            left_hidden.push(fp);
        }
        let base = upstream_oid.unwrap_or(onto_oid);
        let mut left_ids: HashSet<ObjectId> = HashSet::new();
        for info in repo.rev_walk([base]).with_hidden(left_hidden).all()? {
            let id = info?.id;
            if let Some(pid) = super::cherry::commit_patch_id(&repo, id)? {
                left_ids.insert(pid);
            }
        }
        if left_ids.is_empty() {
            HashSet::new()
        } else {
            let mut out = HashSet::new();
            for &id in &todo_range {
                if let Some(pid) = super::cherry::commit_patch_id(&repo, id)? {
                    if left_ids.contains(&pid) {
                        out.insert(id);
                    }
                }
            }
            out
        }
    };


    let apply_backend = ty == Backend::Apply;

    // `git_rebase_config()` seeds `options.gpg_sign_opt` from `commit.gpgSign`,
    // which `-S`/`--no-gpg-sign` then overrides. Signing a replayed commit is
    // not ported, so a rebase that asks for it and would actually create a
    // commit is refused rather than silently producing unsigned commits; a
    // replay that creates nothing is unaffected, exactly as under git.
    let gpg_sign_on = if gpg_sign >= 0 {
        gpg_sign == 1
    } else {
        repo.config_snapshot().boolean("commit.gpgSign") == Some(true)
    };
    let refuse_gpg_sign = || -> anyhow::Error {
        anyhow!(
            "unsupported flag \"--gpg-sign\" (signing replayed commits is not ported; \
             pass --no-gpg-sign, or unset commit.gpgSign, to rebase without signatures)"
        )
    };

    // `can_fast_forward()` holding over a *non-empty* range is the one shape in
    // which a replay is exactly a re-commit rather than a merge: `<onto>` is the
    // merge base of `<onto>`/`<head>` *and* of `<upstream>`/`<head>`, and
    // `<onto>..<head>` is linear. Every picked commit therefore lands on the very
    // parent it already had, so its patch applies to a byte-identical tree and
    // reproduces that commit's tree verbatim. Both blockers named in the module
    // header are vacuous here — there is nothing to three-way merge, and nothing
    // in the range can already be in `<upstream>`, so patch-id equivalence has no
    // work to do. What git rewrites is the commit *metadata*: always the
    // committer, plus the author date under `--ignore-date`. That is why it does
    // not simply leave the branch alone.
    //
    // Reaching this with `can_ff` set implies `REBASE_FORCE` is set: without it
    // the up-to-date exit above already returned.
    let exact_replay = allow_preemptive_ff && can_ff && !replay_range.is_empty();

    // Resolve every step before anything is written, so a refusal below still
    // leaves the repository untouched.
    //
    // `is_linear_history()` stops at a root as well as at `<onto>`, so the walk
    // below re-establishes what the replay actually needs — that `<onto>` really
    // is reachable by first parents — and yields `None` if it is not, falling
    // through to the refusals rather than replaying a range it did not verify.
    let plan = if exact_replay {
        first_parent_plan(&repo, head_oid, onto_oid)?
    } else {
        None
    };
    // The exact-replay shortcut is a *merge-backend* shape only. `cmd_rebase()`
    // sends every apply-backend rebase that is not a plain fast-forward through
    // `run_am()` (builtin/rebase.c:1892-1911), and that path is now ported, so
    // short-cutting it here would diverge on everything `git am` does along the
    // way — the `Applying:` lines, the session directory, and `Patch is empty.`
    // on a commit that changes no tree.
    let exact_replay = plan.is_some() && !apply_backend;
    let plan = if exact_replay { plan.unwrap_or_default() } else { Vec::new() };

    if exact_replay {
        // `--signoff`/`--trailer` rewrite the *message* of every picked commit
        // (a Signed-off-by / custom trailer). Both are applied by the replay loop
        // below, in `do_pick_commit()`'s order (sequencer.c:2433-2437): sign-off
        // first, then the command-line trailers.
        if gpg_sign_on {
            return Err(refuse_gpg_sign());
        }
    } else if apply_backend {
        // The apply backend detaches first and only then notices it merely
        // fast-forwarded. Deciding here keeps a refused rebase from mutating
        // anything.
        // Signing a replayed commit is unported everywhere in this file, and
        // `run_am()` would otherwise hand `git am` a `-S` it refuses mid-replay,
        // after HEAD had already moved.
        if gpg_sign_on && branch_base != Some(head_oid) {
            return Err(refuse_gpg_sign());
        }
    }
    // Genuine picks (a real three-way merge per commit) are replayed in the finish
    // below via [`crate::merge_apply`]; no refusal here anymore.

    // --- the finish --------------------------------------------------------
    // Serialize the whole read-modify-write through the repo coordinator (a
    // no-op when no daemon is running), matching the merge/zsync write path.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // `--autostash`: now that the rebase is committed to moving HEAD (every early
    // refusal is behind us), snapshot the dirty tree and reset it clean. The
    // stash `W` commit is re-applied with a three-way merge onto the rebased tip
    // at each completion point below (and, on a conflict-stop, by
    // `--continue`/`--abort` via the persisted `autostash` state file).
    let autostash_oid = if autostash_wanted {
        let oid = crate::porcelain::stash::create_autostash(&repo)?;
        if flags & NO_QUIET != 0 {
            println!("Created autostash: {}", oid.to_hex_with_len(7));
        }
        Some(oid)
    } else {
        None
    };

    // Capture the current (clean) index BEFORE any ref moves: it mirrors the old
    // tree and carries the filesystem stats reused for unchanged files. Taken
    // first because `index_or_load_from_head` would otherwise fall back to the
    // *new* HEAD if a repository happened to have no index file on disk.
    let old_index = repo.index_or_load_from_head()?.into_owned();

    // --- the merge backend: the instruction sheet --------------------------
    //
    // Every merge-backend rebase runs through the sequencer, not only `-i`:
    // `run_specific_rebase()` forces `GIT_SEQUENCE_EDITOR=:` when `-i` was not
    // given rather than taking a different path, so the todo list is built,
    // (not) edited and executed either way. Only the exact-replay shape above
    // and the apply backend bypass it.
    if !apply_backend && !exact_replay && !replay_range.is_empty() {
        if gpg_sign_on {
            return Err(refuse_gpg_sign());
        }
        let head_name = match &branch {
            Some(b) => b.as_bstr().to_string(),
            None => "detached HEAD".to_string(),
        };
        // `rebase.rescheduleFailedExec` is the default the command line
        // overrides; the command line already `die()`d if it needed the merge
        // backend and did not have it.
        let reschedule = if reschedule_failed_exec >= 0 {
            reschedule_failed_exec == 1
        } else {
            repo.config_snapshot().boolean("rebase.rescheduleFailedExec") == Some(true)
        };
        return sequencer_rebase(SequencerStart {
            repo: &repo,
            // `revs.reverse = 1`: the instruction sheet is oldest first, while
            // the walk above yields newest first.
            range: todo_range.iter().rev().copied().collect(),
            state: RebaseState {
                head_name,
                onto: onto_oid,
                orig_head: head_oid,
                squash_onto,
                allow_ff: flags & FORCE == 0,
                quiet: flags & NO_QUIET == 0,
                verbose: flags & VERBOSE != 0,
                reschedule_failed_exec: reschedule,
                signoff,
                trailers: trailers.clone(),
                rerere_autoupdate,
                committer_date_is_author_date,
                ignore_date,
                empty,
                strategy: strategy.clone(),
                strategy_opts: strategy_opts.clone(),
            },
            onto_spec: &onto_spec,
            upstream: upstream_oid,
            exec: &exec,
            autosquash: autosquash == 1,
            keep_empty,
            interactive: flags & INTERACTIVE_EXPLICIT != 0,
            autostash: autostash_oid,
            rebase_merges: rebase_merges == 1,
            old_index: &old_index,
            cherry: todo::CherryMark::new(patchsame, flags & NO_QUIET != 0),
            update_refs: update_refs == 1,
        });
    }

    if apply_backend && flags & NO_QUIET != 0 {
        println!("First, rewinding head to replay your work on top of it...");
    }

    // git writes ORIG_HEAD only once it commits to actually rebasing. It is a
    // pseudo-ref, so no reflog is created for it (gix applies git's own
    // `should_autocreate_reflog` rule).
    write_orig_head(&repo, head_oid)?;

    // The sequencer detaches HEAD at <onto> first, ...
    set_head(
        &repo,
        Target::Object(onto_oid),
        &format!("{} (start): checkout {onto_spec}", reflog_action()),
    )?;

    // ... moves the worktree and index onto the new tree, ...
    //
    // ... except during an exact replay, which ends on `<head>`'s own tree — the
    // tree the worktree and index already hold. The round trip down to `<onto>`
    // and back would rewrite every differing file twice to land exactly where it
    // started, so it is skipped rather than performed and undone.
    let should_interrupt = AtomicBool::new(false);
    if !exact_replay {
        update_clean_worktree(&repo, &old_index, onto_oid, &should_interrupt)?;
    }

    // ... replays each commit onto the growing tip, ...
    let mut tip = onto_oid;
    // `record_in_rewritten()`'s payload for the exact-replay shape. The sequencer
    // accumulates the same `<old> <new>` pairs in `$state_dir/rewritten-list`;
    // this path has no state directory to keep them in (it never stops), so the
    // list is built in memory and handed to the same hook at the end.
    let mut rewritten_pairs: Vec<u8> = Vec::new();
    if exact_replay {
        // One `now` for the whole run: git caches `ident_default_date()` per
        // process, so every commit `--ignore-date` restamps gets one value.
        let now = gix::date::Time::now_local_or_utc();
        let committer = repo
            .committer()
            .ok_or_else(|| anyhow!("committer identity is not configured"))??
            .to_owned()?;
        let total = plan.len();
        for (n, step) in plan.iter().enumerate() {
            if flags & NO_QUIET != 0 {
                if apply_backend {
                    // `git am` announces each patch by the first line of its
                    // message — not the folded summary the reflog gets.
                    let subject = match step.message.find_byte(b'\n') {
                        Some(p) => &step.message[..p],
                        None => &step.message[..],
                    };
                    println!("Applying: {}", subject.as_bstr());
                } else {
                    eprint!(
                        "Rebasing ({}/{total}){}",
                        n + 1,
                        if flags & VERBOSE != 0 { "\n" } else { "\r" }
                    );
                }
            }

            // `--ignore-date`/`--reset-author-date` drops the recorded author
            // date for the current time; `--committer-date-is-author-date` then
            // copies whichever author date survived onto the committer.
            let mut author = step.author.clone();
            if ignore_date {
                author.time = now;
            }
            let mut committer = committer.clone();
            if committer_date_is_author_date {
                committer.time = author.time;
            }

            // `do_pick_commit()` (sequencer.c:2433-2437):
            //
            // ```c
            // if (opts->signoff && !is_fixup(command))
            //         append_signoff(&ctx->message, 0, 0);
            // if (opts->trailer_args.nr && !is_fixup(command))
            //         amend_strbuf_with_trailers(&ctx->message, &opts->trailer_args);
            // ```
            //
            // An exact replay only ever picks, so neither `is_fixup` guard fires.
            let message = apply_trailers(sign_off(&committer, signoff, &step.message)?, &trailers)?;

            let new = repo
                .write_object(&gix::objs::Commit {
                    message,
                    tree: step.tree,
                    author,
                    committer,
                    encoding: None,
                    parents: std::iter::once(tip).collect(),
                    extra_headers: Default::default(),
                })?
                .detach();
            set_head(
                &repo,
                Target::Object(new),
                &gix::reference::log::message(&format!("{} (pick)", reflog_action()), step.message.as_bstr(), 1)
                    .to_string(),
            )?;
            // `try_to_commit()`'s `post-commit` (sequencer.c:1697). `git am`,
            // which is what the apply backend replays through, does not run it
            // in rebasing mode — measured against stock, whose `rebase --apply`
            // fires none.
            if !apply_backend {
                run_commit_hooks(&repo, None, new);
            }
            rewritten_pairs.extend_from_slice(format!("{} {new}\n", step.oid).as_bytes());
            tip = new;
        }
        // `do_pick_commit()` runs every pick through merge-ort, and
        // `merge_switch_to_result()` records each result as `AUTO_MERGE`
        // (merge-ort.c:4950); the last pick's is the one that survives the run,
        // since nothing on the success path removes it. An exact replay reuses
        // each commit's tree verbatim instead of merging for it — the merge base
        // is the pick's own parent, so the answer is known — but the artifact is
        // the same tree, so it is recorded here rather than skipped along with
        // the merge. The apply backend is excluded: `git am` applies patches and
        // only falls back to a three-way merge per patch, so a clean replay
        // through it writes none.
        // Gated on `REBASE_FORCE` for the same reason the sequencer path is
        // gated on `!fast_forward`: with `allow_ff` still on, `do_pick_commit()`
        // takes its fast-forward arm and never calls `do_recursive_merge()`, so
        // no merge-ort runs and no `AUTO_MERGE` is written. `-f`/`--no-ff` clear
        // `allow_ff`, and so do `--signoff`, `--committer-date-is-author-date`,
        // `--ignore-date` and `--reset-author-date`, which is exactly the set of
        // rebases stock leaves an `AUTO_MERGE` behind for.
        if !apply_backend && flags & FORCE != 0 {
            if let Some(last) = plan.last() {
                crate::merge_apply::write_auto_merge(&repo, last.tree)?;
            }
        }
        // The apply backend runs the hook from inside `git am`
        // (builtin/am.c:1930-1934), which is over before `run_am()` calls
        // `move_to_original_branch()` — so the hook sees a still-detached `HEAD`
        // and the branch still on its pre-rebase tip. The merge backend runs it
        // from `pick_commits()`'s tail, after the branch has been re-pointed.
        // Both were measured; a hook that reads refs can tell them apart, so the
        // two calls are placed separately rather than merged.
        if apply_backend {
            rewritten::post_rewrite(&repo, &rewritten_pairs);
        }
    } else if apply_backend && branch_base == Some(head_oid) {
        // `if (oideq(&branch_base, &options.orig_head->object.oid))`
        // (builtin/rebase.c:1892): the detach above already moved everything, so
        // there is nothing to replay. Printed on stdout, ungated by `--quiet`.
        println!("Fast-forwarded {branch_name} to {onto_spec}.");
    } else if apply_backend {
        // `run_specific_rebase()` -> `run_am()` (builtin/rebase.c:620).
        let head_name = branch.as_ref().map(|b| b.as_bstr().to_string());
        // `options.revisions`: `<restrict|upstream|onto>..<orig_head>`, quoted in
        // the "cannot rebase them" diagnostic and nowhere else.
        let revisions_label = format!(
            "{}..{}",
            if root {
                onto_oid
            } else {
                fork_point_oid.or(upstream_oid).unwrap_or(onto_oid)
            },
            head_oid
        );
        // `if (options.signoff) { strvec_push(&options.git_am_opts, "--signoff"); … }`
        // (builtin/rebase.c:1640-1643). It is pushed *after* the "am options
        // require --apply" check, so `--signoff` alone never implies the apply
        // backend; adding it here keeps that ordering. The merge backend applies
        // its sign-off through `sign_off()` instead and ignores `git_am_opts`.
        let mut am_opts = git_am_opts.clone();
        if signoff {
            am_opts.push("--signoff".to_string());
        }
        let am = AmRun {
            repo: &repo,
            onto: onto_oid,
            upstream: upstream_oid,
            orig_head: head_oid,
            restrict_revision: fork_point_oid,
            root,
            revisions: &revisions_label,
            am_opts: &am_opts,
            rerere_autoupdate,
            quiet: flags & NO_QUIET == 0,
            head_name: head_name.clone(),
        };
        match run_am(&am)? {
            AmOutcome::Done(new_tip) => tip = new_tip,
            AmOutcome::Stopped(code) => {
                // `run_specific_rebase()`'s tail: a stopped `am` leaves its
                // session directory, and `rebase_write_basic_state()` adds the
                // `head-name`/`onto`/`orig-head` files `git rebase --continue`
                // reads back on top of it.
                rebase_write_basic_state_apply(
                    &rebase_apply_dir(&repo),
                    head_name.as_deref(),
                    onto_oid,
                    head_oid,
                    flags & NO_QUIET == 0,
                    flags & VERBOSE != 0,
                    rerere_autoupdate,
                )?;
                // `cmd_rebase()` ends in `return !!ret` (builtin/rebase.c:1920),
                // so every failure the backend reports collapses to exit 1 —
                // `git am`'s own 128 for `Patch is empty.` or a conflict included.
                let _ = code;
                return Ok(ExitCode::from(1));
            }
        }
    } else if flags & FORCE != 0 && flags & NO_QUIET != 0 {
        // `complete_action()` appends a `noop` item to an empty todo list, and
        // with `allow_ff` off `skip_unnecessary_picks()` cannot drop it, so
        // `pick_commits()` reports exactly one step.
        eprint!(
            "Rebasing (1/1){}",
            if flags & VERBOSE != 0 { "\n" } else { "\r" }
        );
    }

    // ... then re-points the branch and re-attaches HEAD to it. On a detached
    // HEAD there is no branch, and git writes no `rebase (finish)` entry.
    let label = match &branch {
        Some(b) => {
            let name = b.as_bstr().to_string();
            repo.edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!("{} (finish): {name} onto {onto_oid}", reflog_action()).into(),
                    },
                    expected: PreviousValue::MustExistAndMatch(Target::Object(head_oid)),
                    new: Target::Object(tip),
                },
                name: b.clone(),
                deref: false,
            })?;
            let message = format!("{} (finish): returning to {name}", reflog_action());
            set_head(&repo, Target::Symbolic(b.clone()), &message)?;
            // The vendored `gix-ref` writes no reflog line for a symbolic-target
            // update, so `HEAD`'s own log would lose the entry git ends every
            // rebase with — the same compensation `checkout` makes.
            super::checkout::record_head_move(&repo, Some(tip), Some(tip), &message);
            name
        }
        None => "detached HEAD".to_string(),
    };

    if flags & VERBOSE != 0 {
        verbose_replay_diffstat(head_oid, tip)?;
    }

    // `pick_commits()`'s tail order (sequencer.c:5190-5210): the `post-rewrite`
    // hook runs after the branch has been re-pointed and `HEAD` re-attached, and
    // before the autostash is restored and the summary line printed.
    if !apply_backend {
        rewritten::post_rewrite(&repo, &rewritten_pairs);
    }

    // The single-shot rebase completed; re-apply the autostash onto the new tip
    // (before the summary line, matching git's finish_rebase ordering).
    if let Some(oid) = autostash_oid {
        crate::porcelain::stash::apply_autostash(&repo, oid, flags & NO_QUIET == 0)?;
    }
    super::maintenance::run_auto_maintenance(&repo, flags & NO_QUIET == 0)?;
    // The apply backend's fast-forward finishes silently; only the sequencer
    // announces itself.
    if !apply_backend && flags & NO_QUIET != 0 {
        eprintln!("Successfully rebased and updated {label}.");
    }
    Ok(ExitCode::SUCCESS)
}

/// Mint the synthesized root commit git's `--root` (without `--onto`) creates as
/// its stand-in `<onto>`: an empty-tree commit with no parents carrying the
/// configured author and committer. `builtin/rebase.c` writes this to the object
/// database while resolving `<onto>`, before it validates the operand count, so
/// reproducing git's ordering means writing it here — even on the invocations git
/// goes on to reject with the usage block. Only the loose object is written; no
/// ref, reflog, `ORIG_HEAD`, or index entry is touched, matching git.
///
/// The returned id is git's `options.squash_onto`: it stands in as `<onto>` (and
/// so names the `rebase (start): checkout <oid>` reflog entry), and a pick made
/// while the tip is still this commit becomes a new root commit rather than its
/// child.
fn write_synth_root(repo: &gix::Repository) -> Result<ObjectId> {
    let author = repo
        .author()
        .ok_or_else(|| anyhow!("author identity is not configured"))??
        .to_owned()?;
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow!("committer identity is not configured"))??
        .to_owned()?;
    Ok(repo
        .write_object(&gix::objs::Commit {
            message: BString::default(),
            tree: ObjectId::empty_tree(repo.object_hash()),
            author,
            committer,
            encoding: None,
            parents: Default::default(),
            extra_headers: Default::default(),
        })?
        .detach())
}

/// Resolve `onto..head` into replay steps by walking first parents from `head`
/// down to `onto`, oldest first — the order both backends replay in.
///
/// `None` means `onto` was not reached: a root or a merge came first, so the
/// range is not the plain re-commit the caller is about to perform.
fn first_parent_plan(
    repo: &gix::Repository,
    head: ObjectId,
    onto: ObjectId,
) -> Result<Option<Vec<Replay>>> {
    let mut plan = Vec::new();
    let mut cur = head;
    while cur != onto {
        let commit = repo.find_commit(cur)?;
        let mut parents = commit.parent_ids();
        let (Some(parent), None) = (parents.next(), parents.next()) else {
            return Ok(None);
        };
        let parent = parent.detach();
        plan.push(Replay {
            oid: cur,
            tree: commit.tree_id()?.detach(),
            parent_tree: repo.find_commit(parent)?.tree_id()?.detach(),
            message: commit.message_raw()?.to_owned(),
            author: commit.author()?.to_owned()?,
        });
        cur = parent;
    }
    plan.reverse();
    Ok(Some(plan))
}

/// git's `can_fast_forward()`: `<head>` already sits on top of `<onto>` and
/// nothing between `<upstream>` and `<head>` would be replayed. Multiple
/// merge-bases on either side make git give up on the shortcut, so they do here
/// too, and the history from `<onto>` up to `<head>` must be linear.
fn can_fast_forward(
    repo: &gix::Repository,
    branch_base: Option<ObjectId>,
    onto: ObjectId,
    upstream: ObjectId,
    head: ObjectId,
) -> Result<bool> {
    if branch_base != Some(onto) {
        return Ok(false);
    }
    if merge_base_unique(repo, upstream, head)? != Some(onto) {
        return Ok(false);
    }
    is_linear_history(repo, onto, head)
}

/// git's `is_linear_history()`: walk first-and-only parents from `to` down to
/// `from`; any merge on the way means the range is not a plain fast-forward.
fn is_linear_history(repo: &gix::Repository, from: ObjectId, to: ObjectId) -> Result<bool> {
    let mut cur = to;
    while cur != from {
        let parents: Vec<ObjectId> = repo.find_commit(cur)?.parent_ids().map(|p| p.detach()).collect();
        match parents.len() {
            0 => return Ok(true),
            1 => cur = parents[0],
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// The single merge base of `a` and `b`, or `None` when there is none or more
/// than one — the case git models with a null `branch_base`.
fn merge_base_unique(
    repo: &gix::Repository,
    a: ObjectId,
    b: ObjectId,
) -> Result<Option<ObjectId>> {
    let bases = repo.merge_bases_many(a, &[b])?;
    Ok(if bases.len() == 1 {
        Some(bases[0].detach())
    } else {
        None
    })
}

/// Record `oid` as a fork-point candidate if it is a real, not-yet-seen commit.
fn collect_fork_rev(
    repo: &gix::Repository,
    oid: ObjectId,
    revs: &mut Vec<ObjectId>,
    seen: &mut HashSet<ObjectId>,
) {
    if !oid.is_null() && seen.insert(oid) && repo.find_commit(oid).is_ok() {
        revs.push(oid);
    }
}

/// Port of git's `get_fork_point()`: where `head` forked from the `upstream`
/// ref, refined via that ref's reflog so a rewound upstream's dropped commits
/// are excluded from the rebase. It is the unique merge-base of `head` with the
/// set of the upstream's historical reflog tips (every entry's new oid plus the
/// oldest entry's old oid), and that base must itself be one of those tips —
/// otherwise `None`, and the caller falls back to the plain merge-base, exactly
/// as git does. A ref without a reflog falls back to its current tip.
fn get_fork_point(
    repo: &gix::Repository,
    upstream_spec: &str,
    head: ObjectId,
) -> Result<Option<ObjectId>> {
    let Some(reference) = repo.try_find_reference(upstream_spec)? else {
        return Ok(None);
    };

    let mut revs: Vec<ObjectId> = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    // Walk the reflog newest-first (owned lines); collect every `new` oid, and
    // remember the last-seen (== oldest) entry's `old` oid to add afterwards.
    let mut oldest_prev: Option<ObjectId> = None;
    {
        let mut platform = reference.log_iter();
        if let Some(iter) = platform.rev()? {
            for line in iter {
                let line = line?;
                collect_fork_rev(repo, line.new_oid, &mut revs, &mut seen);
                oldest_prev = Some(line.previous_oid);
            }
        }
    }
    if let Some(prev) = oldest_prev {
        collect_fork_rev(repo, prev, &mut revs, &mut seen);
    }
    if revs.is_empty() {
        // No reflog: git falls back to the ref's current tip.
        collect_fork_rev(repo, reference.id().detach(), &mut revs, &mut seen);
    }
    if revs.is_empty() {
        return Ok(None);
    }

    let bases = repo.merge_bases_many(head, &revs)?;
    if bases.len() != 1 {
        return Ok(None);
    }
    let base = bases[0].detach();
    // git requires the fork point to be one of the reflog entries.
    if !revs.contains(&base) {
        return Ok(None);
    }
    Ok(Some(base))
}

/// Resolve `spec` and peel it to a commit id, or `None` when either step fails —
/// git reports both as one "invalid" message rather than surfacing the cause.
fn peel_to_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let id = repo.rev_parse_single(spec).ok()?;
    Some(id.object().ok()?.peel_to_commit().ok()?.id)
}

/// `try_to_commit()`'s tail (sequencer.c:1697-1699) — the hooks every commit the
/// sequencer writes *in process* still owes:
///
/// ```c
/// run_commit_hook(0, r->index_file, NULL, "post-commit", NULL);
/// if (flags & AMEND_MSG)
///         commit_post_rewrite(r, current_head, oid);
/// ```
///
/// This port writes those commit objects itself rather than re-entering
/// `git commit`, so without this the hooks that `porcelain::commit` would have
/// run never fire: a merge-backend rebase silently skipped `post-commit` for
/// every replayed commit, and `post-rewrite amend` for every `fixup`/`squash`.
/// `amended` is git's `current_head`, i.e. the commit `AMEND_MSG` replaced —
/// `None` for a fresh commit, which is what suppresses the `amend` half.
///
/// Both are notification hooks: their exit status is dropped, exactly as
/// `run_commit_hook()`'s and `run_rewrite_hook()`'s are at this call site.
fn run_commit_hooks(repo: &gix::Repository, amended: Option<ObjectId>, new: ObjectId) {
    let _ = crate::hooks::run(repo, "post-commit", &[], None);
    if let Some(old) = amended {
        let payload = format!("{old} {new}\n");
        let _ = crate::hooks::run(repo, "post-rewrite", &["amend"], Some(payload.as_bytes()));
    }
}

/// `peek_command()` (sequencer.c:4647-4656): the first instruction at or after
/// `at` that actually does something — `noop`, `drop` and comments are skipped —
/// or `None` when the sheet has nothing left.
///
/// Callers only ever ask `is_fixup()` of the answer, and `is_fixup(-1)` is false,
/// so `None` behaves like "not a fixup".
fn peek_command(list: &todo::List, at: usize) -> Option<todo::Cmd> {
    list.items[at.min(list.items.len())..]
        .iter()
        .map(|i| i.cmd)
        .find(|c| !c.is_noop())
}

/// `do_pick_commit()`'s trailer half (sequencer.c:2436-2437):
///
/// ```c
/// if (opts->trailer_args.nr && !is_fixup(command))
///         amend_strbuf_with_trailers(&ctx->message, &opts->trailer_args);
/// ```
///
/// Returns `message` untouched when no `--trailer` was given, so every call site
/// can apply it unconditionally the way the C tests `opts->trailer_args.nr`.
fn apply_trailers(message: BString, trailers: &[String]) -> Result<BString> {
    if trailers.is_empty() {
        return Ok(message);
    }
    Ok(BString::from(super::interpret_trailers::amend_with_trailers(
        &message, trailers,
    )?))
}

/// `$state_dir/rewritten-pending` and `$state_dir/rewritten-list` — the
/// `post-rewrite` payload a merge-backend rebase accumulates as it runs
/// (sequencer.c:156-163).
///
/// The two files exist because a fixup/squash chain rewrites several old commits
/// into one new one: each member's *old* id is appended to `rewritten-pending`
/// while the chain is still open, and only once the next instruction is not a
/// fixup is the whole pending set paired with the single `HEAD` the chain
/// produced and moved to `rewritten-list`. Keeping them on disk (rather than in
/// this process) is what makes a conflict stop, `--continue` and `--skip` carry
/// the mapping across invocations, exactly as git does.
mod rewritten {
    use super::*;

    /// `record_in_rewritten()` (sequencer.c:2188-2201): remember `oid` as
    /// rewritten, and — unless the next instruction is a `fixup`/`squash`, which
    /// would meld into the same new commit — pair the whole pending set with the
    /// commit `HEAD` now names.
    pub(super) fn record(repo: &gix::Repository, dir: &std::path::Path, oid: ObjectId, next_is_fixup: bool) {
        let Ok(mut out) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("rewritten-pending"))
        else {
            // `fopen_or_warn()` returning NULL: git warns and gives up on this
            // one entry rather than failing the rebase.
            eprintln!(
                "warning: unable to access '{}'",
                dir.join("rewritten-pending").display()
            );
            return;
        };
        use std::io::Write;
        let _ = writeln!(out, "{oid}");
        drop(out);
        if !next_is_fixup {
            flush(repo, dir);
        }
    }

    /// `flush_rewritten_pending()` (sequencer.c:2163-2186): every pending old id
    /// becomes a `<old> <new>` line naming the current `HEAD`, appended to
    /// `rewritten-list`; the pending file is then removed.
    ///
    /// Silently does nothing when the pending file is absent or `HEAD` cannot be
    /// read — the C's three `&&`-joined conditions.
    pub(super) fn flush(repo: &gix::Repository, dir: &std::path::Path) {
        let Ok(buf) = std::fs::read_to_string(dir.join("rewritten-pending")) else {
            return;
        };
        if buf.is_empty() {
            return;
        }
        let Ok(new) = repo.head_id() else { return };
        let Ok(mut out) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("rewritten-list"))
        else {
            return;
        };
        use std::io::Write;
        // `while (*bol) { eol = strchrnul(bol, '\n'); … }` — a trailing newline
        // ends the walk rather than producing an empty final entry.
        for old in buf.split('\n').filter(|l| !l.is_empty()) {
            let _ = writeln!(out, "{old} {}", new.detach());
        }
        drop(out);
        let _ = std::fs::remove_file(dir.join("rewritten-pending"));
    }

    /// `pick_commits()`'s tail (sequencer.c:5190-5207): flush what is still
    /// pending, then — only if the resulting list has something in it — copy
    /// notes for the rewrite and run the `post-rewrite` hook with `rebase` as its
    /// argument and the list on stdin.
    ///
    /// The hook's exit status is deliberately dropped: `run_hooks_opt()`'s return
    /// value is not assigned at the call site, so a `post-rewrite` that fails
    /// does not fail the rebase (measured against stock: a hook exiting 3 still
    /// leaves `git rebase` at 0).
    pub(super) fn run_post_rewrite_hook(repo: &gix::Repository, dir: &std::path::Path) {
        flush(repo, dir);
        let path = dir.join("rewritten-list");
        let Ok(list) = std::fs::read(&path) else { return };
        if list.is_empty() {
            return;
        }
        post_rewrite(repo, &list);
    }

    /// The hook call itself, shared with the exact-replay finish, which rewrites
    /// commits without ever building a `$state_dir` to keep the list in.
    ///
    /// `hook_opt.path_to_stdin = rebase_path_rewritten_list()` — the payload is
    /// the file's bytes on the hook's stdin, one `<old> <new>` pair per line,
    /// with `rebase` as `$1`.
    pub(super) fn post_rewrite(repo: &gix::Repository, payload: &[u8]) {
        if payload.is_empty() {
            return;
        }
        // git shells out to `git notes copy --for-rewrite=rebase` just before
        // this and explicitly ignores the result ("we don't care if this copying
        // failed"). Nothing in this port rewrites notes, so the step is a no-op
        // rather than a divergence: with no `notes.rewriteRef` configured stock
        // copies nothing either.
        let _ = crate::hooks::run(repo, "post-rewrite", &["rebase"], Some(payload));
    }
}

/// `try_to_commit()`'s date rules (sequencer.c:1636-1682), for one replayed
/// commit.
///
/// ```c
/// if (opts->committer_date_is_author_date) {
///         if (!opts->ignore_date) { ...date = author's date... }
///         else reset_ident_date();
///         committer = fmt_ident(..., opts->ignore_date ? NULL : date.buf, ...);
/// } else reset_ident_date();
///
/// if (opts->ignore_date) {
///         ...
///         author = fmt_ident(name, email, WANT_AUTHOR_IDENT, NULL, IDENT_STRICT);
/// }
/// ```
///
/// A `NULL` date argument to `fmt_ident` means "now", so `--ignore-date` restamps
/// the author and `--committer-date-is-author-date` copies whichever author date
/// then survives onto the committer. The two compose: given both, git calls
/// `reset_ident_date()` and dates *both* identities by the same fresh value,
/// which is why `now` is taken from the committer captured once for the run
/// rather than read per commit.
fn apply_date_options(
    author: &mut gix::actor::Signature,
    committer: &mut gix::actor::Signature,
    now: gix::date::Time,
    ignore_date: bool,
    committer_date_is_author_date: bool,
) {
    if ignore_date {
        author.time = now;
    }
    if committer_date_is_author_date {
        committer.time = author.time;
    }
}

/// `(worktree differs from index, index differs from HEAD, unmerged paths)` —
/// the two predicates behind `has_unstaged_changes()` and
/// `has_uncommitted_changes()`, plus the paths `refresh_index()` announces.
///
/// An unmerged entry makes both `diff-files` and `diff-index --cached` report a
/// change, so a conflicted index is both unstaged *and* uncommitted regardless
/// of what the worktree looks like.
pub(super) fn dirty_state(repo: &gix::Repository) -> Result<(bool, bool, Vec<String>)> {
    let mut conflicts: Vec<String> = Vec::new();
    {
        let index = repo.index_or_load_from_head()?.into_owned();
        let backing = index.path_backing();
        let mut last: Option<String> = None;
        for e in index.entries() {
            if e.stage_raw() == 0 {
                continue;
            }
            let path = e.path_in(backing).to_string();
            // git prints one line per conflicted path, not per stage.
            if last.as_deref() != Some(path.as_str()) {
                conflicts.push(path.clone());
                last = Some(path);
            }
        }
    }
    if !conflicts.is_empty() {
        return Ok((true, true, conflicts));
    }

    let mut unstaged = false;
    let mut staged = false;
    let patterns: Vec<BString> = Vec::new();
    for item in repo.status(gix::progress::Discard)?.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(_) => staged = true,
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::EntryStatus;
                match iw {
                    Item::Modification { status, .. } => match status {
                        // Untracked and up-to-date entries do not block a rebase.
                        EntryStatus::NeedsUpdate(_) => {}
                        _ => unstaged = true,
                    },
                    Item::Rewrite { .. } => unstaged = true,
                    Item::DirectoryContents { .. } => {}
                }
            }
        }
    }
    Ok((unstaged, staged, conflicts))
}

/// The `opts->verbose` tail of `pick_commits()` (sequencer.c): once the
/// branch has been re-pointed and `HEAD` re-attached, `-v` prints a diffstat of
/// what the whole replay changed — `diff_tree_oid(orig-head, HEAD)` — before the
/// autostash is re-applied and the `Successfully rebased …` line is written.
///
/// `log_tree_opt.diffopt.output_format` is `DIFF_FORMAT_DIFFSTAT` alone here, so
/// unlike the upstream diffstat above there is no `--summary` half. A rebase that
/// reproduced its commits' trees verbatim (every `--root` no-op, every
/// fast-forward) diffs to nothing and so prints nothing, exactly as git does.
fn verbose_replay_diffstat(orig_head: ObjectId, tip: ObjectId) -> Result<()> {
    super::diff::diff(&["--stat".to_string(), orig_head.to_string(), tip.to_string()])?;
    Ok(())
}

fn full_name(name: &str) -> Result<FullName> {
    name.try_into()
        .map_err(|e| anyhow!("invalid ref name {name}: {e}"))
}

/// Point `HEAD` at `target` (an object for a detached `HEAD`, a ref to attach
/// it), writing `message` to the `HEAD` reflog.
/// The `.git/rebase-merge` state a merge-backend rebase carries, written by
/// `write_basic_state()` (sequencer.c) before the first instruction runs and
/// read back by `read_populate_opts()` on `--continue` / `--skip` / `--abort`.
///
/// The instruction stream itself is *not* part of this struct: it lives in
/// `git-rebase-todo` (what is left to do) and `done` (what has been done), and
/// is parsed by [`super::rebase_todo::List::parse`].
/// `enum rebase_empty_type` (builtin/rebase.c) — what a commit that comes out
/// empty does. `cmd_rebase()` turns it into the two sequencer knobs it actually
/// reads: `drop_redundant_commits` (`drop`), `keep_redundant_commits` (`keep`),
/// or neither (`stop`), each recorded as its own marker file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Empty {
    Drop,
    Keep,
    Stop,
}

struct RebaseState {
    /// Full name of the branch being rebased (`refs/heads/…`) or `detached HEAD`.
    head_name: String,
    onto: ObjectId,
    orig_head: ObjectId,
    /// `--root` without `--onto`: the synthesized empty root commit standing in
    /// as `<onto>`. A pick made while the tip still *is* that commit becomes a
    /// new root commit rather than its child. git records the same value in
    /// `$state_dir/squash-onto`.
    squash_onto: Option<ObjectId>,
    /// `opts.allow_ff` — false under `-f`/`--no-ff`. Not persisted: git writes no
    /// state file for it, so `--continue` after a stop re-derives it from the
    /// resuming command line (which has no `-f`) and the `signoff`/`cdate_is_adate`/
    /// `ignore_date` markers alone, and fast-forwards again.
    allow_ff: bool,
    /// `-q`/`--quiet`: no `Rebasing (n/m)` progress and no
    /// `Successfully rebased …` summary. git records the same thing as the
    /// presence of `$state_dir/quiet`.
    quiet: bool,
    /// `-v`/`--verbose`: the progress line ends in `\n` rather than `\r`.
    verbose: bool,
    /// `--reschedule-failed-exec` / `rebase.rescheduleFailedExec`: an `exec`
    /// that exits non-zero is put back at the head of the todo list instead of
    /// being consumed, so `--continue` retries it.
    reschedule_failed_exec: bool,
    /// `--signoff`: every replayed commit gains a `Signed-off-by` trailer built
    /// from the committer identity. git records it as the one-liner
    /// `$state_dir/signoff`, whose mere presence also re-implies `REBASE_FORCE`
    /// on `--continue`.
    signoff: bool,
    /// `--trailer <t>` (repeatable): every replayed commit's message is run
    /// through `amend_strbuf_with_trailers()` with these arguments. git records
    /// them in `$state_dir/trailer`, one per line, and `read_trailers()`
    /// (sequencer.c:3168-3193) reads them back on `--continue`.
    trailers: Vec<String>,
    /// `opts.allow_rerere_auto` — what [`Sequencer::stop_for_conflict`] passes to
    /// `repo_rerere()`. `Some(true)` stages a replayed resolution, `Some(false)`
    /// leaves it unstaged, `None` defers to `rerere.autoupdate`. git records it
    /// as the one-liner `$state_dir/allow_rerere_autoupdate`, holding the flag
    /// as spelled, and writes no file at all when it was never given.
    rerere_autoupdate: Option<bool>,
    /// `--committer-date-is-author-date`: the committer of every replayed commit
    /// is dated by that commit's author date. git records it as the presence of
    /// `$state_dir/cdate_is_adate` and, on `--continue`, re-clears `allow_ff`
    /// from it (sequencer.c:3234-3237) — a fast-forwarded pick would keep the
    /// old committer date and defeat the option.
    committer_date_is_author_date: bool,
    /// `--ignore-date`/`--reset-author-date`: the *author* date of every replayed
    /// commit is replaced with the current time. Recorded as
    /// `$state_dir/ignore_date` and likewise re-clears `allow_ff`
    /// (sequencer.c:3239-3242).
    ignore_date: bool,
    /// `opts->drop_redundant_commits` / `opts->keep_redundant_commits` — what
    /// `--empty` becomes once `cmd_rebase()` has translated it
    /// (builtin/rebase.c:192-193). `drop` sets the first, `keep` the second, and
    /// `stop` neither, which is what makes `allow_empty()` halt. Recorded as the
    /// presence of `$state_dir/drop_redundant_commits` /
    /// `$state_dir/keep_redundant_commits` (sequencer.c:3344-3347).
    empty: Empty,
    /// `opts->strategy` — `-s <name>`, or `ort` when only `-X` was given. git
    /// records it as the one-liner `$state_dir/strategy` and reads it back on
    /// `--continue` (`read_strategy_opts()`, sequencer.c:3156-3166).
    strategy: Option<String>,
    /// `opts->xopts` — every `-X <opt>`, plus the `ignore-space-change` that
    /// `--ignore-whitespace` turns into on the merge backend
    /// (builtin/rebase.c:1546-1548). Recorded as `$state_dir/strategy_opts`, one
    /// `quote_cmdline()`-quoted word per option on a single line.
    strategy_opts: Vec<String>,
}

fn rebase_merge_dir(repo: &gix::Repository) -> std::path::PathBuf {
    repo.git_dir().join("rebase-merge")
}

/// `rebase_path_update_refs()` (sequencer.c:186-189) — where `--update-refs`
/// parks the `(ref, before, after)` triples it will apply when the rebase ends.
fn update_refs_path(repo: &gix::Repository) -> std::path::PathBuf {
    rebase_merge_dir(repo).join("update-refs")
}

/// `write_update_refs_state()` (sequencer.c:4414-4461): three lines per record —
/// the refname, the id it held when the sheet was built, and the id the rebase
/// has moved it to so far (null until the matching `update-ref` instruction
/// runs). An empty list removes the file.
fn write_update_refs_state(
    repo: &gix::Repository,
    refs: &[(String, ObjectId, ObjectId)],
) -> Result<()> {
    let path = update_refs_path(repo);
    if refs.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = String::new();
    for (name, before, after) in refs {
        body.push_str(name);
        body.push('\n');
        body.push_str(&before.to_string());
        body.push('\n');
        body.push_str(&after.to_string());
        body.push('\n');
    }
    std::fs::write(&path, body)?;
    Ok(())
}

/// `sequencer_get_update_refs_state()` (sequencer.c:6868-6907). A missing file is
/// an empty list — that is the "no `--update-refs`" case, not an error.
fn read_update_refs_state(repo: &gix::Repository) -> Result<Vec<(String, ObjectId, ObjectId)>> {
    let body = match std::fs::read_to_string(update_refs_path(repo)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => bail!("could not read update-refs state: {e}"),
    };
    let mut out = Vec::new();
    let mut lines = body.lines();
    while let Some(name) = lines.next() {
        let (Some(before), Some(after)) = (lines.next(), lines.next()) else {
            eprintln!(
                "warning: update-refs file at '{}' is invalid",
                update_refs_path(repo).display()
            );
            return Ok(out);
        };
        let (Ok(before), Ok(after)) = (
            ObjectId::from_hex(before.as_bytes()),
            ObjectId::from_hex(after.as_bytes()),
        ) else {
            eprintln!(
                "warning: update-refs file at '{}' is invalid",
                update_refs_path(repo).display()
            );
            return Ok(out);
        };
        out.push((name.to_string(), before, after));
    }
    Ok(out)
}

/// `do_update_ref()` (sequencer.c:4559-4579) — the `update-ref` instruction
/// itself. It moves nothing: it only records where `HEAD` is now as the record's
/// `after`, so the ref lands there once the whole sheet has run.
fn do_update_ref(repo: &gix::Repository, refname: &str) -> Result<()> {
    let mut list = read_update_refs_state(repo)?;
    let head = repo.head_id()?.detach();
    for rec in &mut list {
        if rec.0 == refname {
            rec.2 = head;
            break;
        }
    }
    write_update_refs_state(repo, &list)
}

/// `do_update_refs()` (sequencer.c:4581-4630) — the finish. Every record is
/// applied with its `before` as the expected old value, so a ref someone moved
/// during the rebase is refused rather than clobbered, and the two reports name
/// the refs that landed and the refs that did not.
///
/// git reads the records here; this takes them as an argument because the
/// caller has already had to load them (the state directory is removed before
/// this point in the port's `finish()`).
fn apply_pending_ref_updates(
    repo: &gix::Repository,
    list: &[(String, ObjectId, ObjectId)],
    quiet: bool,
) -> bool {
    let mut updated = String::new();
    let mut failed = String::new();
    let mut ok = true;
    for (name, before, after) in list {
        let Ok(full) = full_name(name) else {
            ok = false;
            failed.push_str(&format!("\t{name}\n"));
            continue;
        };
        let expected = if before.is_null() {
            PreviousValue::MustNotExist
        } else {
            PreviousValue::MustExistAndMatch(Target::Object(*before))
        };
        // `refs_update_ref(…, &rec->after, …)`: a null `after` is a deletion, as
        // it is anywhere else in git's ref API.
        let change = if after.is_null() {
            Change::Delete {
                expected,
                log: RefLog::AndReference,
                message: "rewritten during rebase".into(),
            }
        } else {
            Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "rewritten during rebase".into(),
                },
                expected,
                new: Target::Object(*after),
            }
        };
        match repo.edit_reference(RefEdit { change, name: full, deref: false }) {
            Ok(_) => updated.push_str(&format!("\t{name}\n")),
            Err(_) => {
                ok = false;
                failed.push_str(&format!("\t{name}\n"));
            }
        }
    }
    if !quiet && !(updated.is_empty() && failed.is_empty()) {
        eprint!("Updated the following refs with --update-refs:\n{updated}");
        if !ok {
            eprint!("Failed to update the following refs with --update-refs:\n{failed}");
        }
    }
    ok
}

/// `write_basic_state()` — the option state the sequencer needs to resume.
///
/// The `interactive` marker is written unconditionally because every
/// merge-backend rebase runs through the sequencer (`run_specific_rebase()`
/// forces `GIT_SEQUENCE_EDITOR=:` when `-i` was not given, rather than taking a
/// different code path), and `init_basic_state()` writes it for all of them.
fn write_basic_state(repo: &gix::Repository, st: &RebaseState) -> Result<()> {
    let dir = rebase_merge_dir(repo);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("head-name"), format!("{}\n", st.head_name))?;
    std::fs::write(dir.join("onto"), format!("{}\n", st.onto))?;
    std::fs::write(dir.join("orig-head"), format!("{}\n", st.orig_head))?;
    std::fs::write(dir.join("interactive"), b"")?;
    match st.squash_onto {
        Some(oid) => std::fs::write(dir.join("squash-onto"), format!("{oid}\n"))?,
        None => {
            let _ = std::fs::remove_file(dir.join("squash-onto"));
        }
    }
    // No `no-ff` marker: `write_basic_state()`/`save_opts()` persist `allow_ff` only
    // for cherry-pick/revert (`options.allow-ff` in `$GIT_DIR/sequencer/opts`,
    // sequencer.c:3698), never for a rebase. `read_populate_opts()`'s rebase arm
    // (sequencer.c:3223-3247) reads no such file, so `-f`/`--no-ff` is *forgotten*
    // across a stop and `--continue` runs with `allow_ff` back on. Measured: after
    // `git rebase -f -i` stops at a `break`, stock's `--continue` logs
    // `rebase: fast-forward` twice where writing this marker logs `rebase (pick):`.
    marker(&dir, "quiet", st.quiet)?;
    marker(&dir, "verbose", st.verbose)?;
    marker(&dir, "reschedule-failed-exec", st.reschedule_failed_exec)?;
    // `write_file(rebase_path_cdate_is_adate(), "%s", "")` /
    // `write_file(rebase_path_ignore_date(), "%s", "")` (sequencer.c:3348-3351).
    marker(&dir, "cdate_is_adate", st.committer_date_is_author_date)?;
    marker(&dir, "ignore_date", st.ignore_date)?;
    marker(&dir, "no-reschedule-failed-exec", !st.reschedule_failed_exec)?;
    // `write_file(rebase_path_drop_redundant_commits(), "%s", "")` /
    // `write_file(rebase_path_keep_redundant_commits(), "%s", "")`
    // (sequencer.c:3344-3347). `--empty=stop` writes neither, which is exactly
    // what makes `allow_empty()` return 0 and halt the pick.
    marker(&dir, "drop_redundant_commits", st.empty == Empty::Drop)?;
    marker(&dir, "keep_redundant_commits", st.empty == Empty::Keep)?;
    // `write_file(rebase_path_strategy(), "%s\n", opts->strategy)` and
    // `write_strategy_opts()` (sequencer.c:3330-3333). The options line is
    // `quote_cmdline()`'d — every word wrapped in `"` with `"` and `\` escaped —
    // because `read_strategy_opts()` puts it back through `split_cmdline()`.
    match &st.strategy {
        Some(name) => std::fs::write(dir.join("strategy"), format!("{name}\n"))?,
        None => {
            let _ = std::fs::remove_file(dir.join("strategy"));
        }
    }
    if st.strategy_opts.is_empty() {
        let _ = std::fs::remove_file(dir.join("strategy_opts"));
    } else {
        std::fs::write(dir.join("strategy_opts"), quote_cmdline(&st.strategy_opts))?;
    }
    // `write_file(state_dir_path("signoff", opts), "--signoff")` — content-bearing,
    // unlike the empty markers above, though only its presence is ever read back.
    if st.signoff {
        std::fs::write(dir.join("signoff"), b"--signoff")?;
    } else {
        let _ = std::fs::remove_file(dir.join("signoff"));
    }
    // `save_opts()` (sequencer.c:3356-3363): one argument per line, each with a
    // trailing newline, and no file at all when none were given — which is what
    // makes `read_trailers()`'s `errno == ENOENT` arm the "no `--trailer`" case.
    if st.trailers.is_empty() {
        let _ = std::fs::remove_file(dir.join("trailer"));
    } else {
        let mut body = String::new();
        for t in &st.trailers {
            body.push_str(t);
            body.push('\n');
        }
        std::fs::write(dir.join("trailer"), body)?;
    }
    match st.rerere_autoupdate {
        Some(true) => std::fs::write(
            dir.join("allow_rerere_autoupdate"),
            b"--rerere-autoupdate\n",
        )?,
        Some(false) => std::fs::write(
            dir.join("allow_rerere_autoupdate"),
            b"--no-rerere-autoupdate\n",
        )?,
        None => {
            let _ = std::fs::remove_file(dir.join("allow_rerere_autoupdate"));
        }
    }
    Ok(())
}

/// `quote_cmdline()` (alias.c:100-115): every word in double quotes, with `"`
/// and `\` backslash-escaped, joined by single spaces — the form
/// `split_cmdline()` reads back. The `\n` git's `write_file(…, "%s\n", …)`
/// appends is part of the file, not of the quoting.
fn quote_cmdline(argv: &[String]) -> String {
    let mut out = String::new();
    for (i, arg) in argv.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push('"');
        for c in arg.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
    }
    out.push('\n');
    out
}

/// `split_cmdline()` (alias.c:126-176) as `parse_strategy_opts()` uses it: split
/// on unquoted whitespace, honour `'` and `"` as quote characters, and treat a
/// backslash outside single quotes as an escape. Malformed input (an unclosed
/// quote, a trailing backslash) is a `BUG()` in git — unreachable, since the
/// only writer is `quote_cmdline()` — so it is simply parsed as far as it goes
/// here.
fn split_cmdline(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quoted: Option<char> = None;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match quoted {
            None if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            None if c == '\'' || c == '"' => {
                quoted = Some(c);
                started = true;
            }
            Some(q) if c == q => quoted = None,
            _ => {
                started = true;
                if c == '\\' && quoted != Some('\'') {
                    match it.next() {
                        Some(esc) => cur.push(esc),
                        None => break,
                    }
                } else {
                    cur.push(c);
                }
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// git records a boolean option as the presence or absence of an empty file.
fn marker(dir: &std::path::Path, name: &str, on: bool) -> Result<()> {
    if on {
        std::fs::write(dir.join(name), b"")?;
    } else {
        let _ = std::fs::remove_file(dir.join(name));
    }
    Ok(())
}

/// `read_populate_opts()` — read back what [`write_basic_state`] wrote.
fn read_basic_state(repo: &gix::Repository) -> Result<RebaseState> {
    let dir = rebase_merge_dir(repo);
    let read = |f: &str| -> Result<String> {
        Ok(std::fs::read_to_string(dir.join(f))
            .map_err(|e| anyhow!("reading rebase state {f}: {e}"))?
            .trim()
            .to_string())
    };
    Ok(RebaseState {
        head_name: read("head-name")?,
        onto: ObjectId::from_hex(read("onto")?.as_bytes())?,
        orig_head: ObjectId::from_hex(read("orig-head")?.as_bytes())?,
        squash_onto: read("squash-onto")
            .ok()
            .and_then(|s| ObjectId::from_hex(s.as_bytes()).ok()),
        // `read_populate_opts()` (sequencer.c:3228-3242) clears `allow_ff` for
        // exactly three flag files — `signoff`, `cdate_is_adate` and
        // `ignore_date` — because each of them rewrites a date a fast-forward
        // would have kept. `-f`/`--no-ff` itself is *not* among them and is
        // persisted nowhere, so a resumed rebase fast-forwards again.
        allow_ff: !dir.join("signoff").exists()
            && !dir.join("cdate_is_adate").exists()
            && !dir.join("ignore_date").exists(),
        quiet: dir.join("quiet").exists(),
        verbose: dir.join("verbose").exists(),
        reschedule_failed_exec: dir.join("reschedule-failed-exec").exists(),
        signoff: dir.join("signoff").exists(),
        trailers: read_trailers(&dir)?,
        // `read_oneliner(…, READ_ONELINER_SKIP_IF_EMPTY)` followed by an exact
        // match on the flag as spelled: anything else leaves the option unset.
        rerere_autoupdate: match read("allow_rerere_autoupdate").as_deref() {
            Ok("--rerere-autoupdate") => Some(true),
            Ok("--no-rerere-autoupdate") => Some(false),
            _ => None,
        },
        committer_date_is_author_date: dir.join("cdate_is_adate").exists(),
        ignore_date: dir.join("ignore_date").exists(),
        // `read_populate_opts()` (sequencer.c:3249-3253) reads the two markers
        // independently; only one of them is ever written, so the pair maps back
        // onto a single `--empty` value.
        empty: if dir.join("drop_redundant_commits").exists() {
            Empty::Drop
        } else if dir.join("keep_redundant_commits").exists() {
            Empty::Keep
        } else {
            Empty::Stop
        },
        strategy: read("strategy").ok().filter(|s| !s.is_empty()),
        strategy_opts: read("strategy_opts")
            .ok()
            .map(|s| split_cmdline(&s))
            .unwrap_or_default(),
    })
}

/// `read_trailers()` (sequencer.c:3168-3193) — the `--trailer` arguments a
/// stopped rebase saved, read back so `--continue` keeps applying them.
///
/// ```c
/// len = strbuf_read_file(buf, rebase_path_trailer(), 0);
/// if (len > 0) {
///         while ((nl = strchr(p, '\n'))) {
///                 *nl = '\0';
///                 if (!*p) return error(_("trailers file contains empty line"));
///                 strvec_push(&opts->trailer_args, p);
///                 p = nl + 1;
///         }
/// } else if (!len) return error(_("trailers file is empty"));
/// else if (errno != ENOENT) return error(_("cannot read trailers files"));
/// ```
///
/// Only complete lines are taken (a trailing fragment with no `\n` is ignored,
/// as the C's `while (strchr(p, '\n'))` is); a present-but-empty file and an
/// embedded blank line are both errors, while a missing file simply means no
/// `--trailer` was given.
fn read_trailers(dir: &std::path::Path) -> Result<Vec<String>> {
    let body = match std::fs::read_to_string(dir.join("trailer")) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => bail!("cannot read trailers files: {e}"),
    };
    if body.is_empty() {
        bail!("trailers file is empty");
    }
    let mut out = Vec::new();
    for line in body.split_inclusive('\n') {
        let Some(line) = line.strip_suffix('\n') else { break };
        if line.is_empty() {
            bail!("trailers file contains empty line");
        }
        out.push(line.to_string());
    }
    Ok(out)
}

/// The commit a stopped rebase was applying (`$state_dir/stopped-sha`), or
/// `None` when the rebase stopped somewhere that names no commit (`break`, a
/// failed `exec`).
fn read_stopped_sha(repo: &gix::Repository) -> Option<ObjectId> {
    let raw = std::fs::read_to_string(rebase_merge_dir(repo).join("stopped-sha")).ok()?;
    ObjectId::from_hex(raw.trim().as_bytes()).ok()
}

/// The autostash commit a stopped `--autostash` rebase saved in its state dir,
/// if any. Written by [`Sequencer::stop_for_conflict`] on a conflict-stop, consumed by
/// `--continue`/`--abort` to re-apply the user's changes once the rebase ends.
fn read_autostash(repo: &gix::Repository) -> Option<ObjectId> {
    let raw = std::fs::read_to_string(rebase_merge_dir(repo).join("autostash")).ok()?;
    ObjectId::from_hex(raw.trim().as_bytes()).ok()
}

/// `git rebase --abort`: restore the worktree, index and branch to `orig-head`,
/// re-attach `HEAD`, clear the merge state the stop recorded, and drop the state
/// directory.
fn rebase_abort(repo: &gix::Repository) -> Result<ExitCode> {
    let st = read_basic_state(repo)?;
    let should_interrupt = AtomicBool::new(false);
    let old_index = repo.index_or_load_from_head()?.into_owned();
    let orig_tree = repo.find_commit(st.orig_head)?.tree_id()?.detach();
    // A hard restore: every tracked file (including any left with conflict markers)
    // is overwritten from orig-head, and files the rebase added are removed. The
    // diff-based `update_clean_worktree` cannot be used here because the index is
    // conflicted (stage 1/2/3 entries), so it would leave markers in place.
    restore_worktree_to_tree(repo, &old_index, orig_tree, &should_interrupt)?;

    // ```c
    // strbuf_addf(&head_msg, "%s (abort): returning to %s",
    //             options.reflog_action,
    //             options.head_name ? options.head_name
    //                               : oid_to_hex(&options.orig_head->object.oid));
    // ropts.head_msg = head_msg.buf;
    // ropts.branch = options.head_name;
    // ```
    //
    // (builtin/rebase.c:1395-1416.) `head_name` is the **fully qualified** ref
    // read back from `$state_dir/head-name`, so the line reads `rebase (abort):
    // returning to refs/heads/main` — the ref is part of the message, and
    // `@{n}`/`git reflog grep` consumers read it out of there. A detached rebase
    // has no `head_name` and names the object id instead.
    //
    // `reset_head()` is given no `branch_msg`, and `update_refs()` falls back to
    // `reflog_branch ? reflog_branch : reflog_head` (reset.c:72-75) — so the
    // branch update carries the *same* string as the `HEAD` update rather than a
    // wording of its own.
    let head_msg = format!(
        "{} (abort): returning to {}",
        reflog_action(),
        if st.head_name == "detached HEAD" { st.orig_head.to_string() } else { st.head_name.clone() }
    );
    if st.head_name != "detached HEAD" {
        let name = full_name(&st.head_name)?;
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: head_msg.clone().into(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(st.orig_head),
            },
            name: name.clone(),
            deref: false,
        })?;
        set_head(repo, Target::Symbolic(name), &head_msg)?;
    } else {
        set_head(repo, Target::Object(st.orig_head), &head_msg)?;
    }
    // `remove_branch_state(the_repository, 0)` (builtin/rebase.c's `ACTION_ABORT`),
    // which is `remove_merge_branch_state()` plus `SQUASH_MSG` (branch.c:803-818),
    // and `finish_rebase()`'s `delete_ref(NULL, "REBASE_HEAD", …)`
    // (builtin/rebase.c:551). An abort is the one resume path that drops
    // `REBASE_HEAD` — `--continue` and `--skip` both leave it naming the commit
    // they were applying, which stock does too.
    for f in [
        "MERGE_HEAD",
        "MERGE_RR",
        "MERGE_MSG",
        "MERGE_MODE",
        "AUTO_MERGE",
        "SQUASH_MSG",
        "REBASE_HEAD",
    ] {
        let _ = std::fs::remove_file(repo.git_dir().join(f));
    }
    // Re-apply any autostash the interrupted rebase saved, onto the restored
    // orig-head tree, before dropping the state dir that holds its reference.
    let autostash = read_autostash(repo);
    let _ = std::fs::remove_dir_all(rebase_merge_dir(repo));
    if let Some(oid) = autostash {
        crate::porcelain::stash::apply_autostash(repo, oid, false)?;
    }
    super::maintenance::run_auto_maintenance(repo, false)?;
    Ok(ExitCode::SUCCESS)
}

/// Hard-restore the worktree and index to `tree`: overwrite every tracked file
/// from it (discarding conflict markers and local changes) and delete files
/// tracked in `old` but absent from `tree`. Like `reset --hard`, but starting
/// from a possibly-conflicted index.
fn restore_worktree_to_tree(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to restore"))?
        .to_owned();
    let mut new_index = repo.index_from_tree(&tree)?;
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    crate::worktree::checkout_subset(
        &mut new_index,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        should_interrupt,
        opts,
    )?;

    // Remove files tracked before but not in the restored tree.
    let new_paths: std::collections::HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    let backing = old.path_backing();
    for e in old.entries() {
        let path = e.path_in(backing);
        if !new_paths.contains(&path.to_owned()) {
            if let Some(full) = repo.workdir_path(path) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    // `unpack_trees()` ends with `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`

    // (unpack-trees.c:2088-2092), so the index git leaves here carries a cache-tree.

    super::write_tree::rebuild_cache_tree(repo, &mut new_index);
    new_index.write(crate::config::index_write_options(repo))?;
    Ok(())
}

/// `git rebase --show-current-patch`: show the commit that the stopped merge-backend
/// rebase was applying.
///
/// `builtin/rebase.c`'s `ACTION_SHOW_CURRENT_PATCH` runs `git show REBASE_HEAD --`
/// (rebase.c:354-361). `REBASE_HEAD` names the commit whose pick stopped on a
/// conflict — the same commit this module records as `stopped-sha` in the
/// `.git/rebase-merge` state ([`read_stopped_sha`]). `git show` prints the
/// resolved commit's full object id on its `commit <oid>` line rather than the ref
/// spelling, so showing the stopped id directly is byte-for-byte what `git show
/// REBASE_HEAD --` emits. The trailing `--` reproduces git's empty pathspec.
///
/// A rebase stopped at a `break` or a failed `exec` names no commit, which is
/// exactly the case where `REBASE_HEAD` does not resolve either.
fn rebase_show_current_patch(repo: &gix::Repository) -> Result<ExitCode> {
    let Some(stopped) = read_stopped_sha(repo) else {
        eprintln!("fatal: No rebase in progress?");
        return Ok(ExitCode::from(128));
    };
    super::show(&[stopped.to_string(), "--".to_string()])
}

/// `git rebase --quit`: drop the state directory and leave `HEAD` where it is.
fn rebase_quit(repo: &gix::Repository) -> Result<ExitCode> {
    let _ = std::fs::remove_dir_all(rebase_merge_dir(repo));
    Ok(ExitCode::SUCCESS)
}



// ---------------------------------------------------------------------------
// The apply backend: `run_am()` (builtin/rebase.c:620)
// ---------------------------------------------------------------------------

/// `rebase_resolvemsg` (sequencer.c:511). `run_am()` hands it to every `git am`
/// it spawns, and `die_user_resolve()` prints it *instead of* `am`'s own
/// `--continue`/`--skip`/`--abort` block — which is why a conflicted
/// `git rebase --apply` talks about `git rebase` and never mentions `git am`.
const REBASE_RESOLVEMSG: &str = concat!(
    "Resolve all conflicts manually, mark them as resolved with\n",
    "\"git add/rm <conflicted_files>\", then run \"git rebase --continue\".\n",
    "You can instead skip this commit: run \"git rebase --skip\".\n",
    "To abort and get back to the state before \"git rebase\", run \"git rebase --abort\"."
);

/// `opts->state_dir` for `REBASE_APPLY` (builtin/rebase.c:1266) — the very
/// directory `git am` uses for its own session, which is what lets `git rebase
/// --continue` be `git am --resolved` and nothing more.
fn rebase_apply_dir(repo: &gix::Repository) -> std::path::PathBuf {
    repo.git_dir().join("rebase-apply")
}

/// What `run_am()` reports back.
enum AmOutcome {
    /// The whole mailbox was applied; the payload is the new detached `HEAD`.
    Done(ObjectId),
    /// `am` stopped — a conflict, an empty patch, or a refusal. Its own
    /// diagnostics are already on the terminal and the session directory is left
    /// in place for `--continue`/`--skip`/`--abort`.
    Stopped(ExitCode),
}

/// Everything `run_am()` reads out of `struct rebase_options`.
struct AmRun<'a> {
    repo: &'a gix::Repository,
    onto: ObjectId,
    upstream: Option<ObjectId>,
    orig_head: ObjectId,
    /// `opts->restrict_revision` — the `--fork-point` refinement, appended to the
    /// range as `^<oid>`.
    restrict_revision: Option<ObjectId>,
    root: bool,
    /// `opts->revisions`, used only in the "cannot rebase them" diagnostic.
    revisions: &'a str,
    /// `opts->git_am_opts` — the `-C<n>`/`-p<n>`/`--whitespace=`/`--signoff`/`-q`
    /// set `cmd_rebase()` accumulated for the backend.
    am_opts: &'a [String],
    rerere_autoupdate: Option<bool>,
    quiet: bool,
    head_name: Option<String>,
}

/// `run_am()` — the apply backend's whole replay.
///
/// git turns the range into a mailbox with `git format-patch` and pipes it into
/// `git am --rebasing`. The mailbox is *not* how the patches travel: under
/// `--rebasing`, `parse_mail_rebase()` (builtin/am.c:1464) reads only each
/// message's `From <oid>` postmark and regenerates the patch from that commit,
/// so `format-patch`'s real job here is to decide **which** commits are replayed
/// and **in what order** — `--cherry-pick --right-only` drops the ones already
/// upstream, `--topo-order` fixes the sequence.
///
/// That is also why the mailbox must come from `format-patch` rather than be
/// synthesised: `.git/rebase-apply/0001`… survive a conflict stop and are
/// therefore observable, and `--show-current-patch=raw` prints one verbatim.
fn run_am(a: &AmRun<'_>) -> Result<AmOutcome> {
    use std::io::IsTerminal;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe()
        .map_err(|e| anyhow!("cannot locate the running executable: {e}"))?;
    let workdir = a.repo.workdir().map(|w| w.to_path_buf());
    let rebased_patches = a.repo.git_dir().join("rebased-patches");

    let out = std::fs::File::create(&rebased_patches)
        .map_err(|e| anyhow!("could not open '{}' for writing: {e}", rebased_patches.display()))?;

    let mut fp = Command::new(&exe);
    fp.arg("format-patch").args([
        "-k",
        "--stdout",
        "--full-index",
        "--cherry-pick",
        "--right-only",
        "--default-prefix",
        "--no-renames",
        "--no-cover-letter",
        "--pretty=mboxrd",
        "--topo-order",
        "--no-base",
    ]);
    // `opts->git_format_patch_opt`, whose only contributor is
    // `if (isatty(2) && options.flags & REBASE_NO_QUIET)` (builtin/rebase.c:1565).
    if !a.quiet && std::io::stderr().is_terminal() {
        fp.arg("--progress");
    }
    // `<onto|upstream>...<orig_head>`: `opts->root` is "now equivalent to
    // !opts->upstream", so the symmetric range starts at `<onto>` under `--root`.
    let base = if a.root {
        a.onto
    } else {
        a.upstream.unwrap_or(a.onto)
    };
    fp.arg(format!("{}...{}", base.to_hex(), a.orig_head.to_hex()));
    if let Some(r) = a.restrict_revision {
        fp.arg(format!("^{}", r.to_hex()));
    }
    if let Some(w) = &workdir {
        fp.current_dir(w);
    }
    fp.stdout(Stdio::from(out)).stderr(Stdio::piped());
    let fp_out = fp
        .spawn()
        .and_then(|c| c.wait_with_output())
        .map_err(|e| anyhow!("failed to run format-patch: {e}"))?;

    if !fp_out.status.success() {
        let _ = std::fs::remove_file(&rebased_patches);
        // `reset_head(oid = orig_head, branch = head_name)` — git puts the branch
        // back exactly where it was before the detach above.
        reset_to_orig_head(a.repo, a.orig_head, a.head_name.as_deref())?;
        let err = String::from_utf8_lossy(&fp_out.stderr).into_owned();
        // A missing flag in *this* binary is not the failure git's message
        // describes, so it is named instead of dressed up as one.
        if err.contains("unrecognized argument") || err.contains("unsupported flag") {
            bail!(
                "the apply backend builds its mailbox with `git format-patch`, which rejected \
                 the invocation `run_am()` makes:\n    {}\nThe consumer half — \
                 `git am --rebasing --patch-format=mboxrd` — is ported and byte-identical to \
                 stock. Use the merge backend (`--merge`, the default) until format-patch \
                 accepts the flag above",
                err.trim()
            );
        }
        eprint!("{err}");
        eprintln!(
            "error: \ngit encountered an error while preparing the patches to replay\n\
             these revisions:\n\n    {}\n\nAs a result, git cannot rebase them.",
            a.revisions
        );
        return Ok(AmOutcome::Stopped(ExitCode::from(1)));
    }

    let outcome = spawn_am(a, |am| {
        am.arg("--rebasing")
            .arg(format!("--resolvemsg={REBASE_RESOLVEMSG}"))
            .arg("--patch-format=mboxrd");
        let inp = std::fs::File::open(&rebased_patches)?;
        am.stdin(Stdio::from(inp));
        Ok(())
    })?;
    let _ = std::fs::remove_file(&rebased_patches);
    outcome
        .map(|()| finish_am(a.repo))
        .unwrap_or_else(|code| Ok(AmOutcome::Stopped(code)))
}

/// `run_am()`'s `ACTION_CONTINUE`/`ACTION_SKIP` arms (builtin/rebase.c:631-650):
/// the resume verbs are `git am --resolved`/`--skip` with the rebase's own
/// resolve message, and nothing else.
fn run_am_resume(a: &AmRun<'_>, skip: bool) -> Result<AmOutcome> {
    let outcome = spawn_am(a, |am| {
        am.arg(if skip { "--skip" } else { "--resolved" })
            .arg(format!("--resolvemsg={REBASE_RESOLVEMSG}"));
        Ok(())
    })?;
    outcome
        .map(|()| finish_am(a.repo))
        .unwrap_or_else(|code| Ok(AmOutcome::Stopped(code)))
}

/// The `git am` child every `run_am()` arm spawns: the accumulated
/// `git_am_opts`, the `<action> (pick)` reflog action, and the rerere switch,
/// plus whatever the caller adds.
fn spawn_am(
    a: &AmRun<'_>,
    configure: impl FnOnce(&mut std::process::Command) -> std::io::Result<()>,
) -> Result<std::result::Result<(), ExitCode>> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow!("cannot locate the running executable: {e}"))?;
    let mut am = std::process::Command::new(&exe);
    am.arg("am");
    for o in a.am_opts {
        am.arg(o);
    }
    match a.rerere_autoupdate {
        Some(true) => {
            am.arg("--rerere-autoupdate");
        }
        Some(false) => {
            am.arg("--no-rerere-autoupdate");
        }
        None => {}
    }
    // `strvec_pushf(&am.env, GIT_REFLOG_ACTION "=%s (pick)", opts->reflog_action)`
    // — every commit `am` writes gets a `rebase (pick): <subject>` reflog line.
    am.env("GIT_REFLOG_ACTION", format!("{} (pick)", reflog_action()));
    if let Some(w) = a.repo.workdir() {
        am.current_dir(w);
    }
    configure(&mut am).map_err(|e| anyhow!("failed to prepare git am: {e}"))?;
    let status = am
        .status()
        .map_err(|e| anyhow!("failed to run git am: {e}"))?;
    if status.success() {
        return Ok(Ok(()));
    }
    let code = u8::try_from(status.code().unwrap_or(1)).unwrap_or(1);
    Ok(Err(ExitCode::from(code)))
}

/// The tail of a successful `run_am()`: `am --rebasing` leaves its session
/// directory behind ("it's up to the caller to take care of housekeeping",
/// builtin/am.c:1937), and `finish_rebase()` is the caller that removes it.
fn finish_am(repo: &gix::Repository) -> Result<AmOutcome> {
    let _ = std::fs::remove_dir_all(rebase_apply_dir(repo));
    let tip = repo
        .head_id()
        .map_err(|e| anyhow!("could not read HEAD after git am: {e}"))?
        .detach();
    Ok(AmOutcome::Done(tip))
}

/// `reset_head(oid = orig_head, branch = head_name, flags = RESET_HEAD_HARD)` —
/// what both `run_am()`'s format-patch failure path and `rebase --abort` use to
/// put the branch and worktree back where the rebase found them.
fn reset_to_orig_head(
    repo: &gix::Repository,
    orig_head: ObjectId,
    head_name: Option<&str>,
) -> Result<()> {
    let should_interrupt = AtomicBool::new(false);
    let old_index = repo.index_or_load_from_head()?.into_owned();
    let tree = repo.find_commit(orig_head)?.tree_id()?.detach();
    restore_worktree_to_tree(repo, &old_index, tree, &should_interrupt)?;
    match head_name.filter(|n| *n != "detached HEAD") {
        Some(name) => {
            let full = full_name(name)?;
            repo.edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!("{} (abort): returning to {name}", reflog_action()).into(),
                    },
                    expected: PreviousValue::Any,
                    new: Target::Object(orig_head),
                },
                name: full.clone(),
                deref: false,
            })?;
            set_head(
                repo,
                Target::Symbolic(full),
                &format!("{} (abort): returning to {name}", reflog_action()),
            )?;
        }
        None => set_head(
            repo,
            Target::Object(orig_head),
            &format!("{} (abort): returning to {orig_head}", reflog_action()),
        )?,
    }
    Ok(())
}

/// `rebase_write_basic_state()` (builtin/rebase.c:333) for the apply backend.
///
/// `run_specific_rebase()` calls it only when `run_am()` came back non-zero and
/// the state directory still exists, so these files appear next to `git am`'s own
/// session files exactly when a replay stopped — which is what makes the stopped
/// session resumable by `git rebase --continue` rather than only `git am`.
fn rebase_write_basic_state_apply(
    dir: &std::path::Path,
    head_name: Option<&str>,
    onto: ObjectId,
    orig_head: ObjectId,
    quiet: bool,
    verbose: bool,
    rerere_autoupdate: Option<bool>,
) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    std::fs::write(
        dir.join("head-name"),
        format!("{}\n", head_name.unwrap_or("detached HEAD")),
    )?;
    std::fs::write(dir.join("onto"), format!("{onto}\n"))?;
    std::fs::write(dir.join("orig-head"), format!("{orig_head}\n"))?;
    if quiet {
        std::fs::write(dir.join("quiet"), b"")?;
    }
    if verbose {
        std::fs::write(dir.join("verbose"), b"")?;
    }
    match rerere_autoupdate {
        Some(true) => std::fs::write(dir.join("allow_rerere_autoupdate"), b"--rerere-autoupdate")?,
        Some(false) => {
            std::fs::write(dir.join("allow_rerere_autoupdate"), b"--no-rerere-autoupdate")?
        }
        None => {}
    }
    Ok(())
}

/// `read_basic_state()` for an apply-backend session: the three files
/// `rebase_write_basic_state()` wrote, plus the rerere switch.
struct ApplyState {
    head_name: Option<String>,
    onto: ObjectId,
    orig_head: ObjectId,
    quiet: bool,
    verbose: bool,
    rerere_autoupdate: Option<bool>,
}

fn read_apply_state(repo: &gix::Repository) -> Result<ApplyState> {
    let dir = rebase_apply_dir(repo);
    let read = |name: &str| -> Result<String> {
        Ok(std::fs::read_to_string(dir.join(name))
            .map_err(|e| anyhow!("could not read '{}': {e}", dir.join(name).display()))?
            .trim_end_matches('\n')
            .to_string())
    };
    let head_name = read("head-name")?;
    let rr = std::fs::read_to_string(dir.join("allow_rerere_autoupdate")).ok();
    Ok(ApplyState {
        head_name: (head_name != "detached HEAD").then_some(head_name),
        onto: ObjectId::from_hex(read("onto")?.as_bytes())?,
        orig_head: ObjectId::from_hex(read("orig-head")?.as_bytes())?,
        quiet: dir.join("quiet").exists(),
        verbose: dir.join("verbose").exists(),
        rerere_autoupdate: rr.map(|s| s.trim() == "--rerere-autoupdate"),
    })
}

/// `cmd_rebase()`'s `ACTION_CONTINUE`/`ACTION_SKIP`/`ACTION_ABORT`/`ACTION_QUIT`
/// for a `.git/rebase-apply` session (builtin/rebase.c:1352-1440).
fn rebase_apply_resume(repo: &gix::Repository, action: ModeOption) -> Result<ExitCode> {
    let st = read_apply_state(repo)?;
    let dir = rebase_apply_dir(repo);
    match action {
        ModeOption::Quit => {
            let _ = std::fs::remove_dir_all(&dir);
            return Ok(ExitCode::SUCCESS);
        }
        ModeOption::Abort => {
            // `rerere_clear(&merge_rr)`.
            let _ = std::fs::remove_file(repo.git_dir().join("MERGE_RR"));
            reset_to_orig_head(repo, st.orig_head, st.head_name.as_deref())?;
            for f in [
                "MERGE_HEAD",
                "MERGE_RR",
                "MERGE_MSG",
                "MERGE_MODE",
                "AUTO_MERGE",
                "SQUASH_MSG",
                "REBASE_HEAD",
            ] {
                let _ = std::fs::remove_file(repo.git_dir().join(f));
            }
            let _ = std::fs::remove_dir_all(&dir);
            super::maintenance::run_auto_maintenance(repo, false)?;
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    let a = AmRun {
        repo,
        onto: st.onto,
        upstream: None,
        orig_head: st.orig_head,
        restrict_revision: None,
        root: false,
        revisions: "",
        am_opts: &[],
        rerere_autoupdate: st.rerere_autoupdate,
        quiet: st.quiet,
        head_name: st.head_name.clone(),
    };
    let skip = action == ModeOption::Skip;
    if skip {
        // `ACTION_SKIP`'s `reset_head(RESET_HEAD_HARD)` before the `am --skip`:
        // `am` does its own `clean_index`, but git discards the worktree here
        // first so a half-applied patch cannot survive into the next one.
        let _ = std::fs::remove_file(repo.git_dir().join("MERGE_RR"));
    }
    match run_am_resume(&a, skip)? {
        AmOutcome::Done(tip) => {
            move_to_original_branch(repo, tip, st.head_name.as_deref(), st.onto)?;
            let _ = std::fs::remove_file(repo.git_dir().join("REBASE_HEAD"));
            let _ = std::fs::remove_file(repo.git_dir().join("AUTO_MERGE"));
            super::maintenance::run_auto_maintenance(repo, st.quiet)?;
            Ok(ExitCode::SUCCESS)
        }
        AmOutcome::Stopped(code) => {
            rebase_write_basic_state_apply(
                &dir,
                st.head_name.as_deref(),
                st.onto,
                st.orig_head,
                st.quiet,
                st.verbose,
                st.rerere_autoupdate,
            )?;
            // `return !!ret` again: a resumed apply-backend rebase that stops
            // reports 1, not `git am`'s 128.
            let _ = code;
            Ok(ExitCode::from(1))
        }
    }
}

/// `move_to_original_branch()` (builtin/rebase.c:592): `RESET_HEAD_REFS_ONLY` —
/// point the branch at the replayed tip and re-attach `HEAD` to it, touching
/// neither the index nor the worktree (they already hold that tree).
fn move_to_original_branch(
    repo: &gix::Repository,
    tip: ObjectId,
    head_name: Option<&str>,
    onto: ObjectId,
) -> Result<()> {
    let Some(name) = head_name else {
        return Ok(()); /* nothing to move back to */
    };
    let full = full_name(name)?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("{} (finish): {name} onto {onto}", reflog_action()).into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(tip),
        },
        name: full.clone(),
        deref: false,
    })?;
    let message = format!("{} (finish): returning to {name}", reflog_action());
    set_head(repo, Target::Symbolic(full), &message)?;
    super::checkout::record_head_move(repo, Some(tip), Some(tip), &message);
    Ok(())
}

/// Everything `do_interactive_rebase()` carries into `complete_action()`.
struct SequencerStart<'a> {
    repo: &'a gix::Repository,
    /// The replay range, oldest first.
    range: Vec<ObjectId>,
    state: RebaseState,
    /// The `<onto>` spelling that goes into the `rebase (start): checkout <…>`
    /// reflog entry.
    /// `options.upstream` — `None` under `--root`, which is what makes
    /// `get_revision_ranges()` print the head alone rather than a range.
    upstream: Option<ObjectId>,
    onto_spec: &'a str,
    exec: &'a [String],
    autosquash: bool,
    keep_empty: bool,
    /// `REBASE_INTERACTIVE_EXPLICIT`. Without it `run_specific_rebase()` forces
    /// `GIT_SEQUENCE_EDITOR=:`, i.e. the sheet is used exactly as generated.
    interactive: bool,
    /// `--rebase-merges`: the sheet rebuilds the branch topology instead of
    /// flattening it, so `make_script_with_merges()` generates it.
    rebase_merges: bool,
    autostash: Option<ObjectId>,
    old_index: &'a gix::index::File,
    /// `revs.cherry_mark = !reapply_cherry_picks` — the commits
    /// `sequencer_make_script()` drops because their patch is already upstream.
    cherry: todo::CherryMark,
    /// `--update-refs` / `rebase.updateRefs`: `complete_action()` passes it to
    /// `todo_list_add_update_ref_commands()` (sequencer.c:6581), which is the
    /// only thing that puts `update-ref` lines in the sheet.
    update_refs: bool,
}

/// `do_interactive_rebase()` followed by `complete_action()`: build the
/// instruction sheet, let the user edit it, then execute it.
fn sequencer_rebase(start: SequencerStart<'_>) -> Result<ExitCode> {
    let repo = start.repo;
    let dir = rebase_merge_dir(repo);
    let abbreviate = repo
        .config_snapshot()
        .boolean("rebase.abbreviateCommands")
        == Some(true);

    // `sequencer_make_script()`.
    let script = if start.rebase_merges {
        // `get_revision_ranges()` builds `<upstream|onto>...<orig_head>`, and
        // `handle_dotdot_1()` records the *merge bases* of that symmetric range as
        // `revs->cmdline.rev[0]` — before either endpoint — with `BOTTOM` set. That
        // first merge base is therefore the commit `make_script_with_merges()`
        // pre-labels `onto`, not `<onto>` itself. Only the first is used, so a
        // criss-cross history with several bases seeds just one, as git does.
        let base_rev = start.upstream.unwrap_or(start.state.onto);
        let label_onto = repo
            .merge_base(base_rev, start.state.orig_head)
            .ok()
            .map(|id| id.detach());
        todo::make_script_with_merges(
            repo,
            &start.range,
            label_onto,
            start.keep_empty,
            abbreviate,
            &start.cherry,
        )?
    } else {
        todo::make_script(repo, &start.range, start.keep_empty, abbreviate, &start.cherry)?
    };
    start.cherry.advise(repo);

    // `init_basic_state()`: the state directory exists before the editor runs,
    // so an interrupted edit is still an in-progress rebase.
    write_basic_state(repo, &start.state)?;
    if let Some(oid) = start.autostash {
        std::fs::write(dir.join("autostash"), format!("{oid}\n"))?;
    }

    let (mut list, ok) = todo::List::parse(repo, &script, false);
    if !ok {
        crate::git_fatal!("generated an unusable todo list");
    }

    // `complete_action()`.
    if list.items.is_empty() {
        list.items.push(todo::Item::new(todo::Cmd::Noop));
    }
    // `if (update_refs && todo_list_add_update_ref_commands(todo_list))`
    // (sequencer.c:6581): the `update-ref` lines go in *before* `--autosquash`
    // reorders and before `--exec` interleaves, so a ref stays attached to the
    // pick it decorated even after both have run.
    if start.update_refs {
        let records = todo::add_update_ref_commands(repo, &mut list)?;
        let null = ObjectId::null(repo.object_hash());
        let state: Vec<(String, ObjectId, ObjectId)> =
            records.into_iter().map(|(n, before)| (n, before, null)).collect();
        write_update_refs_state(repo, &state)?;
    }
    if start.autosquash {
        todo::rearrange_squash(repo, &mut list)?;
    }
    todo::add_exec_commands(&mut list, start.exec);
    if list.count_commands() == 0 {
        finish_early(repo, start.autostash)?;
        eprintln!("error: nothing to do");
        return Ok(ExitCode::from(1));
    }

    let short_range = short_revisions(repo, start.upstream, start.state.orig_head);
    let short_onto = todo::short_name(repo, start.state.onto);
    let mut new_list = match edit_todo_list(
        repo,
        &list,
        Some(&short_range),
        Some(&short_onto),
        start.interactive,
        abbreviate,
    )? {
        EditOutcome::Ok(new) => new,
        EditOutcome::NothingToDo => {
            finish_early(repo, start.autostash)?;
            eprintln!("error: nothing to do");
            return Ok(ExitCode::from(1));
        }
        EditOutcome::EditorFailed => {
            finish_early(repo, start.autostash)?;
            return Ok(ExitCode::from(1));
        }
        EditOutcome::Rejected => {
            // git's `res == -4`: the sheet is unusable, but the rebase is
            // already checked out at `<onto>` so the user can fix it with
            // `--edit-todo`. Checking out first keeps the two in step.
            checkout_onto(repo, &start)?;
            return Ok(ExitCode::from(1));
        }
    };

    // `skip_unnecessary_picks()`: with `allow_ff`, the leading picks that would
    // land on the parent they already have are moved to `done` and the rebase
    // starts from the last of them instead of `<onto>`.
    let mut base = start.state.onto;
    if start.state.allow_ff {
        skip_unnecessary_picks(repo, &mut new_list, &mut base)?;
    }
    std::fs::write(
        dir.join("git-rebase-todo"),
        new_list.to_bytes(repo, None, 0),
    )?;

    // `checkout_onto()`: ORIG_HEAD, then detach `HEAD` at the base and move the
    // worktree onto its tree.
    //
    // The reflog names `<onto>` as the caller spelled it (sequencer.c:4875 passes
    // `onto_name`, not the id), even when `skip_unnecessary_picks()` has advanced the
    // base past it — the entry describes the rebase, not the commit it landed on.
    let onto_label = start.onto_spec.to_string();
    write_orig_head(repo, start.state.orig_head)?;
    set_head(
        repo,
        Target::Object(base),
        &format!("{} (start): checkout {onto_label}", reflog_action()),
    )?;
    let should_interrupt = AtomicBool::new(false);
    update_clean_worktree(repo, start.old_index, base, &should_interrupt)?;

    let mut seq = Sequencer::new(repo, start.state)?;
    seq.autostash = start.autostash;
    seq.run(new_list, 0)
}

/// `git rebase --edit-todo`: re-open the instruction sheet of a rebase that is
/// already in progress.
fn rebase_edit_todo(repo: &gix::Repository) -> Result<ExitCode> {
    let dir = rebase_merge_dir(repo);
    let abbreviate = repo
        .config_snapshot()
        .boolean("rebase.abbreviateCommands")
        == Some(true);
    let raw = std::fs::read(dir.join("git-rebase-todo"))
        .map_err(|e| anyhow!("could not read '{}'. {e}", dir.join("git-rebase-todo").display()))?;
    // `strbuf_stripspace(&todo_list.buf, comment_line_str)` — the help block
    // from the previous round is dropped before it is shown again.
    let comment = todo::comment_prefix(repo);
    let stripped = super::stripspace::strip_space(&raw, Some(comment.as_bytes()));
    let (list, _) = todo::List::parse(repo, &stripped, dir.join("done").exists());
    match edit_todo_list(repo, &list, None, None, true, abbreviate)? {
        EditOutcome::Ok(new) => {
            std::fs::write(dir.join("git-rebase-todo"), new.to_bytes(repo, None, 0))?;
            Ok(ExitCode::SUCCESS)
        }
        EditOutcome::NothingToDo => Ok(ExitCode::SUCCESS),
        // `edit_todo_file()` (rebase-interactive.c) just propagates the failure;
        // `--edit-todo` never removes the state directory, so an interrupted
        // edit leaves the rebase exactly as it found it.
        EditOutcome::EditorFailed | EditOutcome::Rejected => Ok(ExitCode::from(1)),
    }
}

/// What the editor round-trip produced.
enum EditOutcome {
    Ok(todo::List),
    /// The user emptied the sheet on the initial edit: git aborts the rebase.
    NothingToDo,
    /// `edit_todo_list()`'s `-2`: the sequence editor exited non-zero. It has
    /// already reported itself, so `complete_action()` only tears the rebase
    /// down (sequencer.c:6601-6605) — autostash back, state directory gone,
    /// exit 1 with nothing further printed.
    EditorFailed,
    /// The sheet does not parse, or dropped commits under
    /// `rebase.missingCommitsCheck=error`.
    Rejected,
}

/// `edit_todo_list()`: write the sheet with its help block, hand it to the
/// sequence editor, read it back, strip the comments, parse it, and compare it
/// against the backup for accidentally dropped commits.
fn edit_todo_list(
    repo: &gix::Repository,
    list: &todo::List,
    revisions: Option<&str>,
    onto: Option<&str>,
    interactive: bool,
    abbreviate: bool,
) -> Result<EditOutcome> {
    let dir = rebase_merge_dir(repo);
    let initial = revisions.is_some() && onto.is_some();
    let flags = todo::SHORTEN_IDS | if abbreviate { todo::ABBREVIATE_CMDS } else { 0 };

    let mut shown = list.to_bytes(repo, None, flags);
    todo::append_help(repo, &mut shown, list.count_commands(), revisions, onto);
    let todo_path = dir.join("git-rebase-todo");
    std::fs::write(&todo_path, &shown)?;

    // The backup keeps full object ids, so `todo_list_check()` compares commits
    // rather than abbreviations.
    let mut backup = list.to_bytes(repo, None, flags & !todo::SHORTEN_IDS);
    todo::append_help(repo, &mut backup, list.count_commands(), revisions, onto);
    std::fs::write(dir.join("git-rebase-todo.backup"), &backup)?;

    // Without an explicit `-i`, `run_specific_rebase()` sets the sequence editor
    // to `:` — the sheet comes back exactly as written.
    if interactive && !todo::launch_sequence_editor(repo, &todo_path)? {
        return Ok(EditOutcome::EditorFailed);
    }

    let comment = todo::comment_prefix(repo);
    let edited = std::fs::read(&todo_path)?;
    let stripped = super::stripspace::strip_space(&edited, Some(comment.as_bytes()));
    if initial && stripped.is_empty() {
        return Ok(EditOutcome::NothingToDo);
    }
    let (new, ok) = todo::List::parse(repo, &stripped, dir.join("done").exists());
    if !ok {
        eprint!("{}", todo::EDIT_TODO_ADVICE);
        return Ok(EditOutcome::Rejected);
    }
    if todo::check(repo, list, &new) {
        std::fs::write(dir.join("dropped"), b"")?;
        return Ok(EditOutcome::Rejected);
    }
    let _ = std::fs::remove_file(dir.join("dropped"));
    Ok(EditOutcome::Ok(new))
}

/// `apply_autostash(); sequencer_remove_state();` — the teardown git runs when
/// it decides there is nothing to do after all, before the rebase has moved
/// anything.
fn finish_early(repo: &gix::Repository, autostash: Option<ObjectId>) -> Result<()> {
    let _ = std::fs::remove_dir_all(rebase_merge_dir(repo));
    if let Some(oid) = autostash {
        crate::porcelain::stash::apply_autostash(repo, oid, false)?;
    }
    Ok(())
}

/// `checkout_onto()` on the `res == -4` path: the sheet was rejected, but the
/// state directory stays so `--edit-todo` can fix it, and `HEAD` is detached at
/// `<onto>` so the two agree.
fn checkout_onto(repo: &gix::Repository, start: &SequencerStart<'_>) -> Result<()> {
    write_orig_head(repo, start.state.orig_head)?;
    set_head(
        repo,
        Target::Object(start.state.onto),
        &format!("{} (start): checkout {}", reflog_action(), start.onto_spec),
    )?;
    let should_interrupt = AtomicBool::new(false);
    update_clean_worktree(repo, start.old_index, start.state.onto, &should_interrupt)
}

/// git writes `ORIG_HEAD` only once it commits to actually rebasing. It is a
/// pseudo-ref, so no reflog is created for it under the default
/// `core.logAllRefUpdates` (gix applies git's own `should_autocreate_reflog`
/// rule); `core.logAllRefUpdates=always` is what makes the entry appear, and
/// therefore what makes its message observable.
///
/// The message is *not* the bare action name. Both start paths reach
/// `reset_working_tree()` with `RESET_WORKING_TREE_UPDATE_ORIG_HEAD` set and no
/// `orig_head_msg` of their own — `sequencer.c:4936-4946` for the merge backend
/// (`checkout_onto()`, which passes only `.head_msg` and
/// `.default_reflog_action`) and `builtin/rebase.c:1882-1889` for the apply
/// backend. `reset.c` then builds the message itself:
///
/// ```c
/// if ((update_orig_head && !reflog_orig_head) || !reflog_head) {
///         if (!default_reflog_action)
///                 BUG("default_reflog_action must be given when reflog messages are omitted");
///         reflog_action = getenv(GIT_REFLOG_ACTION_ENVIRONMENT);
///         strbuf_addf(&msg, "%s: ", reflog_action ? reflog_action :
///                                                   default_reflog_action);
/// }
/// prefix_len = msg.len;
///
/// if (update_orig_head) {
///         ...
///                 if (!reflog_orig_head) {
///                         strbuf_addstr(&msg, "updating ORIG_HEAD");
///                         reflog_orig_head = msg.buf;
///                 }
/// ```
/// (`reset.c:34-50`)
///
/// So the entry reads `<action>: updating ORIG_HEAD`, where `<action>` is
/// `$GIT_REFLOG_ACTION` when set and `rebase` otherwise — exactly
/// [`reflog_action`]. Only the two start sites update `ORIG_HEAD`;
/// `--continue`/`--skip`/`--abort` pass `RESET_WORKING_TREE_UPDATE_HEAD` without
/// `…_UPDATE_ORIG_HEAD` (`builtin/rebase.c:1390-1391`, `1416-1417`) and so leave
/// it alone.
fn write_orig_head(repo: &gix::Repository, head: ObjectId) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("{}: updating ORIG_HEAD", reflog_action()).into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(head),
        },
        name: full_name("ORIG_HEAD")?,
        deref: false,
    })?;
    Ok(())
}

/// `get_revision_ranges()`'s `shortrevisions`: `<short upstream>..<short head>`,
/// or just the short head when there is no upstream — the `--root` shape, where
/// `<onto>` stands in as the base and git prints the head alone.
fn short_revisions(repo: &gix::Repository, upstream: Option<ObjectId>, head: ObjectId) -> String {
    let short_head = todo::short_name(repo, head);
    match upstream {
        Some(b) => format!("{}..{short_head}", todo::short_name(repo, b)),
        None => short_head,
    }
}
/// `git rebase --continue` / `--skip`.
///
/// `sequencer_continue()`: read the state and the remaining instruction stream,
/// conclude whatever the rebase stopped in the middle of
/// (`commit_staged_changes()`), then hand back to the instruction loop.
///
/// `--skip` is git's `ACTION_SKIP`, which throws the half-applied work away
/// (`reset --hard HEAD`) before doing the same thing; the instruction that
/// stopped has already been moved to `done` by `save_todo()`, so dropping it is
/// exactly "do not commit what is staged".
fn rebase_continue(repo: &gix::Repository, skip: bool) -> Result<ExitCode> {
    let st = read_basic_state(repo)?;
    let dir = rebase_merge_dir(repo);
    let (list, parsed_ok) = todo::List::parse(
        repo,
        &std::fs::read(dir.join("git-rebase-todo")).unwrap_or_default(),
        dir.join("done").exists(),
    );
    if !parsed_ok {
        eprintln!("error: please fix this using 'git rebase --edit-todo'.");
        return Ok(ExitCode::from(1));
    }

    let mut seq = Sequencer::new(repo, st)?;
    seq.load_fixup_state()?;

    // ```c
    // } else if (file_exists(rebase_path_stopped_sha())) {
    //         if (read_oneliner(&buf, rebase_path_stopped_sha(), READ_ONELINER_SKIP_IF_EMPTY) &&
    //             !get_oid_hex(buf.buf, &oid))
    //                 record_in_rewritten(&oid, peek_command(&todo_list, 0));
    // }
    // ```
    // (sequencer.c:5505-5514). The instruction that stopped has already been
    // moved to `done`, so the offset is 0 — the *next* thing to run. C reads the
    // file after `commit_staged_changes()`, which never removes it; this port's
    // `commit_staged_changes` does, and `--skip` removes it too, so the id is
    // captured here and recorded once the commit that replaces it exists.
    let stopped = read_stopped_sha(repo);
    let next_is_fixup = peek_command(&list, 0).is_some_and(|c| c.is_fixup());

    if skip {
        // Discard the stopped instruction's half-applied work: restore the
        // worktree and index to the tip the rebase had reached.
        let old = repo.index_or_load_from_head()?.into_owned();
        let tip = repo.head_id()?.detach();
        let tip_tree = repo.find_commit(tip)?.tree_id()?.detach();
        restore_worktree_to_tree(repo, &old, tip_tree, &seq.should_interrupt)?;
        // A skipped fixup/squash leaves the chain's message state stale; git
        // rebuilds it in `commit_staged_changes()`. Dropping the whole chain is
        // the same thing whenever the skipped instruction was the only member.
        let _ = std::fs::remove_file(dir.join("stopped-sha"));
        // The per-instruction reset the pick loop runs before every instruction
        // (sequencer.c:4624-4629) — the merge state the stop recorded belongs to
        // the instruction being thrown away. `REBASE_HEAD` is *not* dropped here:
        // stock leaves it naming the skipped commit, and this measured the same.
        let _ = std::fs::remove_file(repo.git_dir().join("MERGE_MSG"));
        let _ = std::fs::remove_file(repo.git_dir().join("AUTO_MERGE"));
    } else if let Some(code) = seq.commit_staged_changes()? {
        return Ok(code);
    }

    if let Some(oid) = stopped {
        rewritten::record(repo, &dir, oid, next_is_fixup);
    }

    seq.refresh_index()?;
    seq.run(list, 0)
}

/// `commit_staged_changes()`'s half that matters here: turn what the user
/// staged into the commit the stopped instruction was going to make.
///
/// Returns `Some(exit_code)` when the rebase must stop again (unstaged changes,
/// or an unresolved index), and `None` when it may proceed.
impl Sequencer<'_> {
    fn commit_staged_changes(&mut self) -> Result<Option<ExitCode>> {
        let repo = self.repo;
        let dir = rebase_merge_dir(repo);
        let index = repo.index_or_load_from_head()?.into_owned();
        if index.entries().iter().any(|e| e.stage_raw() != 0) {
            eprintln!("error: you must edit all merge conflicts and then");
            eprintln!("mark them as resolved using git add");
            return Ok(Some(ExitCode::from(1)));
        }
        let (unstaged, _staged, _conflicts) = dirty_state(repo)?;
        if unstaged {
            eprintln!("error: cannot rebase: You have unstaged changes.");
            eprintln!("error: Please commit or stash them.");
            return Ok(Some(ExitCode::from(1)));
        }

        let tree = tree_from_index(repo, &index)?;
        let head = repo.head_id()?.detach();
        let head_commit = repo.find_commit(head)?;
        // `is_clean`: the index already matches HEAD, so the instruction that
        // stopped left nothing to commit (an `edit`/`break` stop the user did
        // not amend, or a conflict resolved back to the tip).
        let is_clean = head_commit.tree_id()?.detach() == tree;
        let amend = dir.join("amend").exists();

        let message = std::fs::read(dir.join("message")).ok();
        // `if (is_clean) { … if (!final_fixup) { ret = 0; goto out; } }`: an
        // index that already matches `HEAD` has nothing to commit, whatever the
        // `amend` marker says — an `edit` stop the user amended (or did not
        // touch at all) lands here and just carries on.
        //
        // git's one exception is `final_fixup`, which it reaches only after
        // *skipping* the last member of a fixup chain; that re-clean-up is not
        // modelled here, so a `--skip` of a final `squash` leaves the melded
        // message with its comment block rather than re-running the editor.
        if is_clean {
            let _ = std::fs::remove_file(dir.join("message"));
            let _ = std::fs::remove_file(dir.join("stopped-sha"));
            let _ = std::fs::remove_file(dir.join("amend"));
            return Ok(None);
        }

        // `run_git_commit(rebase_path_message(), …, ALLOW_EMPTY | EDIT_MSG
        // [| AMEND_MSG])`: git re-enters `git commit` here, which is what makes
        // `--continue` open the message in the editor.
        let mut args: Vec<String> = vec!["-n".into(), "--no-gpg-sign".into()];
        if amend {
            args.push("--amend".into());
        }
        let msg_path = dir.join("message");
        if message.is_some() {
            args.push("-F".into());
            args.push(msg_path.display().to_string());
        }
        args.push("-e".into());
        args.push("--allow-empty".into());
        // The author of the commit being replayed, saved by `write_author_script`
        // when the instruction started, survives the interruption.
        let mut env = self.author_env();
        // `const char *reflog_action = reflog_message(opts, "continue", NULL);`
        // (sequencer.c:5267), handed to `run_git_commit(…, reflog_action, …)`
        // (sequencer.c:5429-5430) which exports it as `GIT_REFLOG_ACTION`
        // (sequencer.c:1141). `reflog_message()` composes
        // `<sequencer_reflog_action()> (continue)` (sequencer.c:2243-2261), so the
        // resumed commit is logged `rebase (continue): <subject>` — and
        // `zzz (continue): …` when the caller named the action.
        env.push((
            "GIT_REFLOG_ACTION".to_string(),
            format!("{} (continue)", reflog_action()),
        ));
        let code = self.run_commit(&args, env)?;
        if code != 0 {
            return Ok(Some(ExitCode::from(code as u8)));
        }
        let _ = std::fs::remove_file(dir.join("message"));
        let _ = std::fs::remove_file(dir.join("amend"));
        let _ = std::fs::remove_file(dir.join("stopped-sha"));
        if self.fixup_count > 0 {
            let _ = std::fs::remove_file(dir.join("message-fixup"));
            let _ = std::fs::remove_file(dir.join("message-squash"));
            let _ = std::fs::remove_file(dir.join("current-fixups"));
            self.fixups.clear();
            self.fixup_count = 0;
        }
        Ok(None)
    }
}

/// The instruction executor — git's `pick_commits()` and the `do_*()` helpers
/// it dispatches to.
///
/// One instance owns everything an instruction needs: the repository, the
/// resumable state, the growing index, and the fixup-chain bookkeeping
/// (`replay_ctx`'s `current_fixups` / `current_fixup_count`).
struct Sequencer<'r> {
    repo: &'r gix::Repository,
    st: RebaseState,
    committer: gix::actor::Signature,
    /// `ident_default_date()`, cached once for the run exactly as git caches it
    /// per process, so every commit `--ignore-date` restamps gets one value.
    /// It is *not* the committer's time: `fmt_ident(…, NULL, …)` reads the wall
    /// clock and ignores `GIT_COMMITTER_DATE`, which is why `--ignore-date` moves
    /// the author date even when the committer date is pinned by the environment.
    now: gix::date::Time,
    should_interrupt: AtomicBool,
    /// The autostash to re-apply when the rebase concludes, if any. Carried in
    /// `$state_dir/autostash` across interruptions.
    autostash: Option<ObjectId>,
    /// The index the next three-way merge starts from — the tip's tree plus any
    /// stat data carried over, so a completed rebase leaves a cheap `status`.
    index: gix::index::File,
    /// `todo_list->done_nr` / `total_nr`, the two numbers in `Rebasing (n/m)`.
    done_nr: usize,
    total_nr: usize,
    /// `ctx->current_fixup_count` — how many messages the running squash chain
    /// has already melded.
    fixup_count: usize,
    /// `ctx->current_fixups` — the chain itself, one `<command> <oid>` per line,
    /// mirrored into `$state_dir/current-fixups`.
    fixups: Vec<String>,
    /// `opts->xopts` after `parse_merge_opt()` has read every one of them —
    /// what `do_pick_commit()`'s `do_recursive_merge()` and `do_merge()` hand to
    /// merge-ort. Parsed once per run, because git parses once per
    /// `try_merge_strategy()` and every pick in a run shares the options.
    xopts: crate::merge_apply::StrategyOptions,
}

/// What one instruction did.
enum Step {
    /// Move on to the next instruction.
    Next,
    /// Stop, leaving the rebase resumable, with this exit code.
    Stop(u8),
}

impl<'r> Sequencer<'r> {
    fn new(repo: &'r gix::Repository, st: RebaseState) -> Result<Self> {
        let committer = repo
            .committer()
            .ok_or_else(|| anyhow!("committer identity is not configured"))??
            .to_owned()?;
        let index = repo.index_or_load_from_head()?.into_owned();
        // `try_merge_strategy()` (builtin/merge.c:815-822) seeds `-s subtree`
        // with an empty `subtree_shift` *before* running the `-X` loop, so an
        // explicit `-Xsubtree=<prefix>` still wins. Anything else `-s` names is
        // merge-ort under a different label as far as these options go.
        let seed = crate::merge_apply::StrategyOptions {
            subtree_shift: (st.strategy.as_deref() == Some("subtree"))
                .then(gix::bstr::BString::default),
            ..Default::default()
        };
        let xopts = crate::merge_apply::StrategyOptions::parse_from(seed, &st.strategy_opts)
            .map_err(|e| anyhow!("{e}"))?;
        Ok(Sequencer {
            repo,
            st,
            committer,
            now: gix::date::Time::now_local_or_utc(),
            should_interrupt: AtomicBool::new(false),
            autostash: read_autostash(repo),
            index,
            done_nr: 0,
            total_nr: 0,
            fixup_count: 0,
            fixups: Vec::new(),
            xopts,
        })
    }

    fn dir(&self) -> std::path::PathBuf {
        rebase_merge_dir(self.repo)
    }

    /// Re-derive the working index from `HEAD` — used after anything that moved
    /// `HEAD` or the worktree outside the merge machinery (`exec`, `--continue`).
    fn refresh_index(&mut self) -> Result<()> {
        self.index = self.repo.index_or_load_from_head()?.into_owned();
        Ok(())
    }

    /// Read `$state_dir/current-fixups` back into `ctx->current_fixups`.
    fn load_fixup_state(&mut self) -> Result<()> {
        let raw = std::fs::read_to_string(self.dir().join("current-fixups")).unwrap_or_default();
        self.fixups = raw.lines().map(str::to_string).collect();
        self.fixup_count = self.fixups.len();
        Ok(())
    }

    /// `read_populate_todo()`'s progress accounting: `done_nr` is how many
    /// instructions the `done` file records, `total_nr` that plus what is left.
    fn count_progress(&mut self, list: &todo::List) -> Result<()> {
        let done = std::fs::read(self.dir().join("done")).unwrap_or_default();
        let (done_list, _) = todo::List::parse(self.repo, &done, true);
        self.done_nr = done_list.count_commands();
        self.total_nr = self.done_nr + list.count_commands();
        std::fs::write(self.dir().join("end"), format!("{}\n", self.total_nr))?;
        Ok(())
    }

    /// `save_todo()`: `git-rebase-todo` keeps everything *after* the instruction
    /// about to run, which is appended to `done` instead. That split is what
    /// makes a stop resumable — `--continue` concludes the instruction in `done`
    /// and then runs the file from the top.
    fn save_todo(&self, list: &todo::List, next: usize) -> Result<()> {
        let dir = self.dir();
        let rest = todo::List { items: list.items[next.min(list.items.len())..].to_vec() };
        std::fs::write(dir.join("git-rebase-todo"), rest.to_bytes(self.repo, None, 0))?;
        if next > 0 {
            let one = todo::List { items: vec![list.items[next - 1].clone()] };
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("done"))?;
            std::io::Write::write_all(&mut f, &one.to_bytes(self.repo, None, 0))?;
        }
        Ok(())
    }

    /// `pick_commits()` — run the instruction stream from `start`.
    fn run(&mut self, list: todo::List, start: usize) -> Result<ExitCode> {
        self.count_progress(&list)?;
        let dir = self.dir();
        for f in ["message", "stopped-sha", "amend", "patch"] {
            let _ = std::fs::remove_file(dir.join(f));
        }

        let mut i = start;
        while i < list.items.len() {
            let item = list.items[i].clone();
            self.save_todo(&list, i + 1)?;
            // `pick_commits()` clears the previous instruction's per-step records
            // before running this one (sequencer.c:5043-5048):
            //
            // ```c
            // unlink(rebase_path_author_script());
            // unlink(git_path_merge_head(r));
            // refs_delete_ref(…, "AUTO_MERGE", NULL, REF_NO_DEREF);
            // refs_delete_ref(…, "REBASE_HEAD", NULL, REF_NO_DEREF);
            // ```
            //
            // They run for every instruction including comments, and *before* the
            // instruction executes — so a pick's `AUTO_MERGE` survives only until
            // the next instruction starts. That is why `git rebase --exec` leaves
            // none behind while a plain rebase does: the trailing `exec` clears
            // the last pick's record and writes no merge of its own.
            let _ = std::fs::remove_file(dir.join("author-script"));
            let git_dir = self.repo.git_dir();
            for name in ["MERGE_HEAD", "AUTO_MERGE", "REBASE_HEAD"] {
                let _ = std::fs::remove_file(git_dir.join(name));
            }

            if item.cmd != todo::Cmd::Comment {
                self.done_nr += 1;
                std::fs::write(dir.join("msgnum"), format!("{}\n", self.done_nr))?;
                if !self.st.quiet {
                    eprint!(
                        "Rebasing ({}/{}){}",
                        self.done_nr,
                        self.total_nr,
                        if self.st.verbose { "\n" } else { "\r" }
                    );
                }
            }

            let step = match item.cmd {
                todo::Cmd::Break => {
                    self.term_clear_line();
                    self.stopped_at_head()?;
                    return Ok(ExitCode::SUCCESS);
                }
                // `item->command <= TODO_SQUASH`, minus `revert`, which only
                // `git revert` ever writes into an instruction sheet.
                todo::Cmd::Pick
                | todo::Cmd::Edit
                | todo::Cmd::Reword
                | todo::Cmd::Fixup
                | todo::Cmd::Squash => {
                    // `is_final_fixup()` (sequencer.c:4632-4645): the last member
                    // of a fixup/squash chain is the one that cleans the combined
                    // message up. Both it and `peek_command()` look past `noop`,
                    // `drop` and comment lines, so a comment between a `fixup` and
                    // the next `pick` does not end the chain.
                    let next_is_fixup =
                        peek_command(&list, i + 1).is_some_and(|c| c.is_fixup());
                    let final_fixup = item.cmd.is_fixup() && !next_is_fixup;
                    let step = self.pick_one_commit(&item, final_fixup)?;
                    // ```c
                    // if (is_rebase_i(opts) && !res)
                    //         record_in_rewritten(&item->commit->object.oid,
                    //                             peek_command(todo_list, 1));
                    // ```
                    // (sequencer.c:4968-4970). `edit` returns at 4965 before this,
                    // so its commit is recorded by `--continue` from
                    // `stopped-sha` instead; every other command records only when
                    // the pick succeeded, which is what `Step::Next` means here.
                    if item.cmd != todo::Cmd::Edit && matches!(step, Step::Next) {
                        if let Some(oid) = item.commit {
                            rewritten::record(self.repo, &dir, oid, next_is_fixup);
                        }
                    }
                    step
                }
                todo::Cmd::Exec => self.do_exec(&item)?,
                todo::Cmd::Noop | todo::Cmd::Drop | todo::Cmd::Comment => Step::Next,
                todo::Cmd::Label => self.do_label(&item)?,
                todo::Cmd::Reset => self.do_reset(&item)?,
                todo::Cmd::Merge => {
                    let step = self.do_merge(&item)?;
                    // ```c
                    // else if (item->commit)
                    //         record_in_rewritten(&item->commit->object.oid,
                    //                             peek_command(todo_list, 1));
                    // ```
                    // (sequencer.c:5088-5090): a recreated merge maps its original
                    // onto the new one, exactly as a pick does.
                    if matches!(step, Step::Next) {
                        if let Some(oid) = item.commit {
                            let next_is_fixup =
                                peek_command(&list, i + 1).is_some_and(|c| c.is_fixup());
                            rewritten::record(self.repo, &dir, oid, next_is_fixup);
                        }
                    }
                    step
                }
                // `do_update_ref()` (sequencer.c:5096-5101): nothing moves yet,
                // the record just learns where `HEAD` reached. The refs are
                // written in one pass at the very end.
                todo::Cmd::UpdateRef => {
                    do_update_ref(self.repo, item.arg.to_str_lossy().trim())?;
                    Step::Next
                }
                todo::Cmd::Invalid => {
                    self.term_clear_line();
                    eprintln!("error: please fix this using 'git rebase --edit-todo'.");
                    return Ok(ExitCode::from(1));
                }
                todo::Cmd::Revert => {
                    self.term_clear_line();
                    anyhow::bail!("unsupported todo command \"revert\" (only `git revert` produces it)")
                }
            };
            match step {
                Step::Next => i += 1,
                Step::Stop(code) => {
                    return Ok(if code == 0 {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(code)
                    })
                }
            }
        }
        self.finish()
    }

    /// `term_clear_line()` (pager.c): wipe the `Rebasing (n/m)\r` progress line
    /// before printing anything that must stay on screen.
    ///
    /// A redirected stderr never carried the progress line's `\r` anywhere
    /// visible, so git returns immediately rather than injecting an escape
    /// sequence into a log; that early return is why a scripted rebase's stderr
    /// is free of control bytes. A dumb terminal, which has no erase sequence,
    /// gets a line's worth of spaces instead.
    fn term_clear_line(&self) {
        if self.st.quiet || self.st.verbose {
            return;
        }
        if !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            return;
        }
        if std::env::var("TERM").map(|t| t == "dumb").unwrap_or(true) {
            eprint!("\r{:80}\r", "");
        } else {
            eprint!("\r\x1b[K");
        }
    }

    /// `stopped_at_head()` — what `break` reports.
    ///
    /// The name printed is `get_message()`'s `label`: `<abbrev> (<subject>)`,
    /// not the subject alone.
    fn stopped_at_head(&self) -> Result<()> {
        let head = self.repo.head_id()?.detach();
        let commit = self.repo.find_commit(head)?;
        let subject = first_line(commit.message_raw()?);
        eprintln!(
            "Stopped at {} ({})",
            todo::short_name(self.repo, head),
            subject.to_str_lossy()
        );
        Ok(())
    }

    /// `do_exec()` — run the rest of the line through the shell.
    fn do_exec(&mut self, item: &todo::Item) -> Result<Step> {
        let command = item.arg.to_str_lossy().into_owned();
        self.term_clear_line();
        if !self.st.quiet {
            eprintln!("Executing: {command}");
        }
        // `cmd.use_shell = 1; strvec_push(&cmd.args, command_line);` — a
        // one-element argv, so `prepare_shell_cmd` takes the no-`"$@"` branch.
        let status =
            crate::external::prepare_shell_cmd_str(&command, crate::external::NO_ARGS)
                .env_remove("GIT_CHERRY_PICK_HELP")
                .status()
                .map_err(|e| anyhow!("cannot run '{command}': {e}"))?;
        self.refresh_index()?;
        let (unstaged, staged, _) = dirty_state(self.repo)?;
        let dirty = unstaged || staged;
        let mut failed = !status.success();
        if failed {
            eprintln!(
                "warning: execution failed: {command}\n{}You can fix the problem, and then run\n\n  git rebase --continue\n\n",
                if dirty {
                    "and made changes to the index and/or the working tree.\n"
                } else {
                    ""
                }
            );
        } else if dirty {
            eprintln!(
                "warning: execution succeeded: {command}\nbut left changes to the index and/or the working tree.\nCommit or stash your changes, and then run\n\n  git rebase --continue\n\n"
            );
            failed = true;
        }
        if !failed {
            return Ok(Step::Next);
        }
        if self.st.reschedule_failed_exec {
            // Put the instruction back so `--continue` retries it, then say so.
            // `advise(rescheduled_advice, …)` quotes the *whole* todo line, not
            // just the command, which is what the user would have to edit.
            self.reschedule(item)?;
            let line = todo::List { items: vec![item.clone()] }.to_bytes(self.repo, None, 0);
            crate::advice::print_hint(&format!(
                "Could not execute the todo command\n\n    {}\n\n\
                 It has been rescheduled; To edit the command before continuing, please\n\
                 edit the todo list first:\n\n    git rebase --edit-todo\n    git rebase --continue",
                line.as_bstr().trim_end().to_str_lossy(),
            ));
        }
        Ok(Step::Stop(1))
    }

    /// Put `item` back at the head of `git-rebase-todo` — git's `reschedule`
    /// path, which re-runs `save_todo()` with the current index rather than the
    /// next one.
    fn reschedule(&self, item: &todo::Item) -> Result<()> {
        let path = self.dir().join("git-rebase-todo");
        let rest = std::fs::read(&path).unwrap_or_default();
        let mut out = todo::List { items: vec![item.clone()] }.to_bytes(self.repo, None, 0);
        out.extend_from_slice(&rest);
        std::fs::write(path, out)?;
        Ok(())
    }

    /// `pick_one_commit()` → `do_pick_commit()`: `pick`, `reword`, `edit`,
    /// `squash` and `fixup`, which differ only in what happens once the picked
    /// tree is in place.
    fn pick_one_commit(&mut self, item: &todo::Item, final_fixup: bool) -> Result<Step> {
        let repo = self.repo;
        let oid = item
            .commit
            .ok_or_else(|| anyhow!("{} without a commit", item.cmd.name()))?;
        let commit = repo.find_commit(oid)?;
        let message: BString = commit.message_raw()?.to_owned();
        let subject = first_line(message.as_bstr());
        let short = todo::short_name(repo, oid);
        let head = repo.head_id()?.detach();

        // `do_pick_commit()` (sequencer.c:2433-2437) rewrites `ctx->message`
        // *before* `do_recursive_merge()`, which is what makes the buffer written
        // to `$state_dir/message` and `MERGE_MSG` on a conflict stop already
        // carry the sign-off and the trailers — so `--continue` commits them.
        // Applying them only on the success path (as this did) silently dropped
        // both from every commit whose pick had to be resolved by hand.
        //
        // Both guards are `!is_fixup(command)`: a `fixup`/`squash` contributes to
        // a message that `update_squash_messages()` owns, and gets its trailers
        // there instead.
        let final_message = if item.cmd.is_fixup() {
            message.clone()
        } else {
            apply_trailers(
                sign_off(&self.committer, self.st.signoff, &message)?,
                &self.st.trailers,
            )?
        };

        // `CREATE_ROOT_COMMIT`: while the tip is still the stand-in `<onto>` that
        // `--root` without `--onto` minted, a pick becomes a new *root* commit
        // rather than that stand-in's child.
        let create_root = item.cmd.is_pick_or_similar() && Some(head) == self.st.squash_onto;
        if create_root && item.cmd.is_fixup() {
            crate::git_fatal!("cannot fixup root commit");
        }

        let parent = commit.parent_ids().next().map(|p| p.detach());
        let base_tree = match parent {
            Some(p) => repo.find_commit(p)?.tree_id()?.detach(),
            None => ObjectId::empty_tree(repo.object_hash()),
        };
        let head_tree = repo.find_commit(head)?.tree_id()?.detach();
        let ctree = commit.tree_id()?.detach();

        // `do_pick_commit()`'s fast-forward arm: with `opts->allow_ff` (no
        // `-f`/`--no-ff`), a non-fixup pick that would land on the very parent it
        // already has is not re-committed — `HEAD` moves straight to the existing
        // commit and the reflog records `rebase: fast-forward`, so the commit id
        // survives the rebase.
        //
        // The three-way merge still runs first: it is what keeps the worktree and
        // index in step, and with `base == ours` the merged tree is exactly the
        // picked commit's tree, so it changes no content.
        let other_label = format!("{short} ({})", subject.to_str_lossy());
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(b"HEAD")),
            current: Some(BStr::new(b"HEAD")),
            other: Some(BStr::new(other_label.as_bytes())),
        };
        // `show_output = !is_rebase_i(opts) || !result.clean` (sequencer.c:783):
        // a rebase shows merge-ort's `Auto-merging`/`CONFLICT` block **only when
        // the merge came out unclean**. Printing it unconditionally made
        // `git rebase -Xours`/`-Xtheirs` — where the option resolves every hunk —
        // announce a merge stock replays silently. The decision needs the result,
        // so the lines come back with it and are printed here.
        let applied = crate::merge_apply::three_way_merge_with_options(
            repo,
            base_tree,
            head_tree,
            ctree,
            &self.index,
            labels,
            &self.should_interrupt,
            false,
            &self.xopts,
        )?;
        if !applied.conflicts.is_empty() {
            for line in &applied.messages {
                println!("{line}");
            }
        }
        self.index = applied.index;
        self.index.write(crate::config::index_write_options(repo))?;

        if !applied.conflicts.is_empty() {
            // `merge_switch_to_result()` records the merged tree as `AUTO_MERGE`
            // whether or not the merge came out clean (merge-ort.c:4702-4707) —
            // a conflicted pick is precisely when `git diff AUTO_MERGE` is worth
            // having, and `sequencer_post_commit_cleanup()` is what removes it
            // again once the stop is over.
            crate::merge_apply::write_auto_merge(repo, applied.tree_id)?;
            // `if (is_rebase_i(opts) && write_author_script(msg.message) < 0)`
            // (sequencer.c:2298) runs *before* the merge, so the picked commit's
            // author survives a conflict: both a `git commit` made during the
            // stop and `--continue` read it back through `read_env_script()`.
            // Writing it only on the paths that reach the commit left a stop with
            // no record of the author it was replaying.
            write_author_script(repo, &commit)?;
            return self.stop_for_conflict(
                item,
                oid,
                &short,
                &subject,
                &final_message,
                Some(&applied.conflicts),
            );
        }

        let fast_forward = self.st.allow_ff
            && !item.cmd.is_fixup()
            && match parent {
                Some(p) => p == head,
                None => create_root,
            };
        // `merge_switch_to_result()`'s `write_auto_merge` region — but only for
        // a pick that git really merged. `do_pick_commit()` tests the
        // fast-forward arm *before* `do_recursive_merge()` and `goto leave`s
        // through `fast_forward_to()`, so no merge-ort runs and no `AUTO_MERGE`
        // is written; this port runs the merge either way (it is what keeps the
        // worktree and index in step) and so has to withhold the record here
        // instead. Observable: `git rebase --onto main HEAD~1` leaves none while
        // `git rebase -f HEAD~1`, whose `-f` clears `allow_ff`, leaves the last
        // pick's tree.
        if !fast_forward {
            crate::merge_apply::write_auto_merge(repo, applied.tree_id)?;
        }
        if fast_forward {
            write_author_script(repo, &commit)?;
            set_head(repo, Target::Object(oid), "rebase: fast-forward")?;
            if item.cmd == todo::Cmd::Reword {
                return self.reword();
            }
            if item.cmd == todo::Cmd::Edit {
                return self.stop_for_edit(oid, &short, item);
            }
            return Ok(Step::Next);
        }

        // `allow_empty()`: a pick that leaves the index unchanged is either a
        // commit that was *already* empty — which `opts->allow_empty` (always on
        // for rebase) keeps, so `--keep-empty` round-trips an empty commit — or
        // a commit whose patch turned out to be already present.
        //
        // The second case is git's `drop_redundant_commits`, which fires in the
        // pick loop for exactly this reason — the pick produced nothing because
        // the patch was already there — and names the commit in full.
        //
        // (git's *other* drop, `sequencer_make_script()`'s `cherry_mark`, removes
        // such a commit from the sheet before the run and says
        // `warning: skipped previously applied commit <abbrev>` instead. That one
        // needs a patch id per commit, which nothing vendored computes; the
        // observable difference is only that the sheet here still counts the
        // commit, so the progress line reads one step longer.)
        //
        // `allow_empty()` returns 1 for an originally-empty commit (rebase always
        // sets `opts->allow_empty`, so `--keep-empty` round-trips one), and for a
        // *became*-empty commit it is `--empty` that decides:
        //
        // ```c
        // else if (opts->keep_redundant_commits) return 1;   /* --empty=keep */
        // else if (opts->drop_redundant_commits) return 2;   /* --empty=drop */
        // else                                   return 0;   /* --empty=stop */
        // ```
        //
        // (sequencer.c:1810-1817). Dropping unconditionally — as this did — lost
        // the commit under `--empty=keep` and never halted under `--empty=stop`,
        // both at exit 0.
        //
        // A `fixup`/`squash` is excluded here: it amends the commit already at
        // `HEAD` rather than adding one, so an unchanged tree is the chain doing
        // its job, not a redundant pick. git reaches `allow_empty()` for those
        // too, but through `try_to_commit()`'s `AMEND_MSG` arm, which compares
        // against `HEAD`'s *parent* instead.
        if !item.cmd.is_fixup() && applied.tree_id == head_tree {
            let originally_empty = todo::is_original_commit_empty(repo, &commit)?;
            if !originally_empty {
                match self.st.empty {
                    // `allow_empty() == 1`: `flags |= ALLOW_EMPTY`, the commit is
                    // written even though it changes nothing. Falls through.
                    Empty::Keep => {}
                    Empty::Drop => {
                        self.term_clear_line();
                        let subject = commit
                            .message()
                            .map(|m| m.summary().to_string())
                            .unwrap_or_default();
                        // `allow_empty() == 2` (sequencer.c:2500-2511) tears the
                        // pick's state back down before it reports the drop:
                        // `CHERRY_PICK_HEAD`, `MERGE_MSG` and `AUTO_MERGE` all go,
                        // so a dropped pick leaves no trace of the merge that
                        // produced it.
                        let git_dir = repo.git_dir();
                        let _ = std::fs::remove_file(git_dir.join("CHERRY_PICK_HEAD"));
                        let _ = std::fs::remove_file(git_dir.join("MERGE_MSG"));
                        let _ = std::fs::remove_file(git_dir.join("AUTO_MERGE"));
                        eprintln!(
                            "dropping {} {subject} -- patch contents already upstream",
                            oid.to_hex()
                        );
                        return Ok(Step::Next);
                    }
                    // `allow_empty() == 0`: `do_commit()` runs without
                    // `ALLOW_EMPTY`, `try_to_commit()` returns 1, and the pick is
                    // handed to a real `git commit` that refuses it.
                    Empty::Stop => {
                        return self.stop_for_empty(item, &commit, &short, &final_message);
                    }
                }
            }
        }

        write_author_script(repo, &commit)?;

        if item.cmd.is_fixup() {
            return self.commit_fixup(item, &commit, applied.tree_id, final_fixup);
        }

        let mut author = commit.author()?.to_owned()?;
        let mut committer = self.committer.clone();
        apply_date_options(
            &mut author,
            &mut committer,
            self.now,
            self.st.ignore_date,
            self.st.committer_date_is_author_date,
        );
        let parents = if create_root {
            Default::default()
        } else {
            std::iter::once(head).collect()
        };
        // The sign-off and the `--trailer` arguments went on above, before the
        // merge, so that a conflict stop records them too. The reflog entry below
        // still names the original subject, which neither touches.
        let new = repo
            .write_object(&gix::objs::Commit {
                message: final_message,
                tree: applied.tree_id,
                author,
                committer,
                encoding: None,
                parents,
                extra_headers: Default::default(),
            })?
            .detach();
        set_head(
            repo,
            Target::Object(new),
            &gix::reference::log::message(
                &format!("{} ({})", reflog_action(), item.cmd.name()),
                message.as_bstr(),
                1,
            )
            .to_string(),
        )?;
        // A plain pick carries no `AMEND_MSG`, so only `post-commit` fires here;
        // a `reword` adds the `amend` half below, from the `git commit --amend`
        // it re-enters.
        run_commit_hooks(repo, None, new);

        match item.cmd {
            todo::Cmd::Reword => self.reword(),
            // `error_with_patch(r, commit, …)` (sequencer.c:4965) is handed
            // `item->commit` — the commit being *replayed*, not the one the pick
            // just wrote. `make_patch()` puts that id in `stopped-sha` and
            // `REBASE_HEAD`, which is what `--continue` records as the "old" half
            // of the `post-rewrite` pair and what `git show REBASE_HEAD` resolves.
            // Passing `new` here made both name the replacement, so the pair came
            // out as `<new> <new>` and `REBASE_HEAD` pointed at the wrong commit.
            todo::Cmd::Edit => self.stop_for_edit(oid, &short, item),
            _ => Ok(Step::Next),
        }
    }

    /// `sequencer_remove_state()`: drop every `refs/rewritten/<label>` the run
    /// created, listed in `$state_dir/refs-to-delete`. Best effort, as git's is —
    /// a ref that will not delete only earns a warning.
    fn delete_rewritten_refs(&self) {
        let Ok(body) = std::fs::read_to_string(self.dir().join("refs-to-delete")) else {
            return;
        };
        for name in body.lines().filter(|l| !l.is_empty()) {
            let Ok(full) = gix::refs::FullName::try_from(name) else {
                eprintln!("warning: could not delete '{name}'");
                continue;
            };
            let deleted = self.repo.edit_reference(RefEdit {
                change: Change::Delete {
                    expected: PreviousValue::Any,
                    log: RefLog::AndReference,
                    message: Default::default(),
                },
                name: full,
                deref: false,
            });
            if deleted.is_err() {
                eprintln!("warning: could not delete '{name}'");
            }
        }
    }

    /// `do_label()`: point `refs/rewritten/<name>` at the current `HEAD` so a
    /// later `reset`/`merge` can name this position, and remember the ref so the
    /// end of the rebase can delete it again.
    fn do_label(&mut self, item: &todo::Item) -> Result<Step> {
        let name = item.arg.to_str_lossy().into_owned();
        if name == "#" {
            self.term_clear_line();
            crate::git_fatal!("illegal label name: '{name}'");
        }
        let full = format!("refs/rewritten/{name}");
        let head = self.repo.head_id()?.detach();
        self.repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("{} (label) '{name}'", reflog_action()).into(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(head),
            },
            name: full
                .as_str()
                .try_into()
                .map_err(|e| anyhow!("illegal label name: '{name}': {e}"))?,
            deref: false,
        })?;
        // `safe_append(rebase_path_refs_to_delete(), …)`.
        let path = self.dir().join("refs-to-delete");
        let mut body = std::fs::read_to_string(&path).unwrap_or_default();
        body.push_str(&full);
        body.push('\n');
        std::fs::write(path, body)?;
        Ok(Step::Next)
    }

    /// `do_reset()`: move `HEAD`, the index and the worktree to a label (or to a
    /// raw revision, which is what `make_script_with_merges()` writes for a base
    /// outside the replayed range).
    fn do_reset(&mut self, item: &todo::Item) -> Result<Step> {
        // git takes the label as the first whitespace-delimited word of the
        // argument, so the `# <oneline>` the sheet carries is ignored.
        let arg = item.arg.to_str_lossy().into_owned();
        let name = arg.split_whitespace().next().unwrap_or_default().to_string();
        if name == "[new" || arg.starts_with("[new root]") {
            self.term_clear_line();
            bail!(
                "`reset [new root]` is not ported (it needs the `--root` stand-in commit \
                 `do_reset()` mints); the rebase is still resumable with `git rebase --abort`"
            );
        }
        let oid = self.lookup_label(&name)?;
        let head = self.repo.head_id()?.detach();
        update_clean_worktree(self.repo, &self.index, oid, &self.should_interrupt)?;
        self.refresh_index()?;
        // `refs_update_ref()` on a value the ref already holds writes neither the
        // ref nor a reflog line, so a `reset` that does not move HEAD is silent.
        if oid != head {
            set_head(
                self.repo,
                Target::Object(oid),
                &format!("{} (reset): '{name}'", reflog_action()),
            )?;
        }
        Ok(Step::Next)
    }

    /// `lookup_label()`: `refs/rewritten/<label>` if it exists, else `<label>`
    /// read as an ordinary revision.
    fn lookup_label(&self, label: &str) -> Result<ObjectId> {
        let full = format!("refs/rewritten/{label}");
        if let Ok(Some(mut r)) = self.repo.try_find_reference(full.as_str()) {
            if let Ok(id) = r.peel_to_id() {
                return Ok(id.detach());
            }
        }
        self.repo
            .rev_parse_single(label)
            .map(|id| id.detach())
            .map_err(|_| anyhow!("could not resolve '{full}'"))
    }

    /// `do_merge()`: recreate a merge commit whose sides the sheet named by label.
    ///
    /// Covered: the fast-forward shortcut (`HEAD` and every merge head are still
    /// the original merge's parents, so its commit id survives the rebase) and a
    /// genuine two-parent `ort` merge, including a conflict stop.
    ///
    /// Not ported, and refused rather than approximated: octopus merges (git
    /// shells out to `git merge -s octopus`), an explicit `-s <strategy>`, the
    /// `merge -c` editor variant, `merge` with no `-C`/`-c` original, and
    /// merging onto the `--root` stand-in commit.
    fn do_merge(&mut self, item: &todo::Item) -> Result<Step> {
        let repo = self.repo;
        let Some(original) = item.commit else {
            self.term_clear_line();
            bail!(
                "`merge` without `-C <commit>` is not ported (git synthesizes a \
                 `Merge branch '…'` message for it); the rebase is still resumable with \
                 `git rebase --abort`"
            );
        };
        if item.flags & todo::EDIT_MERGE_MSG != 0 {
            self.term_clear_line();
            bail!("`merge -c` (reword the merge message) is not ported");
        }

        // The argument is the merge heads, optionally followed by `# <oneline>`.
        let arg = item.arg.to_str_lossy().into_owned();
        let heads: Vec<&str> = arg
            .split_whitespace()
            .take_while(|w| *w != "#")
            .collect();
        if heads.is_empty() {
            self.term_clear_line();
            crate::git_fatal!("nothing to merge: '{arg}'");
        }
        if heads.len() > 1 {
            self.term_clear_line();
            bail!(
                "octopus `merge` is not ported (git runs `git merge -s octopus` for more than \
                 one merge head); the rebase is still resumable with `git rebase --abort`"
            );
        }
        let merge_head = self.lookup_label(heads[0])?;
        let head = repo.head_id()?.detach();

        // `can_fast_forward`: HEAD is still the original merge's first parent and
        // the merge heads are still its remaining parents, so the original merge
        // commit already *is* the answer.
        let original_parents: Vec<ObjectId> = repo
            .find_commit(original)?
            .parent_ids()
            .map(|p| p.detach())
            .collect();
        let can_fast_forward = self.st.allow_ff
            && original_parents.first() == Some(&head)
            && original_parents.len() == 2
            && original_parents[1] == merge_head;
        if can_fast_forward {
            update_clean_worktree(repo, &self.index, original, &self.should_interrupt)?;
            self.refresh_index()?;
            set_head(repo, Target::Object(original), "rebase: fast-forward")?;
            return Ok(Step::Next);
        }

        // `merge_ort_recursive()` over the one merge base. Several bases would
        // need the virtual common ancestor git builds by merging them, which is
        // not reachable from here.
        let bases = repo.merge_bases_many(head, &[merge_head])?;
        if bases.len() > 1 {
            self.term_clear_line();
            bail!(
                "`merge` across {} merge bases is not ported (git folds them into one virtual \
                 ancestor); the rebase is still resumable with `git rebase --abort`",
                bases.len()
            );
        }
        let base_tree = match bases.first() {
            Some(id) => repo.find_object(id.detach())?.peel_to_tree()?.id,
            None => ObjectId::empty_tree(repo.object_hash()),
        };
        let head_tree = repo.find_commit(head)?.tree_id()?.detach();
        let other_tree = repo.find_commit(merge_head)?.tree_id()?.detach();
        let other_label = format!("refs/rewritten/{}", heads[0]);
        let applied = crate::merge_apply::three_way_merge_with_options(
            repo,
            base_tree,
            head_tree,
            other_tree,
            &self.index,
            gix::merge::blob::builtin_driver::text::Labels {
                ancestor: None,
                current: Some(BStr::new(b"HEAD")),
                other: Some(BStr::new(other_label.as_bytes())),
            },
            &self.should_interrupt,
            false,
            &self.xopts,
        )?;
        // `if (ret <= 0) fputs(o.obuf.buf, stdout)` (sequencer.c:4360-4361) —
        // `do_merge()` buffers merge-ort's output (`o.buffer_output = 2`) and
        // flushes it only when the merge did not come out clean.
        if !applied.conflicts.is_empty() {
            for line in &applied.messages {
                println!("{line}");
            }
        }
        self.index = applied.index;
        self.index.write(crate::config::index_write_options(repo))?;

        let commit = repo.find_commit(original)?;
        let message: BString = commit.message_raw()?.to_owned();
        if !applied.conflicts.is_empty() {
            // `MERGE_HEAD`/`MERGE_MODE` are what let `git commit` conclude the
            // stopped merge.
            std::fs::write(repo.git_dir().join("MERGE_HEAD"), format!("{merge_head}\n"))?;
            std::fs::write(repo.git_dir().join("MERGE_MODE"), "no-ff\n")?;
            std::fs::write(repo.git_dir().join("MERGE_MSG"), &message)?;
            let short = todo::short_name(repo, original);
            let subject = first_line(message.as_bstr());
            return self.stop_for_conflict(item, original, &short, &subject, &message, None);
        }

        write_author_script(repo, &commit)?;
        let mut author = commit.author()?.to_owned()?;
        let mut committer = self.committer.clone();
        apply_date_options(
            &mut author,
            &mut committer,
            self.now,
            self.st.ignore_date,
            self.st.committer_date_is_author_date,
        );
        let signed = sign_off(&self.committer, self.st.signoff, &message)?;
        let new = repo
            .write_object(&gix::objs::Commit {
                message: signed,
                tree: applied.tree_id,
                author,
                committer,
                encoding: None,
                parents: [head, merge_head].into_iter().collect(),
                extra_headers: Default::default(),
            })?
            .detach();
        set_head(
            repo,
            Target::Object(new),
            &gix::reference::log::message(
                &format!("{} (merge)", reflog_action()),
                message.as_bstr(),
                1,
            )
            .to_string(),
        )?;
        // `do_merge()` concludes through `run_git_commit()` (sequencer.c:4394),
        // i.e. a real `git commit`, which runs `post-commit`. It carries no
        // `--amend`, so there is no `post-rewrite amend` half.
        run_commit_hooks(repo, None, new);
        Ok(Step::Next)
    }

    /// `reword`: amend the commit just made, with the editor open on its
    /// message. git reaches this through `run_git_commit(NULL, …, EDIT_MSG |
    /// VERIFY_MSG | AMEND_MSG | ALLOW_EMPTY)`, i.e. a real `git commit --amend`.
    fn reword(&mut self) -> Result<Step> {
        self.term_clear_line();
        let args: Vec<String> = vec![
            "--amend".into(),
            "--no-gpg-sign".into(),
            "-e".into(),
            "--allow-empty".into(),
        ];
        let env = self.author_env();
        let code = self.run_commit(&args, env)?;
        self.refresh_index()?;
        if code != 0 {
            return Ok(Step::Stop(code as u8));
        }
        Ok(Step::Next)
    }

    /// `edit`: the pick landed, now hand control back to the user.
    ///
    /// `pick_one_commit()` prints `Stopped at <short>...  <subject>` and then
    /// `error_with_patch(…, exit_code = 0, to_amend = 1)`, which records the
    /// `amend` marker `--continue` needs and returns 0 — an `edit` stop is a
    /// success, not a failure.
    fn stop_for_edit(&mut self, at: ObjectId, short: &str, item: &todo::Item) -> Result<Step> {
        self.term_clear_line();
        eprintln!("Stopped at {short}...  {}", item.arg.to_str_lossy());
        let dir = self.dir();
        // `error_with_patch(…, to_amend = 1)` runs `make_patch()` first
        // (sequencer.c:3505-3507), so an `edit` stop records the same
        // `REBASE_HEAD`/`patch`/`message` a conflict stop does — that is what
        // makes `git rebase --show-current-patch` work at an `edit`.
        make_patch(self.repo, &dir, &self.repo.find_commit(at)?)?;
        std::fs::write(dir.join("amend"), format!("{}\n", self.repo.head_id()?.detach()))?;
        eprintln!(
            "You can amend the commit now, with\n\n  git commit --amend \n\n\
             Once you are satisfied with your changes, run\n\n  git rebase --continue"
        );
        Ok(Step::Stop(0))
    }

    /// `squash` / `fixup`: meld the picked tree into the commit already at
    /// `HEAD` by amending it, with the combined message
    /// [`update_squash_messages`](Self::update_squash_messages) built.
    fn commit_fixup(
        &mut self,
        item: &todo::Item,
        commit: &gix::Commit<'_>,
        tree: ObjectId,
        final_fixup: bool,
    ) -> Result<Step> {
        let repo = self.repo;
        let dir = self.dir();
        self.update_squash_messages(item, commit)?;

        let head = repo.head_id()?.detach();
        let head_commit = repo.find_commit(head)?;
        // `AMEND_MSG`: the new commit takes HEAD's parents and HEAD's author.
        let parents: Vec<ObjectId> = head_commit.parent_ids().map(|p| p.detach()).collect();
        let mut author = head_commit.author()?.to_owned()?;
        let mut committer = self.committer.clone();
        apply_date_options(
            &mut author,
            &mut committer,
            self.now,
            self.st.ignore_date,
            self.st.committer_date_is_author_date,
        );

        if !final_fixup {
            // Mid-chain: no editor, the message is the running combination.
            let message = std::fs::read(dir.join("message-squash"))?;
            let cleaned = super::stripspace::strip_space(&message, None);
            let reflog_msg = fixup_reflog_message(&cleaned);
            let new = repo
                .write_object(&gix::objs::Commit {
                    message: cleaned.into(),
                    tree,
                    author,
                    committer: committer.clone(),
                    encoding: None,
                    parents: parents.into_iter().collect(),
                    extra_headers: Default::default(),
                })?
                .detach();
            set_head(repo, Target::Object(new), &reflog_msg)?;
            // `flags |= AMEND_MSG` (sequencer.c:2414), so `try_to_commit()` runs
            // `commit_post_rewrite()` as well as `post-commit`: every member of a
            // fixup chain reports the commit it replaced.
            run_commit_hooks(repo, Some(head), new);
            return Ok(Step::Next);
        }

        // The last instruction of the chain. `do_pick_commit()` picks the
        // message file: a `fixup`-only chain keeps the first commit's message
        // (`message-fixup`) and never opens an editor; anything with a `squash`
        // in it copies the combined message to `.git/SQUASH_MSG` and sets
        // `EDIT_MSG`, so `git commit` opens it.
        let fixup_msg = dir.join("message-fixup");
        if fixup_msg.exists() {
            let message = std::fs::read(&fixup_msg)?;
            let cleaned = super::stripspace::strip_space(&message, None);
            let reflog_msg = fixup_reflog_message(&cleaned);
            let new = repo
                .write_object(&gix::objs::Commit {
                    message: cleaned.into(),
                    tree,
                    author,
                    committer: committer.clone(),
                    encoding: None,
                    parents: parents.into_iter().collect(),
                    extra_headers: Default::default(),
                })?
                .detach();
            set_head(repo, Target::Object(new), &reflog_msg)?;
            run_commit_hooks(repo, Some(head), new);
        } else {
            let squash_msg = repo.git_dir().join("SQUASH_MSG");
            std::fs::copy(dir.join("message-squash"), &squash_msg)?;
            let _ = std::fs::remove_file(repo.git_dir().join("MERGE_MSG"));
            self.term_clear_line();
            let args: Vec<String> = vec![
                "-n".into(),
                "--amend".into(),
                "--no-gpg-sign".into(),
                "-F".into(),
                squash_msg.display().to_string(),
                "-e".into(),
                "--allow-empty".into(),
            ];
            let env = self.author_env();
            let code = self.run_commit(&args, env)?;
            self.refresh_index()?;
            if code != 0 {
                return Ok(Step::Stop(code as u8));
            }
        }
        for f in ["message-fixup", "message-squash", "current-fixups"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        self.fixups.clear();
        self.fixup_count = 0;
        Ok(Step::Next)
    }

    /// `update_squash_messages()` — build `$state_dir/message-squash`, the
    /// running combination of every message the chain has melded so far.
    ///
    /// The first member of a chain seeds the file from `HEAD`'s message under a
    /// `# This is a combination of 2 commits.` header; each later member appends
    /// its own under `# This is the commit message #N:`. A `fixup` contributes
    /// its message *commented out* (`will be skipped`), which is what makes the
    /// combined message drop it once `git commit`'s cleanup strips comments.
    ///
    /// `fixup -C` (`amend!`) replaces rather than appends: it writes
    /// `message-fixup` so the chain's final commit takes this message alone.
    fn update_squash_messages(
        &mut self,
        item: &todo::Item,
        commit: &gix::Commit<'_>,
    ) -> Result<()> {
        let repo = self.repo;
        let dir = self.dir();
        let comment = todo::comment_prefix(repo);
        let is_squash = item.cmd == todo::Cmd::Squash;
        // `is_fixup_flag()`: a `fixup -C`/`-c` behaves like a squash as far as
        // the message is concerned — it contributes a real message, not a
        // commented-out one.
        let replaces = item.cmd == todo::Cmd::Fixup
            && item.flags & (todo::REPLACE_FIXUP_MSG | todo::EDIT_FIXUP_MSG) != 0;
        let body = commit.message_raw()?.to_owned();

        let mut buf: Vec<u8> = Vec::new();
        if self.fixup_count > 0 {
            let existing = std::fs::read(dir.join("message-squash"))
                .map_err(|e| anyhow!("could not read '{}': {e}", dir.join("message-squash").display()))?;
            // Replace the leading `# This is a combination of N commits.` header
            // with the new count.
            let rest = match existing.iter().position(|&b| b == b'\n') {
                Some(p) if existing.starts_with(comment.as_bytes()) => &existing[p..],
                _ => &existing[..],
            };
            buf.extend_from_slice(
                format!(
                    "{comment} This is a combination of {} commits.",
                    self.fixup_count + 2
                )
                .as_bytes(),
            );
            buf.extend_from_slice(rest);
        } else {
            let head = repo.head_id()?.detach();
            let head_message = repo.find_commit(head)?.message_raw()?.to_owned();
            if item.cmd == todo::Cmd::Fixup && item.flags == 0 {
                // A plain `fixup` keeps only the previous commit's message; git
                // stashes it now so the chain's end can use it without the
                // editor.
                std::fs::write(dir.join("message-fixup"), &head_message)?;
            }
            buf.extend_from_slice(
                format!("{comment} This is a combination of 2 commits.\n{comment} ").as_bytes(),
            );
            buf.extend_from_slice(if replaces {
                b"The 1st commit message will be skipped:".as_slice()
            } else {
                b"This is the 1st commit message:".as_slice()
            });
            buf.extend_from_slice(b"\n\n");
            if replaces {
                buf.extend_from_slice(&super::stripspace::comment_lines(
                    &head_message,
                    comment.as_bytes(),
                ));
            } else {
                buf.extend_from_slice(&head_message);
            }
        }

        if is_squash || replaces {
            // `append_squash_message()`.
            //
            // A melded commit whose own subject is `squash!`/`fixup!`/`amend!`
            // contributes that subject *commented out*: the marker named the
            // target and has no business surviving into the combined message.
            // Only the subject is commented; the body below it is kept.
            let commented_len = if body.starts_with(b"amend!")
                || ((is_squash || self.seen_squash())
                    && (body.starts_with(b"squash!") || body.starts_with(b"fixup!")))
            {
                todo::commit_subject_length(&body)
            } else {
                0
            };
            self.fixup_count += 1;
            buf.extend_from_slice(
                format!(
                    "\n{comment} This is the commit message #{}:\n\n",
                    self.fixup_count + 1
                )
                .as_bytes(),
            );
            buf.extend_from_slice(&super::stripspace::comment_lines(
                &body[..commented_len],
                comment.as_bytes(),
            ));
            // `fixup_off = buf->len;` — the offset `message-fixup` is cut from
            // below, taken before the body is appended so the trailers appended
            // to `buf` afterwards land inside that cut.
            let fixup_off = buf.len();
            let rest = &body[commented_len..];
            buf.extend_from_slice(rest);
            if replaces && !self.seen_squash() {
                // `append_squash_message()` (sequencer.c:2028-2039):
                //
                // ```c
                // /* fixup -C after squash behaves like squash */
                // if (is_fixup_flag(command, flag) && !seen_squash(ctx)) {
                //         /*
                //          * We're replacing the commit message so we need to
                //          * append any trailers if the user requested
                //          * '--signoff' or '--trailer'.
                //          */
                //         if (opts->signoff)
                //                 append_signoff(buf, 0, 0);
                //         if (opts->trailer_args.nr)
                //                 amend_strbuf_with_trailers(buf, &opts->trailer_args);
                // ```
                //
                // This is the one place a `fixup`/`squash` gets them: the guards
                // in `do_pick_commit()` skip every fixup, so a `fixup -C` — which
                // *replaces* the message rather than melding into one that
                // already carries them — would otherwise lose both.
                let signed = sign_off(&self.committer, self.st.signoff, &BString::from(buf))?;
                buf = apply_trailers(signed, &self.st.trailers)?.into();
                // `fixup -C` outside a squash chain replaces the message
                // outright; the chain's end takes this alone, minus the blank
                // lines the commented-out subject left behind.
                std::fs::write(
                    dir.join("message-fixup"),
                    todo::skip_blank_lines(&buf[fixup_off..]),
                )?;
            } else {
                let _ = std::fs::remove_file(dir.join("message-fixup"));
            }
        } else {
            self.fixup_count += 1;
            buf.extend_from_slice(
                format!(
                    "\n{comment} The commit message #{} will be skipped:\n\n",
                    self.fixup_count + 1
                )
                .as_bytes(),
            );
            buf.extend_from_slice(&super::stripspace::comment_lines(
                &body,
                comment.as_bytes(),
            ));
        }

        std::fs::write(dir.join("message-squash"), &buf)?;
        self.fixups.push(format!(
            "{} {}",
            item.cmd.name(),
            item.commit.expect("fixup names a commit")
        ));
        std::fs::write(dir.join("current-fixups"), self.fixups.join("\n"))?;
        Ok(())
    }

    /// `seen_squash()`: does the running chain contain a `squash`?
    fn seen_squash(&self) -> bool {
        self.fixups.iter().any(|l| l.starts_with("squash"))
    }

    /// `make_patch()` + `error_with_patch()`: record everything `--continue`
    /// and `--show-current-patch` need, then report the conflict.
    fn stop_for_conflict(
        &mut self,
        item: &todo::Item,
        oid: ObjectId,
        short: &str,
        subject: &BString,
        message: &BString,
        // The conflicted paths a `pick` stopped on, or `None` from the `merge`
        // command — `do_merge()` has its own `MERGE_MSG` and writes it before
        // getting here.
        conflicts: Option<&[BString]>,
    ) -> Result<Step> {
        let dir = self.dir();
        // The `merge` command's stop never reaches `make_patch()` below, so the
        // stopped commit is recorded here; a `pick`'s `make_patch()` rewrites the
        // same id.
        std::fs::write(dir.join("stopped-sha"), format!("{oid}\n"))?;
        // `do_recursive_merge()` appends `append_conflicts_hint()`'s block to the
        // message buffer, and `do_pick_commit()` then writes that buffer out with
        // `write_message(msgbuf.buf, msgbuf.len, git_path_merge_msg(r), 0)`
        // (sequencer.c:2309-2310). `MERGE_MSG` is what a bare `git commit` made
        // during the stop picks up, so without it the resolved pick loses the
        // message it was replaying.
        //
        // `append_conflicts_hint()` (sequencer.c:721-744) builds it with
        // `comment_line_str`, not a literal `#`:
        //
        // ```c
        // strbuf_addch(msgbuf, '\n');
        // strbuf_commented_addf(msgbuf, comment_line_str, "Conflicts:\n");
        // …
        //         strbuf_commented_addf(msgbuf, comment_line_str, "\t%s\n", ce->name);
        // ```
        //
        // `strbuf_commented_addf` puts a space after the prefix unless the line
        // starts with `\n` or `\t` (`add_lines()`, strbuf.c:374-391), which is
        // why the header is `<c> Conflicts:` while each path is `<c>\t<path>`.
        //
        // Hardcoding `#` here was committed-data corruption, not cosmetics: the
        // block is stripped from the final message by the comment cleanup, which
        // strips `core.commentChar` — so under any other setting the whole
        // `# Conflicts:` block survived `--continue` into the commit object.
        // Measured against stock under `core.commentChar = |`, `//`, `;;;` and
        // `core.commentString = ;`.
        let comment = todo::comment_prefix(self.repo);
        let hinted = conflicts.map(|paths| {
            let mut out = message.to_vec();
            out.push(b'\n');
            out.extend_from_slice(comment.as_bytes());
            out.extend_from_slice(b" Conflicts:\n");
            for path in paths {
                out.extend_from_slice(comment.as_bytes());
                out.push(b'\t');
                out.extend_from_slice(&path[..]);
                out.push(b'\n');
            }
            out
        });
        // The message `--continue` will commit. A conflicted fixup/squash
        // commits the running combination instead of the picked commit's own
        // message (`error_failed_squash()`), and that copy also replaces
        // `MERGE_MSG` (sequencer.c:3548-3555).
        if item.cmd.is_fixup() && dir.join("message-squash").exists() {
            std::fs::copy(dir.join("message-squash"), dir.join("message"))?;
            if conflicts.is_some() {
                std::fs::copy(dir.join("message"), self.repo.git_dir().join("MERGE_MSG"))?;
            }
        } else {
            match &hinted {
                Some(hinted) => {
                    std::fs::write(dir.join("message"), hinted)?;
                    std::fs::write(self.repo.git_dir().join("MERGE_MSG"), hinted)?;
                }
                None => std::fs::write(dir.join("message"), message)?,
            }
        }
        // `error_with_patch()` opens with `make_patch()` whenever it has a commit
        // (sequencer.c:3505-3507); the `message` above is already on disk, so the
        // `if (!file_exists(…))` arm there leaves it alone.
        if conflicts.is_some() {
            make_patch(self.repo, &dir, &self.repo.find_commit(oid)?)?;
        }
        if let Some(oid) = self.autostash {
            let _ = std::fs::write(dir.join("autostash"), format!("{oid}\n"));
        }
        self.term_clear_line();
        eprintln!("error: could not apply {short}... {}", subject.to_str_lossy());
        // `print_advice()`: the sequencer's `rebase_resolvemsg`, gated on
        // `advice.mergeConflict` and carrying the `Disable this message with …`
        // trailer while that key is unset.
        crate::advice::Advice::MergeConflict.advise_in(
            self.repo,
            concat!(
                "Resolve all conflicts manually, mark them as resolved with\n",
                "\"git add/rm <conflicted_files>\", then run \"git rebase --continue\".\n",
                "You can instead skip this commit: run \"git rebase --skip\".\n",
                "To abort and get back to the state before \"git rebase\", run \"git rebase --abort\".",
            ),
        );
        // `repo_rerere(r, opts->allow_rerere_auto)` — `do_pick_commit()` runs it
        // between `print_advice()` and the `error_with_patch()` line below, so a
        // conflict a previous run already resolved is replayed into the worktree
        // (and staged under `--rerere-autoupdate`/`rerere.autoupdate`) and a new
        // one has its preimage recorded. The index is on disk by now — the pick
        // wrote it before reaching here — and rerere reopens it, so a staged
        // replay lands in the file `--continue` will read.
        super::rerere::repo_rerere(self.repo, self.st.rerere_autoupdate)?;
        // `error_with_patch()` reports the *todo line's* argument, not the
        // commit's subject: with `rebase.instructionFormat` in play the two
        // differ, and this is the one that shows what the sheet said.
        eprintln!("Could not apply {short}... {}", item.arg.to_str_lossy());
        Ok(Step::Stop(1))
    }

    /// `--empty=stop` — `allow_empty()` returned 0, so the pick has to halt.
    ///
    /// git does not print this itself. `do_commit()` calls `try_to_commit()`
    /// without `ALLOW_EMPTY`, that returns 1 the moment the tree equals the first
    /// parent's (sequencer.c:1583-1604), and `do_commit()` then records
    /// `REBASE_HEAD` and re-enters a *real* `git commit` (sequencer.c:1750-1755)
    /// whose refusal — the `The previous cherry-pick is now empty…` advice plus a
    /// full `git status` — is what the user sees. `run_command_silent_on_success()`
    /// (sequencer.c:1093-1108) merges the child's stdout into its stderr and only
    /// forwards it when the child failed, which is why every one of those lines
    /// arrives on **stderr** even though `git status` writes to stdout. Then
    /// `error_with_patch()` adds `Could not apply <short>... <arg>`.
    ///
    /// So this spawns the same child rather than reimplementing `git commit`'s
    /// refusal, exactly as git does.
    fn stop_for_empty(
        &mut self,
        item: &todo::Item,
        commit: &gix::Commit<'_>,
        short: &str,
        message: &BString,
    ) -> Result<Step> {
        let repo = self.repo;
        let oid = commit.id().detach();
        let git_dir = repo.git_dir();
        // Every pick writes both before it commits (sequencer.c:2450-2479); a
        // pick that goes on to commit has them removed again, a halted one does
        // not. `MERGE_MSG` is also the `-F` file the child below is given.
        std::fs::write(git_dir.join("MERGE_MSG"), message)?;
        std::fs::write(git_dir.join("CHERRY_PICK_HEAD"), format!("{oid}\n"))?;
        write_author_script(repo, commit)?;
        // `write_rebase_head()` in `do_commit()`'s `res == 1` arm.
        std::fs::write(git_dir.join("REBASE_HEAD"), format!("{oid}\n"))?;
        self.term_clear_line();
        // `run_git_commit(msg_file, …, flags = 0)`: `-n` (no `VERIFY_MSG`),
        // `--no-gpg-sign`, `-F <MERGE_MSG>`, `--cleanup=verbatim` (no
        // `CLEANUP_MSG`, no `--signoff` on the replay opts) and
        // `--allow-empty-message` (no `EDIT_MSG`). No `--allow-empty`, which is
        // the whole point.
        let msg_path = git_dir.join("MERGE_MSG");
        let args: Vec<String> = vec![
            "-n".into(),
            "--no-gpg-sign".into(),
            "-F".into(),
            msg_path.display().to_string(),
            "--cleanup=verbatim".into(),
            "--allow-empty-message".into(),
        ];
        let out = self.run_commit_capture(&args, self.author_env())?;
        if !out.0 {
            let mut err = std::io::stderr();
            use std::io::Write;
            let _ = err.write_all(&out.1);
        }
        // `error_with_patch(r, commit, arg, arg_len, opts, res = 1, to_amend = 0)`.
        make_patch(repo, &self.dir(), commit)?;
        eprintln!("Could not apply {short}... {}", item.arg.to_str_lossy());
        Ok(Step::Stop(1))
    }

    /// The `GIT_AUTHOR_*` environment `read_env_script()` reconstructs from
    /// `$state_dir/author-script`, so a commit made by `git commit` on
    /// `--continue` keeps the replayed commit's author.
    fn author_env(&self) -> Vec<(String, String)> {
        let raw = match std::fs::read_to_string(self.dir().join("author-script")) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut env = Vec::new();
        for line in raw.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            if !key.starts_with("GIT_AUTHOR_") {
                continue;
            }
            // `parse_key_value_squoted()`: the value is sq-quoted.
            let value = value.trim_matches('\'').replace("'\\''", "'");
            env.push((key.to_string(), value));
        }
        env
    }

    /// [`run_commit`](Self::run_commit) with the child's output captured rather
    /// than inherited — `run_command_silent_on_success()` (sequencer.c:1093-1108),
    /// which folds stdout into stderr and forwards the buffer only on failure.
    ///
    /// Capturing means a real child process: the in-process entry point writes
    /// straight to this process's descriptors. git forks here too
    /// (`cmd.git_cmd = 1`), so the shape matches.
    ///
    /// Returns whether the child succeeded, and everything it wrote.
    fn run_commit_capture(
        &self,
        args: &[String],
        env: Vec<(String, String)>,
    ) -> Result<(bool, Vec<u8>)> {
        let exe = std::env::current_exe()
            .map_err(|e| anyhow!("cannot locate the running executable: {e}"))?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("commit").args(args);
        cmd.env("GIT_REFLOG_ACTION", reflog_action());
        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(w) = self.repo.workdir() {
            cmd.current_dir(w);
        }
        let out = cmd
            .output()
            .map_err(|e| anyhow!("failed to run git commit: {e}"))?;
        // **stderr before stdout**, which is what `cmd->stdout_to_stderr = 1`
        // actually produces here. Both descriptors are the one pipe, so the
        // bytes arrive in flush order, not in write order: the child's stdout is
        // a pipe and therefore fully buffered by C stdio, while its stderr is
        // unbuffered. `prepare_to_commit()` writes the status report to stdout
        // *first* and the `The previous cherry-pick is now empty…` advice to
        // stderr *second* (builtin/commit.c:1085-1097), yet stock's output has
        // the advice first — the status sits in the stdio buffer until the child
        // exits. Measured against stock; concatenating stdout first reversed the
        // two blocks.
        let mut buf = out.stderr;
        buf.extend_from_slice(&out.stdout);
        Ok((out.status.success(), buf))
    }

    /// `run_git_commit()` — git re-enters `git commit` for every path that needs
    /// the message editor or `--amend`'s bookkeeping, so this does too.
    fn run_commit(&self, args: &[String], env: Vec<(String, String)>) -> Result<i32> {
        let restore: Vec<(String, Option<String>)> = env
            .iter()
            .map(|(k, _)| (k.clone(), std::env::var(k).ok()))
            .collect();
        for (k, v) in &env {
            // SAFETY: the sequencer is single-threaded here — the merge machinery
            // has finished and no worker holds the environment.
            unsafe { std::env::set_var(k, v) };
        }
        let out = super::commit::commit(args);
        for (k, v) in restore {
            unsafe {
                match v {
                    Some(v) => std::env::set_var(&k, v),
                    None => std::env::remove_var(&k),
                }
            }
        }
        let code = out?;
        // `ExitCode` is opaque; the only thing the caller needs is success.
        Ok(if format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS) { 0 } else { 1 })
    }

    /// `pick_commits()`'s tail: re-point the branch at the new tip, re-attach
    /// `HEAD`, re-apply the autostash and drop the state directory.
    fn finish(&mut self) -> Result<ExitCode> {
        let repo = self.repo;
        let tip = repo.head_id()?.detach();
        let label = if self.st.head_name != "detached HEAD" {
            let name = full_name(&self.st.head_name)?;
            let label = name.as_bstr().to_string();
            repo.edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!(
                            "{} (finish): {} onto {}",
                            reflog_action(),
                            name.as_bstr(),
                            self.st.onto
                        )
                        .into(),
                    },
                    expected: PreviousValue::MustExistAndMatch(Target::Object(self.st.orig_head)),
                    new: Target::Object(tip),
                },
                name: name.clone(),
                deref: false,
            })?;
            let message = format!("{} (finish): returning to {label}", reflog_action());
            set_head(repo, Target::Symbolic(name), &message)?;
            // The vendored `gix-ref` writes no reflog line for a symbolic-target
            // update, so `HEAD`'s own log would lose the entry git ends every
            // rebase with — the same compensation `checkout` makes.
            super::checkout::record_head_move(repo, Some(tip), Some(tip), &message);
            label
        } else {
            "detached HEAD".to_string()
        };
        if self.st.verbose {
            verbose_replay_diffstat(self.st.orig_head, tip)?;
        }
        self.delete_rewritten_refs();
        // `pick_commits()`'s tail (sequencer.c:5190-5207): flush whatever the last
        // instruction left pending, then hand the accumulated `<old> <new>` list
        // to the `post-rewrite` hook. It runs after the branch has been re-pointed
        // and before the autostash is restored, and its exit status is ignored.
        // This has to precede the state directory going away — the list lives in
        // it.
        rewritten::run_post_rewrite_hook(repo, &rebase_merge_dir(repo));
        // `do_update_refs()` runs *after* the `Successfully rebased and updated
        // <ref>.` line and before `sequencer_remove_state()`
        // (sequencer.c:5210-5229), so the records have to survive the state
        // directory being taken down here.
        let pending_ref_updates = read_update_refs_state(repo)?;
        let _ = std::fs::remove_dir_all(rebase_merge_dir(repo));
        if let Some(oid) = self.autostash {
            crate::porcelain::stash::apply_autostash(repo, oid, self.st.quiet)?;
        }
        super::maintenance::run_auto_maintenance(repo, self.st.quiet)?;
        if !self.st.quiet {
            self.term_clear_line();
            eprintln!("Successfully rebased and updated {label}.");
        }
        if !pending_ref_updates.is_empty()
            && !apply_pending_ref_updates(repo, &pending_ref_updates, self.st.quiet)
        {
            return Ok(ExitCode::from(1));
        }
        Ok(ExitCode::SUCCESS)
    }
}

/// The reflog line a `fixup`/`squash` writes: `rebase (fixup): <subject>`.
///
/// `try_to_commit()` ends in `update_head_with_reflog(…, reflog_action, …)`
/// (sequencer.c), the same call every other pick makes, so the melded commit's
/// own subject is appended exactly as a `pick`'s is. Writing the bare
/// `rebase (fixup)` left the entry with no subject, which is the one line in a
/// squashed rebase's reflog that says *what* was squashed.
fn fixup_reflog_message(message: &[u8]) -> String {
    gix::reference::log::message(
        &format!("{} (fixup)", reflog_action()),
        message.as_bstr(),
        1,
    )
    .to_string()
}

/// `make_patch()` (sequencer.c:3440-3485) — everything an `error_with_patch()`
/// stop records about the commit it stopped on:
///
/// ```c
/// if (write_message(hex, strlen(hex), rebase_path_stopped_sha(), 1) < 0)
///         return -1;
/// res |= write_rebase_head(&commit->object.oid);
/// …
/// log_tree_opt.diffopt.file = fopen("$dir/patch", "w");
/// …
/// strbuf_addf(&buf, "%s/message", get_dir(opts));
/// if (!file_exists(buf.buf))
///         res |= write_message(…);
/// ```
///
/// Each of the three has a reader: `REBASE_HEAD` is what `git show REBASE_HEAD`
/// resolves and what `git commit` tests for its `FROM_REBASE_PICK` shortcut
/// (`porcelain::commit`), `patch` is what `--show-current-patch --diff` prints,
/// and `message` is what `--continue` commits — which is why it is only written
/// when nothing has put one there already.
fn make_patch(repo: &gix::Repository, dir: &std::path::Path, commit: &gix::Commit<'_>) -> Result<()> {
    let oid = commit.id().detach();
    std::fs::write(dir.join("stopped-sha"), format!("{oid}\n"))?;
    // `write_rebase_head()` (sequencer.c:1614-1621). The pseudo-ref is a bare
    // loose file holding the id, with no reflog — verified against stock, whose
    // `.git/logs` gains nothing when the stop happens.
    std::fs::write(repo.git_dir().join("REBASE_HEAD"), format!("{oid}\n"))?;
    let parent = commit.parent_ids().next().map(|p| p.detach());
    let patch = super::diff::commit_patch(repo, commit, parent, 3)?;
    std::fs::write(dir.join("patch"), patch)?;
    let message = dir.join("message");
    if !message.exists() {
        // `write_message(…, append_eol = 1)` appends a newline unconditionally
        // (sequencer.c:498), so the raw message — which already ends in one —
        // lands with a trailing blank line.
        let mut body = commit.message_raw()?.to_vec();
        body.push(b'\n');
        std::fs::write(message, body)?;
    }
    Ok(())
}

/// `write_author_script()`: the replayed commit's author, in the sq-quoted
/// `KEY='value'` form `read_env_script()` reads back, so an interrupted
/// instruction can be concluded by `git commit` without losing the author.
fn write_author_script(repo: &gix::Repository, commit: &gix::Commit<'_>) -> Result<()> {
    let author = commit.author()?;
    let sq = |v: &BStr| format!("'{}'", v.to_str_lossy().replace('\'', "'\\''"));
    let time = author.time().unwrap_or_default();
    // `GIT_AUTHOR_DATE='@<seconds> <+-HHMM>'` — git's "raw" date spelling, the
    // one form `parse_date()` round-trips without a timezone database.
    let (sign, off) = if time.offset < 0 { ('-', -time.offset) } else { ('+', time.offset) };
    let body = format!(
        "GIT_AUTHOR_NAME={}\nGIT_AUTHOR_EMAIL={}\nGIT_AUTHOR_DATE='@{} {sign}{:02}{:02}'\n",
        sq(author.name),
        sq(author.email),
        time.seconds,
        off / 3600,
        (off % 3600) / 60,
    );
    std::fs::write(rebase_merge_dir(repo).join("author-script"), body)?;
    Ok(())
}

/// The first line of a commit message — git's `find_commit_subject()` for the
/// purpose of the one-line reports the sequencer prints.
fn first_line(message: &BStr) -> BString {
    match message.find_byte(b'\n') {
        Some(p) => message[..p].as_bstr().to_owned(),
        None => message.to_owned(),
    }
}

/// `skip_unnecessary_picks()` — drop the leading picks whose parent is already
/// the base, recording them in `done` so the progress numbers stay honest.
///
/// Returns the base the surviving instructions start from: a run of picks that
/// would each land on the parent they already have is exactly the range that is
/// already in place, so the rebase starts at the last of them instead of
/// re-picking it.
fn skip_unnecessary_picks(
    repo: &gix::Repository,
    list: &mut todo::List,
    base: &mut ObjectId,
) -> Result<()> {
    let mut i = 0;
    while i < list.items.len() {
        let item = &list.items[i];
        if item.cmd.is_noop() {
            i += 1;
            continue;
        }
        if item.cmd != todo::Cmd::Pick {
            break;
        }
        let Some(oid) = item.commit else { break };
        let commit = repo.find_commit(oid)?;
        let mut parents = commit.parent_ids();
        let Some(parent) = parents.next() else { break };
        if parents.next().is_some() {
            break; // merge commit
        }
        if parent.detach() != *base {
            break;
        }
        *base = oid;
        i += 1;
    }
    if i > 0 {
        std::fs::write(
            rebase_merge_dir(repo).join("done"),
            list.to_bytes(repo, Some(i), 0),
        )?;
        list.items.drain(..i);
        // ```c
        // if (is_fixup(peek_command(todo_list, 0)))
        //         record_in_rewritten(base_oid, peek_command(todo_list, 0));
        // ```
        // (sequencer.c:6429-6430). A fixup at the head of what is left will meld
        // into the commit the skipped picks fast-forwarded to, so that commit is
        // one of the ones being rewritten — even though no instruction picked it.
        // The record stays pending (the next command is a fixup by construction),
        // so it is paired with whatever the chain produces.
        if peek_command(list, 0).is_some_and(|c| c.is_fixup()) {
            rewritten::record(repo, &rebase_merge_dir(repo), *base, true);
        }
    }
    Ok(())
}

/// Write a tree object capturing the stage-0 entries of `index`.
fn tree_from_index(repo: &gix::Repository, index: &gix::index::File) -> Result<ObjectId> {
    let hash = repo.object_hash();
    let mut editor = gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, hash);
    let backing = index.path_backing();
    for entry in index.entries() {
        if entry.stage_raw() != 0 {
            continue;
        }
        let path = entry.path_in(backing);
        let mode = entry
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow!("index entry `{path}` has an unrepresentable mode"))?;
        editor.upsert(path.split(|&b| b == b'/').map(|c| c.as_bstr()), mode.kind(), entry.id)?;
    }
    Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?)
}

/// `sequencer_reflog_action()` (sequencer.c): the word every rebase reflog entry
/// is prefixed with — `GIT_REFLOG_ACTION` when the caller set one, `rebase`
/// otherwise.
///
/// `pull` sets the variable to its own command line, which is why a
/// `git pull --rebase` leaves `pull --rebase (pick): …` in the reflog where a
/// bare rebase leaves `rebase (pick): …`.
fn reflog_action() -> String {
    std::env::var("GIT_REFLOG_ACTION").unwrap_or_else(|_| "rebase".to_string())
}

/// `--signoff`: `append_signoff(&msgbuf, 0, 0)` over a replayed commit's message,
/// with the trailer built from the *committer* identity (`WANT_COMMITTER_IDENT`,
/// no date). Returns the message unchanged when `--signoff` was not given.
///
/// `append_signoff()` merges into an existing trailer block, so a commit already
/// carrying this exact sign-off as its last trailer is left alone. Its port lives
/// in `commit.rs` and works on `String`, which is why a non-UTF-8 message is
/// reported rather than round-tripped lossily.
fn sign_off(
    committer: &gix::actor::Signature,
    signoff: bool,
    message: &BString,
) -> Result<BString> {
    if !signoff {
        return Ok(message.clone());
    }
    let Ok(mut text) = String::from_utf8(message.to_vec()) else {
        bail!("--signoff on a non-UTF-8 commit message is not ported");
    };
    let ident = format!("{} <{}>", committer.name, committer.email);
    super::commit::append_signoff(&mut text, &ident, 0, false);
    Ok(BString::from(text))
}

fn set_head(repo: &gix::Repository, target: Target, message: &str) -> Result<()> {
    // What `HEAD` resolved to before the move, and after it: the vendored `gix-ref`
    // writes a null old field for a `deref: false` update whose previous value was a
    // symref, and drops the entry entirely when the new target is symbolic. Both are
    // repaired by `record_head_move()`, the same way `git checkout` repairs them.
    let from = repo
        .head()
        .ok()
        .and_then(|mut h| h.try_peel_to_id().ok().flatten().map(|id| id.detach()));
    let to = match &target {
        Target::Object(id) => Some(*id),
        Target::Symbolic(name) => repo
            .find_reference(name.as_ref())
            .ok()
            .and_then(|mut r| r.peel_to_id_in_place().ok().map(|id| id.detach())),
    };
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: target,
        },
        name: full_name("HEAD")?,
        deref: false,
    })?;
    super::checkout::record_head_move(repo, from, to, message);
    Ok(())
}

/// Move a clean worktree and its index from the state captured in `old` to the
/// tree of commit `new_commit`, writing only the files that changed.
///
/// Same reconcile path as `porcelain::merge`: the change set is derived by
/// comparing the old index against the new tree-index (file-level granularity),
/// added/modified files are checked out via `gix-worktree-state`, removed files
/// are deleted, and the new index is written reusing prior stats for unchanged
/// entries so a later status stays cheap.
fn update_clean_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_commit: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let new_tree_id = repo.find_object(new_commit)?.peel_to_tree()?.id;

    // Index the current entries by path for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    // Full target index (all new-tree entries) — what is finally written; a
    // reduced copy of only the changed entries is what is checked out.
    let mut new_index = repo.index_from_tree(&new_tree_id)?;
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        // Present before with identical content and mode → unchanged, drop it.
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        // Absent before → an addition, keep it.
        None => false,
    });

    // Write the changed files into the (clean) worktree, overwriting in place.
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        should_interrupt,
        opts,
    )?;

    // Remove files present in the old index but not the new tree.
    let new_paths: HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing);
            if !new_paths.contains(&path.to_owned()) {
                if let Some(full) = repo.workdir_path(path) {
                    let _ = std::fs::remove_file(full);
                }
            }
        }
    }

    // Fresh stats produced by the checkout for the changed entries.
    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    // Changed entries get their fresh stat; unchanged entries reuse the old one.
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }

    // Drop any stale cache-tree extension before persisting.
    // `unpack_trees()` ends with `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`
    // (unpack-trees.c:2088-2092), so the index git leaves here carries a cache-tree.
    super::write_tree::rebuild_cache_tree(repo, &mut new_index);
    new_index.write(crate::config::index_write_options(repo))?;

    Ok(())
}
