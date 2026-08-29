//! `git merge` — fast-forward, `--no-ff` over a fast-forwardable history,
//! `--abort` and `--quit`.
//!
//! What is served natively via the vendored gitoxide crates:
//!
//! * A fast-forward merge: the ref being merged is a descendant of the current
//!   `HEAD` (their merge-base is `HEAD` itself). The branch `HEAD` points to is
//!   advanced (or `HEAD` itself on a detached head), and the worktree + index
//!   are moved to the new tree — writing only the paths the two trees disagree
//!   on, so local work outside that footprint is carried through untouched.
//! * `--no-ff` over that same fast-forwardable history. The merged tree is then
//!   exactly the tree of the ref being merged — when the merge-base *is* our
//!   own commit, the three-way merge of every path resolves to theirs — so the
//!   merge commit is written directly with no three-way machinery involved.
//! * A real merge of diverged histories (with or without `--no-ff`), via the
//!   shared three-way merge in [`crate::merge_apply`]: `Auto-merging`/`CONFLICT`
//!   reporting, a clean two-parent merge commit, or — on conflict —
//!   `MERGE_HEAD`/`MERGE_MSG` plus the conflicted index and worktree markers, then
//!   `Automatic merge failed; fix conflicts and then commit the result.` (exit 1).
//! * `--abort` / `--quit`: `--quit` drops the in-progress merge state files;
//!   `--abort` additionally restores the index and the merge-affected worktree
//!   paths to `HEAD`, as `git reset --merge` does.
//!
//! Also served, as faithful ports of git's behaviour:
//!
//! * The dirty-worktree policy, which is two separate gates rather than one
//!   (see [`crate::merge_guard`]). A strategy — `ort`, `ours`, `octopus` — first
//!   refuses when the index differs from `HEAD` anywhere (merge-ort's
//!   `merge_start()`, exit 2 behind `Merge with strategy <name> failed.`); a
//!   fast-forward skips that gate entirely and accepts staged work. Then the
//!   checkout itself refuses per path (`twoway_merge()` with `verify_uptodate()`
//!   / `verify_absent()`): only paths the two trees disagree on are examined, so
//!   an unrelated local edit is never a reason to stop, while one the merge
//!   would overwrite — or an untracked file in the way — produces git's
//!   `error: Your local changes …` / `The following untracked working tree
//!   files …` block, `Aborting`, and exit 1 from a fast-forward or 2 from a
//!   strategy.
//! * `--squash`/`--no-squash`: fold the merge into the worktree/index without a
//!   commit or ref move, writing `SQUASH_MSG` (a port of `squash_message()`,
//!   including the `git log`-medium body).
//! * `--commit`/`--no-commit`: `--no-commit` records `MERGE_HEAD`/`MERGE_MODE`/
//!   `MERGE_MSG` and stops with `Automatic merge went well; stopped before
//!   committing as requested`, leaving `git commit` (or `--continue`) to finish.
//! * `--continue`: finalize a resolved, staged in-progress merge.
//! * `-s ours` (and `-s ort`/`octopus`): `ours` records every head as a parent
//!   but keeps our tree verbatim.
//! * `git merge FETCH_HEAD` — the form `pull` runs — is `handle_fetch_head()`:
//!   the heads are the for-merge lines of `.git/FETCH_HEAD`, so one line is an
//!   ordinary merge and several are an octopus, and the description each line
//!   carries is what the message is built from. `fmt_merge_msg_title()`'s
//!   grouping applies, so two branches from one remote read
//!   `Merge branches 'a' and 'b' of <url>` rather than naming the URL twice. The
//!   reflog still records what the *command line* said, which for this form is
//!   the object id of each head.
//! * `--allow-unrelated-histories`: merge with an empty base tree; without it,
//!   `fatal: refusing to merge unrelated histories` (exit 128).
//! * `--signoff`, `-F`/`--file`, `--cleanup=<mode>`, `-q`/`--quiet`,
//!   `-v`/`--verbose`, and `--no-verify` (bypassing the `pre-merge-commit` and
//!   `commit-msg` hooks).
//! * `--log[=<n>]`/`--no-log` (and its `merge.log`/`merge.summary` defaults): the
//!   `* <origin>:` shortlog of every merged head folded into the merge message,
//!   a port of `fmt_merge_msg()`'s shortlog loop — including the
//!   `: (<n> commits)` header plus trailing `...` when more commits were merged
//!   than listed, the `merge.branchdesc` branch description, and the `By`/`Via`
//!   credit lines (which git emits only under `--edit`).
//! * `--compact-summary`/`--no-compact-summary` and `merge.stat`/`merge.diffstat`
//!   (`false`, `true`, `compact`): git's `show_diffstat` tri-state. The compact
//!   form drops the `create mode`/`delete mode` summary block and folds it into
//!   each diffstat name as ` (new)`, ` (new +x)`, ` (new +l)`, ` (gone)`,
//!   ` (mode +x)`, ` (mode -x)`, ` (mode +l)`, ` (mode -l)` — a port of
//!   `get_compact_summary()`. `--no-compact-summary` suppresses the diffstat
//!   entirely, as `option_parse_compact_summary()`'s `unset` does.
//! * `-e`/`--edit`/`--no-edit`, resolved through `default_edit_option()`: the
//!   merge message is written to `MERGE_MSG` with git's commented editor block
//!   below it (the scissors variant under `--cleanup=scissors`), opened with the
//!   `GIT_EDITOR` → `core.editor` → `$VISUAL` → `$EDITOR` → `vi` chain (`:` is
//!   the no-op editor), and the edited text is then stripped of comment lines,
//!   since an edited message defaults to `COMMIT_MSG_CLEANUP_ALL`. A failing
//!   editor or an empty message aborts with git's `Not committing merge; use
//!   'git commit' to complete the merge.` and leaves the merge in progress.
//! * `save_state()`: before a strategy runs, a dirty worktree is snapshotted into a
//!   `git stash create` commit — dangling, and what `git fsck --dangling` reports after
//!   a merge over local changes. It exists so `restore_state()` can rewind a strategy
//!   that failed part-way; the strategies here compute the whole result before touching
//!   the worktree, so nothing has to be rewound and the snapshot is only ever a record.
//!   A strategy refused because the *index* does not match HEAD also logs the no-op
//!   `<reflog action>: updating HEAD` git logs there (a refusal over unstaged changes
//!   does not).
//! * `--autostash`/`--no-autostash` and `merge.autoStash`: a dirty worktree is
//!   snapshotted into a stash-like commit parked under the `MERGE_AUTOSTASH` ref
//!   (`Created autostash: <id>`), the merge runs against the clean tree, and the
//!   changes are re-applied afterwards (`Applied autostash.`). A merge that stops
//!   early — conflict, `--squash`, `--no-commit` — leaves the ref in place and
//!   prints git's ``When finished, apply stashed changes with `git stash pop` ``.
//! * `-S`/`--gpg-sign[=<keyid>]`/`--no-gpg-sign` and `commit.gpgsign`: the merge
//!   commit is signed through `gpg.program` with `-S<keyid>`, else
//!   `user.signingKey`, else the committer identity (git's `get_signing_key()`),
//!   and the armored signature is carried as the commit's `gpgsig` header. A gpg
//!   failure reproduces git's `error: gpg failed to sign the data:` followed by
//!   gpg's own diagnostics and `fatal: failed to write commit object`.
//! * `--progress`/`--no-progress`: accepted and inert. In `builtin/merge.c`
//!   `show_progress` has exactly one consumer, `o.show_rename_progress`, which
//!   forces merge-ort's delayed "Performing inexact rename detection" meter;
//!   this build's merge runs no such meter, so neither spelling can change a
//!   byte of its output.
//! * `commit.cleanup` — the same `cleanup_arg` `--cleanup` sets.
//! * `-m`/`--message` accumulation: repeated `-m` values are joined into
//!   paragraphs by a blank line (a port of `option_parse_message`), so
//!   `-m a -m b` produces `a\n\nb`.
//! * The value-clearing negations `--no-message` (empty the accumulated
//!   message), `--no-into-name` (restore the real target destination) and
//!   `--no-cleanup` (back to the default `whitespace` mode) — ports of git's
//!   OPT_STRING/OPT_CLEANUP `unset` behaviour.
//! * `--into-name <name>`: a port of git's `into_name` — override the merge
//!   message's destination (the ` into <name>` title and the
//!   `merge.suppressDest` test), rather than the real current branch.
//! * `--rerere-autoupdate` / `--no-rerere-autoupdate`: git's
//!   `OPT_RERERE_AUTOUPDATE`, handed to the `repo_rerere()` that
//!   `suggest_conflicts()` runs once the conflicted index is written. Set stages
//!   a replayed resolution (`Staged '<path>' using previous resolution.`), unset
//!   leaves it in the worktree (`Resolved …`), and neither defers to
//!   `rerere.autoupdate`.
//! * The default-matching negations `--no-strategy` (git's
//!   `option_parse_strategy` no-ops on `unset`) and `--overwrite-ignore`,
//!   accepted as no-ops: each names behaviour this build already performs
//!   (ignored files overwritten), so passing them reproduces stock git rather
//!   than erroring.
//! * `--[no-]verify-signatures` and `merge.verifySignatures`: a port of
//!   `verify_merge_signature()`, run over the heads left after the
//!   already-reachable ones are dropped, with `gpg.minTrustLevel` deciding
//!   whether git applies its own `TRUST_MARGINAL` floor on top.
//!
//! * `-X`/`--strategy-option`: the values are collected raw and handed to
//!   `merge_apply::StrategyOptions` (a port of merge-ort's `parse_merge_opt`)
//!   from inside [`try_merge_strategy`], where git parses them — which is why
//!   `Already up to date.`, a plain fast-forward, `-s ours` and the octopus
//!   strategy all accept a value the `ort` path would reject. Honoured:
//!   every branch of `parse_merge_opt()` — `ours`, `theirs`,
//!   `subtree[=<prefix>]` (a port of `match-trees.c`'s tree shifting),
//!   `patience`, `histogram`,
//!   `diff-algorithm=<myers|default|minimal|patience|histogram>`,
//!   `ignore-space-change`, `ignore-all-space`, `ignore-space-at-eol`,
//!   `ignore-cr-at-eol` (`xdl_recmatch()`'s `XDF_IGNORE_*` rules, as canonical
//!   line images — see `crate::merge_ws`), `renormalize`, `no-renormalize`,
//!   `no-renames`, `find-renames[=<n>]` and `rename-threshold=<n>`. An
//!   unrecognised value reproduces git's
//!   `fatal: unknown strategy option: -X<value>`.
//!
//! * `-s recursive` and `-s subtree`: git 2.55 has no separate `recursive`
//!   back-end left — `try_merge_strategy()` routes `recursive`, `subtree` and
//!   `ort` to the same `merge_ort_recursive()` (builtin/merge.c:800-834) — so
//!   both run the `ort` engine here, `subtree` adding the automatic subtree
//!   shift (builtin/merge.c:815-816) and the `NO_FAST_FORWARD` attribute
//!   (builtin/merge.c:107). The name is echoed back as git echoes `wt_strategy`
//!   (builtin/merge.c:1794), so `-s recursive` reports
//!   `Merge made by the 'recursive' strategy.`
//!
//! * `-s resolve`, `-s octopus` and `-s ours`: the back-ends git runs out of
//!   process (`try_merge_command()`, merge.c:22-42), reached through
//!   [`resolve_attempt`], [`octopus_attempt`] and [`ours_attempt`]. `resolve` is
//!   [`super::merge_resolve`], a port of `git-merge-resolve.sh`'s
//!   `read-tree`/`write-tree`/`merge-index` chain; `octopus` over a *single*
//!   head is `git-merge-octopus`'s own "Reject if this is not an octopus" exit
//!   2; `ours` ignores the head count entirely and keeps our tree verbatim. `-X`
//!   is not parsed for any of them: `try_merge_command()` re-spells each value
//!   `--<value>` onto the back-end's command line, where `git-merge-resolve`
//!   hands it to `read-tree` and `git-merge-octopus` counts it as a merge base.
//!
//! * The `allow_trivial` in-index merge (`Trying really trivial in-index
//!   merge...` / `Wonderful.` / `In-index merge`): `all_strategy[]` gives
//!   `octopus` and `resolve` no `NO_TRIVIAL` (builtin/merge.c:102-107), so those
//!   two — over one head, one merge base and a merge that will be committed —
//!   run [`read_tree_trivial`] first, which is `git read-tree -u -m --trivial`
//!   in process. The attribute is a union over the *whole* `-s` list
//!   (builtin/merge.c:1611-1612), so one `-s ort` alongside `-s resolve`
//!   suppresses the pre-pass; `NO_FAST_FORWARD` unions the same way
//!   (builtin/merge.c:1609-1610), which is why `-s ort -s ours` records a merge
//!   commit over a history `ort` alone would have fast-forwarded.
//!
//! * Several `-s` in one command line: git keeps them all in `use_strategies`
//!   and [`merge_with_strategies`] walks them in order, printing
//!   `Trying merge strategy <name>...`, rewinding between attempts with
//!   [`restore_state`] — `read-tree -v --reset -u <head>` then `stash apply
//!   --index`, the real thing, not a printed line — and keeping the attempt
//!   [`evaluate_result`] scores best (builtin/merge.c:1778-1859). A tie keeps the
//!   *later* one (`cnt <= best_cnt`); a winner that is not the last attempt is
//!   re-run after one more rewind, behind `Using the <name> strategy to prepare
//!   resolving by hand.`. One `-s` is the degenerate case of the same loop.
//!
//! * The head count, not the `-s` spelling, picks the engine.
//!   `add_strategies(pull_octopus, DEFAULT_OCTOPUS)` fires only when no `-s` was
//!   given (builtin/merge.c:1600-1606), so a named two-head strategy over three
//!   heads fails with `error: Not handling anything other than two heads
//!   merge.` rather than quietly octopusing.
//!
//! What is refused or deferred rather than faked:
//!
//! * `--no-overwrite-ignore`: needs gitignore-aware checkout.
//!
//! Known fidelity gaps, stated rather than hidden: `merge.directoryRenames` is
//! never read — every merge behaves as `=true`, where git's default is
//! `=conflict`, so a file added into a directory the other side renamed is moved
//! silently instead of conflicting (`gix-merge` has no directory-rename input to
//! drive); `merge.renameLimit`/`diff.renameLimit` are only honoured when
//! `merge.renames`/`diff.renames` is also set, because gitoxide's
//! `diff_resource_cache` returns before reading the limit otherwise;
//! `diff.algorithm=patience` reaches the blob merge as histogram, since
//! gitoxide's configuration cache reports patience as unimplemented and falls
//! back leniently (`-Xpatience` is unaffected — it bypasses the cache); the
//! diffstat is computed
//! with rename detection off, while `git merge` enables it, so a merge that
//! renames a file reports it as a delete plus a create instead of a `rename`
//! summary line; `--verbose`'s extra stderr diagnostics are not
//! emitted; a `pre-merge-commit` hook that edits the index is not reflected
//! in the committed tree (the pre-computed merge tree is committed); the
//! `prepare-commit-msg` hook is not run before the editor; `default_edit_option`
//! tests that stdin and stdout are both terminals rather than that they are the
//! same file; `--signoff` adds its trailer before the `--edit` comment block
//! rather than through `ignored_log_message_bytes()`;
//! and a directory standing where a merge wants
//! to write is left to the checkout instead of going through
//! `verify_clean_subdirectory()`, so untracked files inside it are not counted
//! (a gitlink whose submodule is already checked out passes under git too).

use anyhow::Result;
// Every `print!`/`println!` below goes through git's stdout buffer; see
// `crate::cstdio` and the `defer()` call in `merge()`.
use crate::cstdio::{print, println};
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stage, Stat};
use gix::object::tree::{diff::Action, diff::Change as TreeChange, EntryKind};
use gix::objs::WriteTo;
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};
use gix::revision::walk::Sorting;

use super::diffstat::{self, StatWidths};
use super::filespec;
use gix::traverse::commit::simple::CommitTimeOrder;

/// `cmd_merge()`'s `struct option builtin_merge_options[]` (builtin/merge.c), in
/// table order, as [`super::resolve_long`] reads it.
///
/// `--ff-only` and `-F`/`--file` carry `PARSE_OPT_NONEG`, so neither has a `--no-`
/// spelling; `no-verify` is an entry spelled with its own `no-`, which
/// parse-options reads as the *unset* sense of `verify`.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "stat",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "summary",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "compact-summary",             neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "log",                         neg: true,  arg: super::Arg::Optional },
    super::LongOpt { name: "squash",                      neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "commit",                      neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "edit",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "cleanup",                     neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "ff",                          neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "ff-only",                     neg: false, arg: super::Arg::None },
    super::LongOpt { name: "rerere-autoupdate",           neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "verify-signatures",           neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "strategy",                    neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "strategy-option",             neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "message",                     neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "file",                        neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "into-name",                   neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "verbose",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "quiet",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "abort",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "quit",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "continue",                    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "allow-unrelated-histories",   neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "progress",                    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "gpg-sign",                    neg: true,  arg: super::Arg::Optional },
    super::LongOpt { name: "autostash",                   neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "overwrite-ignore",            neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "signoff",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "no-verify",                   neg: true,  arg: super::Arg::None },
];
/// `usage_with_options()` over `builtin/merge.c`'s option table.
const USAGE: &str = r"usage: git merge [<options>] [<commit>...]
   or: git merge --abort
   or: git merge --continue

    -n                    do not show a diffstat at the end of the merge
    --[no-]stat           show a diffstat at the end of the merge
    --[no-]summary        (synonym to --stat)
    --[no-]compact-summary
                          show a compact-summary at the end of the merge
    --[no-]log[=<n>]      add (at most <n>) entries from shortlog to merge commit message
    --[no-]squash         create a single commit instead of doing a merge
    --[no-]commit         perform a commit if the merge succeeds (default)
    -e, --[no-]edit       edit message before committing
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    --[no-]ff             allow fast-forward (default)
    --ff-only             abort if fast-forward is not possible
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --[no-]verify-signatures
                          verify that the named commit has a valid GPG signature
    -s, --[no-]strategy <strategy>
                          merge strategy to use
    -X, --[no-]strategy-option <option=value>
                          option for selected merge strategy
    -m, --[no-]message <message>
                          merge commit message (for a non-fast-forward merge)
    -F, --file <path>     read message from file
    --[no-]into-name <name>
                          use <name> instead of the real target
    -v, --[no-]verbose    be more verbose
    -q, --[no-]quiet      be more quiet
    --[no-]abort          abort the current in-progress merge
    --[no-]quit           --abort but leave index and working tree alone
    --[no-]continue       continue the current in-progress merge
    --[no-]allow-unrelated-histories
                          allow merging unrelated histories
    --[no-]progress       force progress reporting
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit
    --[no-]autostash      automatically stash/stash pop before and after
    --[no-]overwrite-ignore
                          update ignored files (default)
    --[no-]signoff        add a Signed-off-by trailer
    --no-verify           bypass pre-merge-commit and commit-msg hooks
    --verify              opposite of --no-verify

";

/// git's `DEFAULT_MERGE_LOG_LEN` — how many shortlog entries a valueless
/// `--log` (or `merge.log = true`) asks for.
const DEFAULT_MERGE_LOG_LEN: i64 = 20;

/// The mutually exclusive top-level modes of `git merge`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Merge,
    Abort,
    Quit,
    Continue,
}

/// How the fast-forward question is answered, mirroring git's `fast_forward`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ff {
    Allow,
    Never,
    Only,
}

/// The merge engine selected by `-s`/`--strategy`.
///
/// git 2.55 has no separate `recursive` back-end left: `try_merge_strategy()`
/// sends `recursive`, `subtree` and `ort` down the same `merge_ort_recursive()`
/// branch (builtin/merge.c:800-834), so all three share one variant here. The
/// only thing `subtree` adds is `o.subtree_shift = ""` before the `-X` loop
/// (builtin/merge.c:815-816) plus the `NO_FAST_FORWARD` attribute
/// (builtin/merge.c:107), which is why it needs a variant of its own.
///
/// `resolve` and `octopus` are the two strategies git still runs out of process
/// (`try_merge_command()` spawns `git merge-<name>`), and the two whose entries
/// in `all_strategy[]` carry no `NO_TRIVIAL` (builtin/merge.c:102-107) — which is
/// what puts the `allow_trivial` in-index merge in front of them and only them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Strategy {
    /// git's `ort`, and its `recursive` alias.
    Ort,
    /// `-s subtree`: `ort` with an automatic subtree shift and no fast-forward.
    Subtree,
    /// `-s ours`: record every head as a parent but keep our tree verbatim.
    Ours,
    /// `-s resolve`: `git-merge-resolve`, run out of process by
    /// `try_merge_command()` and ported as [`super::merge_resolve`].
    Resolve,
    /// `-s octopus`: `git-merge-octopus`, likewise out of process.
    Octopus,
}

impl Strategy {
    /// `if (use_strategies[i]->attr & NO_TRIVIAL) allow_trivial = 0;`
    /// (builtin/merge.c:1611-1612). `all_strategy[]` sets `NO_TRIVIAL` on
    /// `recursive`, `ort`, `ours` and `subtree` but not on `octopus` or `resolve`
    /// (builtin/merge.c:102-107), so those two — and the two-head default only
    /// when neither was named — reach the in-index pre-pass.
    fn allows_trivial(self) -> bool {
        matches!(self, Strategy::Resolve | Strategy::Octopus)
    }

    /// The engines `try_merge_strategy()` hands to `merge_ort_recursive()`
    /// (builtin/merge.c:800-801), which refuses anything but two heads.
    fn is_ort(self) -> bool {
        matches!(self, Strategy::Ort | Strategy::Subtree)
    }

    /// `if (use_strategies[i]->attr & NO_FAST_FORWARD) fast_forward = FF_NO;`
    /// (builtin/merge.c:1609-1610). `all_strategy[]` sets `NO_FAST_FORWARD` on
    /// `ours` and `subtree` only (builtin/merge.c:106-107), so either one
    /// anywhere in the `-s` list forces a merge commit over a history that
    /// could have fast-forwarded — even when the strategy that ends up
    /// answering the merge is a different one.
    fn no_fast_forward(self) -> bool {
        matches!(self, Strategy::Ours | Strategy::Subtree)
    }
}

/// One entry of git's `use_strategies` (builtin/merge.c:82): the engine plus the
/// name it was spelled with. The name is kept because `wt_strategy` is echoed
/// verbatim — `-s recursive` reports `Merge made by the 'recursive' strategy.`
/// even though the engine that ran was `ort`.
#[derive(Clone, PartialEq, Eq)]
struct Pick {
    kind: Strategy,
    name: String,
}

impl Pick {
    fn new(kind: Strategy, name: &str) -> Self {
        Pick { kind, name: name.to_string() }
    }
}

/// `--cleanup=<mode>` — how the commit message is stripped, a port of git's
/// `cleanup_mode` / `strbuf_stripspace`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    /// `whitespace` when a message is supplied without an editor (merge's default).
    Default,
    Verbatim,
    Whitespace,
    Strip,
    Scissors,
}

/// git's `show_diffstat` tri-state (`MERGE_SHOW_DIFFSTAT` /
/// `MERGE_SHOW_COMPACTSUMMARY` / off), driven by `--stat`/`--no-stat`,
/// `--compact-summary`/`--no-compact-summary` and `merge.stat`/`merge.diffstat`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatMode {
    /// `show_diffstat == 0`: nothing is printed after the merge.
    None,
    /// `MERGE_SHOW_DIFFSTAT`: the diffstat plus the `create mode`/`delete mode`
    /// summary block (`DIFF_FORMAT_DIFFSTAT | DIFF_FORMAT_SUMMARY`).
    Diffstat,
    /// `MERGE_SHOW_COMPACTSUMMARY`: the diffstat alone, with the summary folded
    /// into each name as ` (new)`/` (gone)`/` (mode +x)`… (`stat_with_summary`).
    CompactSummary,
}

/// Everything the argument loop gathers for a real merge, so the merge helpers
/// take one struct rather than a growing parameter list.
struct Opts {
    ff: Ff,
    /// Whether `--no-ff` was passed explicitly (needed for the `--squash`
    /// incompatibility check, which git keys off the literal flag).
    no_ff_given: bool,
    stat: StatMode,
    /// `-m`/`--message` or `-F`/`--file` contents (the latter read eagerly).
    message: Option<String>,
    squash: bool,
    /// `--commit`/`--no-commit` as given; `None` leaves the default (`!squash`).
    commit: Option<bool>,
    /// `--commit` was given explicitly (for the `--squash` incompatibility check).
    commit_given: bool,
    signoff: bool,
    /// `--verify-signatures` / `--no-verify-signatures`; `None` defers to
    /// `merge.verifySignatures`.
    verify_signatures: Option<bool>,
    allow_unrelated: bool,
    no_verify: bool,
    quiet: bool,
    cleanup: Cleanup,
    /// `use_strategies` (builtin/merge.c:82): *every* `-s`, in the order given.
    /// git tries them one after another, rewinding between attempts, and keeps
    /// the one that scored best — so the list has to survive parsing whole.
    ///
    /// Empty means none was given, which is what `if (!use_strategies)`
    /// (builtin/merge.c:1600) tests before picking a default: `pull_twohead`
    /// (`ort`) for one head, `pull_octopus` for several. A named strategy is
    /// used for *both* head counts, which is why `-s ort a b` fails instead of
    /// quietly octopusing.
    strategies: Vec<Pick>,
    /// `-X`/`--strategy-option` values, kept raw. git stores them in a strvec and
    /// only runs `parse_merge_opt()` on them from inside `try_merge_strategy()`,
    /// so a bogus value is diagnosed at merge time, not at parse time.
    strategy_options: Vec<String>,
    /// `--into-name <name>`: use `<name>` instead of the real target branch when
    /// composing the merge message's ` into <name>` title (a port of git's
    /// `into_name`, which overrides `current_branch` in `fmt_merge_msg`).
    into_name: Option<String>,
    /// `--log[=<n>]`/`--no-log`, seeded from `merge.log`/`merge.summary`: how
    /// many shortlog entries to fold into the merge message. git keeps the count
    /// signed, so a negative `--log=<n>` lists nothing but still emits the block.
    log_len: i64,
    /// `merge.branchdesc` — whether `branch.<name>.description` is spliced into
    /// a merged local branch's shortlog block.
    branch_desc: bool,
    /// `-e`/`--edit`/`--no-edit`; `None` leaves git's `default_edit_option()` to
    /// decide (no message given and stdin/stdout are the same terminal).
    edit: Option<bool>,
    /// `--autostash`/`--no-autostash` and `merge.autoStash`.
    autostash: bool,
    /// `-S`/`--gpg-sign[=<keyid>]`/`--no-gpg-sign` and `commit.gpgsign`:
    /// `Some(key)` signs the merge commit, an empty key deferring to
    /// `user.signingKey` (and, failing that, the committer identity, as git's
    /// `get_signing_key()` does).
    sign: Option<String>,
    /// `--rerere-autoupdate`/`--no-rerere-autoupdate` — git's `allow_rerere_auto`,
    /// handed to `repo_rerere()` in `suggest_conflicts()`. `None` leaves
    /// `rerere.autoupdate` in charge.
    rerere_autoupdate: Option<bool>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            ff: Ff::Allow,
            no_ff_given: false,
            stat: StatMode::Diffstat,
            message: None,
            squash: false,
            commit: None,
            commit_given: false,
            signoff: false,
            verify_signatures: None,
            allow_unrelated: false,
            no_verify: false,
            quiet: false,
            cleanup: Cleanup::Default,
            strategies: Vec::new(),
            strategy_options: Vec::new(),
            into_name: None,
            log_len: -1,
            branch_desc: false,
            edit: None,
            autostash: false,
            sign: None,
            rerere_autoupdate: None,
        }
    }
}

impl Opts {
    /// `append_strategy(get_strategy(name))` (builtin/merge.c:232-243): every
    /// `-s` is appended, duplicates included — `-s ort -s ort` really does run
    /// `ort` twice, with a rewind in between.
    fn push_strategy(&mut self, strategy: Strategy, name: &str) {
        self.strategies.push(Pick::new(strategy, name));
    }

    /// `use_strategies` after `if (!use_strategies)` has filled in a default
    /// (builtin/merge.c:1601-1608). The default is chosen by the *head count*
    /// and only when no `-s` was given: `add_strategies(pull_twohead,
    /// DEFAULT_TWOHEAD)` for one head, `add_strategies(pull_octopus,
    /// DEFAULT_OCTOPUS)` for several.
    ///
    /// **`pull.twohead` and `pull.octopus`** are those two strings, read by
    /// `git_merge_config()` (builtin/merge.c:708-713). The keys are spelled
    /// `pull.*` and live in *this* command, not in `pull`: `git pull` never
    /// looks at them, it forwards its heads to `merge`, so the two settings
    /// govern `git merge` just as much as a pull. Set, the value is a
    /// *space-separated list* of strategies (`add_strategies()`,
    /// builtin/merge.c:872-889) that is tried in order until one succeeds, and
    /// it replaces the built-in default rather than adding to it. Unset,
    /// `add_strategies()` falls through to the `all_strategy[]` entries carrying
    /// the attribute — `ort` for two heads, `octopus` for more.
    ///
    /// An unknown name in the list is `get_strategy()`'s error, so a typo in
    /// `pull.twohead` fails the merge with the same two lines a bad `-s` does.
    fn picks(
        &self,
        head_count: usize,
        config: &StrategyConfig,
    ) -> std::result::Result<Vec<Pick>, ExitCode> {
        if !self.strategies.is_empty() {
            return Ok(self.strategies.clone());
        }
        let (configured, fallback) = match head_count > 1 {
            true => (config.octopus.as_deref(), Strategy::Octopus),
            false => (config.twohead.as_deref(), Strategy::Ort),
        };
        let Some(configured) = configured else {
            let name = match fallback {
                Strategy::Octopus => "octopus",
                _ => "ort",
            };
            return Ok(vec![Pick::new(fallback, name)]);
        };
        let mut picks = Vec::new();
        // `string_list_split(&list, string, " ", -1)` keeps empty fields, so a
        // value of `" "` really does ask for two nameless strategies and dies on
        // the first.
        for name in configured.split(char::from(32)) {
            picks.push(Pick::new(resolve_strategy(name)?, name));
        }
        Ok(picks)
    }

    /// `option_commit`: `--no-commit` clears it, and so does `--squash`
    /// (builtin/merge.c's `if (squash) … option_commit = 0`). It gates the whole
    /// `allow_trivial` block (builtin/merge.c:1701) and decides whether a clean
    /// strategy result is committed (builtin/merge.c:1826).
    fn option_commit(&self) -> bool {
        self.commit != Some(false) && !self.squash
    }
}

/// The two default-strategy strings `git_merge_config()` keeps
/// (builtin/merge.c:110, :708-713): `pull.octopus` for a merge with more than
/// one head, `pull.twohead` for the ordinary one-head case.
///
/// An **empty** value counts as configured, not as unset. `git_config_string()`
/// stores `""`, `add_strategies()` sees a non-NULL string and splits it, and the
/// one empty field it yields reaches `get_strategy("")` — which names no
/// strategy, so stock dies:
///
/// ```text
/// $ git -c pull.twohead= merge side
/// Could not find merge strategy ''.
/// Available strategies are: octopus ours recursive resolve subtree.
/// ```
///
/// Treating it as unset here would silently merge with `ort` instead.
#[derive(Default)]
struct StrategyConfig {
    twohead: Option<String>,
    octopus: Option<String>,
}

/// Read [`StrategyConfig`] from the repository configuration.
fn strategy_config(repo: &gix::Repository) -> StrategyConfig {
    let snapshot = repo.config_snapshot();
    let string = |key: &str| snapshot.string(key).map(|v| v.to_str_lossy().into_owned());
    StrategyConfig { twohead: string("pull.twohead"), octopus: string("pull.octopus") }
}

/// `git_merge_config()`'s `branch.<current>.mergeoptions` capture
/// (builtin/merge.c:667-674) split the way `parse_branch_merge_options()`
/// (builtin/merge.c:641-659) splits it.
///
/// `branch` is `refs_resolve_refdup(…, "HEAD", 0, …)` with `refs/heads/` stripped
/// (builtin/merge.c:1393-1397). The `0` flags matter: the ref is resolved without
/// `RESOLVE_REF_READING`, so an *unborn* branch still yields its own name, and a
/// detached HEAD yields the literal `HEAD` — which is why
/// `branch.HEAD.mergeoptions` is a real (if odd) setting.
///
/// The error text is git's, down to the lowercase `mergeoptions` spelling the
/// `die()` format string hard-codes regardless of how the key was written in the
/// file.
fn branch_merge_options() -> std::result::Result<Vec<String>, String> {
    let Ok(repo) = crate::setup::discover() else {
        return Ok(Vec::new());
    };
    let branch = match repo.head_ref() {
        Ok(Some(r)) => r.name().shorten().to_string(),
        // `git symbolic-ref HEAD` failing is a detached HEAD, where
        // `resolve_refdup` hands back the unresolved `HEAD` itself.
        _ => "HEAD".to_string(),
    };
    let Some(raw) = repo.config_snapshot().string(&format!("branch.{branch}.mergeoptions")) else {
        return Ok(Vec::new());
    };
    let raw = raw.to_string();
    match crate::alias::split_cmdline(&raw) {
        Ok(words) => Ok(words),
        Err(e) => Err(format!("Bad branch.{branch}.mergeoptions string: {e}")),
    }
}

pub fn merge(args: &[String]) -> Result<ExitCode> {
    // `Updating <a>..<b>`, `Fast-forward` and the diffstat are stdout
    // (builtin/merge.c); a refused checkout's `error: …`/`Aborting` is stderr.
    // stdio holds the stdout half off a terminal, which is why stock prints the
    // refusal first into a pipe. See `crate::cstdio`.
    crate::cstdio::defer();
    let mut op = Op::Merge;
    let mut opts = Opts::default();
    let mut refs: Vec<String> = Vec::new();
    // A pending `-F`/`--file` read, resolved after parsing so the diagnostic
    // order matches git (options first, file open second).
    let mut file: Option<String> = None;
    // git's `merge_log_config`, applied only after the options are parsed: a
    // `--log=<n>` that leaves the count negative falls back to it.
    let mut merge_log_config: i64 = 0;

    // The `git_merge_config()` defaults, applied before the CLI options below
    // override them. merge.suppressDest is consulted later, in `dest_suppressed`,
    // when the default merge message's title is composed.
    if let Ok(repo) = crate::setup::discover() {
        let snap = repo.config_snapshot();
        match snap.string("merge.ff").map(|v| v.to_string().to_ascii_lowercase()).as_deref() {
            Some("only") => opts.ff = Ff::Only,
            Some("false" | "no" | "off" | "0") => opts.ff = Ff::Never,
            Some(_) => opts.ff = Ff::Allow, // true/yes/on/1/valueless → allow
            None => {}
        }
        if let Some(mode) = stat_config(&snap) {
            opts.stat = mode;
        }
        merge_log_config = shortlog_config(&snap);
        opts.branch_desc = snap.boolean("merge.branchdesc").unwrap_or(false);
        opts.autostash = snap.boolean("merge.autoStash").unwrap_or(false);
        // `commit.gpgsign` sets git's `sign_commit` to the empty key, meaning
        // "sign, letting `get_signing_key()` choose".
        if snap.boolean("commit.gpgsign") == Some(true) {
            opts.sign = Some(String::new());
        }
        // `commit.cleanup` feeds the same `cleanup_arg` `--cleanup` sets.
        if let Some(v) = snap.string("commit.cleanup") {
            match parse_cleanup(&v.to_string()) {
                Some(mode) => opts.cleanup = mode,
                None => {
                    eprintln!("fatal: Invalid cleanup mode {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    // `parse_branch_merge_options()` (builtin/merge.c:641-659), driven from
    // :1407-1408. `branch.<current>.mergeoptions` is not a set of typed knobs but
    // a *command line*: git splits it with `split_cmdline()` and runs the whole
    // `builtin_merge_options` table over it, with `argv[0]` faked as
    // `branch.*.mergeoptions`, **before** `parse_options()` sees the real argv.
    // Both writes land in the same variables, so anything the user typed
    // afterwards wins — `branch.main.mergeoptions = --no-ff` plus a command-line
    // `--ff` fast-forwards.
    //
    // Splicing the words in front of `args` reproduces that ordering exactly,
    // because this loop is the same table applied left to right. The one thing
    // that must not carry over is a *non-option* word: `parse_options()` leaves
    // those in its own argv and `parse_branch_merge_options()` throws that argv
    // away, so `--no-ff zzjunk` merges without fast-forwarding and never treats
    // `zzjunk` as a head. `config_argc` below is the boundary that drops them.
    let (args, config_argc) = match branch_merge_options() {
        Ok(words) => {
            let n = words.len();
            (words.into_iter().chain(args.iter().cloned()).collect::<Vec<String>>(), n)
        }
        Err(msg) => {
            eprintln!("fatal: {msg}");
            return Ok(ExitCode::from(128));
        }
    };
    let args: &[String] = &args;

    let mut i = 0;
    while i < args.len() {
        // `at` is this argument's own index; `i` steps past it immediately, so it
        // is already `parse_opt_ctx_t`'s "next unread argument" and `take_value`
        // — the shared port of `get_arg()` — can advance it over a value without
        // a second cursor. The two used to be one, and every value-taking option
        // hand-rolled its own missing-value message as a result.
        let at = i;
        i += 1;
        // Respell a unique abbreviation as the name it resolves to, so `--allow-unre`
        // reaches the same arm as `--allow-unrelated-histories`.
        let canonical;
        let a = match super::canonical_long(args[at].as_str(), LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(&args[at], &first, &second, USAGE))
            }
        };
        match a {
            "--abort" => op = Op::Abort,
            "--quit" => op = Op::Quit,
            "--continue" => op = Op::Continue,
            "--ff" => opts.ff = Ff::Allow,
            "--no-ff" => {
                opts.ff = Ff::Never;
                opts.no_ff_given = true;
            }
            "--ff-only" => opts.ff = Ff::Only,
            "--stat" | "--summary" => opts.stat = StatMode::Diffstat,
            "--no-stat" | "--no-summary" | "-n" => opts.stat = StatMode::None,
            // `option_parse_compact_summary`: set → the compact summary, unset →
            // no diffstat at all (`show_diffstat = 0`), not "back to --stat".
            "--compact-summary" => opts.stat = StatMode::CompactSummary,
            "--no-compact-summary" => opts.stat = StatMode::None,
            "--squash" => opts.squash = true,
            "--no-squash" => opts.squash = false,
            "--commit" => {
                opts.commit = Some(true);
                opts.commit_given = true;
            }
            "--no-commit" => opts.commit = Some(false),
            "--signoff" => opts.signoff = true,
            "--no-signoff" => opts.signoff = false,
            "--allow-unrelated-histories" => opts.allow_unrelated = true,
            "--no-allow-unrelated-histories" => opts.allow_unrelated = false,
            "--no-verify" => opts.no_verify = true,
            "--verify" => opts.no_verify = false,
            // `--into-name <name>` / `--into-name=<name>`: override the merge
            // message's destination (port of git's `into_name`).
            "--into-name" => {
                opts.into_name = Some(super::take_value(args, &mut i, a)?.to_string())
            }
            _ if a.starts_with("--into-name=") => {
                opts.into_name = Some(a["--into-name=".len()..].to_string())
            }
            // `--no-into-name`: git's OPT_STRING negation sets `into_name` to
            // NULL, restoring the real target branch as the message destination.
            "--no-into-name" => opts.into_name = None,
            // `show_usage_with_options_if_asked()` (builtin/merge.c:1380) and
            // parse_options' own `internal_help` both answer `-h` on stdout at
            // 129, with no `error:` line.
            // `--help-all` reaches the same renderer with USAGE_FULL, which this
            // table renders identically: it has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => return Ok(super::show_usage(USAGE)),
            // `--log[=<n>]` is git's `OPT_INTEGER` with `PARSE_OPT_OPTARG` and a
            // default of DEFAULT_MERGE_LOG_LEN, so only the `=<n>` spelling takes a
            // value (`--log 5` leaves `5` as a head to merge). `--no-log` is 0.
            "--log" => opts.log_len = DEFAULT_MERGE_LOG_LEN,
            "--no-log" => opts.log_len = 0,
            _ if a.starts_with("--log=") => {
                let value = &a["--log=".len()..];
                match parse_option_int(value) {
                    Some(n) => opts.log_len = n,
                    // parse-options.c distinguishes the two failures: an empty
                    // argument never reaches `git_parse_int`.
                    None if value.is_empty() => {
                        eprintln!("error: option `log' expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                    None => {
                        eprintln!(
                            "error: option `log' expects an integer value with an optional k/m/g suffix"
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `--autostash`: stash the dirty worktree before the merge and restore
            // it afterwards (`create_autostash_ref`/`apply_autostash_ref`).
            "--autostash" => opts.autostash = true,
            "--no-autostash" => opts.autostash = false,
            // `-S`/`--gpg-sign[=<keyid>]`: an OPTARG option, so only the attached
            // spellings carry a key; `--no-gpg-sign` clears `sign_commit`.
            "-S" | "--gpg-sign" => opts.sign = Some(String::new()),
            "--no-gpg-sign" => opts.sign = None,
            _ if a.starts_with("--gpg-sign=") => {
                opts.sign = Some(a["--gpg-sign=".len()..].to_string())
            }
            _ if a.len() > 2 && a.starts_with("-S") && !a.starts_with("--") => {
                opts.sign = Some(a[2..].to_string())
            }
            // `--progress`/`--no-progress` force or suppress git's progress
            // meters. In `builtin/merge.c` `show_progress` feeds exactly one
            // consumer, `o.show_rename_progress`, which drives merge-ort's delayed
            // "Performing inexact rename detection" meter; this build's merge has
            // no such meter to force, so both spellings are accepted and change
            // nothing (see the module docs).
            "--progress" | "--no-progress" => {}
            // `--[no-]rerere-autoupdate`: git's `OPT_RERERE_AUTOUPDATE`, passed to
            // `repo_rerere()` from `suggest_conflicts()`. Set means "stage the
            // replayed resolution", unset means "leave it conflicted in the index
            // and say so"; neither spelling defers to `rerere.autoupdate`.
            "--rerere-autoupdate" => opts.rerere_autoupdate = Some(true),
            "--no-rerere-autoupdate" => opts.rerere_autoupdate = Some(false),
            // Flags whose git behaviour is already this build's default, accepted
            // as no-ops so they match stock git rather than erroring:
            //  * `--overwrite-ignore`: ignored files are overwritten (git's default).
            //  * `--no-strategy`: git's `option_parse_strategy` returns early on
            //    `unset` without clearing the strategy list, so it is a no-op that
            //    leaves any earlier `-s` in force (default `ort` when none given).
            "--overwrite-ignore" | "--no-strategy" => {}
            // `--[no-]verify-signatures` / `merge.verifySignatures`: check every
            // head's signature before merging it. `None` leaves the config to
            // decide.
            "--verify-signatures" => opts.verify_signatures = Some(true),
            "--no-verify-signatures" => opts.verify_signatures = Some(false),
            // Verbosity: git keeps a signed level; only quiet has an observable
            // effect on stdout (it silences the summary/diffstat). `--verbose`'s
            // extra diagnostics go to stderr and are not reproduced.
            "-q" | "--quiet" => opts.quiet = true,
            "-v" | "--verbose" => opts.quiet = false,
            // `-e`/`--edit`/`--no-edit`: whether the merge message is opened in an
            // editor before the merge commit is written. Left `None` here so
            // `default_edit_option()` decides (see `edit_wanted`).
            "-e" | "--edit" => opts.edit = Some(true),
            "--no-edit" => opts.edit = Some(false),
            // `-m`/`--message` accumulate into one buffer, joined by a blank line
            // (git's `option_parse_message`: `buf->len ? "\n\n" : ""`), so
            // `-m a -m b` yields the two-paragraph message `a\n\nb`.
            "-m" | "--message" => {
                // `OPT_CALLBACK('m', "message", …, option_parse_message)`. The
                // callback's own `error(_("switch `m' requires a value"))`
                // (builtin/merge.c:134) is unreachable: `OPTION_CALLBACK` runs
                // `get_arg()` first (parse-options.c:247) and returns -1 on its
                // failure, so the message is `optname()`'s and follows the
                // spelling — ``switch `m'`` for `-m`, ``option `message'`` for
                // the long form.
                let m = super::take_value(args, &mut i, a)?.to_string();
                append_message(&mut opts.message, &m);
            }
            // `--no-message`: clear the accumulated message (git's
            // `option_parse_message` on `unset` does `strbuf_setlen(buf, 0)`).
            "--no-message" => opts.message = None,
            _ if a.starts_with("--message=") => {
                append_message(&mut opts.message, &a["--message=".len()..])
            }
            _ if a.len() > 2 && a.starts_with("-m") && !a.starts_with("--") => {
                append_message(&mut opts.message, &a[2..])
            }
            // `-F`/`--file` is the one option in this table that does *not*
            // follow `optname()`. It is an `OPTION_LOWLEVEL_CALLBACK`, which
            // `do_get_value()` dispatches straight to without calling
            // `get_arg()` (parse-options.c:146-147), so the callback fetches the
            // value itself and words its own refusal:
            //
            // ```c
            //         } else
            //                 return error(_("option `%s' requires a value"),
            //                              opt->long_name);
            // ```
            //
            // (builtin/merge.c:156-157). `opt->long_name` is `file` whichever
            // spelling was typed, so stock answers `git merge -F` with
            // ``option `file' requires a value`` and not ``switch `F'``.
            "-F" | "--file" => {
                file = Some(
                    crate::parseopt::get_arg(args, &mut i, crate::parseopt::OptName::Long("file"))?
                        .to_string(),
                )
            }
            _ if a.starts_with("--file=") => file = Some(a["--file=".len()..].to_string()),
            _ if a.len() > 2 && a.starts_with("-F") && !a.starts_with("--") => {
                file = Some(a[2..].to_string())
            }
            // `OPT_CLEANUP` is an `OPT_STRING`, so a missing value is
            // `get_arg()`'s refusal and never reaches `get_cleanup_mode()`.
            // Reading it as `args.get(i).unwrap_or("")` made the absent value an
            // empty one, which stock never sees: `git merge --cleanup` answered
            // `fatal: Invalid cleanup mode ` at 128 instead of
            // ``error: option `cleanup' requires a value`` at 129.
            "--cleanup" => {
                let mode = super::take_value(args, &mut i, a)?;
                match parse_cleanup(mode) {
                    Some(mode) => opts.cleanup = mode,
                    None => {
                        eprintln!("fatal: Invalid cleanup mode {mode}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            _ if a.starts_with("--cleanup=") => match parse_cleanup(&a["--cleanup=".len()..]) {
                Some(mode) => opts.cleanup = mode,
                None => {
                    eprintln!("fatal: Invalid cleanup mode {}", &a["--cleanup=".len()..]);
                    return Ok(ExitCode::from(128));
                }
            },
            // `--no-cleanup`: git's OPT_CLEANUP is an OPT_STRING, so the negation
            // sets `cleanup_arg` to NULL and `get_cleanup_mode(NULL, 0)` returns
            // the default (`whitespace` without an editor) — our `Cleanup::Default`.
            "--no-cleanup" => opts.cleanup = Cleanup::Default,
            "-s" | "--strategy" => {
                let name = super::take_value(args, &mut i, a)?.to_string();
                match resolve_strategy(&name) {
                    Ok(s) => opts.push_strategy(s, &name),
                    Err(code) => return Ok(code),
                }
            }
            _ if a.starts_with("--strategy=") => {
                let name = &a["--strategy=".len()..];
                match resolve_strategy(name) {
                    Ok(s) => opts.push_strategy(s, name),
                    Err(code) => return Ok(code),
                }
            }
            _ if a.len() > 2 && a.starts_with("-s") && !a.starts_with("--") => {
                let name = &a[2..];
                match resolve_strategy(name) {
                    Ok(s) => opts.push_strategy(s, name),
                    Err(code) => return Ok(code),
                }
            }
            // `-X`/`--strategy-option` is git's `OPT_STRVEC`, so every value is
            // appended and applied in order. The value is only *interpreted* once
            // the `ort` strategy actually runs (see `strategy_options` above).
            "-X" | "--strategy-option" => {
                let v = super::take_value(args, &mut i, a)?.to_string();
                opts.strategy_options.push(v);
            }
            _ if a.starts_with("--strategy-option=") => opts
                .strategy_options
                .push(a["--strategy-option=".len()..].to_string()),
            _ if a.len() > 2 && a.starts_with("-X") && !a.starts_with("--") => {
                opts.strategy_options.push(a[2..].to_string())
            }
            // A long name no table entry claims is `parse_options()`' own refusal —
            // the `error:` line and the block, both on stderr, exit 129 — not a gap
            // in this port. It has to be decided against the table rather than by
            // spelling, because `--ff-only` and `-F`/`--file` are `PARSE_OPT_NONEG`
            // and so have no `--no-` form for parse-options to resolve.
            _ if a.starts_with("--")
                && matches!(
                    super::resolve_long(LONG_OPTS, &a[2..]),
                    super::Resolved::Unknown
                ) =>
            {
                return Ok(super::unknown_option(a, USAGE));
            }
            // Every remaining `-<chars>` token, walked the way
            // `parse_options_step()` walks a short cluster (parse-options.c:
            // 1061-1107): each character is its own option, a value-taking one
            // swallows the rest of the token or the next argv element, and the
            // first character the table does not claim is `PARSE_OPT_UNKNOWN` —
            // reported against the synthetic `-<rest>` the C builds at :1095, so
            // `git merge -nZ` names `Z` and not `n`. The single-character
            // spellings are all matched above; what reaches here is a cluster
            // (`-nq`) or an unknown switch (`-o`), and both used to be
            // `zvcs: merge: unsupported flag …` at exit 1.
            _ if a.len() > 1 && a.starts_with('-') && !a.starts_with("--") => {
                for (off, c) in a.char_indices().skip(1) {
                    let rest = &a[off + c.len_utf8()..];
                    match c {
                        'n' => opts.stat = StatMode::None,
                        'e' => opts.edit = Some(true),
                        'q' => opts.quiet = true,
                        'v' => opts.quiet = false,
                        // `OPT_BOOL('S', "gpg-sign", …)` is `PARSE_OPT_OPTARG`:
                        // an attached key is the value, nothing attached is the
                        // default (sign with the configured key).
                        'S' => {
                            opts.sign = Some(rest.to_string());
                            break;
                        }
                        'm' => {
                            let m = match rest.is_empty() {
                                true => super::take_value(args, &mut i, "-m")?.to_string(),
                                false => rest.to_string(),
                            };
                            append_message(&mut opts.message, &m);
                            break;
                        }
                        // Named `option \`file'` even here — see the `-F` arm.
                        'F' => {
                            file = Some(match rest.is_empty() {
                                true => crate::parseopt::get_arg(
                                    args,
                                    &mut i,
                                    crate::parseopt::OptName::Long("file"),
                                )?
                                .to_string(),
                                false => rest.to_string(),
                            });
                            break;
                        }
                        's' => {
                            let name = match rest.is_empty() {
                                true => super::take_value(args, &mut i, "-s")?.to_string(),
                                false => rest.to_string(),
                            };
                            match resolve_strategy(&name) {
                                Ok(s) => opts.push_strategy(s, &name),
                                Err(code) => return Ok(code),
                            }
                            break;
                        }
                        'X' => {
                            let v = match rest.is_empty() {
                                true => super::take_value(args, &mut i, "-X")?.to_string(),
                                false => rest.to_string(),
                            };
                            opts.strategy_options.push(v);
                            break;
                        }
                        // `internal_help`: the block on stdout at 129, reached as
                        // soon as the first character the table does not define
                        // is `h`.
                        'h' => return Ok(super::show_usage(USAGE)),
                        _ => return Ok(super::unknown_option(&format!("-{}", &a[off..]), USAGE)),
                    }
                }
            }
            // A head to merge — unless it came out of `branch.<n>.mergeoptions`,
            // whose leftover non-options `parse_branch_merge_options()` discards
            // along with the argv it parsed them into.
            _ => {
                if at >= config_argc {
                    refs.push(a.to_string());
                }
            }
        }
    }

    // `if (shortlog_len < 0) shortlog_len = (merge_log_config > 0) ? … : 0;` —
    // an unset (or negative) `--log` count defers to `merge.log`/`merge.summary`,
    // and a negative config value means no shortlog at all.
    if opts.log_len < 0 {
        opts.log_len = if merge_log_config > 0 { merge_log_config } else { 0 };
    }

    // `-F <path>` — read now, after option parsing. `-` and an empty value are
    // stdin, matching git's `read_from_file`/`fix_filename`.
    if let Some(path) = file {
        let data = if path == "-" || path.is_empty() {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut buf)?;
            buf
        } else {
            match std::fs::read(&path) {
                Ok(buf) => buf,
                Err(e) => {
                    eprintln!("fatal: could not open '{path}' for reading: {}", strerror(&e));
                    return Ok(ExitCode::from(128));
                }
            }
        };
        opts.message = Some(String::from_utf8_lossy(&data).into_owned());
    }

    match op {
        // git: `--abort`/`--quit`/`--continue` expect no arguments.
        Op::Abort | Op::Quit | Op::Continue if !refs.is_empty() => {
            let which = match op {
                Op::Abort => "--abort",
                Op::Quit => "--quit",
                _ => "--continue",
            };
            eprintln!("fatal: {which} expects no arguments");
            Ok(ExitCode::from(129))
        }
        Op::Abort => abort(),
        Op::Quit => quit(),
        Op::Continue => continue_merge(&opts),
        Op::Merge => {
            // git's `builtin/merge.c` incompatibility checks, keyed off the literal
            // flags. `--squash` cannot fast-forward, so it clashes with `--no-ff`,
            // and it never commits, so it clashes with `--commit`.
            if opts.squash && opts.commit_given {
                eprintln!("fatal: options '--squash' and '--commit.' cannot be used together");
                return Ok(ExitCode::from(128));
            }
            if opts.squash && opts.no_ff_given {
                eprintln!("fatal: options '--squash' and '--no-ff.' cannot be used together");
                return Ok(ExitCode::from(128));
            }
            // `for (i = 0; i < use_strategies_nr; i++) if (… & NO_FAST_FORWARD)
            // fast_forward = FF_NO;` (builtin/merge.c:1608-1610). `subtree` and
            // `ours` carry that attribute (builtin/merge.c:106-107), so they
            // always record a merge commit. It happens *after* the `--squash`
            // checks above, which is why `--squash -s subtree` is accepted where
            // `--squash --no-ff` dies.
            //
            // The loop is over the whole `-s` list, not over the strategy that
            // ends up answering the merge: `git merge -s ort -s ours <ff-able>`
            // records a merge commit made by `ort`, because `ours` was in the
            // list at the time `fast_forward` was decided (measured against
            // stock 2.55.0). The defaults `if (!use_strategies)` fills in — `ort`
            // and `octopus` — carry neither attribute, so an empty list leaves
            // `fast_forward` alone whatever the head count.
            if opts.strategies.iter().any(|p| p.kind.no_fast_forward()) {
                opts.ff = Ff::Never;
            }
            do_merge(&refs, &opts)
        }
    }
}

// ---------------------------------------------------------------------------
// --abort / --quit
// ---------------------------------------------------------------------------

/// The state files `remove_merge_branch_state()` (branch.c) unlinks.
const MERGE_STATE_FILES: &[&str] = &["MERGE_HEAD", "MERGE_RR", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"];

/// The extra state `remove_branch_state()` unlinks on top of the merge state;
/// `git merge --abort` reaches it by running `git reset --merge`.
const BRANCH_STATE_FILES: &[&str] = &["SQUASH_MSG", "CHERRY_PICK_HEAD", "REVERT_HEAD"];

/// `restore_state()` (builtin/merge.c:403-427): rewind the index and worktree to
/// `head`, then put the `save_state()` snapshot back on top.
///
/// ```c
/// reset_hard(head);                       /* git read-tree -v --reset -u <head> */
/// if (is_null_oid(stash)) goto refresh_cache;
/// git stash apply --index --quiet <stash> /* errors deliberately ignored */
/// ```
///
/// Both halves run as the commands git runs them as, in process. The `stash
/// apply` is itself a merge-ort merge, so it records its own result as
/// `AUTO_MERGE` — which is why a failed merge over a dirty worktree leaves that
/// file behind pointing at the snapshot's tree. A clean worktree is snapshotted
/// as nothing, so no stash is applied and no `AUTO_MERGE` is written.
///
/// Measured on stock 2.55.0 and on this port: `stash create` → `read-tree -v
/// --reset -u HEAD` → `stash apply --index <oid>` round-trips a staged change,
/// an unstaged change and an untracked file byte-for-byte in both, and leaves
/// the same `AUTO_MERGE`. Nothing here is an approximation of the rewind: it is
/// the rewind, which is what lets a second `-s` start from a pristine tree.
fn restore_state(head: ObjectId, snapshot: Option<ObjectId>) -> Result<()> {
    let reset = ["-v", "--reset", "-u", &head.to_string()].map(str::to_string);
    super::read_tree::read_tree(&reset)?;
    let Some(commit) = snapshot else { return Ok(()) };
    let apply = ["apply", "--index", "--quiet", &commit.to_string()].map(str::to_string);
    // "It is OK to ignore error here, for example when there was nothing to
    // restore." (builtin/merge.c:415-418)
    // git applies a merge autostash by spawning `git stash apply`
    // (`apply_autostash_oid()`), so `start_command()`'s `fflush(NULL)`
    // (run-command.c:743) puts everything buffered so far out ahead of it. This
    // port runs `stash` in-process; the flush is what keeps the order the same.
    crate::cstdio::before_spawn();
    let _ = super::stash::stash(&apply);
    Ok(())
}

/// `evaluate_result()` (builtin/merge.c:1070-1091): how badly a strategy did,
/// as the number of paths the user still has to look at.
///
/// ```c
/// run_diff_files(&rev, 0);              /* how many files differ */
/// cnt += count_unmerged_entries();      /* plus every unmerged index entry */
/// ```
///
/// `run_diff_files()` is `git diff-files` — the index against the worktree — and
/// its callback counts one per queued filepair. `count_unmerged_entries()` walks
/// the index counting entries with a non-zero stage, so a path conflicted at all
/// three stages contributes three. git only calls this when several `-s` are in
/// play (builtin/merge.c:1814); with one strategy the score is a constant 0.
fn evaluate_result(repo: &gix::Repository) -> Result<i64> {
    use gix::status::index_worktree::Item as Iw;
    use gix::status::plumbing::index_as_worktree::EntryStatus;

    // `run_diff_files()` walks the index against the worktree with no rename
    // detection and no dirwalk — an untracked file is not a filepair — and
    // queues one pair per path that differs, an unmerged path included.
    let mut cnt: i64 = 0;
    let iter = repo
        .status(gix::progress::Discard)?
        .index_worktree_rewrites(None)
        .untracked_files(gix::status::UntrackedFiles::None)
        .index_worktree_options_mut(|opts| opts.dirwalk_options = None)
        .into_iter(Vec::new())?;
    for item in iter {
        // The platform also yields `TreeIndex` (HEAD↔index) items; `run_diff_files`
        // never looks at those, so they are not counted.
        if let gix::status::Item::IndexWorktree(Iw::Modification { status, .. }) = item? {
            match status {
                EntryStatus::Change(_) | EntryStatus::Conflict { .. } | EntryStatus::IntentToAdd => {
                    cnt += 1
                }
                EntryStatus::NeedsUpdate(_) => {}
            }
        }
    }
    // `count_unmerged_entries()`: one per index entry at a non-zero stage, so a
    // path conflicted at all three stages contributes three.
    let index = repo.open_index()?;
    cnt += index.entries().iter().filter(|e| e.stage() != Stage::Unconflicted).count() as i64;
    Ok(cnt)
}

/// The paths the index still holds at a non-zero stage, in index order and
/// without repeats — what `append_conflicts_hint()` lists under `# Conflicts:`.
fn unmerged_paths(index: &gix::index::File) -> Vec<BString> {
    let backing = index.path_backing().to_owned();
    let mut paths: Vec<BString> = Vec::new();
    for entry in index.entries() {
        if entry.stage() == Stage::Unconflicted {
            continue;
        }
        let path = entry.path_in(&backing).to_owned();
        if paths.last() != Some(&path) {
            paths.push(path);
        }
    }
    paths
}

/// Everything `try_merge_strategy()` is given that does not change between
/// attempts. One struct because git's strategy loop hands the same `common`,
/// `remoteheads` and `head_commit` to every attempt (builtin/merge.c:1796-1798),
/// and re-deriving them per strategy is how the two head counts drifted apart.
struct MergeCtx<'a> {
    /// The specs the *message* names each head with — `msg_specs`, which is the
    /// `FETCH_HEAD` descriptions for a pull and the operands as typed otherwise.
    refs: &'a [String],
    /// What a strategy is handed for each head, which is what `git-merge-octopus`
    /// echoes in its per-head lines: object ids for a `FETCH_HEAD` merge.
    head_labels: &'a [String],
    targets: &'a [ObjectId],
    local_id: ObjectId,
    head_tree: ObjectId,
    branch: Option<&'a FullName>,
    reflog_spec: &'a str,
    /// `common`: the merge bases of the two-head merge, empty for unrelated
    /// histories and unused by the octopus (which re-derives one per head).
    bases: &'a [ObjectId],
    /// The operand as typed, which labels the `>>>>>>>` side of a conflict.
    spec: &'a str,
    /// Whether `HEAD` is *not* one of the merge bases, i.e. whether the histories
    /// really diverged. `--no-ff` reaches a strategy without it.
    diverged: bool,
    /// The composed merge message. git builds it in `collect_parents()` before
    /// any strategy runs, so every attempt commits the same text.
    message: String,
}

/// What one `try_merge_strategy()` call reported (builtin/merge.c:789-851).
///
/// > The backend exits with 1 when conflicts are left to be resolved, with 2
/// > when it does not handle the given merge at all.
enum Attempt {
    /// `ret == 0`: the strategy left its result in the index and the worktree.
    Clean {
        /// The tree `write_tree_trivial()` derives from that index
        /// (builtin/merge.c:1025).
        tree: ObjectId,
        /// The heads `write_merge_heads()` records and `--squash` summarises.
        heads: Vec<ObjectId>,
        /// `mrc` when the parents are not `HEAD` plus every head: an octopus
        /// whose first head fast-forwarded past `HEAD` *replaces* it there.
        parents_override: Option<Vec<ObjectId>>,
        /// What `reflog_action()` records for this strategy's commit.
        spec_label: String,
    },
    /// `ret == 1`: conflicts left in the index for the user to resolve.
    Conflicts(Vec<BString>),
    /// `ret == 2`: the strategy does not handle this merge at all.
    Refused,
    /// Not one of git's three: the attempt answered the whole merge itself and
    /// there is nothing left for the loop or its tail to decide — an octopus
    /// that only fast-forwarded, or `die(_("unknown strategy option: -X%s"))`.
    Done { code: ExitCode, autostash_applied: bool },
}

/// The strategy loop and the tail that picks its winner
/// (builtin/merge.c:1778-1875).
///
/// ```c
/// if (save_state(&stash)) oidclr(&stash, …);
/// for (i = 0; i < use_strategies_nr; i++) {
///         if (i) { printf(_("Rewinding the tree to pristine...\n")); restore_state(…); }
///         if (use_strategies_nr != 1) printf(_("Trying merge strategy %s...\n"), …);
///         wt_strategy = use_strategies[i]->name;
///         ret = try_merge_strategy(wt_strategy, common, remoteheads, head_commit);
///         if (ret < 2) {
///                 if (!ret) { merge_was_ok = 1; best_strategy = wt_strategy; break; }
///                 cnt = (use_strategies_nr > 1) ? evaluate_result() : 0;
///                 if (best_cnt <= 0 || cnt <= best_cnt) { best_strategy = wt_strategy; best_cnt = cnt; }
///         }
/// }
/// ```
///
/// `cnt <= best_cnt` rather than `<`, so on a tie the *later* strategy wins and
/// its result is already in the worktree — which is why `git merge -s ort -s
/// resolve` on a fixture where both conflict on the same two files leaves
/// `resolve`'s `.merge_file_XXXXXX` conflict labels and prints no
/// `Using the … strategy` line (measured against stock 2.55.0).
///
/// One strategy is the degenerate case of the same loop: no `Trying`/`Rewinding`
/// line, `evaluate_result()` never runs, and the tail always finds
/// `best_strategy == wt_strategy`.
fn merge_with_strategies(
    repo: &gix::Repository,
    ctx: &MergeCtx<'_>,
    opts: &Opts,
    picks: &[Pick],
) -> Result<ExitCode> {
    // `create_autostash_ref()` and `save_state()` (builtin/merge.c:1759-1778)
    // bracket the whole loop, not each attempt: one snapshot is what every
    // `restore_state()` below rewinds to.
    let stash = begin_autostash(repo, opts)?;
    // Same `fflush(NULL)` as above: git reaches `stash create` through
    // `run_command()` too.
    crate::cstdio::before_spawn();
    let snapshot = super::stash::create_snapshot(repo)?;

    let mut best: Option<usize> = None;
    let mut best_cnt: i64 = -1;
    let mut wt: usize = 0;
    let mut result: Option<Attempt> = None;

    for (i, pick) in picks.iter().enumerate() {
        if i > 0 {
            println!("Rewinding the tree to pristine...");
            restore_state(ctx.local_id, snapshot)?;
        }
        if picks.len() != 1 {
            println!("Trying merge strategy {}...", pick.name);
        }
        // "Remember which strategy left the state in the working tree."
        wt = i;
        match try_merge_strategy(repo, pick, ctx, opts)? {
            Attempt::Done { code, autostash_applied } => {
                end_autostash(repo, stash, autostash_applied)?;
                return Ok(code);
            }
            // "This strategy worked; no point in trying another." git's
            // `merge_was_ok` flag is `result` holding a `Clean` here: nothing
            // else can overwrite it, because the loop breaks on the spot.
            clean @ Attempt::Clean { .. } => {
                best = Some(i);
                result = Some(clean);
                break;
            }
            conflicts @ Attempt::Conflicts(_) => {
                let cnt = if picks.len() > 1 { evaluate_result(repo)? } else { 0 };
                if best_cnt <= 0 || cnt <= best_cnt {
                    best = Some(i);
                    best_cnt = cnt;
                    result = Some(conflicts);
                }
            }
            Attempt::Refused => {}
        }
    }

    // "If we have a resulting tree, that means the strategy module auto resolved
    // the merge cleanly." `finalize_clean` covers `finish_automerge()` and the
    // `!option_commit` tail below it alike — `--squash` and `--no-commit` stop
    // inside it with the same `Automatic merge went well; stopped before
    // committing as requested` git prints at builtin/merge.c:1868-1870.
    if let Some(Attempt::Clean { tree, heads, parents_override, spec_label }) = &result {
        return finalize_clean(
            repo,
            ctx.local_id,
            heads,
            parents_override.as_deref(),
            ctx.message.clone(),
            *tree,
            ctx.head_tree,
            opts,
            &format!("Merge made by the '{}' strategy.", picks[best.unwrap_or(wt)].name),
            spec_label,
            stash,
        );
    }

    // "Pick the result from the best strategy and have the user fix it up."
    let Some(best) = best else {
        restore_state(ctx.local_id, snapshot)?;
        if picks.len() > 1 {
            eprintln!("No merge strategy handled the merge.");
        } else {
            eprintln!("Merge with strategy {} failed.", picks[0].name);
        }
        end_autostash(repo, stash, false)?;
        return Ok(ExitCode::from(2));
    };

    let conflicts = if best == wt {
        // "We already have its result in the working tree."
        match result {
            Some(Attempt::Conflicts(c)) => c,
            _ => unreachable!("a scored attempt is always a conflicted one"),
        }
    } else {
        println!("Rewinding the tree to pristine...");
        restore_state(ctx.local_id, snapshot)?;
        println!("Using the {} strategy to prepare resolving by hand.", picks[best].name);
        match try_merge_strategy(repo, &picks[best], ctx, opts)? {
            Attempt::Conflicts(c) => c,
            // The re-run is over the same trees the scoring run saw, so it can
            // only land where that one did. Anything else means the rewind did
            // not restore the state the score was taken from.
            _ => unreachable!("the best strategy conflicted once and cannot now do otherwise"),
        }
    };

    stop_for_conflicts(repo, ctx.local_id, ctx.targets, ctx.message.clone(), &conflicts, opts, stash)
}

/// `try_merge_strategy()` (builtin/merge.c:789-851): run one strategy over the
/// index and worktree and report which of git's three statuses it returned.
///
/// The dispatch is git's: `recursive`, `subtree` and `ort` share the
/// `merge_ort_recursive()` branch, everything else goes to
/// `try_merge_command()`. `parse_merge_opt()` runs *here*, per attempt and only
/// on the merge-ort branch (builtin/merge.c:815-823) — which is why a bogus `-X`
/// survives `Already up to date.`, a plain fast-forward and `-s resolve`, and
/// why `-s subtree`'s automatic shift is a seed a later `-Xsubtree=<path>` can
/// still override.
fn try_merge_strategy(
    repo: &gix::Repository,
    pick: &Pick,
    ctx: &MergeCtx<'_>,
    opts: &Opts,
) -> Result<Attempt> {
    if pick.kind.is_ort() {
        // `if (remoteheads->next) { error(…); return 2; }` (builtin/merge.c:809-812)
        if ctx.targets.len() > 1 {
            eprintln!("error: Not handling anything other than two heads merge.");
            return Ok(Attempt::Refused);
        }
        let seed = crate::merge_apply::StrategyOptions {
            subtree_shift: (pick.kind == Strategy::Subtree).then(BString::default),
            ..Default::default()
        };
        let xopts = match crate::merge_apply::StrategyOptions::parse_from(seed, &opts.strategy_options) {
            Ok(x) => x,
            // `die(_("unknown strategy option: -X%s"), xopts.v[x])`
            Err(e) => {
                eprintln!("fatal: {e}");
                return Ok(Attempt::Done { code: ExitCode::from(128), autostash_applied: false });
            }
        };
        return ort_attempt(repo, ctx, opts, &xopts);
    }
    match pick.kind {
        Strategy::Ours => ours_attempt(repo, ctx),
        Strategy::Resolve => resolve_attempt(repo, ctx, opts),
        Strategy::Octopus => octopus_attempt(repo, ctx, opts),
        Strategy::Ort | Strategy::Subtree => unreachable!("handled by the merge-ort branch above"),
    }
}

fn remove_merge_state(git_dir: &Path, and_branch_state: bool) {
    for name in MERGE_STATE_FILES {
        let _ = std::fs::remove_file(git_dir.join(name));
    }
    if and_branch_state {
        for name in BRANCH_STATE_FILES {
            let _ = std::fs::remove_file(git_dir.join(name));
        }
        let _ = std::fs::remove_dir_all(git_dir.join("sequencer"));
    }
}

/// `git merge --quit`: forget the in-progress merge, leaving index and worktree
/// exactly as they are.
fn quit() -> Result<ExitCode> {
    let repo = crate::setup::discover()?;
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    remove_merge_state(repo.git_dir(), false);
    Ok(ExitCode::SUCCESS)
}

/// `git merge --abort`: `git reset --merge` plus dropping the merge state.
///
/// The reset is confined to the paths the merge touched — every path that has a
/// conflicted stage, or whose index entry disagrees with `HEAD` — so unrelated
/// local modifications and untracked files survive, as they do under git.
fn abort() -> Result<ExitCode> {
    let repo = crate::setup::discover()?;
    if !repo.git_dir().join("MERGE_HEAD").exists() {
        eprintln!("fatal: There is no merge to abort (MERGE_HEAD missing).");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let head = repo.head()?;
    let head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    let head_tree = repo.find_object(head_id)?.peel_to_tree()?.id;

    let old_index = repo.index_or_load_from_head()?.into_owned();
    let should_interrupt = AtomicBool::new(false);
    update_worktree(&repo, &old_index, None, head_tree, &should_interrupt)?;

    // git's `reset_refs()` records the pre-reset HEAD in ORIG_HEAD, and the reset it
    // performs (`--merge` to HEAD) logs on `HEAD` even though the branch does not move.
    set_orig_head(&repo, head_id)?;
    super::checkout::record_head_move(&repo, Some(head_id), Some(head_id), "reset: moving to HEAD");
    remove_merge_state(repo.git_dir(), true);

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

/// Port of `verify_merge_signature()` (commit.c). Returns `Some(exit)` for each
/// `die()` — a bad signature, a good-but-untrusted one, or no signature at all —
/// and prints the `has a good GPG signature by` line on the accepting path
/// unless `-q` lowered the verbosity below zero.
///
/// git quotes gpg's *signer name* (`sigc->signer`, the text after the key id on
/// the `GOODSIG`/`BADSIG` status line), not the key id, hence
/// [`crate::gitsig::evaluate_full`].
pub(super) fn verify_merge_signature(
    repo: &gix::Repository,
    id: ObjectId,
    quiet: bool,
    check_trust: bool,
) -> Result<Option<ExitCode>> {
    use crate::gitsig::{GStatus, Trust};

    let hex = id.attach(repo).shorten_or_id().to_string();
    let raw = repo.find_object(id)?.data.clone();
    let sig = crate::gitsig::evaluate_full(&raw);

    // The C switches on `sigc->result`, and every case but 'G' and 'B' — including
    // the untrusted/expired/revoked codes — falls into the same `default: /* 'N' */`
    // arm, so an unverifiable signature reports as an absent one.
    match sig.status {
        GStatus::Good | GStatus::GoodUnknown => {
            if check_trust && sig.trust < Trust::Marginal {
                eprintln!(
                    "fatal: Commit {hex} has an untrusted GPG signature, allegedly by {}.",
                    sig.signer
                );
                return Ok(Some(ExitCode::from(128)));
            }
        }
        GStatus::Bad => {
            eprintln!(
                "fatal: Commit {hex} has a bad GPG signature allegedly by {}.",
                sig.signer
            );
            return Ok(Some(ExitCode::from(128)));
        }
        _ => {
            eprintln!("fatal: Commit {hex} does not have a GPG signature.");
            return Ok(Some(ExitCode::from(128)));
        }
    }
    if !quiet {
        println!("Commit {hex} has a good GPG signature by {}", sig.signer);
    }
    Ok(None)
}

/// `merge_options.verbosity`, as `init_merge_options()` resolves it: the
/// built-in default of 2, overridden by `merge.verbosity`, then by the
/// `GIT_MERGE_VERBOSITY` environment variable (`strtol`, so trailing garbage is
/// ignored and an unparsable value reads as 0). `merge-ort-wrappers.c` turns it
/// into `show_msgs = !!verbosity`, which is the only part of the scale this
/// build's output surface can express: levels 1–5 all print the same
/// `Auto-merging` / `CONFLICT (…)` block, level 0 prints none of it.
fn merge_verbosity(repo: &gix::Repository) -> i64 {
    if let Ok(env) = std::env::var("GIT_MERGE_VERBOSITY") {
        let digits = env.trim_start();
        let end = digits
            .char_indices()
            .position(|(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))))
            .unwrap_or(digits.len());
        return digits[..end].parse().unwrap_or(0);
    }
    repo.config_snapshot().integer("merge.verbosity").unwrap_or(2)
}

/// Port of `setup_with_upstream()`: with no commit on the command line and
/// `merge.defaultToUpstream` on, `git merge` merges the current branch's
/// configured upstream — `branch.<name>.merge` mapped through
/// `remote.<remote>.fetch` to its remote-tracking ref. `Err` carries git's
/// `die()` text (without the `fatal: ` prefix) for each way that can fail.
fn setup_with_upstream(repo: &gix::Repository) -> std::result::Result<Vec<String>, String> {
    use gix::bstr::ByteSlice;

    let head = repo.head_ref().ok().flatten();
    let full = head
        .as_ref()
        .map(|r| r.name().to_owned())
        .ok_or_else(|| "No current branch.".to_string())?;
    let name = full.shorten().to_owned();

    let snap = repo.config_snapshot();
    if snap.string(format!("branch.{name}.remote").as_str()).is_none() {
        return Err("No remote for the current branch.".into());
    }
    // `branch->merge_nr`: the `branch.<name>.merge` entries, in config order.
    let sources: Vec<gix::bstr::BString> = snap
        .plumbing()
        .values::<gix::bstr::BString>(format!("branch.{name}.merge").as_str())
        .unwrap_or_default();
    if sources.is_empty() {
        return Err("No default upstream defined for the current branch.".into());
    }

    // `branch->merge[i]->dst`, i.e. the remote-tracking ref the refspec maps the
    // source to. gix resolves the whole `branch.<name>.{remote,merge}` pair at
    // once, so a single source (git's overwhelmingly common case) is looked up
    // directly; git reports the unmapped ones by name.
    match repo.branch_remote_tracking_ref_name(full.as_ref(), gix::remote::Direction::Fetch) {
        Some(Ok(dst)) if sources.len() == 1 => Ok(vec![dst.as_bstr().to_string()]),
        _ => {
            let remote = snap
                .string(format!("branch.{name}.remote").as_str())
                .map(|v| v.to_str_lossy().into_owned())
                .unwrap_or_default();
            Err(format!(
                "No remote-tracking branch for {} from {remote}",
                sources[0]
            ))
        }
    }
}

fn do_merge(refs: &[String], opts: &Opts) -> Result<ExitCode> {
    let mut repo = crate::setup::discover()?;
    // Moving `HEAD` writes a reflog, and a fast-forward has already touched the
    // worktree by then — a failure there would leave the merge half-applied. git
    // synthesizes an identity for reflog purposes rather than failing.
    crate::ensure_reflog_identity(&mut repo);
    let repo = repo;

    // `if (repo_read_index_unmerged(...)) die_resolve_conflict("merge")`
    // (builtin/merge.c:1472-1473) — the first thing `cmd_merge` does once the
    // `--abort`/`--quit`/`--continue` modes are out of the way, and the reason a
    // second `git merge` after a conflicted one refuses instead of merging.
    // `error_resolve_conflict()` (advice.c:200-225) prints the `error:` line
    // unconditionally and the two-line direction only under
    // `advice.resolveConflict`; `die_resolve_conflict()` adds the `fatal:`.
    let precheck_index = repo.open_index()?;
    if precheck_index.entries().iter().any(|e| e.stage_raw() != 0) {
        eprintln!("error: Merging is not possible because you have unmerged files.");
        crate::advice::Advice::ResolveConflict.advise_plain(
            "Fix them up in the work tree, and then use 'git add/rm <file>'\n\
             as appropriate to mark resolution and make a commit.",
        );
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        return Ok(ExitCode::from(128));
    }
    drop(precheck_index);

    // builtin/merge.c:1475-1485. Reached only with a *resolved* index, which is
    // why the advice says `commit` rather than `add/rm`. Both lines come out of
    // one `die()`, so the second carries no `hint:` prefix — but it is still
    // gated on `advice.resolveConflict`.
    if repo.git_dir().join("MERGE_HEAD").exists() {
        eprintln!("fatal: You have not concluded your merge (MERGE_HEAD exists).");
        if crate::advice::Advice::ResolveConflict.enabled_in(&repo) {
            eprintln!("Please, commit your changes before you merge.");
        }
        return Ok(ExitCode::from(128));
    }

    // builtin/merge.c:1486-1492, the same shape for an unfinished cherry-pick.
    if repo.git_dir().join("CHERRY_PICK_HEAD").exists() {
        eprintln!("fatal: You have not concluded your cherry-pick (CHERRY_PICK_HEAD exists).");
        if crate::advice::Advice::ResolveConflict.enabled_in(&repo) {
            eprintln!("Please, commit your changes before you merge.");
        }
        return Ok(ExitCode::from(128));
    }

    // `if (!argc) { if (default_to_upstream) argc = setup_with_upstream(&argv);
    // else die(...); }` — the sole reader of `merge.defaultToUpstream`, which
    // git defaults to true.
    let upstream_refs;
    let refs: &[String] = if refs.is_empty() {
        if repo.config_snapshot().boolean("merge.defaultToUpstream") == Some(false) {
            eprintln!("fatal: No commit specified and merge.defaultToUpstream not set.");
            return Ok(ExitCode::from(128));
        }
        match setup_with_upstream(&repo) {
            Ok(v) => {
                upstream_refs = v;
                &upstream_refs
            }
            Err(msg) => {
                eprintln!("fatal: {msg}");
                return Ok(ExitCode::from(128));
            }
        }
    } else {
        refs
    };

    // Current HEAD state. An unborn branch has no commit to fast-forward from;
    // a real merge into it would be a checkout, which is out of scope.
    let head = repo.head()?;
    if head.is_unborn() {
        crate::git_fatal!("cannot merge into an unborn branch");
    }
    let local_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    // Owned branch name when attached; `None` when detached. The ref to move is
    // always `HEAD` itself — see [`advance`] — so the branch name is needed only
    // for the merge message.
    let branch: Option<FullName> = head.referent_name().map(std::borrow::ToOwned::to_owned);

    // `git merge FETCH_HEAD` — the form `pull` runs — is `handle_fetch_head()`
    // (builtin/merge.c): the heads are the *for-merge* lines of `.git/FETCH_HEAD`,
    // however many there are, and the description each line carries is what the
    // message is built from. That is why a pull records
    // `Merge branch 'main' of <url>` and not the name of a tracking ref.
    let fetch_head = match refs.len() == 1 && refs[0] == "FETCH_HEAD" {
        true => fetch_head_for_merge(&repo)?,
        false => Vec::new(),
    };
    // What the message names each head as: the FETCH_HEAD descriptions when they
    // are what we merged, and the specs as typed otherwise. The specs themselves
    // stay in `refs` for the reflog and the conflict labels, which git writes
    // from the command line rather than from FETCH_HEAD.
    let msg_specs: Vec<String> = match fetch_head.is_empty() {
        true => refs.to_vec(),
        false => fetch_head.iter().map(|(_, described)| described.clone()).collect(),
    };
    // What a *strategy* was handed, which is what `git-merge-octopus` echoes in
    // its per-head lines: the object id for a FETCH_HEAD merge, since that is
    // what `cmd_merge()` passes on, and the spec as typed otherwise.
    let head_labels: Vec<String> = match fetch_head.is_empty() {
        true => refs.to_vec(),
        false => fetch_head.iter().map(|(id, _)| id.to_string()).collect(),
    };

    // Resolve every ref to merge and peel it to a commit (tags included).
    let mut targets: Vec<ObjectId> = Vec::with_capacity(refs.len());
    for (id, _) in &fetch_head {
        targets.push(*id);
    }
    for spec in refs.iter().filter(|_| fetch_head.is_empty()) {
        // `collect_parents()`'s `get_merge_parent(argv[i])`, which opens with a
        // single `repo_get_oid()` (`commit.c:1881`) — one trip through
        // `get_oid_basic()`, and so one `refname … is ambiguous.` per operand,
        // printed before the peel decides whether the operand is usable at all.
        crate::objname::warn_ambiguous_refname(&repo, spec.as_str());
        // `cmd_merge`'s own refusal, which is neither a `fatal:` nor exit 128:
        // `merge: <arg> - not something we can merge`, on stderr, exit 1. It also
        // covers a name that resolves to something that is not a commit.
        let resolved = repo
            .rev_parse_single(spec.as_str())
            .ok()
            .and_then(|o| o.object().ok())
            .and_then(|o| o.peel_to_commit().ok());
        let Some(commit) = resolved else {
            eprintln!("merge: {spec} - not something we can merge");
            return Err(anyhow::Error::new(crate::fatal::Silent(1)));
        };
        targets.push(commit.id);
    }

    // `collect_parents()` ends every arm with `reduce_parents()`
    // (builtin/merge.c:1219/1228), which reduces `{HEAD} ∪ remoteheads` to its
    // independent members and then drops `HEAD` itself. `HEAD` is in that set
    // because `collect_parents()` inserts it before the operand loop
    // (builtin/merge.c:1214-1215), which is what makes an operand `HEAD` already
    // reaches disappear — so `git merge <side> <ancestor-of-HEAD>` is the
    // two-head `git merge <side>`, ort and all, not an octopus.
    //
    // Everything downstream reads the reduced list: `GIT_REFLOG_ACTION`
    // (merge.c:1493-1496), `--verify-signatures` (merge.c:1486-1491), the
    // generated message (merge.c:1229-1233), the strategy choice
    // (merge.c:1515-1522) and the parents the merge commit records. Reducing
    // here rather than at each of those is what keeps them in step.
    let keep = independent_heads(&repo, local_id, &targets)?;
    let targets = mask(targets, &keep);
    // `refs` is index-parallel with the operands only when the heads came from
    // the command line; a `FETCH_HEAD` merge has one operand and any number of
    // heads, and `FETCH_HEAD` stays the label whichever survive.
    let refs_owned: Vec<String>;
    let refs: &[String] = match fetch_head.is_empty() {
        true => {
            refs_owned = mask(refs.to_vec(), &keep);
            &refs_owned
        }
        false => refs,
    };
    let msg_specs = mask(msg_specs, &keep);
    let head_labels = mask(head_labels, &keep);
    // `GIT_REFLOG_ACTION` is `merge` plus `merge_remote_util(p->item)->name` for
    // each *surviving* head (merge.c:1493-1496) — the same names the strategies
    // are handed, which is why a `FETCH_HEAD` merge reflogs `merge <oid>`.
    let reflog_spec: String = head_labels.join(" ");

    // `collect_parents()`'s *second* pass over the operands, which is where a
    // generated merge message is built:
    //
    // ```c
    // remoteheads = reduce_parents(head_commit, head_subsumed, remoteheads);
    // if (autogen) {
    //         struct commit_list *p;
    //         for (p = remoteheads; p; p = p->next)
    //                 merge_name(merge_remote_util(p->item)->name, autogen);
    // }
    // ```
    //
    // `merge_name()` opens with `get_merge_parent(remote)` too
    // (`builtin/merge.c:560`), so every operand it is asked about is resolved a
    // second time and warns a second time. Both gates are observable: `-m`/`-F`
    // (`have_message`) drops the pass to one warning per operand, `--log` or
    // `merge.log` puts it back even with `-m`, and `reduce_parents()` running
    // first is why merging an ancestor of `HEAD` warns once and not twice — the
    // head is gone from `refs` above before `merge_name()` ever sees it.
    //
    // The message itself is composed much later here (`compose_message`), after
    // `Already up to date.` has had its chance to return, so the pass cannot ride
    // along with it and is done here where git does it.
    if (opts.message.is_none() || opts.log_len != 0) && fetch_head.is_empty() {
        for spec in refs {
            crate::objname::warn_ambiguous_refname(&repo, spec.as_str());
        }
    }

    // `if (!remoteheads) … finish_up_to_date()` (builtin/merge.c:1550-1558):
    // every operand was reachable from `HEAD`, so there is nothing left to merge.
    // `ORIG_HEAD` is still recorded — merge.c:1542 sits above that arm — which is
    // why an up-to-date `git merge` moves it.
    if targets.is_empty() {
        set_orig_head(&repo, local_id)?;
        if !opts.quiet {
            println!("{}", up_to_date_line(opts));
        }
        return Ok(ExitCode::SUCCESS);
    }

    // `--verify-signatures` / `merge.verifySignatures`. git runs this over the
    // heads `collect_parents()` kept, and that function has already dropped every
    // head reachable from HEAD — which is why `Already up to date.` preempts the
    // check while a fast-forward does not.
    if opts.verify_signatures.unwrap_or_else(|| {
        repo.config_snapshot().boolean("merge.verifySignatures") == Some(true)
    }) {
        // `gpg.minTrustLevel` moves the floor into `check_signature()` itself, so
        // git clears its own `TRUST_MARGINAL` test when the key is configured.
        let check_trust = repo.config_snapshot().string("gpg.minTrustLevel").is_none();
        for id in &targets {
            let reachable = repo
                .merge_bases_many(local_id, &[*id])?
                .iter()
                .any(|b| b.detach() == *id);
            if reachable {
                continue;
            }
            if let Some(code) = verify_merge_signature(&repo, *id, opts.quiet, check_trust)? {
                return Ok(code);
            }
        }
    }

    // `use_strategies` with `if (!use_strategies)`'s default filled in
    // (builtin/merge.c:1599-1606). Everything below reads this list rather than
    // any single `-s`: the attribute union that decides `fast_forward` and
    // `allow_trivial`, and the loop that tries each in turn.
    let picks = match opts.picks(targets.len(), &strategy_config(&repo)) {
        Ok(picks) => picks,
        Err(code) => return Ok(code),
    };

    // More than one head. `add_strategies(pull_octopus, DEFAULT_OCTOPUS)`
    // (builtin/merge.c:1605) only picks the octopus when no `-s` was given; a
    // named strategy is used for this head count too, and the two-head engines
    // refuse it. That refusal is the whole point of dispatching on the *resolved*
    // head count rather than on the `-s` spelling: `-s ort a b` must fail, while
    // a bare `git merge a b` — or a single `FETCH_HEAD` naming several for-merge
    // lines, which is how `git pull <remote> <a> <b>` arrives — octopuses.
    //
    // `refs_update_ref("updating ORIG_HEAD", …)` (builtin/merge.c:1636) sits
    // above the strategy dispatch for every head count, so even an octopus that
    // no strategy handles still moves it. git computes the octopus merge bases
    // just before it (builtin/merge.c:1620-1628); `git-merge-octopus` re-derives
    // a base per head as it folds them in, so that list is not carried here.
    if targets.len() > 1 {
        set_orig_head(&repo, local_id)?;
        let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
        let head_tree = repo.find_object(local_id)?.peel_to_tree()?.id;
        let ctx = MergeCtx {
            refs: &msg_specs,
            head_labels: &head_labels,
            targets: &targets,
            local_id,
            head_tree,
            branch: branch.as_ref(),
            reflog_spec: &reflog_spec,
            bases: &[],
            spec: "",
            diverged: true,
            message: compose_message(&repo, &msg_specs, &targets, branch.as_ref(), local_id, opts)?,
        };
        return merge_with_strategies(&repo, &ctx, opts, &picks);
    }

    let spec = refs[0].as_str();
    let target_id = targets[0];

    // merge-base analysis. An empty set of merge bases means unrelated histories,
    // which git refuses without `--allow-unrelated-histories`.
    let bases = repo.merge_bases_many(local_id, &[target_id])?;

    // `refs_update_ref("updating ORIG_HEAD", …)` sits between the merge-base
    // computation and everything that decides what to do with it
    // (builtin/merge.c:1634). Every outcome below is downstream of it: the
    // unrelated-histories refusal, `Already up to date.`, the fast-forward, and
    // the strategy dispatch alike leave `ORIG_HEAD` at the pre-merge `HEAD`.
    // Writing it only on the paths that move `HEAD` left a stale `ORIG_HEAD`
    // from an earlier operation behind an up-to-date `git merge`/`git pull`.
    set_orig_head(&repo, local_id)?;

    if bases.is_empty() && !opts.allow_unrelated {
        eprintln!("fatal: refusing to merge unrelated histories");
        return Ok(ExitCode::from(128));
    }
    if bases.iter().any(|b| b.detach() == target_id) {
        // Target already reachable from HEAD (or identical). git checks this
        // before it consults --no-ff, so --no-ff does not force a commit here.
        if !opts.quiet {
            println!("{}", up_to_date_line(opts));
        }
        return Ok(ExitCode::SUCCESS);
    }
    // Fast-forwardable exactly when HEAD is one of the merge bases.
    let diverged = !bases.iter().any(|b| b.detach() == local_id);
    if diverged && opts.ff == Ff::Only {
        // `die_ff_impossible()` (advice.c): the `advice.diverging` hint comes
        // first, then the `die()`.
        crate::advice::ff_impossible(&repo);
        eprintln!("fatal: Not possible to fast-forward, aborting.");
        return Ok(ExitCode::from(128));
    }

    // A strategy runs for a single head exactly when the fast-forward arm above
    // did not answer the merge (builtin/merge.c:1655-1690): a diverged history,
    // or `--no-ff` over one that could have fast-forwarded.
    let runs_strategy = diverged || opts.ff == Ff::Never;

    // From here on we mutate a ref, the index and the worktree. Serialize the
    // whole read-modify-write through the repo coordinator (a no-op if no
    // daemon is running), matching the zsync/zbump write path.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // `--autostash`: snapshot and reset the dirty worktree so the merge runs against a
    // clean tree and the local changes come back at the end (or stay recoverable in
    // MERGE_AUTOSTASH if it stops). `cmd_merge` creates it per branch, not up front:
    // the fast-forward path prints `Updating <a>..<b>` first, the strategy loop creates
    // it just before the first attempt, and the up-to-date path never gets there at all.
    let old_index = repo.index_or_load_from_head()?.into_owned();
    let head_tree = repo.find_object(local_id)?.peel_to_tree()?.id;
    let target_tree = repo.find_object(target_id)?.peel_to_tree()?.id;

    let should_interrupt = AtomicBool::new(false);
    let message = compose_message(&repo, &msg_specs, &targets, branch.as_ref(), local_id, opts)?;

    // ```c
    // else if (!remoteheads->next && !common->next && option_commit) {
    //         refresh_index(…);
    //         if (allow_trivial && fast_forward != FF_ONLY) {
    // ```
    //
    // (builtin/merge.c:1699-1703) — the `allow_trivial` in-index merge. It needs
    // one head, one merge base, a merge that will be committed, and a strategy
    // whose `all_strategy[]` entry lacks `NO_TRIVIAL`, which leaves `octopus`
    // and `resolve` (builtin/merge.c:102-107, 1611-1612). The `fast_forward !=
    // FF_ONLY` half is already answered: `--ff-only` either fast-forwarded or
    // died above, so it can never reach here with `runs_strategy` set.
    //
    // `for (i = 0; i < use_strategies_nr; i++) if (… & NO_TRIVIAL) allow_trivial
    // = 0;` runs over the whole list (builtin/merge.c:1608-1612), so one `-s ort`
    // anywhere in it suppresses the pre-pass for every other `-s` alongside it —
    // `git merge -s ort -s resolve` prints no `Trying really trivial in-index
    // merge...` where `git merge -s resolve` does (measured against stock).
    if runs_strategy
        && picks.iter().all(|p| p.kind.allows_trivial())
        && bases.len() == 1
        && opts.option_commit()
    {
        // "Must first ensure that index matches HEAD before attempting a trivial
        // merge." — `repo_index_has_changes()` (builtin/merge.c:1712-1719). Its
        // refusal is `error:` alone: no `Merge with strategy … failed.` line,
        // and no reflog entry (measured against stock 2.55.0, which logs
        // `merge …: updating HEAD` only for merge-ort's own index guard).
        let staged = crate::merge_guard::index_changes_from_head(&repo, head_tree, &old_index)?;
        if !staged.is_empty() {
            crate::merge_guard::report_index_changes(&staged);
            return Ok(ExitCode::from(2));
        }
        // Both lines are bare `printf`s with no verbosity check, so `-q` keeps
        // them (builtin/merge.c:1723, 1730).
        println!("Trying really trivial in-index merge...");
        if read_tree_trivial(bases[0].detach(), local_id, target_id)? {
            return merge_trivial(&repo, local_id, target_id, head_tree, message, opts, &reflog_spec);
        }
        println!("Nope.");
    }

    // The strategy loop (builtin/merge.c:1778-1859), which brackets its own
    // `create_autostash_ref()`/`save_state()`.
    if runs_strategy {
        let base_ids: Vec<ObjectId> = bases.iter().map(|b| b.detach()).collect();
        let ctx = MergeCtx {
            refs: &msg_specs,
            head_labels: &head_labels,
            targets: &targets,
            local_id,
            head_tree,
            branch: branch.as_ref(),
            reflog_spec: &reflog_spec,
            bases: &base_ids,
            spec,
            diverged,
            message,
        };
        return merge_with_strategies(&repo, &ctx, opts, &picks);
    }

    // Pure fast-forward territory. `--squash` fast-forwards the *content* but does
    // not move the ref: git updates the worktree, prints the fast-forward summary,
    // then the squash notice and writes SQUASH_MSG.
    if opts.squash {
        if !opts.quiet {
            println!(
                "Updating {}..{}",
                local_id.attach(&repo).shorten()?,
                target_id.attach(&repo).shorten()?
            );
        }
        let stash = begin_autostash(&repo, opts)?;
        if let Some(code) = guard_checkout(&repo, head_tree, target_tree, &old_index, None)? {
            end_autostash(&repo, stash, false)?;
            return Ok(code);
        }
        update_worktree(&repo, &old_index, Some(head_tree), target_tree, &should_interrupt)?;
        if !opts.quiet {
            println!("Fast-forward");
        }
        // `finish()` calls `squash_message()`, which announces itself before it
        // writes `SQUASH_MSG`, and prints the diffstat afterwards. The notice has
        // no verbosity check in git: `-q` silences the block around it, not it.
        println!("Squash commit -- not updating HEAD");
        write_squash_msg(&repo, &[target_id], local_id)?;
        if !opts.quiet {
            print!("{}", diffstat(&repo, head_tree, target_tree, opts.stat)?);
        }
        end_autostash(&repo, stash, false)?;
        return Ok(ExitCode::SUCCESS);
    }

    // Normal fast-forward. `--no-commit` does not stop a fast-forward (there is no
    // merge commit to stop before), matching git.
    //
    // The worktree moves before the ref does, which is the order git's
    // `checkout_fast_forward()` establishes: when the checkout aborts, git leaves
    // the branch where it was and only `ORIG_HEAD` is written (checked against git
    // 2.55.0, whose refusal to clobber an untracked file leaves `refs/heads/main`
    // unmoved). Advancing first would strand a branch two commits ahead of its own
    // checkout, with every later `status` reporting the difference as staged work.
    //
    // `Updating <a>..<b>` is printed *before* the checkout is attempted, as
    // `cmd_merge` does, so a refused fast-forward shows it followed by the
    // refusal — and exits 1, the fast-forward's own failure code, with no
    // strategy-failure line (no strategy ran).
    if !opts.quiet {
        println!(
            "Updating {}..{}",
            local_id.attach(&repo).shorten()?,
            target_id.attach(&repo).shorten()?
        );
    }
    let stash = begin_autostash(&repo, opts)?;
    if let Some(code) = guard_checkout(&repo, head_tree, target_tree, &old_index, None)? {
        end_autostash(&repo, stash, false)?;
        return Ok(code);
    }
    update_worktree(&repo, &old_index, Some(head_tree), target_tree, &should_interrupt)?;
    advance(&repo, local_id, target_id, format!("{}: Fast-forward", reflog_action(&reflog_spec)))?;
    if !opts.quiet {
        println!("Fast-forward");
        print!("{}", diffstat(&repo, head_tree, target_tree, opts.stat)?);
    }
    end_autostash(&repo, stash, true)?;
    // `finish(); remove_merge_branch_state();` — builtin/merge.c:1688. The order
    // is the point: `finish()` is what re-applies the autostash
    // (builtin/merge.c:539), and *that* apply is a `git stash apply` child that
    // records its own `AUTO_MERGE`. Removing the merge state first would leave
    // the file behind, which is exactly what `git pull --autostash` was doing.
    remove_merge_state(repo.git_dir(), false);
    Ok(ExitCode::SUCCESS)
}

/// The `unpack_trees()` gate for the paths that check a tree out directly — the
/// fast-forward, `--squash` and the `--no-ff` shortcut — rather than through
/// [`crate::merge_apply`]. Reports git's refusal and yields the exit code when
/// the move from `head_tree` to `new_tree` would cost local work.
///
/// `strategy` distinguishes the two failure shapes `cmd_merge` produces: a
/// strategy that failed adds `Merge with strategy ort failed.` and exits 2,
/// while a failed `checkout_fast_forward()` just exits 1.
/// `reduce_parents()` (`builtin/merge.c`) as far as the generated merge message
/// needs it: which of `targets` survive, in operand order.
///
/// ```c
/// /* Find what parents to record by checking independent ones. */
/// parents = reduce_heads(remoteheads);
/// ```
///
/// `reduce_heads()` keeps the *independent* commits of `{HEAD} ∪ remoteheads` —
/// the ones no other member of the set reaches — and `collect_parents()` then
/// asks `merge_name()` only about those. That is why `git merge <ancestor>`
/// produces one `refname … is ambiguous.` and not two: the operand is resolved by
/// `get_merge_parent()`, then dropped here, and `merge_name()` never resolves it
/// a second time.
///
/// Duplicates keep their first occurrence, which is what `reduce_heads()`'s
/// `commit_list_insert_by_date` + dedup by `object->flags` amounts to for a
/// repeated operand.
fn independent_heads(
    repo: &gix::Repository,
    head: ObjectId,
    targets: &[ObjectId],
) -> Result<Vec<bool>> {
    // `a` reaches `b` exactly when `b` is one of their merge bases.
    let reaches = |a: ObjectId, b: ObjectId| -> Result<bool> {
        Ok(repo.merge_bases_many(a, &[b])?.iter().any(|base| base.detach() == b))
    };
    let mut keep = Vec::with_capacity(targets.len());
    for (i, &target) in targets.iter().enumerate() {
        let duplicate = targets[..i].contains(&target);
        let subsumed = !duplicate
            && (reaches(head, target)?
                || targets
                    .iter()
                    .enumerate()
                    .filter(|(j, &other)| *j != i && other != target)
                    .try_fold(false, |acc, (_, &other)| {
                        Ok::<_, anyhow::Error>(acc || reaches(other, target)?)
                    })?);
        keep.push(!duplicate && !subsumed);
    }
    Ok(keep)
}

/// Keep the entries of an operand-parallel vector that [`independent_heads`]
/// kept, in operand order — the survivors of `reduce_parents()`.
fn mask<T>(values: Vec<T>, keep: &[bool]) -> Vec<T> {
    values.into_iter().zip(keep).filter(|(_, k)| **k).map(|(v, _)| v).collect()
}


/// The numeric status an [`ExitCode`] carries. `ExitCode` exposes no accessor on
/// stable Rust — only `From<u8>` and equality — so recover it by probing the 256
/// values it can hold, exactly as `crate::run` does at the top of the binary.
/// Needed because the two back-ends below are ports of *programs*: `cmd_merge`
/// branches on the status they exit with, not on a Rust value.
fn exit_status(code: ExitCode) -> u8 {
    (0u8..=255).find(|&n| code == ExitCode::from(n)).unwrap_or(1)
}

/// `read_tree_trivial()` (builtin/merge.c:743-777): `unpack_trees()` over
/// `<common> <head> <one>` with `head_idx = 2`, `merge`, `update`,
/// `verbose_update`, `trivial_merges_only` and `preserve_ignored = 0`, resolving
/// through `threeway_merge()`.
///
/// `git read-tree -u -m --trivial <common> <head> <one>` sets that exact field
/// set: three trees pick `opts.fn = threeway_merge` and `head_idx = stage - 2`,
/// i.e. 2 (builtin/read-tree.c:246-258); `-u` without `--reset` clears
/// `preserve_ignored` (builtin/read-tree.c:229); and `--trivial` *is*
/// `trivial_merges_only` (builtin/read-tree.c:133). So the pre-pass is that
/// command, run in process — including `unpack_failed()`'s
/// `error: Merge requires file-level merging` (unpack-trees.c:2031), which stock
/// prints between `Trying really trivial in-index merge...` and `Nope.`, and
/// including the fact that a failure writes neither the index nor the worktree.
///
/// Returns whether the trivial merge took, i.e. git's `!read_tree_trivial(…)`.
fn read_tree_trivial(common: ObjectId, head: ObjectId, one: ObjectId) -> Result<bool> {
    let argv: Vec<String> = vec![
        "-u".to_string(),
        "-m".to_string(),
        "--trivial".to_string(),
        common.to_string(),
        head.to_string(),
        one.to_string(),
    ];
    Ok(exit_status(super::read_tree::read_tree(&argv)?) == 0)
}

/// `merge_trivial()` (builtin/merge.c:989-1012). The index
/// [`read_tree_trivial`] just wrote *is* the merge result, so `write_tree_trivial()`
/// turns it into a tree and the commit is written over it with `HEAD` and the
/// merged head as parents. `finish()` is handed the literal `In-index merge`
/// where an automerge would pass `Merge made by the '<strategy>' strategy.`, and
/// that string is what the reflog records too.
fn merge_trivial(
    repo: &gix::Repository,
    local_id: ObjectId,
    target_id: ObjectId,
    head_tree: ObjectId,
    message: String,
    opts: &Opts,
    reflog_spec: &str,
) -> Result<ExitCode> {
    let index = repo.open_index()?;
    let result_tree = index_tree(repo, &index)?;
    // A bare `printf` with no verbosity check, so `-q` keeps it too.
    println!("Wonderful.");
    finalize_clean(
        repo,
        local_id,
        &[target_id],
        None,
        message,
        result_tree,
        head_tree,
        opts,
        "In-index merge",
        reflog_spec,
        None,
    )
}

/// `try_merge_command()` (merge.c:22-42) plus the status bookkeeping `cmd_merge`
/// does around it (builtin/merge.c:1800-1881), for `git-merge-resolve` — one of
/// the two strategies git still runs as a separate program.
///
/// The command line is `git merge-resolve --<xopt>… <base>… -- HEAD <head>`:
/// every `-X` is re-spelled `--<value>` and lands *ahead of* the merge bases, and
/// the local side is the literal string `HEAD` while every other operand is an
/// object id (`merge_argument()`). `git-merge-resolve` hands those `--<value>`
/// words straight to `read-tree`, which is why `-X` is never parsed for it.
///
/// > The backend exits with 1 when conflicts are left to be resolved, with 2
/// > when it does not handle the given merge at all.
///
/// 0 is `merge_was_ok`: the index the back-end left behind is the result, so it
/// becomes the tree of the merge commit (`finish_automerge()`'s
/// `write_tree_trivial`).
fn resolve_attempt(repo: &gix::Repository, ctx: &MergeCtx<'_>, opts: &Opts) -> Result<Attempt> {
    // `case "$remotes" in ?*' '?*) exit 2 ;; esac` — git-merge-resolve.sh's
    // "Reject if this is not a two-head merge" gives up silently on more.
    if ctx.targets.len() > 1 {
        return Ok(Attempt::Refused);
    }
    let mut argv: Vec<String> = opts.strategy_options.iter().map(|x| format!("--{x}")).collect();
    argv.extend(ctx.bases.iter().map(ObjectId::to_string));
    argv.push("--".to_string());
    argv.push("HEAD".to_string());
    argv.push(ctx.targets[0].to_string());

    match exit_status(super::merge_resolve::merge_resolve(&argv)?) {
        0 => {
            let index = repo.open_index()?;
            Ok(Attempt::Clean {
                tree: index_tree(repo, &index)?,
                heads: ctx.targets.to_vec(),
                parents_override: None,
                spec_label: ctx.reflog_spec.to_string(),
            })
        }
        1 => Ok(Attempt::Conflicts(unmerged_paths(&repo.open_index()?))),
        _ => Ok(Attempt::Refused),
    }
}

/// The `merge_ort_recursive()` branch of `try_merge_strategy()`
/// (builtin/merge.c:800-845), which `recursive`, `subtree` and `ort` all reach.
///
/// A genuine three-way merge of `HEAD` and the target against their merge base
/// (an empty tree for unrelated histories), applied to the index and worktree.
/// `--no-ff` over a fast-forwardable history takes the shortcut below instead:
/// the merge base is our own commit, so every path resolves to theirs and the
/// merged tree *is* the target's — unless a `-X` is in play, which can reshape
/// the trees and make that untrue.
fn ort_attempt(
    repo: &gix::Repository,
    ctx: &MergeCtx<'_>,
    opts: &Opts,
    xopts: &crate::merge_apply::StrategyOptions,
) -> Result<Attempt> {
    let old_index = repo.index_or_load_from_head()?.into_owned();
    // merge-ort's `merge_start()`: the index must match HEAD before the engine
    // runs, whatever the change is and wherever it sits. A fast-forward never
    // reaches this — git happily fast-forwards over a staged change.
    let staged = crate::merge_guard::index_changes_from_head(repo, ctx.head_tree, &old_index)?;
    if !staged.is_empty() {
        crate::merge_guard::report_index_changes(&staged);
        log_strategy_failure(repo, ctx.local_id, ctx.reflog_spec);
        return Ok(Attempt::Refused);
    }

    let target_id = ctx.targets[0];
    let target_tree = repo.find_object(target_id)?.peel_to_tree()?.id;
    let should_interrupt = AtomicBool::new(false);

    if !ctx.diverged
        && opts.strategy_options.is_empty()
        && xopts.subtree_shift.is_none()
    {
        // The `--no-ff` shortcut. The strategy still ran, so a refused checkout
        // is a strategy failure like any other.
        let clobber =
            crate::merge_guard::verify_two_way(repo, ctx.head_tree, target_tree, &old_index)?;
        if !clobber.is_empty() {
            clobber.report("merge");
            return Ok(Attempt::Refused);
        }
        update_worktree(repo, &old_index, Some(ctx.head_tree), target_tree, &should_interrupt)?;
        // `merge_switch_to_result()` again: the shortcut skips the tree merge
        // because its answer is known, but git still ran merge-ort and recorded
        // that tree. `--no-commit` stops before the commit that would remove it.
        crate::merge_apply::write_auto_merge(repo, target_tree)?;
        return Ok(Attempt::Clean {
            tree: target_tree,
            heads: ctx.targets.to_vec(),
            parents_override: None,
            spec_label: ctx.reflog_spec.to_string(),
        });
    }

    // `merge_ort_internal()` names the recursive base in the same breath, and
    // the name is what a `diff3`/`zdiff3` conflict prints on its `|||||||` line:
    // with no base at all it is the literal `empty tree`; with several bases —
    // which git folds into one virtual commit — `merged common ancestors`; and
    // with exactly one base the base's own abbreviated id
    // (`strbuf_add_unique_abbrev(…, DEFAULT_ABBREV)`).
    let (base_tree, ancestor) = if ctx.bases.is_empty() {
        (gix::ObjectId::empty_tree(repo.object_hash()), "empty tree".to_string())
    } else if ctx.bases.len() == 1 {
        let base = ctx.bases[0];
        (
            repo.find_object(base)?.peel_to_tree()?.id,
            base.attach(repo).shorten_or_id().to_string(),
        )
    } else {
        // ```c
        // merged_merge_bases = pop_commit(&merge_bases);
        // [...]
        //         merge_ort_internal(opt, NULL, merged_merge_bases, commit2, result);
        // ```
        //
        // (`merge_ort_recursive()`, merge-ort.c.) With more than one merge base git does not
        // pick one: it merges them into each other, recursively, and merges the two sides
        // against the *virtual* tree that comes out. Picking a single base instead resolves
        // a criss-cross merge cleanly where git reports a conflict — the wrong answer, not
        // just a different message.
        (virtual_base_tree(repo, ctx.bases)?, "merged common ancestors".to_string())
    };
    let labels = gix::merge::blob::builtin_driver::text::Labels {
        ancestor: Some(BStr::new(ancestor.as_bytes())),
        current: Some(BStr::new(b"HEAD")),
        other: Some(BStr::new(ctx.spec.as_bytes())),
    };
    let merged = crate::merge_apply::three_way_merge_guarded(
        repo,
        base_tree,
        ctx.head_tree,
        target_tree,
        &old_index,
        labels,
        &should_interrupt,
        merge_verbosity(repo) != 0,
        xopts,
        ctx.head_tree,
    )?;
    let applied = match merged {
        crate::merge_apply::Merged::Applied(applied) => applied,
        // `merge_switch_to_result()`'s checkout refused: the merge is computed
        // and reported, but nothing on disk moved.
        crate::merge_apply::Merged::Refused(clobber) => {
            clobber.report("merge");
            return Ok(Attempt::Refused);
        }
    };
    let mut index = applied.index;
    crate::index_racy::write(repo, &mut index)?;
    // `merge_switch_to_result()`'s `write_auto_merge` region: the strategy ran
    // and its result is on disk, so the merged tree is recorded. A commit
    // removes it again; `--no-commit`, `--squash` and a conflict all stop first
    // and leave it for `git diff AUTO_MERGE`.
    crate::merge_apply::write_auto_merge(repo, applied.tree_id)?;

    if applied.conflicts.is_empty() {
        return Ok(Attempt::Clean {
            tree: applied.tree_id,
            heads: ctx.targets.to_vec(),
            parents_override: None,
            spec_label: ctx.reflog_spec.to_string(),
        });
    }
    Ok(Attempt::Conflicts(applied.conflicts))
}

/// The tree git merges against when a pair of commits has more than one merge base: the
/// bases merged into each other, recursively.
///
/// ```c
/// static struct commit *make_virtual_commit(struct repository *repo, struct tree *tree, const char *comment)
/// {
///         struct commit *commit = alloc_commit_node(repo);
///         [...]
/// }
/// ```
///
/// (merge-ort.c.) git's virtual commits are allocated, never written: a criss-cross merge
/// leaves the merged base *tree* and the blobs it needed in the object store, and no commit.
/// gitoxide writes its virtual commits too, so the recursion runs against an in-memory
/// object store and only the objects git would have written are persisted afterwards.
pub(super) fn virtual_base_tree(repo: &gix::Repository, bases: &[ObjectId]) -> Result<ObjectId> {
    let mut mem = repo.clone();
    mem.objects.enable_object_memory();
    let out = mem.virtual_merge_base(bases.iter().copied(), mem.tree_merge_options()?)?;
    let tree = out.tree_id.detach();
    let written = mem
        .objects
        .take_object_memory()
        .expect("object memory was just enabled");
    for (_id, (kind, data)) in written.iter() {
        if *kind == gix::object::Kind::Commit {
            continue;
        }
        gix::objs::Write::write_buf(repo, *kind, data)
            .map_err(|e| anyhow::anyhow!("failed to write merge-base object: {e}"))?;
    }
    Ok(tree)
}

/// `git-merge-ours`, run through `try_merge_command()` like any other
/// out-of-process back-end: every head becomes a parent while our tree is kept
/// verbatim. It ignores the head count entirely, which is why `-s ours` answers
/// an octopus as readily as a two-head merge, and it never fast-forwards
/// (`NO_FAST_FORWARD`, builtin/merge.c:106).
///
/// `builtin/merge-ours.c`: "The index must match HEAD, or this merge cannot
/// proceed" — it exits 2 without a word of its own, and `cmd_merge` supplies the
/// strategy-failure line. Unstaged worktree changes are none of its business:
/// our tree is kept verbatim, so nothing is checked out over them.
fn ours_attempt(repo: &gix::Repository, ctx: &MergeCtx<'_>) -> Result<Attempt> {
    let old_index = repo.index_or_load_from_head()?.into_owned();
    if !crate::merge_guard::index_changes_from_head(repo, ctx.head_tree, &old_index)?.is_empty() {
        log_strategy_failure(repo, ctx.local_id, ctx.reflog_spec);
        return Ok(Attempt::Refused);
    }
    let should_interrupt = AtomicBool::new(false);
    // Our tree is unchanged; sync the index (a no-op checkout).
    update_worktree(repo, &old_index, None, ctx.head_tree, &should_interrupt)?;
    Ok(Attempt::Clean {
        tree: ctx.head_tree,
        heads: ctx.targets.to_vec(),
        parents_override: None,
        spec_label: ctx.reflog_spec.to_string(),
    })
}

/// The conflicted tail of `cmd_merge` (builtin/merge.c:1868-1881): record the
/// in-progress merge — or, under `--squash`, only `SQUASH_MSG` — then
/// `suggest_conflicts()`. Shared by merge-ort and by the out-of-process
/// back-ends, which reach it with the same state and the same wording.
fn stop_for_conflicts(
    repo: &gix::Repository,
    local_id: ObjectId,
    // Every merged head. `write_merge_state()` lists them all in `MERGE_HEAD`,
    // which is what makes the next `git commit` record an n-parent commit — so a
    // conflicted octopus and a conflicted two-head merge take the same path.
    targets: &[ObjectId],
    message: String,
    conflicts: &[BString],
    opts: &Opts,
    stash: Option<ObjectId>,
) -> Result<ExitCode> {
    // Conflicts: record the in-progress merge and stop with git's message.
    //
    // `cmd_merge()` forks here rather than after the notice:
    //
    // ```c
    // if (squash) {
    //         finish(head_commit, remoteheads, NULL, NULL);
    //         …
    // } else
    //         write_merge_state(remoteheads);
    // ```
    //
    // (builtin/merge.c:1770-1775). `finish()` with a NULL new head records
    // no ref move and no merge state — it only reaches `squash_message()`,
    // which announces itself and writes `SQUASH_MSG`. So a conflicted squash
    // leaves neither `MERGE_HEAD` nor `MERGE_MODE`, and that is a state
    // difference rather than a wording one: `MERGE_HEAD` is what makes the
    // next `git commit` write a two-parent commit and what gives
    // `git merge --abort` a merge to abort, and a squash asked for neither.
    let git_dir = repo.git_dir();
    let mut merge_msg = Vec::new();
    if opts.squash {
        // `squash_message()` prints before it writes, with no verbosity
        // check of its own (builtin/merge.c:417).
        println!("Squash commit -- not updating HEAD");
        write_squash_msg(repo, targets, local_id)?;
    } else {
        write_merge_heads(repo, targets, opts.ff)?;
        merge_msg = message.into_bytes();
    }
    // `suggest_conflicts()` opens `MERGE_MSG` with `xfopen(filename, "a")`
    // and appends `append_conflicts_hint()`'s block (builtin/merge.c:967-979),
    // so the hint's leading blank line is always there and a squash — which
    // wrote no message for it to follow — leaves the hint alone in the file.
    merge_msg.extend_from_slice(b"\n# Conflicts:\n");
    for path in conflicts {
        merge_msg.extend_from_slice(b"#\t");
        merge_msg.extend_from_slice(&path[..]);
        merge_msg.push(b'\n');
    }
    std::fs::write(git_dir.join("MERGE_MSG"), &merge_msg)?;
    // `suggest_conflicts()` runs rerere between the `# Conflicts:` hint and
    // the notice: a known resolution is replayed into the worktree (and
    // staged under `--rerere-autoupdate`/`rerere.autoupdate`), an unknown one
    // has its preimage recorded for next time.
    super::rerere::repo_rerere(repo, opts.rerere_autoupdate)?;
    if !opts.quiet {
        println!("Automatic merge failed; fix conflicts and then commit the result.");
    }
    end_autostash(repo, stash, false)?;
    return Ok(ExitCode::from(1));
}

fn guard_checkout(
    repo: &gix::Repository,
    head_tree: ObjectId,
    new_tree: ObjectId,
    index: &gix::index::File,
    // The strategy name to blame when the refusal is a strategy failure, or
    // `None` when the caller is a plain fast-forward checkout (exit 1).
    strategy: Option<&str>,
) -> Result<Option<ExitCode>> {
    let clobber = crate::merge_guard::verify_two_way(repo, head_tree, new_tree, index)?;
    if clobber.is_empty() {
        return Ok(None);
    }
    clobber.report("merge");
    if let Some(name) = strategy {
        eprintln!("Merge with strategy {name} failed.");
        return Ok(Some(ExitCode::from(2)));
    }
    Ok(Some(ExitCode::from(1)))
}

/// [`guard_checkout`] for the octopus strategy, which folds each head in with
/// `git read-tree -u -m` rather than a porcelain checkout: the refusal carries
/// the plumbing wording, one line per path, and the exit code is git's strategy
/// failure.
fn guard_octopus(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
    index: &gix::index::File,
) -> Result<Option<ExitCode>> {
    let clobber = crate::merge_guard::verify_two_way(repo, old_tree, new_tree, index)?;
    if clobber.is_empty() {
        return Ok(None);
    }
    clobber.report_plumbing();
    eprintln!("Merge with strategy octopus failed.");
    Ok(Some(ExitCode::from(2)))
}

/// [`guard_octopus`] for the three-tree fold, which is what a non-fast-forward
/// head actually goes through: `git read-tree -u -m --aggressive $common $MRT
/// $SHA1`.
///
/// It runs *before* the merge rather than over its result because the script
/// only reaches `git write-tree` once `read-tree` has agreed — a head refused
/// here must therefore leave no merged tree behind in the object database, which
/// a check on the merged tree cannot arrange.
fn guard_octopus_three_way(
    repo: &gix::Repository,
    base_tree: ObjectId,
    ours_tree: ObjectId,
    theirs_tree: ObjectId,
    index: &gix::index::File,
) -> Result<Option<ExitCode>> {
    let clobber =
        crate::merge_guard::verify_three_way(repo, base_tree, ours_tree, theirs_tree, index)?;
    if clobber.is_empty() {
        return Ok(None);
    }
    clobber.report_plumbing();
    eprintln!("Merge with strategy octopus failed.");
    Ok(Some(ExitCode::from(2)))
}

/// The clean-merge finish shared by the diverged, `--no-ff`, and `-s ours` paths:
/// squashes, stops before committing, or writes the merge commit, honouring
/// `--signoff`, `--cleanup`, `--no-verify` and `--quiet`. `ORIG_HEAD` was
/// recorded by the caller before its gates ran, as `cmd_merge` records it before
/// the strategy dispatch. `merged_tree` is the already-computed result tree (its
/// worktree/index are assumed synced by the caller); `head_tree` feeds the
/// diffstat.
#[allow(clippy::too_many_arguments)]
fn finalize_clean(
    repo: &gix::Repository,
    local_id: ObjectId,
    targets: &[ObjectId],
    // `mrc` (the merge-result-commit list) when it is not simply `HEAD` plus the
    // merged heads: an octopus whose first head fast-forwarded past `HEAD`
    // *replaces* it there, so `HEAD` is subsumed and does not become a parent.
    parents_override: Option<&[ObjectId]>,
    message: String,
    merged_tree: ObjectId,
    head_tree: ObjectId,
    opts: &Opts,
    // The string `finish()` is handed (builtin/merge.c:1013, 1053): normally
    // `Merge made by the '<wt_strategy>' strategy.`, but the literal `In-index
    // merge` when the `allow_trivial` pre-pass answered the merge. It is both
    // the line printed and the reflog message's tail.
    finish_msg: &str,
    spec_label: &str,
    stash: Option<ObjectId>,
) -> Result<ExitCode> {
    let do_commit = opts.commit.unwrap_or(!opts.squash);

    // `--squash`: no commit, no ref move, no MERGE_HEAD — just SQUASH_MSG.
    if opts.squash {
        // `cmd_merge()` reports this with `fprintf(stderr, …)` after `finish()`,
        // and — like the squash notice — with no verbosity check of its own.
        eprintln!("Automatic merge went well; stopped before committing as requested");
        // No verbosity check on this one in git — `squash_message()` prints it
        // before it writes the file, whatever `-q` says.
        println!("Squash commit -- not updating HEAD");
        write_squash_msg(repo, targets, local_id)?;
        end_autostash(repo, stash, false)?;
        return Ok(ExitCode::SUCCESS);
    }

    // `--no-commit`: leave the merge in progress for `git commit` to finalize.
    if !do_commit {
        let git_dir = repo.git_dir();
        write_merge_heads(repo, targets, opts.ff)?;
        std::fs::write(git_dir.join("MERGE_MSG"), &message)?;
        eprintln!("Automatic merge went well; stopped before committing as requested");
        end_autostash(repo, stash, false)?;
        return Ok(ExitCode::SUCCESS);
    }

    // `pre-merge-commit` runs before the commit; a non-zero exit vetoes it. The
    // hook's own output (inherited on stderr) is the whole diagnostic, as in git.
    if !opts.no_verify && !crate::hooks::run(repo, "pre-merge-commit", &[], None)? {
        return Ok(ExitCode::from(1));
    }

    // git's `prepare_to_commit()` from here: build the buffer, persist the merge
    // state, run the editor and the `commit-msg` hook over that file, then clean
    // the message up and refuse an empty one.
    let edit = match edit_wanted(opts) {
        Ok(edit) => edit,
        Err(code) => return Ok(code),
    };
    let comment = comment_char(repo);
    let mut msg = message;
    if opts.signoff {
        append_signoff(repo, &mut msg)?;
    }
    if edit {
        append_editor_comment(&mut msg, opts.cleanup, &comment);
    }

    let git_dir = repo.git_dir();
    write_merge_heads(repo, targets, opts.ff)?;
    let msg_path = git_dir.join("MERGE_MSG");
    std::fs::write(&msg_path, &msg)?;

    if edit && !launch_editor(repo, &msg_path)? {
        eprintln!("Not committing merge; use 'git commit' to complete the merge.");
        return Ok(ExitCode::from(1));
    }
    // `commit-msg` gets the same file and may rewrite it.
    if !opts.no_verify {
        let arg = msg_path.to_string_lossy().into_owned();
        if !crate::hooks::run(repo, "commit-msg", &[&arg], None)? {
            return Ok(ExitCode::from(1));
        }
    }
    msg = std::fs::read_to_string(&msg_path)?;
    // `get_cleanup_mode(cleanup_arg, 0 < option_edit)`: with no explicit
    // `--cleanup`/`commit.cleanup`, an edited message is stripped of its comment
    // lines (`COMMIT_MSG_CLEANUP_ALL`) while an unedited one only loses
    // whitespace (`COMMIT_MSG_CLEANUP_SPACE`).
    let cleanup = match (opts.cleanup, edit) {
        (Cleanup::Default, true) => Cleanup::Strip,
        (mode, _) => mode,
    };
    msg = cleanup_message(&msg, cleanup, &comment);
    if msg.is_empty() {
        eprintln!("error: Empty commit message.");
        eprintln!("Not committing merge; use 'git commit' to complete the merge.");
        return Ok(ExitCode::from(1));
    }

    let author = repo
        .author()
        .ok_or_else(|| anyhow::anyhow!("author identity is not configured"))??;
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let parents: Vec<ObjectId> = match parents_override {
        Some(mrc) => mrc.to_vec(),
        None => {
            let mut parents = Vec::with_capacity(targets.len() + 1);
            parents.push(local_id);
            parents.extend_from_slice(targets);
            parents
        }
    };
    let mut commit = gix::objs::Commit {
        message: msg.into(),
        tree: merged_tree,
        author: author.to_owned()?,
        committer: committer.to_owned()?,
        encoding: None,
        parents: parents.into_iter().collect(),
        extra_headers: Default::default(),
    };
    // `-S`/`--gpg-sign`/`commit.gpgsign`: sign the serialized commit and carry the
    // armored signature as the `gpgsig` header.
    if let Some(key) = &opts.sign {
        if let Err(code) = sign_commit(repo, &mut commit, key) {
            return Ok(code);
        }
    }
    let new_id = repo.write_object(&commit)?.detach();
    advance(
        repo,
        local_id,
        new_id,
        format!("{}: {finish_msg}", reflog_action(&spec_label)),
    )?;
    if !opts.quiet {
        println!("{finish_msg}");
        print!("{}", diffstat(repo, head_tree, merged_tree, opts.stat)?);
    }
    end_autostash(repo, stash, true)?;
    // git's `finish(); remove_merge_branch_state();` (builtin/merge.c:1007,
    // 1038), in that order — `finish()` re-applies the autostash, whose `git
    // stash apply` child writes its own `AUTO_MERGE`, and only then is the merge
    // state cleared. Clearing first left that file behind.
    remove_merge_state(git_dir, false);
    Ok(ExitCode::SUCCESS)
}

/// git's `write_merge_heads()`: every merged head in `MERGE_HEAD`, plus the
/// `MERGE_MODE` marker that records whether the merge may fast-forward.
fn write_merge_heads(repo: &gix::Repository, targets: &[ObjectId], ff: Ff) -> Result<()> {
    let git_dir = repo.git_dir();
    let mut merge_head = String::new();
    for t in targets {
        merge_head.push_str(&format!("{t}\n"));
    }
    std::fs::write(git_dir.join("MERGE_HEAD"), merge_head)?;
    std::fs::write(git_dir.join("MERGE_MODE"), merge_mode(ff))?;
    Ok(())
}

/// `git-merge-octopus`, run through `try_merge_command()`: fold each head into
/// the result with a three-way merge and hand `cmd_merge` the index that comes
/// out, which becomes an n-parent commit. Any head that cannot merge cleanly
/// stops the octopus there (git does not resolve conflicts under octopus).
///
/// Over a *single* head the script's second guard — "Reject if this is not an
/// octopus -- resolve should be used instead" — is `case "$remotes" in ?*' '?*)
/// ;; *) exit 2 ;; esac`, which needs a non-empty run on both sides of a space
/// in the trailing-space-separated head list. One head fails it, so the script
/// exits 2 before its `diff-index` pre-flight, before a single line of its merge
/// loop, and without printing anything.
fn octopus_attempt(repo: &gix::Repository, ctx: &MergeCtx<'_>, opts: &Opts) -> Result<Attempt> {
    if ctx.targets.len() < 2 {
        return Ok(Attempt::Refused);
    }
    // Every head, resolved by the caller; pair each with its spec for messages.
    let heads: Vec<(String, ObjectId)> = ctx
        .head_labels
        .iter()
        .cloned()
        .zip(ctx.targets.iter().copied())
        .collect();

    let mut cur_index = repo.index_or_load_from_head()?.into_owned();
    let mut mrt = ctx.head_tree; // merge result tree

    // `git-merge-octopus`'s opening `if ! git diff-index --quiet --cached HEAD --`:
    // a staged change stops the octopus before the first head is looked at. The
    // paths are printed by the strategy itself (on stdout, four-space indented),
    // and `cmd_merge` adds the failure line.
    //
    // `collect_parents()` drops every head already reachable from HEAD before a
    // strategy is dispatched, so an octopus with nothing left to merge is
    // answered by the up-to-date path in `do_merge` and never reaches the gate.
    let staged = crate::merge_guard::index_changes_from_head(repo, mrt, &cur_index)?;
    if !staged.is_empty() {
        println!("Error: Your local changes to the following files would be overwritten by merge");
        for path in &staged {
            println!("    {}", quote_path(path));
        }
        log_strategy_failure(repo, ctx.local_id, &ctx.refs.join(" "));
        return Ok(Attempt::Refused);
    }
    // `MRC` (git's merge-result-commit list): the parents of the eventual commit.
    // It starts as HEAD but, while still a single commit, is *replaced* by a head
    // that fast-forwards it (so `merge a b` where main is an ancestor of `a` yields
    // parents `[a, b]`, not `[main, a, b]`).
    let mut mrc: Vec<ObjectId> = vec![ctx.local_id];
    let should_interrupt = AtomicBool::new(false);

    for (spec, head_id) in &heads {
        let tip = if mrc.len() == 1 { mrc[0] } else { ctx.local_id };
        let all_bases = repo.merge_bases_many(tip, &[*head_id])?;
        let common = repo.merge_base(tip, *head_id)?.detach();
        if common == *head_id {
            if !opts.quiet {
                println!("Already up to date with {spec}");
            }
            continue;
        }
        let head_tree = repo.find_object(*head_id)?.peel_to_tree()?.id;

        // Fast-forward: while MRC is still a single commit and it is the merge base,
        // git advances the base line to this head rather than recording a parent.
        if mrc.len() == 1 && common == mrc[0] {
            // `git-merge-octopus` announces each step it takes.
            if !opts.quiet {
                println!("Fast-forwarding to: {spec}");
            }
            // Each head is folded in by `git read-tree -u -m`, whose refusals
            // carry the plumbing wording — `setup_unpack_trees_porcelain()`
            // never runs for a strategy script.
            let clobber = crate::merge_guard::verify_two_way(repo, mrt, head_tree, &cur_index)?;
            if !clobber.is_empty() {
                clobber.report_plumbing();
                return Ok(Attempt::Refused);
            }
            update_worktree(repo, &cur_index, Some(mrt), head_tree, &should_interrupt)?;
            cur_index = repo.index_from_tree(&head_tree)?;
            mrt = head_tree;
            mrc = vec![*head_id];
            continue;
        }

        if !opts.quiet {
            println!("Trying simple merge with {spec}");
        }
        let base_tree = repo.find_object(common)?.peel_to_tree()?.id;
        // `git read-tree -u -m --aggressive $common $MRT $SHA1 || exit 2`, which
        // the script runs before the `git write-tree` that follows it. Refusing
        // here rather than over the merged tree is what keeps a failed octopus
        // from leaving that tree in the object database.
        let clobber =
            crate::merge_guard::verify_three_way(repo, base_tree, mrt, head_tree, &cur_index)?;
        if !clobber.is_empty() {
            clobber.report_plumbing();
            return Ok(Attempt::Refused);
        }
        // `merge_ort_internal()`'s ancestor name again — see the recursive path
        // above. Stock's octopus never reaches a rendering that shows it: it
        // resolves unmerged paths through `git merge-index -o git-merge-one-file`,
        // whose `git merge-file "$src1" "$orig" "$src2"` passes no `-L`, so a
        // `diff3` conflict there is labelled with the run's `.merge_file_XXXXXX`
        // temporary names. This driver renders merge-ort conflicts (the
        // divergence `merge_octopus.rs` documents), so it names the base the way
        // merge-ort does.
        let ancestor = if all_bases.len() > 1 {
            "merged common ancestors".to_string()
        } else {
            common.attach(repo).shorten_or_id().to_string()
        };
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(ancestor.as_bytes())),
            current: Some(BStr::new(b"HEAD")),
            other: Some(BStr::new(spec.as_bytes())),
        };
        let merged = crate::merge_apply::three_way_merge_guarded(
            repo,
            base_tree,
            mrt,
            head_tree,
            &cur_index,
            labels,
            &should_interrupt,
            merge_verbosity(repo) != 0,
            &crate::merge_apply::StrategyOptions::default(),
            mrt,
        )?;
        let applied = match merged {
            crate::merge_apply::Merged::Applied(applied) => applied,
            crate::merge_apply::Merged::Refused(clobber) => {
                clobber.report_plumbing();
                return Ok(Attempt::Refused);
            }
        };
        cur_index = applied.index;
        crate::index_racy::write(repo, &mut cur_index)?;

        if !applied.conflicts.is_empty() {
            // Octopus aborts on the first conflicting head, leaving the
            // conflicted worktree and index. Everything downstream — MERGE_HEAD
            // over every head, the `# Conflicts:` hint, rerere, the notice — is
            // `cmd_merge`'s shared tail, not the strategy's.
            return Ok(Attempt::Conflicts(applied.conflicts));
        }
        mrt = applied.tree_id;
        mrc.push(*head_id);
    }

    // Nothing merged: every head was already reachable.
    //
    // Unreachable through `git merge`, and kept for the shape of the script
    // rather than for a case it answers: `collect_parents()` ends in
    // `reduce_heads()` (see [`independent_heads`]), so every head that survives
    // into `ctx.targets` is independent of `HEAD` and of the others. A head
    // reachable from `HEAD` is dropped there and the up-to-date path in
    // `do_merge` answers the merge before a strategy is dispatched — measured on
    // a fixture whose two heads are both ancestors of `HEAD`, where stock and
    // this port alike print `Already up to date.` without entering the octopus.
    // `git merge-octopus` invoked directly *can* reach it, and that driver is
    // [`super::merge_octopus`], not this one.
    if mrc.len() == 1 && mrc[0] == ctx.local_id {
        if !opts.quiet {
            println!("{}", up_to_date_line(opts));
        }
        return Ok(Attempt::Done { code: ExitCode::SUCCESS, autostash_applied: true });
    }
    // Everything collapsed onto one line via fast-forward — a plain fast-forward,
    // not an octopus commit.
    //
    // Also unreachable through `git merge`, for the same `reduce_heads()`
    // reason: only the *first* head can fast-forward (its merge base with
    // `HEAD` is `HEAD` itself), and a second head that also fast-forwarded would
    // have to descend from the first, which makes the first non-independent and
    // drops it. So the loop leaves `mrc` with either one element that is still
    // `HEAD` (handled above) or two or more. Measured: on a linear `HEAD -> b ->
    // a`, `git merge b a` never reaches the octopus at all — one head survives
    // reduction and stock and this port both take the plain fast-forward path.
    if mrc.len() == 1 {
        advance(
            repo,
            ctx.local_id,
            mrc[0],
            format!("{}: Fast-forward", reflog_action(&ctx.refs.join(" "))),
        )?;
        if !opts.quiet {
            println!("Fast-forward");
        }
        return Ok(Attempt::Done { code: ExitCode::SUCCESS, autostash_applied: true });
    }

    Ok(Attempt::Clean {
        tree: mrt,
        // Every merged head becomes a parent (`mrc` minus HEAD).
        heads: mrc.iter().copied().filter(|p| *p != ctx.local_id).collect(),
        // `git-merge-octopus` commits exactly its MRC: when the first head
        // fast-forwarded the base line past `HEAD`, `HEAD` was replaced there and
        // is not a parent — `merge a b` from an ancestor yields `[a, b]`, not
        // `[HEAD, a, b]`.
        parents_override: Some(mrc),
        spec_label: ctx.refs.join(" "),
    })
}

/// The default octopus commit subject: `Merge branches 'a', 'b' and 'c'`.
fn octopus_message(refs: &[&str]) -> String {
    let quoted: Vec<String> = refs.iter().map(|r| format!("'{r}'")).collect();
    let joined = match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, init)) => format!("{} and {}", init.join(", "), last),
        None => String::new(),
    };
    format!("Merge branches {joined}\n")
}

// ---------------------------------------------------------------------------
// Configuration: merge.stat/merge.diffstat, merge.log/merge.summary
// ---------------------------------------------------------------------------

/// `git_parse_maybe_bool_text()`: the textual booleans only. An empty value is
/// false and anything else is "not a boolean" (`None`).
///
/// Fidelity gap shared with the rest of this build: gix reports a *valueless*
/// key (`[merge]\n\tstat`) as an empty value, where git would see `NULL` and
/// treat it as true.
fn maybe_bool_text(value: &BStr) -> Option<bool> {
    let text = value.to_str().ok()?;
    if text.is_empty() {
        return Some(false);
    }
    match text.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// `merge.stat` / `merge.diffstat` — equivalent keys in `git_merge_config()`, so
/// the last one in configuration order wins. A boolean picks between no diffstat
/// and the ordinary one; the literal `compact` selects the compact summary; any
/// other value falls back to the ordinary diffstat (git's `default:` branch).
fn stat_config(snapshot: &gix::config::Snapshot<'_>) -> Option<StatMode> {
    let mut chosen = None;
    for section in snapshot.plumbing().sections() {
        let header = section.header();
        if header.subsection_name().is_some()
            || !header.name().to_string().eq_ignore_ascii_case("merge")
        {
            continue;
        }
        // Per-name cursors over the two spellings, walked in the order the
        // section actually lists them (as `comment_string` does for core.*).
        let body = section.body();
        let stats = body.values("stat");
        let diffstats = body.values("diffstat");
        let (mut stat_at, mut diffstat_at) = (0usize, 0usize);

        for value_name in body.value_names() {
            let value = if value_name.eq_ignore_ascii_case("stat") {
                let value = stats.get(stat_at);
                stat_at += 1;
                value
            } else if value_name.eq_ignore_ascii_case("diffstat") {
                let value = diffstats.get(diffstat_at);
                diffstat_at += 1;
                value
            } else {
                continue;
            };
            let Some(value) = value else { continue };
            let value: &BStr = value.as_ref();
            chosen = Some(match maybe_bool_text(value) {
                Some(false) => StatMode::None,
                Some(true) => StatMode::Diffstat,
                None if value == BStr::new("compact") => StatMode::CompactSummary,
                None => StatMode::Diffstat,
            });
        }
    }
    chosen
}

/// `merge.log` / `merge.summary` (the deprecated synonym) folded to a shortlog
/// length by `git_config_bool_or_int()`: an integer is taken as-is, a true
/// boolean is `DEFAULT_MERGE_LOG_LEN`, a false one (and an unset key) is 0.
fn shortlog_config(snapshot: &gix::config::Snapshot<'_>) -> i64 {
    let plumbing = snapshot.plumbing();
    let log = plumbing.values::<BString>("merge.log").unwrap_or_default();
    let summary = plumbing.values::<BString>("merge.summary").unwrap_or_default();
    log.last()
        .or(summary.last())
        .and_then(|v| bool_or_int(v.as_bstr()))
        .unwrap_or(0)
}

/// `git_parse_int()` behind parse-options' `OPT_INTEGER`, through the shared
/// [`crate::optint`] grammar: `strtoimax` with **base 0** (so `0x10` and `010`
/// are hex and octal), leading whitespace and `+` allowed, one optional
/// `k`/`m`/`g` binary suffix. `None` is git's "not an integer".
fn parse_option_int(value: &str) -> Option<i64> {
    crate::optint::integer(&crate::optint::long_opt("log"), value).ok()
}

/// `git_config_bool_or_int()` as the shortlog length reads it.
fn bool_or_int(value: &BStr) -> Option<i64> {
    let text = value.to_str().ok()?;
    if let Ok(n) = text.trim().parse::<i64>() {
        return Some(n);
    }
    match text.to_ascii_lowercase().as_str() {
        "" | "true" | "yes" | "on" => Some(DEFAULT_MERGE_LOG_LEN),
        "false" | "no" | "off" => Some(0),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The merge message: title, `--log` shortlog, `--edit` comment block
// ---------------------------------------------------------------------------

/// The for-merge heads of `.git/FETCH_HEAD` as `(commit, description)`, in file
/// order — `handle_fetch_head()` (builtin/merge.c).
///
/// A row is `<oid>\t<"" | not-for-merge>\t<description>`; only the rows with an
/// empty middle column are merged, and the description is what the fetch stored
/// (`branch 'main' of <url>`, `tag 'v1' of <url>`, or the bare URL for a `HEAD`
/// fetch). An absent file simply means there is nothing to merge.
pub(super) fn fetch_head_for_merge(repo: &gix::Repository) -> Result<Vec<(ObjectId, String)>> {
    let path = repo.git_dir().join("FETCH_HEAD");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut heads = Vec::new();
    for line in text.lines() {
        let mut fields = line.splitn(3, '\t');
        let (Some(oid), Some(kind), description) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !kind.is_empty() {
            continue; // not-for-merge
        }
        let Ok(id) = ObjectId::from_hex(oid.as_bytes()) else {
            continue;
        };
        heads.push((id, description.unwrap_or_default().to_string()));
    }
    Ok(heads)
}

/// `fmt_merge_msg_title()` (fmt-merge-msg.c) over FETCH_HEAD descriptions, or
/// `None` when these are ordinary ref names and the caller's own title applies.
///
/// `handle_line()` splits each description at ` of ` into what was merged and
/// where it came from, then groups by source and by kind, so two branches from
/// one remote read `Merge branches 'a' and 'b' of <url>` rather than repeating
/// the URL. Sources are joined with `; `, kinds within a source with `, `, and
/// the names of one kind with `, ` plus a final ` and `.
fn fetch_head_title(specs: &[String]) -> Option<String> {
    // Sources in first-seen order, each with its four name lists in git's order.
    let mut sources: Vec<(String, [Vec<String>; 4])> = Vec::new();
    let mut described_any = false;
    for spec in specs {
        let (what, src) = match spec.split_once(" of ") {
            Some((what, src)) => (what, src.to_string()),
            // No source: the whole line is one, as it is for a `HEAD` fetch.
            None => (spec.as_str(), spec.clone()),
        };
        let slot = match sources.iter().position(|(s, _)| *s == src) {
            Some(at) => at,
            None => {
                sources.push((src, Default::default()));
                sources.len() - 1
            }
        };
        // `branch 'x'` / `remote-tracking branch 'x'` / `tag 'x'` keep their
        // quoted name; anything else is a generic head named verbatim, and a
        // line that *is* its own source (`pulling_head`) names nothing.
        let (list, name) = if let Some(name) = what.strip_prefix("remote-tracking branch ") {
            described_any = true;
            (1, name)
        } else if let Some(name) = what.strip_prefix("branch ") {
            described_any = true;
            (0, name)
        } else if let Some(name) = what.strip_prefix("tag ") {
            described_any = true;
            (2, name)
        } else if what == sources[slot].0 {
            continue;
        } else {
            (3, what)
        };
        sources[slot].1[list].push(name.to_string());
    }
    if !described_any {
        return None;
    }

    let kinds = [
        ("branch ", "branches "),
        ("remote-tracking branch ", "remote-tracking branches "),
        ("tag ", "tags "),
        ("commit ", "commits "),
    ];
    let mut out = String::from("Merge ");
    for (i, (src, lists)) in sources.iter().enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        let mut first_kind = true;
        for (list, (singular, plural)) in lists.iter().zip(kinds) {
            if list.is_empty() {
                continue;
            }
            if !first_kind {
                out.push_str(", ");
            }
            first_kind = false;
            out.push_str(&join_named(singular, plural, list));
        }
        if src != "." {
            out.push_str(&format!(" of {src}"));
        }
    }
    Some(out)
}

/// `print_joined()`: `<singular><a>`, or `<plural><a>, <b> and <c>`.
fn join_named(singular: &str, plural: &str, names: &[String]) -> String {
    match names {
        [one] => format!("{singular}{one}"),
        _ => {
            let (last, rest) = names.split_last().expect("non-empty");
            format!("{plural}{} and {last}", rest.join(", "))
        }
    }
}

/// A FETCH_HEAD description read back as a [`SpecOrigin`], or `None` when the
/// string is an ordinary ref name.
///
/// `handle_line()` (fmt-merge-msg.c) strips the `branch `/`remote-tracking
/// branch ` prefix for the shortlog origin, keeps `tag ` (so its quotes survive),
/// and takes anything else — a bare URL, for a `HEAD` fetch — verbatim.
fn described_line(spec: &str) -> Option<SpecOrigin> {
    for prefix in ["branch '", "remote-tracking branch '"] {
        if let Some(rest) = spec.strip_prefix(prefix) {
            return Some(SpecOrigin {
                described: spec.to_string(),
                origin: rest.rsplit_once('\'').map_or(rest, |(name, _)| name).to_string(),
                is_local_branch: prefix == "branch '",
            });
        }
    }
    if spec.starts_with("tag '") {
        return Some(SpecOrigin {
            described: spec.to_string(),
            origin: spec.to_string(),
            is_local_branch: false,
        });
    }
    None
}

/// How one merged ref is named, in the two spellings git needs: the `merge_name()`
/// description that goes into the title, and the `handle_line()` *origin* that
/// heads the `--log` shortlog block.
struct SpecOrigin {
    /// e.g. `branch 'topic'`, `tag 'v1'`, `commit 'abc123'`.
    described: String,
    /// e.g. `topic`, `tag 'v1'`, `commit 'abc123'` — git keeps the `tag ` prefix
    /// and drops the quotes only when the whole origin is quoted.
    origin: String,
    /// Whether the origin is a local branch (gates `merge.branchdesc`).
    is_local_branch: bool,
}

/// The whole merge commit message: the title (or the explicit `-m`/`-F` text)
/// followed by the `--log` shortlog of every merged head.
fn compose_message(
    repo: &gix::Repository,
    refs: &[String],
    targets: &[ObjectId],
    branch: Option<&FullName>,
    local_id: ObjectId,
    opts: &Opts,
) -> Result<String> {
    // git's `opts.add_title = !have_message`: an explicit message replaces the
    // generated title but never the shortlog.
    // Heads that came from FETCH_HEAD carry their own descriptions, which git
    // groups by source and kind rather than naming one by one.
    let fetch_head_title = match &opts.message {
        Some(_) => None,
        None => fetch_head_title(refs),
    };
    let mut msg = match (&opts.message, fetch_head_title, refs.len()) {
        (Some(m), _, _) => {
            let mut m = m.clone();
            if !m.ends_with('\n') {
                m.push('\n');
            }
            m
        }
        (None, Some(title), _) => {
            let current = match (opts.into_name.as_deref(), branch) {
                (Some(n), _) => n.to_string(),
                (None, Some(b)) => b.shorten().to_str_lossy().into_owned(),
                (None, None) => "HEAD".to_string(),
            };
            match dest_suppressed(repo, &current) {
                true => format!("{title}\n"),
                false => format!("{title} into {current}\n"),
            }
        }
        (None, _, 1) => merge_message(repo, &refs[0], branch, opts.into_name.as_deref())?,
        (None, _, _) => {
            let specs: Vec<&str> = refs.iter().map(String::as_str).collect();
            octopus_message(&specs)
        }
    };
    if opts.log_len != 0 {
        append_shortlog(repo, refs, targets, local_id, opts, &mut msg)?;
    }
    Ok(msg)
}

/// The `--log[=<n>]` block: one `* <origin>:` shortlog per merged head, a port of
/// `fmt_merge_msg()`'s shortlog loop. `credit_people` (the `By`/`Via` comment
/// lines) is on only under `--edit`, as `builtin/merge.c` sets it.
fn append_shortlog(
    repo: &gix::Repository,
    refs: &[String],
    targets: &[ObjectId],
    head: ObjectId,
    opts: &Opts,
    out: &mut String,
) -> Result<()> {
    let comment = comment_char(repo);
    let credit = matches!(edit_wanted(opts), Ok(true));
    complete_line(out);
    for (spec, tip) in refs.iter().zip(targets.iter()) {
        let origin = describe_spec(repo, spec);
        shortlog(repo, &origin, *tip, head, opts, &comment, credit, out)?;
    }
    complete_line(out);
    Ok(())
}

/// git's `shortlog()`: the `* <name>:` block for one merged tip, listing at most
/// `limit` subjects (newest first) and switching the header to
/// `: (<n> commits)` with a trailing `...` when more were merged.
#[allow(clippy::too_many_arguments)]
fn shortlog(
    repo: &gix::Repository,
    origin: &SpecOrigin,
    tip: ObjectId,
    head: ObjectId,
    opts: &Opts,
    comment: &str,
    credit: bool,
    out: &mut String,
) -> Result<()> {
    let limit = opts.log_len;
    let mut count = 0usize;
    let mut subjects: Vec<String> = Vec::new();
    let mut authors: Vec<(BString, usize)> = Vec::new();
    let mut committers: Vec<(BString, usize)> = Vec::new();

    let walk = repo
        .rev_walk([tip])
        .with_hidden([head])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    for info in walk.all()? {
        let commit = info?.object()?;
        if commit.parent_ids().count() > 1 {
            // Merges are not listed, but their committer is still credited.
            record_person(&mut committers, commit.committer()?.trim().name);
            continue;
        }
        if count == 0 {
            record_person(&mut committers, commit.committer()?.trim().name);
        }
        record_person(&mut authors, commit.author()?.trim().name);
        count += 1;
        if subjects.len() as i64 > limit {
            continue;
        }
        let message = commit.message()?;
        let subject = message.summary();
        if subject.is_empty() {
            subjects.push(commit.id.to_hex().to_string());
        } else {
            subjects.push(subject.to_str_lossy().into_owned());
        }
    }

    if credit {
        add_people_info(repo, &mut authors, &mut committers, comment, out);
    }

    if count as i64 > limit {
        out.push_str(&format!("\n* {}: ({count} commits)\n", origin.origin));
    } else {
        out.push_str(&format!("\n* {}:\n", origin.origin));
    }

    if origin.is_local_branch && opts.branch_desc {
        add_branch_desc(repo, &origin.origin, out);
    }

    for (i, subject) in subjects.iter().enumerate() {
        if i as i64 >= limit {
            out.push_str("  ...\n");
        } else {
            out.push_str(&format!("  {subject}\n"));
        }
    }
    Ok(())
}

/// git's `record_person()`: count one appearance, keeping the list sorted by name.
fn record_person(people: &mut Vec<(BString, usize)>, name: &BStr) {
    match people.binary_search_by(|(known, _)| known.as_bstr().cmp(name)) {
        Ok(at) => people[at].1 += 1,
        Err(at) => people.insert(at, (name.to_owned(), 1)),
    }
}

/// git's `add_people_info()`: the `By`/`Via` credit lines, ordered by descending
/// appearance count.
fn add_people_info(
    repo: &gix::Repository,
    authors: &mut [(BString, usize)],
    committers: &mut [(BString, usize)],
    comment: &str,
    out: &mut String,
) {
    authors.sort_by_key(|a| std::cmp::Reverse(a.1));
    committers.sort_by_key(|c| std::cmp::Reverse(c.1));
    let me_author = identity(repo.author());
    let me_committer = identity(repo.committer());
    credit_people(authors, "By", me_author.as_deref(), comment, out);
    credit_people(committers, "Via", me_committer.as_deref(), comment, out);
}

/// `git_author_info(IDENT_NO_DATE)` / `git_committer_info(IDENT_NO_DATE)`.
fn identity(
    configured: Option<std::result::Result<gix::actor::SignatureRef<'_>, gix::config::time::Error>>,
) -> Option<String> {
    let signature = configured?.ok()?;
    Some(format!(
        "{} <{}>",
        signature.name.to_str_lossy(),
        signature.email.to_str_lossy()
    ))
}

/// git's `credit_people()`: the line is skipped when nobody, or only the
/// configured identity, would be credited.
fn credit_people(
    people: &[(BString, usize)],
    label: &str,
    me: Option<&str>,
    comment: &str,
    out: &mut String,
) {
    let only_me = people.len() == 1
        && me.is_some_and(|me| {
            me.as_bytes()
                .strip_prefix(people[0].0.as_slice())
                .is_some_and(|rest| rest.starts_with(b" <"))
        });
    if people.is_empty() || only_me {
        return;
    }
    out.push_str(&format!("\n{comment} {label} "));
    add_people_count(people, out);
}

/// git's `add_people_count()`.
fn add_people_count(people: &[(BString, usize)], out: &mut String) {
    match people {
        [] => {}
        [(name, _)] => out.push_str(&name.to_str_lossy()),
        [(a, an), (b, bn)] => out.push_str(&format!(
            "{} ({an}) and {} ({bn})",
            a.to_str_lossy(),
            b.to_str_lossy()
        )),
        [(a, an), ..] => out.push_str(&format!("{} ({an}) and others", a.to_str_lossy())),
    }
}

/// git's `add_branch_desc()`: `branch.<name>.description`, one `  : <line>` per
/// line (a literal prefix, not a comment one), ahead of the shortlog subjects.
fn add_branch_desc(repo: &gix::Repository, name: &str, out: &mut String) {
    let snapshot = repo.config_snapshot();
    let Some(desc) = snapshot.string(format!("branch.{name}.description").as_str()) else {
        return;
    };
    for line in desc.to_str_lossy().split_inclusive('\n') {
        out.push_str(&format!("  : {line}"));
    }
    complete_line(out);
}

/// git's `strbuf_add_commented_lines()`: every line gets the comment prefix, and
/// a separating space unless the line is empty or starts with a tab.
fn add_commented_lines(text: &str, comment: &str, out: &mut String) {
    for line in text.split_inclusive('\n') {
        out.push_str(comment);
        if !line.starts_with('\n') && !line.starts_with('\t') {
            out.push(' ');
        }
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
    }
}

/// git's `strbuf_complete_line()`.
fn complete_line(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// The comment block `prepare_to_commit()` appends below the message when an
/// editor is opened. Under `--cleanup=scissors` the cut line comes first and the
/// closing sentence changes, since comment lines survive that mode.
fn append_editor_comment(msg: &mut String, cleanup: Cleanup, comment: &str) {
    // git strips the message's trailing newline and adds one back; the net effect
    // on a message that already ends in a newline is nothing.
    complete_line(msg);
    if cleanup == Cleanup::Scissors {
        msg.push_str(&format!(
            "{comment} ------------------------ >8 ------------------------\n"
        ));
        add_commented_lines(
            "Do not modify or remove the line above.\nEverything below it will be ignored.\n",
            comment,
            msg,
        );
        msg.push_str(&format!("{comment}\n"));
    }
    add_commented_lines(
        "Please enter a commit message to explain why this merge is necessary,\n\
         especially if it merges an updated upstream into a topic branch.\n\n",
        comment,
        msg,
    );
    if cleanup == Cleanup::Scissors {
        add_commented_lines("An empty message aborts the commit.\n", comment, msg);
    } else {
        add_commented_lines(
            &format!(
                "Lines starting with '{comment}' will be ignored, and an empty message aborts\n\
                 the commit.\n"
            ),
            comment,
            msg,
        );
    }
}

// ---------------------------------------------------------------------------
// --edit: the editor hand-off
// ---------------------------------------------------------------------------

/// git's `default_edit_option()` resolved against `-e`/`--edit`/`--no-edit`: an
/// explicit flag wins, an explicit message means no editor, `GIT_MERGE_AUTOEDIT`
/// decides next, and otherwise the editor is opened only for an interactive run.
///
/// Fidelity gap: git additionally requires stdin and stdout to be the *same*
/// file (same device/inode/mode), which is not reachable without `fstat` on the
/// descriptors; both being terminals is the test here.
fn edit_wanted(opts: &Opts) -> std::result::Result<bool, ExitCode> {
    if let Some(edit) = opts.edit {
        return Ok(edit);
    }
    if opts.message.is_some() {
        return Ok(false);
    }
    if let Ok(value) = std::env::var("GIT_MERGE_AUTOEDIT") {
        return match value.to_ascii_lowercase().as_str() {
            "" | "true" | "yes" | "on" | "1" => Ok(true),
            "false" | "no" | "off" | "0" => Ok(false),
            _ => {
                eprintln!("fatal: Bad value '{value}' in environment 'GIT_MERGE_AUTOEDIT'");
                Err(ExitCode::from(128))
            }
        };
    }
    Ok(std::io::stdin().is_terminal() && std::io::stdout().is_terminal())
}

/// git's `git_editor()`: `GIT_EDITOR`, then `core.editor`, then `$VISUAL`
/// (skipped on a dumb terminal), then `$EDITOR`, then `vi` — and nothing at all
/// when the terminal is dumb and none of them is set.
fn resolve_editor(repo: &gix::Repository, dumb: bool) -> Option<String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(e) = env("GIT_EDITOR") {
        return Some(e);
    }
    if let Some(e) = repo.config_snapshot().string("core.editor") {
        return Some(e.to_string());
    }
    if !dumb {
        if let Some(e) = env("VISUAL") {
            return Some(e);
        }
    }
    if let Some(e) = env("EDITOR") {
        return Some(e);
    }
    if dumb {
        return None;
    }
    Some("vi".to_string())
}

/// Open `path` in the configured editor and wait, git's `launch_editor()`. The
/// command runs through the shell so `core.editor = "code -w"` works, and stdio
/// is inherited so an interactive editor owns the terminal. `:` is git's
/// documented no-op editor. Returns whether the edit succeeded; the diagnostics
/// on failure are git's own.
fn launch_editor(repo: &gix::Repository, path: &Path) -> Result<bool> {
    let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(true);
    let Some(editor) = resolve_editor(repo, dumb) else {
        eprintln!("error: Terminal is dumb, but EDITOR unset");
        return Ok(false);
    };
    if editor == ":" {
        return Ok(true);
    }
    // `start_command()`'s `fflush(NULL)` (run-command.c:743): the editor takes
    // over the terminal, so nothing may still be sitting in the buffer.
    crate::cstdio::before_spawn();
    let status = crate::external::prepare_shell_cmd_str(&editor, [path]).status();
    match status {
        Ok(status) if status.success() => Ok(true),
        Ok(_) => {
            eprintln!("error: there was a problem with the editor '{editor}'");
            Ok(false)
        }
        Err(e) => {
            eprintln!("error: unable to start editor '{editor}': {e}");
            Ok(false)
        }
    }
}

// ---------------------------------------------------------------------------
// --autostash
// ---------------------------------------------------------------------------

/// The ref git parks the autostash commit under while the merge runs.
const MERGE_AUTOSTASH: &str = "MERGE_AUTOSTASH";

/// git's `create_autostash_ref()`: with `--autostash` (or `merge.autoStash`) and
/// a dirty worktree, snapshot the local changes into a stash-like commit, reset
/// the tracked tree to `HEAD`, and remember the commit under `MERGE_AUTOSTASH`.
/// A clean worktree stashes nothing, exactly as git's own dirty-check decides.
fn begin_autostash(repo: &gix::Repository, opts: &Opts) -> Result<Option<ObjectId>> {
    if !opts.autostash || !repo.is_dirty()? {
        return Ok(None);
    }
    let id = crate::porcelain::stash::create_autostash(repo)?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "merge: autostash".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name: MERGE_AUTOSTASH
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name {MERGE_AUTOSTASH}: {e}"))?,
        deref: false,
    })?;
    println!("Created autostash: {}", id.attach(repo).shorten_or_id());
    Ok(Some(id))
}

/// The other half: `apply_autostash_ref()` once the merge produced a new `HEAD`,
/// or — when it stopped early (conflict, `--squash`, `--no-commit`) — git's
/// pointer at the stash it left behind under `MERGE_AUTOSTASH`.
fn end_autostash(repo: &gix::Repository, stash: Option<ObjectId>, applied: bool) -> Result<()> {
    let Some(id) = stash else { return Ok(()) };
    if !applied {
        println!("When finished, apply stashed changes with `git stash pop`");
        return Ok(());
    }
    // The shared apply reports on stdout for `rebase`; merge's own notices go to
    // stderr, so it runs quiet here and the messages are emitted below.
    let conflicts = crate::porcelain::stash::apply_autostash(repo, id, true)?;
    if conflicts.is_empty() {
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
                message: Default::default(),
            },
            name: MERGE_AUTOSTASH
                .try_into()
                .map_err(|e| anyhow::anyhow!("invalid ref name {MERGE_AUTOSTASH}: {e}"))?,
            deref: false,
        })?;
        eprintln!("Applied autostash.");
    } else {
        // `apply_save_autostash_oid()`: a re-apply that conflicted stores the snapshot as
        // a real stash entry, which is what makes the advice below true, and drops
        // MERGE_AUTOSTASH now that `refs/stash` holds it.
        crate::porcelain::stash::store_commit(repo, id, "autostash")?;
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
                message: Default::default(),
            },
            name: MERGE_AUTOSTASH
                .try_into()
                .map_err(|e| anyhow::anyhow!("invalid ref name {MERGE_AUTOSTASH}: {e}"))?,
            deref: false,
        })?;
        // `apply_autostash()`'s wording for a re-apply that could not be completed.
        eprintln!("Your local changes are stashed, however applying them");
        eprintln!("resulted in conflicts.  You can either resolve the conflicts");
        eprintln!("and then discard the stash with \"git stash drop\", or, if you");
        eprintln!("do not want to resolve them now, run \"git reset --hard\" and");
        eprintln!("apply the local changes later by running \"git stash pop\".");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// -S / --gpg-sign
// ---------------------------------------------------------------------------

/// git's `sign_commit_to_strbuf()` (commit.c): sign the serialized commit and
/// attach the detached signature as its `gpgsig` header.
///
/// Everything about *which* backend, key and program that uses lives in
/// [`crate::gitsig::Signer`] — the same one `git commit -S` goes through.
/// Re-deriving it here is how this used to read `gpg.program` and nothing else,
/// which ran `gpg -bsa` against an ssh key whenever `gpg.format = ssh` was set
/// and reported `No secret key` for a key that signs fine.
fn sign_commit(
    repo: &gix::Repository,
    commit: &mut gix::objs::Commit,
    key: &str,
) -> std::result::Result<(), ExitCode> {
    let mut signer = crate::gitsig::Signer::resolve(repo);
    // `sign_buffer(..., signing_key, SIGN_BUFFER_USE_DEFAULT_KEY)`: a non-empty
    // `-S<keyid>` wins, an empty one leaves `get_signing_key()` in charge.
    if !key.is_empty() {
        signer.key = Some(key.to_string());
    }

    let mut payload = Vec::new();
    if let Err(e) = commit.write_to(&mut payload) {
        eprintln!("fatal: failed to serialize commit object: {e}");
        return Err(ExitCode::from(128));
    }
    match signer.sign(&payload) {
        Ok(signature) => {
            commit
                .extra_headers
                .push(("gpgsig".into(), signature.into()));
            Ok(())
        }
        Err(e) => {
            // `sign_buffer` reported with `error()`; `commit_tree_extended`'s
            // caller adds the `die()`. A `Fatal` is `get_signing_key()` dying on
            // its own, and nothing follows it.
            match e {
                crate::gitsig::SignFailure::Silent => {}
                crate::gitsig::SignFailure::Fatal(m) => eprintln!("fatal: {m}"),
                crate::gitsig::SignFailure::Error(m) => {
                    eprintln!("{}", crate::gitsig::report("error: ", &m));
                    eprintln!("fatal: failed to write commit object");
                }
            }
            Err(ExitCode::from(128))
        }
    }
}

// ---------------------------------------------------------------------------
// Option plumbing: strategy, cleanup, squash message, signoff, --continue
// ---------------------------------------------------------------------------

/// `MERGE_MODE`'s body: git writes `no-ff` (no trailing newline) when the merge
/// must not fast-forward, and an empty file otherwise.
fn merge_mode(ff: Ff) -> &'static [u8] {
    if ff == Ff::Never {
        b"no-ff"
    } else {
        b""
    }
}

/// Append one `-m`/`--message` value to the accumulating message buffer, joining
/// paragraphs with a blank line. Port of `option_parse_message()`
/// (builtin/merge.c): `strbuf_addf(buf, "%s%s", buf->len ? "\n\n" : "", arg)`.
fn append_message(buf: &mut Option<String>, arg: &str) {
    match buf {
        Some(existing) => {
            existing.push_str("\n\n");
            existing.push_str(arg);
        }
        None => *buf = Some(arg.to_string()),
    }
}

/// Map a `--cleanup=<mode>` value to its mode, or `None` for an invalid one.
fn parse_cleanup(value: &str) -> Option<Cleanup> {
    Some(match value {
        "default" => Cleanup::Default,
        "verbatim" => Cleanup::Verbatim,
        "whitespace" => Cleanup::Whitespace,
        "strip" => Cleanup::Strip,
        "scissors" => Cleanup::Scissors,
        _ => return None,
    })
}

/// Resolve a `-s`/`--strategy` value onto the engine that runs it.
///
/// `recursive` and `subtree` are not separate back-ends in git 2.55:
/// `try_merge_strategy()` dispatches `recursive`, `subtree` and `ort` alike to
/// `merge_ort_recursive()` (builtin/merge.c:800-834), differing only in the
/// `o.subtree_shift = ""` `subtree` sets first (builtin/merge.c:815-816). Both
/// therefore map onto the same engine here, `subtree` carrying its shift.
///
/// `resolve` and `octopus` are the two back-ends git still runs out of process
/// (`try_merge_command()`), and the two the `allow_trivial` in-index merge
/// precedes (builtin/merge.c:1611-1612, 1703-1731). Both get a variant of their
/// own so the head count — which `-s` parsing cannot know — decides between
/// `git-merge-octopus`'s two-or-more-heads path and its `exit 2` over one head.
///
/// An unknown name reproduces git's `Could not find merge strategy` diagnostic.
fn resolve_strategy(name: &str) -> std::result::Result<Strategy, ExitCode> {
    match name {
        "ort" | "recursive" => Ok(Strategy::Ort),
        "subtree" => Ok(Strategy::Subtree),
        "ours" => Ok(Strategy::Ours),
        "resolve" => Ok(Strategy::Resolve),
        "octopus" => Ok(Strategy::Octopus),
        _ => {
            eprintln!("Could not find merge strategy '{name}'.");
            eprintln!("Available strategies are: octopus ours recursive resolve subtree.");
            // `get_strategy()` ends its own diagnostic with a bare `exit(1)`
            // (builtin/merge.c:220) — not `die()`, so not 128, and not the
            // parse-options 129 either. Measured against stock 2.55.0:
            // `git merge -s bogus -s ort side` exits 1 having printed only
            // these two lines.
            Err(ExitCode::from(1))
        }
    }
}

/// `core.commentChar` for cleanup, defaulting to `#` (and treating `auto` and an
/// empty value as `#`). The full multi-valued `commentChar`/`commentString`
/// interleaving lives in `fmt-merge-msg`; a single character covers the merge
/// message paths.
fn comment_char(repo: &gix::Repository) -> String {
    match repo.config_snapshot().string("core.commentChar") {
        Some(v) => {
            let s = v.to_string();
            if s.is_empty() || s == "auto" {
                "#".to_string()
            } else {
                s
            }
        }
        None => "#".to_string(),
    }
}

/// Append git's `Signed-off-by:` trailer (from the committer identity) to a merge
/// message, inserting a blank separator line when the message does not already end
/// with one. This is the common title-only case; a message that already ends in a
/// trailer block is not de-duplicated (git's `append_signoff` scans for that).
fn append_signoff(repo: &gix::Repository, msg: &mut String) -> Result<()> {
    let sig = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let trailer = format!(
        "Signed-off-by: {} <{}>",
        sig.name.to_str_lossy(),
        sig.email.to_str_lossy()
    );
    if !msg.ends_with('\n') {
        msg.push('\n');
    }
    if !msg.ends_with("\n\n") {
        msg.push('\n');
    }
    msg.push_str(&trailer);
    msg.push('\n');
    Ok(())
}

/// git's `cleanup_message()` / `strbuf_stripspace()` for the modes merge exposes.
fn cleanup_message(input: &str, mode: Cleanup, comment: &str) -> String {
    match mode {
        Cleanup::Verbatim => input.to_string(),
        Cleanup::Scissors => {
            let marker = format!("{comment} ------------------------ >8 ------------------------");
            stripspace(&input[..scissors_cut(input, &marker)], None)
        }
        Cleanup::Strip => stripspace(input, Some(comment)),
        // merge's default when a message is supplied without an editor is
        // `whitespace`: strip trailing whitespace and blank runs, keep comments.
        Cleanup::Whitespace | Cleanup::Default => stripspace(input, None),
    }
}

/// The byte offset of the scissors line (`# ----- >8 -----`), or the input length
/// when it is absent; everything from there on is dropped.
fn scissors_cut(input: &str, marker: &str) -> usize {
    let mut pos = 0;
    for line in input.split_inclusive('\n') {
        if line.strip_suffix('\n').unwrap_or(line) == marker {
            return pos;
        }
        pos += line.len();
    }
    input.len()
}

/// Port of `strbuf_stripspace()`: rtrim every line, drop leading/trailing blank
/// lines and collapse consecutive blank lines to one; when `comment` is set,
/// lines starting with it are removed entirely.
fn stripspace(input: &str, comment: Option<&str>) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 1);
    let mut empties = 0usize;
    let mut rest = bytes;

    while !rest.is_empty() {
        let len = match rest.iter().position(|&b| b == b'\n') {
            Some(offset) => offset + 1,
            None => rest.len(),
        };
        let (line, tail) = rest.split_at(len);
        rest = tail;

        if let Some(c) = comment {
            if line.starts_with(c.as_bytes()) {
                continue;
            }
        }

        let mut end = line.len();
        while end > 0 && line[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end == 0 {
            empties += 1;
            continue;
        }
        if empties > 0 && !out.is_empty() {
            out.push(b'\n');
        }
        empties = 0;
        out.extend_from_slice(&line[..end]);
        out.push(b'\n');
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Write `SQUASH_MSG`, a port of `squash_message()` (builtin/merge.c): the header
/// line, then, for every non-merge commit reachable from `targets` but not from
/// `head` (newest first), a `commit <id>` line and its `git log`-medium body.
fn write_squash_msg(repo: &gix::Repository, targets: &[ObjectId], head: ObjectId) -> Result<()> {
    let mut out = String::from("Squashed commit of the following:\n");
    let walk = repo
        .rev_walk(targets.iter().copied())
        .with_hidden([head])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    for info in walk.all()? {
        let commit = info?.object()?;
        // `rev.ignore_merges = 1`: merge commits are skipped.
        if commit.parent_ids().count() > 1 {
            continue;
        }
        out.push('\n');
        out.push_str(&format!("commit {}\n", commit.id));
        let author = commit.author()?;
        out.push_str(&format!(
            "Author: {} <{}>\n",
            author.name.to_str_lossy(),
            author.email.to_str_lossy()
        ));
        let date = author.time()?.format_or_unix(gix::date::time::format::DEFAULT);
        out.push_str(&format!("Date:   {date}\n\n"));
        // medium format indents every message line by four spaces, empty ones too.
        let raw = commit.message_raw()?;
        let body = raw.to_str_lossy();
        for line in body.trim_end_matches('\n').split('\n') {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    std::fs::write(repo.git_dir().join("SQUASH_MSG"), out)?;
    Ok(())
}

/// Build a tree object from `index` (all stage-0 entries) and return its id, the
/// standard gix editor pass over the index in path order — the tree `--continue`
/// commits.
pub(crate) fn index_tree(repo: &gix::Repository, index: &gix::index::File) -> Result<ObjectId> {
    let backing = index.path_backing();
    let mut editor =
        gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, repo.object_hash());
    for entry in index.entries() {
        let path = entry.path_in(backing);
        let mode = entry
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("index entry `{path}` has an unrepresentable mode"))?;
        editor.upsert(path.split(|&b| b == b'/').map(|c| c.as_bstr()), mode.kind(), entry.id)?;
    }
    Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?)
}

/// `cmd_merge`'s `setenv("GIT_REFLOG_ACTION", "merge <heads>", 0)` (builtin/merge.c:1586)
/// followed by `"<GIT_REFLOG_ACTION>: <msg>"` (builtin/merge.c:492).
///
/// The `setenv` does not overwrite, so a caller that already set the variable wins:
/// that is how `git pull` leaves `pull: Fast-forward` in the reflog where a bare
/// merge leaves `merge origin/main: Fast-forward`, and how `git rebase`'s
/// integration steps keep their own action. Reading it here is the half of that
/// mechanism the merge side owes; `pull` has always set it.
/// The no-op HEAD update git records (`<reflog action>: updating HEAD`) when a strategy
/// refuses because the *index* does not match HEAD. Measured against stock 2.55.0 for
/// `ort`, `ours` and `octopus`: the staged-change refusal logs it, while the refusal
/// over unstaged worktree changes ("Your local changes … would be overwritten") does
/// not, so this is tied to the index guards alone.
fn log_strategy_failure(repo: &gix::Repository, head: ObjectId, spec: &str) {
    let msg = format!("{}: updating HEAD", reflog_action(spec));
    super::checkout::record_head_move(repo, Some(head), Some(head), &msg);
}

fn reflog_action(spec: &str) -> String {
    std::env::var("GIT_REFLOG_ACTION").unwrap_or_else(|_| format!("merge {spec}"))
}

/// `git merge --continue`: finish a merge whose conflicts have been resolved and
/// staged, writing the merge commit from the current index and clearing the
/// in-progress state, exactly as `git commit` does when `MERGE_HEAD` is present.
fn continue_merge(opts: &Opts) -> Result<ExitCode> {
    let repo = crate::setup::discover()?;
    let git_dir = repo.git_dir().to_owned();
    if !git_dir.join("MERGE_HEAD").exists() {
        eprintln!("fatal: There is no merge in progress (MERGE_HEAD missing).");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Refuse while the index still carries conflicted (stage 1/2/3) entries.
    let index = repo.open_index()?;
    if index.entries().iter().any(|e| e.stage() != Stage::Unconflicted) {
        eprintln!("error: Committing is not possible because you have unmerged files.");
        // `error_resolve_conflict` (sequencer.c) prints the error unconditionally
        // and the two-line direction only under `advice.resolveConflict`.
        crate::advice::Advice::ResolveConflict.advise_plain(
            "Fix them up in the work tree, and then use 'git add/rm <file>'\n\
             as appropriate to mark resolution and make a commit.",
        );
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        return Ok(ExitCode::from(128));
    }

    let head = repo.head()?;
    if head.is_unborn() {
        crate::git_fatal!("cannot conclude a merge on an unborn branch");
    }
    let local_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();

    // Parents: HEAD first, then every id listed in MERGE_HEAD.
    let mut parents: Vec<ObjectId> = vec![local_id];
    let merge_head = std::fs::read_to_string(git_dir.join("MERGE_HEAD"))?;
    for line in merge_head.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        parents.push(
            ObjectId::from_hex(line.as_bytes())
                .map_err(|e| anyhow::anyhow!("invalid id in MERGE_HEAD: {e}"))?,
        );
    }

    // Message from MERGE_MSG, comment lines (the `# Conflicts:` block) stripped as
    // git's finalize cleanup does.
    let raw = std::fs::read_to_string(git_dir.join("MERGE_MSG")).unwrap_or_default();
    let comment = comment_char(&repo);
    let mut msg = cleanup_message(&raw, Cleanup::Strip, &comment);
    if opts.signoff {
        append_signoff(&repo, &mut msg)?;
    }
    if !opts.no_verify {
        let msg_path = git_dir.join("COMMIT_EDITMSG");
        std::fs::write(&msg_path, &msg)?;
        let arg = msg_path.to_string_lossy().into_owned();
        if !crate::hooks::run(&repo, "commit-msg", &[&arg], None)? {
            return Ok(ExitCode::from(1));
        }
        msg = std::fs::read_to_string(&msg_path)?;
    }
    let subject = msg.lines().next().unwrap_or("").to_string();

    let tree_id = index_tree(&repo, &index)?;
    let commit_id = repo.commit("HEAD", &msg, tree_id, parents)?;

    remove_merge_state(&git_dir, true);
    let _ = crate::hooks::run(&repo, "post-merge", &["0"], None);

    if !opts.quiet {
        let short = commit_id.shorten_or_id();
        let branch_label = match repo.head_name()? {
            Some(name) => name.shorten().to_string(),
            None => "detached HEAD".to_string(),
        };
        println!("[{branch_label} {short}] {subject}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `strerror(errno)`: the bare message, without Rust's ` (os error <n>)` tail.
fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

/// Move `name` from `old` to `new`, writing `reflog` as the reflog message.
fn advance(repo: &gix::Repository, old: ObjectId, new: ObjectId, reflog: String) -> Result<()> {
    // git's `finish()` calls `update_ref(msg, "HEAD", …)`, and updating a symref
    // writes the entry to `.git/logs/HEAD` *and*, through the deref, to the
    // branch's own log. Editing the branch directly writes only the branch log
    // and leaves a hole in `HEAD`'s history where the merge was — `git reflog`
    // then shows the checkout before it as the most recent thing that happened.
    // The same edit is what `commit` uses (`commit.rs:1556`).
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: reflog.into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(old)),
            new: Target::Object(new),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
        deref: true,
    })?;
    Ok(())
}

/// `finish_up_to_date()`: the notice a merge with nothing to do prints, which
/// names the squash that will not happen when `--squash` asked for one.
fn up_to_date_line(opts: &Opts) -> &'static str {
    if opts.squash {
        "Already up to date. (nothing to squash)"
    } else {
        "Already up to date."
    }
}

/// Point `ORIG_HEAD` at `id`, as git does before it moves `HEAD`.
fn set_orig_head(repo: &gix::Repository, id: ObjectId) -> Result<()> {
    let name: FullName = "ORIG_HEAD"
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid ref name ORIG_HEAD: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "updating ORIG_HEAD".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name,
        deref: false,
    })?;
    Ok(())
}

/// The merge commit's message.
///
/// Port of `merge_name()` (builtin/merge.c) feeding `fmt_merge_msg_title()`
/// (fmt-merge-msg.c): the ref is described by the category it resolved into,
/// and ` into <branch>` is appended unless the current branch matches a
/// `merge.suppressDest` glob (defaulting to `main`/`master`), see
/// `dest_suppressed`.
fn merge_message(
    repo: &gix::Repository,
    spec: &str,
    branch: Option<&FullName>,
    into_name: Option<&str>,
) -> Result<String> {
    let described = describe_spec(repo, spec).described;

    // git's `into_name` overrides the destination name verbatim (used both for
    // the ` into <name>` title and the `merge.suppressDest` test); otherwise the
    // shortened current branch name (or `HEAD` when detached) is used.
    let current = match into_name {
        Some(n) => n.to_string(),
        None => match branch {
            Some(b) => b.shorten().to_str_lossy().into_owned(),
            None => "HEAD".to_string(),
        },
    };
    let mut out = format!("Merge {described}");
    if !dest_suppressed(repo, &current) {
        out.push_str(&format!(" into {current}"));
    }
    out.push('\n');
    Ok(out)
}

/// Port of `dest_suppressed()` and the default seeding in `fmt_merge_msg()`
/// (fmt-merge-msg.c): the merge title's ` into <branch>` is dropped when the
/// current branch matches any glob in `merge.suppressDest`, tested with
/// `wildmatch(pattern, branch, WM_PATHNAME)` — case-sensitive, and `*` does not
/// cross a `/`. The variable is multi-valued and accumulates in config order;
/// an empty value clears whatever was gathered so far. When the key is never
/// set at all, the list defaults to `main` then `master`.
fn dest_suppressed(repo: &gix::Repository, branch: &str) -> bool {
    let patterns = suppress_dest_patterns(repo);
    let value = branch.as_bytes().as_bstr();
    patterns
        .iter()
        .any(|p| gix::glob::wildmatch(p.as_bstr(), value, gix::glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL))
}

/// The accumulated `merge.suppressDest` pattern list, resolving git's
/// empty-value-clears rule and its `main`/`master` default when unset.
///
/// Fidelity gap: a *valueless* `merge.suppressDest` (no `=`) makes git die with
/// `config_error_nonbool` at config-parse time; gix reports it as an empty
/// value, indistinguishable from `suppressDest=`, so here it clears the list
/// rather than aborting. This is a config-subsystem limitation shared across
/// keys, not specific to the merge logic.
fn suppress_dest_patterns(repo: &gix::Repository) -> Vec<BString> {
    match repo.config_snapshot().raw_values("merge.suppressDest") {
        Ok(values) => {
            let mut list: Vec<BString> = Vec::new();
            for v in values {
                if v.is_empty() {
                    list.clear();
                } else {
                    list.push(v);
                }
            }
            list
        }
        // `suppress_dest_pattern_seen` never set → the built-in default.
        Err(_) => vec![BString::from("main"), BString::from("master")],
    }
}

/// Classify one merged ref the way `merge_name()` writes it into git's
/// FETCH_HEAD-shaped buffer and `handle_line()` reads it back out.
///
/// gix resolves a partial name through the same rule list git's `dwim_ref`
/// uses ("", tags, heads, remotes), so the full name it lands on is the category
/// git would have reported. An invalid ref name (`main~2`) is not an error here,
/// it just means no ref matched.
///
/// The origin is `handle_line()`'s: the `branch `/`remote-tracking branch `
/// prefix is dropped and the surrounding quotes with it, `tag ` is kept (so the
/// quotes survive), and anything else — a raw commit — is used verbatim.
fn describe_spec(repo: &gix::Repository, spec: &str) -> SpecOrigin {
    // A FETCH_HEAD line arrives already described — `handle_line()` reads exactly
    // these forms out of the file — so it is passed through rather than looked up
    // as a ref name, which is what it is not.
    if let Some(origin) = described_line(spec) {
        return origin;
    }
    if let Ok(Some(r)) = repo.try_find_reference(spec) {
        let full = r.name().as_bstr().to_str_lossy().into_owned();
        if full.starts_with("refs/heads/") {
            return SpecOrigin {
                described: format!("branch '{spec}'"),
                origin: spec.to_string(),
                is_local_branch: true,
            };
        }
        if full.starts_with("refs/tags/") {
            return SpecOrigin {
                described: format!("tag '{spec}'"),
                origin: format!("tag '{spec}'"),
                is_local_branch: false,
            };
        }
        if full.starts_with("refs/remotes/") {
            return SpecOrigin {
                described: format!("remote-tracking branch '{spec}'"),
                origin: spec.to_string(),
                is_local_branch: false,
            };
        }
    } else if let Some((name, early)) = early_part_of_branch(repo, spec) {
        // `branch 'x' (early part)` no longer ends in a quote, so git's
        // quote-stripping leaves the trailing tag — and the quotes — in place.
        return SpecOrigin {
            described: format!("branch '{name}'{}", if early { " (early part)" } else { "" }),
            origin: if early {
                format!("'{name}' (early part)")
            } else {
                name
            },
            is_local_branch: true,
        };
    }
    SpecOrigin {
        described: format!("commit '{spec}'"),
        origin: format!("commit '{spec}'"),
        is_local_branch: false,
    }
}

/// `merge_name()`'s second attempt: `<name>^^^` or `<name>~<number>` naming a
/// point inside an existing branch. The suffix is stripped and, if a branch by
/// the remaining name exists, that branch is what git reports — tagged
/// `(early part)` whenever the suffix actually walks back at least one commit.
fn early_part_of_branch(repo: &gix::Repository, spec: &str) -> Option<(String, bool)> {
    let bytes = spec.as_bytes();
    let mut len = 0usize;
    let mut early = false;

    let carets = bytes.iter().rev().take_while(|&&b| b == b'^').count();
    if carets > 0 && carets < bytes.len() {
        len = carets;
        early = true;
    } else if carets == 0 {
        if let Some(tilde) = spec.rfind('~') {
            let digits = &bytes[tilde + 1..];
            if digits.iter().all(u8::is_ascii_digit) {
                len = 1 + digits.len();
                // "name~" means "name~1"; "name~0" walks back nothing.
                early = digits.is_empty() || digits.iter().any(|&b| b != b'0');
            }
        }
    }

    if len == 0 || len >= bytes.len() {
        return None;
    }
    let stripped = &spec[..bytes.len() - len];
    match repo.try_find_reference(format!("refs/heads/{stripped}").as_str()) {
        Ok(Some(_)) => Some((stripped.to_string(), early)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Worktree + index transition
// ---------------------------------------------------------------------------

/// Move the worktree and its index from the state captured in `old` to
/// `new_tree`, writing only the paths that changed.
///
/// Added/modified files are checked out via `gix-worktree-state`, removed files
/// are deleted, and the new index is written reusing prior stats for unchanged
/// entries so a later status stays cheap.
///
/// `old_tree` names the tree the worktree currently holds, and decides which of
/// git's two shapes this takes:
///
/// * `Some(tree)` — `unpack_trees()` with `twoway_merge`: the footprint is the
///   paths `tree` and `new_tree` disagree on, and every entry outside it is
///   `keep_entry()`d, so staged work the merge does not touch survives. This is
///   what the fast-forward paths need, since git fast-forwards over a staged
///   change rather than refusing it.
/// * `None` — the index is the only reference point (`--abort` restoring a
///   conflicted state, `-s ours` keeping its own tree): the change set comes
///   from comparing `old` against `new_tree`, and `new_tree` becomes the index
///   wholesale. A path carrying a conflicted stage in `old` is always treated as
///   changed there, since its worktree file holds conflict markers rather than
///   any indexed blob.
fn update_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    old_tree: Option<ObjectId>,
    new_tree: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    // Index the current entries by path for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    let mut conflicted: HashSet<BString> = HashSet::new();
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing).to_owned();
            if e.stage_raw() != 0 {
                conflicted.insert(path.clone());
            }
            old_map.insert(path, (e.id, e.mode, e.stat));
        }
    }

    // `twoway_merge()`'s footprint when the tree being left is known: the paths
    // the two trees disagree on. Everything else is `keep_entry()`d below, so a
    // staged change outside the footprint survives untouched.
    let touched: Option<HashSet<BString>> = match old_tree {
        Some(old_tree) => {
            let before = repo.index_from_tree(&old_tree)?;
            let mut set: HashMap<BString, (ObjectId, Mode)> = HashMap::new();
            {
                let backing = before.path_backing();
                for e in before.entries() {
                    set.insert(e.path_in(backing).to_owned(), (e.id, e.mode));
                }
            }
            let after = repo.index_from_tree(&new_tree)?;
            let mut touched: HashSet<BString> = HashSet::new();
            {
                let backing = after.path_backing();
                for e in after.entries() {
                    let path = e.path_in(backing).to_owned();
                    match set.remove(&path) {
                        Some((id, mode)) if id == e.id && mode == e.mode => {}
                        _ => {
                            touched.insert(path);
                        }
                    }
                }
            }
            // What is left in `set` is only in the old tree: the move drops it.
            touched.extend(set.into_keys());
            Some(touched)
        }
        None => None,
    };

    // Full target index (all new-tree entries) — the basis of what is finally
    // written; a reduced copy of only the changed entries is what is checked out.
    let mut new_index = repo.index_from_tree(&new_tree)?;
    let mut subset = repo.index_from_tree(&new_tree)?;
    subset.remove_entries(|_, path, entry| {
        let path = path.to_owned();
        if let Some(touched) = &touched {
            return !touched.contains(&path);
        }
        if conflicted.contains(&path) {
            return false;
        }
        match old_map.get(&path) {
            // Present before with identical content and mode → unchanged, drop it.
            Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
            // Absent before → an addition, keep it.
            None => false,
        }
    });

    // Write the changed files into the worktree, overwriting in place.
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    // Remove the files the move drops. With the footprint known that is only a
    // path the new tree lost; without it, anything the index held and the new
    // tree does not.
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
            let owned = path.to_owned();
            if new_paths.contains(&owned) {
                continue;
            }
            if touched.as_ref().is_some_and(|t| !t.contains(&owned)) {
                continue;
            }
            if let Some(full) = repo.workdir_path(path) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    // Fresh stats produced by the checkout for the changed entries, with the content they belong
    // to: a stat is only valid for the entry that names the blob it was measured from, and
    // stamping it on any other entry hides a real difference from `status`, `diff` and `add`.
    let mut subset_stats: HashMap<BString, (ObjectId, gix::index::entry::Mode, Stat)> =
        HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    // Changed entries get their fresh stat; unchanged entries reuse the old one.
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some((_, _, stat)) =
                subset_stats.get(&path).filter(|(id, mode, _)| *id == e.id && *mode == e.mode)
            {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode && !conflicted.contains(&path) {
                    e.stat = *stat;
                }
            }
        }
    }

    // `keep_entry()`: outside the footprint the index entry survives as it was —
    // a staged modification keeps its blob, a staged deletion stays deleted, and
    // a staged addition the trees never knew about is carried over. Skipped when
    // the footprint is unknown, where the new tree simply is the new index.
    if let Some(touched) = &touched {
        // Untouched paths the index no longer carries stay gone, and a path
        // carrying conflict stages is dropped here so the push-back below can
        // restore every stage rather than flatten it onto the tree's entry.
        new_index.remove_entries(|_, path, _| {
            !touched.contains(path)
                && (!old_map.contains_key(path) || conflicted.contains(&path.to_owned()))
        });
        {
            let backing = new_index.path_backing().to_owned();
            for e in new_index.entries_mut() {
                let path = e.path_in(&backing).to_owned();
                if touched.contains(&path) {
                    continue;
                }
                if let Some((oid, mode, stat)) = old_map.get(&path) {
                    e.id = *oid;
                    e.mode = *mode;
                    e.stat = *stat;
                }
            }
        }
        let kept: HashSet<BString> = {
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
            if kept.contains(&path.to_owned()) || touched.contains(&path.to_owned()) {
                continue;
            }
            new_index.dangerously_push_entry(e.stat, e.id, e.flags, e.mode, path);
        }
        new_index.sort_entries();
    }

    // Drop any stale cache-tree extension before persisting.
    // `unpack_trees()` ends with `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`
    // (unpack-trees.c:2088-2092), so the index git leaves here carries a cache-tree.
    super::write_tree::rebuild_cache_tree(repo, &mut new_index);
    crate::index_racy::write(repo, &mut new_index)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Diffstat and summary (diff.c)
// ---------------------------------------------------------------------------

/// One diffstat row.
struct StatRow {
    /// Quoted path, as git's `fill_print_name` produces it.
    name: String,
    /// Inserted lines, or the new blob's byte size when `binary`.
    added: u64,
    /// Deleted lines, or the old blob's byte size when `binary`.
    deleted: u64,
    binary: bool,
    /// `get_compact_summary()`'s annotation for this pair, used only under
    /// `--compact-summary`; `None` for a content-only modification.
    compact: Option<&'static str>,
}

/// Port of `get_compact_summary()` (diff.c): the parenthesized note
/// `--compact-summary` folds into a diffstat name, in git's order — creation
/// (`new`/`new +x`/`new +l`), deletion (`gone`), then the symlink and
/// executable-bit mode transitions.
/// Whether a tree-diff change names a directory rather than a leaf.
///
/// `for_each_to_obtain_tree` reports the changed tree object *and* recurses into
/// it; git's diffstat comes from `diff-tree -r`, which reports leaves only.
fn is_tree_change(change: &TreeChange<'_, '_, '_>) -> bool {
    match change {
        TreeChange::Addition { entry_mode, .. } | TreeChange::Deletion { entry_mode, .. } => {
            entry_mode.is_tree()
        }
        TreeChange::Modification {
            previous_entry_mode,
            entry_mode,
            ..
        } => previous_entry_mode.is_tree() || entry_mode.is_tree(),
        TreeChange::Rewrite {
            source_entry_mode,
            entry_mode,
            ..
        } => source_entry_mode.is_tree() || entry_mode.is_tree(),
    }
}

fn compact_comment(old: Option<EntryKind>, new: Option<EntryKind>) -> Option<&'static str> {
    // DIFF_STATUS_ADDED.
    if old.is_none() {
        return Some(match new {
            Some(EntryKind::Link) => "new +l",
            Some(EntryKind::BlobExecutable) => "new +x",
            _ => "new",
        });
    }
    // DIFF_STATUS_DELETED.
    if new.is_none() {
        return Some("gone");
    }
    let (old, new) = (old.expect("old present"), new.expect("new present"));
    let (old_link, new_link) = (old == EntryKind::Link, new == EntryKind::Link);
    if old_link && !new_link {
        Some("mode -l")
    } else if !old_link && new_link {
        Some("mode +l")
    } else if old == EntryKind::Blob && new == EntryKind::BlobExecutable {
        Some("mode +x")
    } else if old == EntryKind::BlobExecutable && new == EntryKind::Blob {
        Some("mode -x")
    } else {
        None
    }
}




/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: &[u8]) -> String {
    crate::quote::quoted_name_string(path)
}

/// The block `finish()` (builtin/merge.c) prints after a merge, rendered as one
/// string: `DIFF_FORMAT_DIFFSTAT | DIFF_FORMAT_SUMMARY` for `--stat`, and the
/// diffstat alone with `stat_with_summary` — each name carrying its
/// ` (new)`/` (gone)`/` (mode +x)` annotation — for `--compact-summary`.
fn diffstat(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
    mode: StatMode,
) -> Result<String> {
    if mode == StatMode::None {
        return Ok(String::new());
    }
    let (mut rows, summary) = collect(repo, old_tree, new_tree)?;
    let mut out = String::new();
    if mode == StatMode::CompactSummary {
        // `fill_print_name()` appends the annotation to the name, so it counts
        // towards the name column's width.
        for row in &mut rows {
            if let Some(comment) = row.compact {
                row.name.push_str(&format!(" ({comment})"));
            }
        }
    }
    emit_stats(&mut out, &rows);
    if mode == StatMode::Diffstat {
        for line in &summary {
            out.push_str(&format!(" {line}\n"));
        }
    }
    Ok(out)
}

/// Walk the tree-to-tree diff once, producing the stat rows and the summary
/// lines, both ordered by path as git's tree recursion orders them.
fn collect(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
) -> Result<(Vec<StatRow>, Vec<String>)> {
    let old = repo.find_tree(old_tree)?;
    let new = repo.find_tree(new_tree)?;
    let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;

    // Per row: path (for ordering), display name, line counts, and the id of each
    // side paired with whether that side is a gitlink — needed when the blob diff
    // declined, either because a side is binary (git reports its size) or because
    // a side is a submodule (git diffs its `Subproject commit` line). Contents are
    // looked up after the walk so the callback stays infallible.
    type RawRow = (
        BString,
        String,
        Option<(u64, u64)>,
        Option<(ObjectId, bool)>,
        Option<(ObjectId, bool)>,
        Option<&'static str>,
    );
    let mut raw: Vec<RawRow> = Vec::new();
    let mut summary: Vec<(BString, String)> = Vec::new();

    let mut platform = old.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });
    let _rewrites = platform.for_each_to_obtain_tree(&new, |change| {
        // The walk reports a changed directory alongside the files it recurses
        // into; git's diffstat is over `diff-tree -r`, which names only the
        // leaves. Without this a merge that touches `lib/lib.txt` also prints a
        // bogus `lib | Bin <n> -> <n> bytes` row for the tree object itself.
        if is_tree_change(&change) {
            return Ok(Action::Continue(()));
        }
        let path: BString = change.location().to_owned();
        let display = quote_path(&path[..]);
        let (old_id, new_id, compact) = match change {
            TreeChange::Addition { entry_mode, id, .. } => {
                summary.push((
                    path.clone(),
                    format!("create mode {:06o} {display}", entry_mode.value()),
                ));
                (
                    None,
                    Some((id.detach(), entry_mode.is_commit())),
                    compact_comment(None, Some(entry_mode.kind())),
                )
            }
            TreeChange::Deletion { entry_mode, id, .. } => {
                summary.push((
                    path.clone(),
                    format!("delete mode {:06o} {display}", entry_mode.value()),
                ));
                (
                    Some((id.detach(), entry_mode.is_commit())),
                    None,
                    compact_comment(Some(entry_mode.kind()), None),
                )
            }
            TreeChange::Modification {
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
                ..
            } => {
                if previous_entry_mode.value() != entry_mode.value() {
                    summary.push((
                        path.clone(),
                        format!(
                            "mode change {:06o} => {:06o} {display}",
                            previous_entry_mode.value(),
                            entry_mode.value()
                        ),
                    ));
                }
                (
                    Some((previous_id.detach(), previous_entry_mode.is_commit())),
                    Some((id.detach(), entry_mode.is_commit())),
                    compact_comment(Some(previous_entry_mode.kind()), Some(entry_mode.kind())),
                )
            }
            // Rewrites cannot occur: rename tracking is off above.
            TreeChange::Rewrite {
                source_entry_mode,
                source_id,
                entry_mode,
                id,
                ..
            } => (
                Some((source_id.detach(), source_entry_mode.is_commit())),
                Some((id.detach(), entry_mode.is_commit())),
                compact_comment(Some(source_entry_mode.kind()), Some(entry_mode.kind())),
            ),
        };

        let counts = change
            .diff(&mut resource_cache)
            .ok()
            .and_then(|mut p| p.line_counts().ok())
            .flatten()
            .map(|c| (u64::from(c.insertions), u64::from(c.removals)));
        raw.push((path, display, counts, old_id, new_id, compact));

        resource_cache.clear_resource_cache_keep_allocation();
        Ok::<_, std::convert::Infallible>(Action::Continue(()))
    })?;
    drop(platform);

    // What a side diffs as, per `diff_populate_filespec()`: the blob for a real
    // entry, and for a gitlink the `Subproject commit <oid>` line git substitutes
    // for an object that lives in the submodule rather than here. An absent side
    // is empty, which is the 0 `diff_filespec_size` reports for an invalid
    // filespec.
    let content = |side: Option<(ObjectId, bool)>| -> Result<Vec<u8>> {
        match side {
            None => Ok(Vec::new()),
            Some((id, is_submodule)) => filespec::content_of(repo, id, is_submodule),
        }
    };

    let mut rows: Vec<(BString, StatRow)> = Vec::with_capacity(raw.len());
    for (path, name, counts, old_id, new_id, compact) in raw {
        // The tree diff has no blob to hand its resource cache for a gitlink, so it
        // yields no line counts; git diffs the substituted `Subproject commit`
        // lines, which is what makes a bumped submodule the ` 1 insertion(+), 1
        // deletion(-)` git reports rather than a lookup of a commit this object
        // database legitimately does not have.
        let submodule_side = matches!(old_id, Some((_, true))) || matches!(new_id, Some((_, true)));
        let counts = match counts {
            None if submodule_side => {
                let (added, deleted) =
                    filespec::count_changed_lines(&content(old_id)?, &content(new_id)?)?;
                Some((added as u64, deleted as u64))
            }
            other => other,
        };
        let row = match counts {
            Some((added, deleted)) => StatRow {
                name,
                added,
                deleted,
                binary: false,
                compact,
            },
            None => StatRow {
                name,
                added: content(new_id)?.len() as u64,
                deleted: content(old_id)?.len() as u64,
                binary: true,
                compact,
            },
        };
        rows.push((path, row));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    summary.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((
        rows.into_iter().map(|(_, r)| r).collect(),
        summary.into_iter().map(|(_, l)| l).collect(),
    ))
}

/// The rows [`super::diffstat::show_stats`] renders. A binary row's two "counts"
/// are the blob sizes, which is what `builtin_diffstat()` stores for one.
fn stat_rows(files: &[StatRow]) -> Vec<diffstat::StatFile> {
    files
        .iter()
        .map(|f| diffstat::StatFile {
            print_name: f.name.clone().into_bytes(),
            added: f.added,
            deleted: f.deleted,
            binary: f.binary,
            // `finish()` renders the merge result against HEAD, a committed pair.
            is_unmerged: false,
        })
        .collect()
}

/// `show_stats()` (diff.c). `builtin/merge.c:515` calls `init_diffstat_widths()`,
/// so the post-merge diffstat scales to `term_columns()` like `git diff` does.
fn emit_stats(out: &mut String, files: &[StatRow]) {
    let mut bytes = Vec::new();
    diffstat::show_stats(
        &mut bytes,
        &stat_rows(files),
        &StatWidths::default(),
        &super::diff_color::DiffColors::disabled(),
    );
    out.push_str(&String::from_utf8_lossy(&bytes));
}