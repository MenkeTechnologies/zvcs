//! `git cherry-pick <commit>...` — replay the change each commit introduces on
//! top of `HEAD`, recording a new commit for each.
//!
//! ## Order of operations
//!
//! git's `builtin/revert.c` runs a fixed pipeline, and the exit code a given
//! command line produces depends entirely on which stage it dies in. This port
//! reproduces that pipeline stage for stage, because a great many command lines
//! never reach the pick itself:
//!
//! 1. **Option parsing**, left to right. `--cleanup` is stored raw and only its
//!    *final* value is validated once parsing finishes (last-wins;
//!    `--no-cleanup` clears it), so `--cleanup=bogus --cleanup=default` is fine
//!    and `--cleanup=default --cleanup=bogus` dies. A bad final `--cleanup` mode
//!    dies with `fatal: Invalid cleanup mode <v>` and status 128 *here*, before
//!    anything else — including before the sequencer verbs, so
//!    `--cleanup=bogus --quit` is a failure even though `--quit` alone succeeds.
//!    `--empty` and `-m`, by contrast, validate *eagerly* per value as git's
//!    parse-options callbacks do, so a bad one of those (status 129) outranks a
//!    later bad `--cleanup`. A missing or malformed option value prints only its
//!    `error:` line and exits 129 without the usage block. Options git does not
//!    know are *kept*, not rejected (`PARSE_OPT_KEEP_UNKNOWN_OPT`), and only turn
//!    into an error in stage 5.
//!    The four sequencer verbs share one `OPT_CMDMODE` slot, so a *second,
//!    different* verb is rejected the instant parsing reaches it, with
//!    `error: options '<this>' and '<earlier>' cannot be used together` and
//!    status 129 — before the `--cleanup` validation above, and outranked only
//!    by an in-parse error that came earlier in the argument list. Repeating one
//!    verb is fine.
//! 2. **Sequencer verbs** (`--quit`, `--continue`, `--abort`, `--skip`). Options
//!    that would have no meaning alongside a verb are rejected with
//!    `fatal: cherry-pick: <opt> cannot be used with <verb>` and status 128.
//!    `--quit` with nothing in progress is a silent success; the other three
//!    report `no cherry-pick[ or revert] in progress` and exit 128.
//! 3. **No revisions given** → the usage block on stderr, status 129.
//! 4. **Revision resolution**, in argument order. The first spec that does not
//!    resolve dies with `fatal: bad revision '<spec>'` and status 128. This is
//!    why `--strategy=nonexistent-strategy README.md` reports the *revision*, not
//!    the strategy: nothing validates a strategy name before this point.
//! 5. **Leftover unknown options** → usage, status 129. A bad revision in stage 4
//!    outranks this, matching `setup_revisions`, which dies on the revision as it
//!    walks the argument list and only reports leftovers once it finishes.
//! 6. **Dirty worktree** → `fatal: cherry-pick failed`, status 128.
//! 7. The picks themselves.
//!
//! ## What is served
//!
//! ## Merge strategies
//!
//! `do_pick_commit` branches on the strategy *name* alone, and validates none of
//! it beforehand. No `--strategy`, `--strategy=recursive` and `--strategy=ort`
//! all take the in-process three-way merge described below; every other name is
//! handed to `try_merge_command`, which runs `git merge-<name>` as a child with
//! `merge-<name> [--<xopt>...] <base> -- <HEAD> <commit>` and adopts its exit
//! status. That is why `--strategy=octopus` fails with status 2 and *no* hints
//! (`git merge-octopus` refuses a two-head merge), `--strategy=resolve` succeeds
//! after printing `Trying simple merge.`, and `--strategy=nonexistent-strategy`
//! reaches the child and dies as an unknown git subcommand with status 1 — the
//! status that means "conflict", so that one *does* print the resolution hints.
//!
//! `-X`/`--strategy-option` values accumulate (`OPT_STRVEC`, so
//! `--no-strategy-option` clears the whole list) and are passed straight through
//! to a strategy child. On the built-in path git feeds each one to
//! `parse_merge_opt()` and throws the answer away, so an unrecognised `-X` is
//! silently ignored rather than fatal.
//!
//! Each pick is a three-way tree merge with *base* = the picked commit's
//! mainline parent (`-m`, default the first), *ours* = the current `HEAD` tree
//! and *theirs* = the picked commit's tree. A commit with no parents is replayed
//! against the empty tree, exactly as `do_pick_commit` does, and `-m` is not
//! consulted for it at all. The merge itself is `gix`'s tree merge, so renames
//! and hunk-level content merges are served rather than approximated.
//!
//! A pick that cannot be resolved stops the way git's does: the merge result —
//! conflict markers included — is checked out, the conflicting paths are given
//! stage 1/2/3 index entries, `CHERRY_PICK_HEAD`, `AUTO_MERGE` and a `MERGE_MSG`
//! carrying git's `# Conflicts:` hint are written, an `Auto-merging` and a
//! `CONFLICT (...)` line go to stdout, and the exit status is 1.
//!
//! ## The sequencer
//!
//! `sequencer_pick_revisions()` splits on the command line, not on how many
//! commits it ends up replaying: exactly one plain `<commit>` operand takes
//! `single_pick()`, which writes no `.git/sequencer` at all, and anything else
//! creates the directory and runs `pick_commits()`. Both shapes are served here
//! — ranges are still refused as *spellings*, so a sequence is always two or
//! more explicit operands — and the on-disk state is [`crate::sequencer`]'s:
//! `head`, `opts`, `abort-safety` written at the start, `todo` rewritten before
//! every instruction, the whole directory removed once the last one lands.
//!
//! That state is what the four verbs consume:
//!
//! * **`--continue`** commits the stopped pick (`continue_single_pick()`) and
//!   then replays the rest of the todo, resuming at the instruction *after* the
//!   one that stopped.
//! * **`--abort`** rewinds to `sequencer/head` with `git reset --merge`, but
//!   only when `abort-safety` still matches `HEAD`; a commit the user made by
//!   hand instead earns `warning: You seem to have moved HEAD.` and no rewind.
//! * **`--skip`** resets the stopped pick away and continues, which is why
//!   `git reset` must not delete a todo list with instructions still in it.
//! * **`--quit`** drops the state and leaves the worktree alone.
//!
//! With no sequence live each of these reduces to its single-pick form, which is
//! also where the `no cherry-pick or revert in progress` refusals come from.
//!
//! ## Empty results
//!
//! A pick whose merged tree equals the `HEAD` tree is *empty*, and git splits
//! that into two cases governed by different flags:
//!
//! - **Initially empty** (the picked commit and its parent have the same tree):
//!   committed when `--allow-empty` or `--empty=keep` is given, otherwise the
//!   pick stops. `--empty=drop` does *not* drop these.
//! - **Became empty** (the change is already upstream): committed under
//!   `--empty=keep` / `--keep-redundant-commits`, skipped with a
//!   `dropping <oid> <subject> -- patch contents already upstream` line on
//!   stderr under `--empty=drop`, and stops under the default `--empty=stop`.
//!
//! Stopping writes `CHERRY_PICK_HEAD` and `MERGE_MSG`, prints git's
//! cherry-pick-in-progress status block on stdout and its advice on stderr, and
//! exits 1 — matching stock git, whose worktree is necessarily clean at that
//! point, so the `nothing to commit, working tree clean` tail is unconditional.
//! git's `MERGE_RR` is not written: it is rerere bookkeeping, and rerere does
//! not participate here.
//!
//! ## `AUTO_MERGE`
//!
//! `merge_switch_to_result()` (merge-ort.c) records the merged tree as
//! `AUTO_MERGE` for every result it checks out — clean or conflicted, committed
//! or not — so stock leaves `.git/AUTO_MERGE` behind after a *successful*
//! `git cherry-pick <commit>` and `git rev-parse AUTO_MERGE` resolves there.
//! Only the in-process merge goes through merge-ort, which is why a
//! `--strategy=<name>` child leaves none, and why a true `--ff` pick (which
//! never merges at all) leaves none either. `allow_empty() == 2` — the
//! `--empty=drop` case — deletes it again along with `CHERRY_PICK_HEAD` and
//! `MERGE_MSG`.
//!
//! ## Supported flags
//!
//! `-x`, `-m`/`--mainline`, `--ff`/`--no-ff`, `--allow-empty`,
//! `--allow-empty-message`, `--empty=stop|drop|keep`,
//! `--keep-redundant-commits`, `-s`/`--signoff`, `--cleanup=<mode>`,
//! `--no-edit`, `--commit`, the `--no-` forms of the above, `--quit`, and the
//! no-ops `-r`, `--no-gpg-sign` (we never sign) and `--rerere-autoupdate`
//! (rerere only participates in conflicts, which are refused anyway).
//!
//! Refused with a precise message: `-e`/`--edit`, `-S`, and commit *ranges*
//! (name each commit instead; two or more operands run the same sequencer a
//! range would). The `-X` values `gix-merge` cannot express — the whitespace family,
//! `subtree` and `renormalize` — are refused by the shared
//! `merge_tree::StrategyOptions`, at the point the merge would have used them.
//!
//! Repository state for a successful pick matches git: the author signature
//! (name, email and time) is preserved from the picked commit, the committer
//! comes from configuration, and the reflog entry is `cherry-pick: <subject>`
//! (or `cherry-pick: fast-forward`). Mailmap is not applied to the printed
//! identities, so a repository that rewrites the picked author via `.mailmap`
//! would see a different ` Author:`. The detached-HEAD status header names the
//! current commit rather than the reflog-recorded detach point.
//!
//! The worktree-update helper below is a verbatim port of the one in
//! `porcelain::merge`; it cannot be shared because that module is private and
//! this port may only add a single file.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::objs::tree::EntryMode;
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// git's own `cherry-pick` usage block, verbatim, including the trailing blank
/// line. Printed on stderr for every 129 exit that is not a value error.
const USAGE: &str = "\
usage: git cherry-pick [--edit] [-n] [-m <parent-number>] [-s] [-x] [--ff]
                       [-S[<keyid>]] <commit>...
   or: git cherry-pick (--continue | --skip | --abort | --quit)

    --quit                end revert or cherry-pick sequence
    --continue            resume revert or cherry-pick sequence
    --abort               cancel revert or cherry-pick sequence
    --skip                skip current commit and continue
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    -n, --no-commit       don't automatically commit
    --commit              opposite of --no-commit
    -e, --[no-]edit       edit the commit message
    -s, --[no-]signoff    add a Signed-off-by trailer
    -m, --[no-]mainline <parent-number>
                          select mainline parent
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --[no-]strategy <strategy>
                          merge strategy
    -X, --[no-]strategy-option <option>
                          option for merge strategy
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit
    -x                    append commit name
    --[no-]ff             allow fast-forward
    --[no-]allow-empty    preserve initially empty commits
    --[no-]allow-empty-message
                          allow commits with empty messages
    --[no-]keep-redundant-commits
                          deprecated: use --empty=keep instead
    --empty (stop|drop|keep)
                          how to handle commits that become empty

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `-r`.
/// Captured byte-for-byte from stock git 2.55.0's `git cherry-pick --help-all`.
const USAGE_ALL: &str = r#"usage: git cherry-pick [--edit] [-n] [-m <parent-number>] [-s] [-x] [--ff]
                       [-S[<keyid>]] <commit>...
   or: git cherry-pick (--continue | --skip | --abort | --quit)

    --quit                end revert or cherry-pick sequence
    --continue            resume revert or cherry-pick sequence
    --abort               cancel revert or cherry-pick sequence
    --skip                skip current commit and continue
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    -n, --no-commit       don't automatically commit
    --commit              opposite of --no-commit
    -e, --[no-]edit       edit the commit message
    -r                    no-op (backward compatibility)
    -s, --[no-]signoff    add a Signed-off-by trailer
    -m, --[no-]mainline <parent-number>
                          select mainline parent
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --[no-]strategy <strategy>
                          merge strategy
    -X, --[no-]strategy-option <option>
                          option for merge strategy
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit
    -x                    append commit name
    --[no-]ff             allow fast-forward
    --[no-]allow-empty    preserve initially empty commits
    --[no-]allow-empty-message
                          allow commits with empty messages
    --[no-]keep-redundant-commits
                          deprecated: use --empty=keep instead
    --empty (stop|drop|keep)
                          how to handle commits that become empty

"#;

/// The usage block on stderr, status 129 — git's `usage_with_options`.
fn usage() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// A single `error:` line on stderr, status 129 — git's `error()` returns from
/// an option callback, and `parse_options` exits without reprinting usage.
fn opt_error(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(129)
}

/// `fatal: <message>`, status 128 — git's `die()`.
fn fatal(message: &str) -> ExitCode {
    eprintln!("fatal: {message}");
    ExitCode::from(128)
}

/// git's sequencer failure shape: the specific `error:` line, then the generic
/// `fatal: cherry-pick failed`, status 128.
fn sequencer_failed(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    eprintln!("fatal: cherry-pick failed");
    ExitCode::from(128)
}

/// What to do with a pick whose result is empty (`--empty`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Empty {
    Stop,
    Drop,
    Keep,
}

/// `--cleanup` modes. `Default` resolves to `Whitespace` here because it only
/// differs from it when an editor runs, and `--edit` is refused.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    Verbatim,
    Whitespace,
    Strip,
    Scissors,
    Default,
}

impl Cleanup {
    /// git's `describe_cleanup_mode()` (sequencer.c), the spelling `save_opts`
    /// records. `Default` resolves through `get_cleanup_mode(arg, use_editor=1)`
    /// — `builtin/revert.c` always passes `1` — so it names `strip`, not the
    /// `whitespace` this port's non-editor picks behave as.
    fn name(self) -> &'static str {
        match self {
            Cleanup::Verbatim => "verbatim",
            Cleanup::Whitespace => "whitespace",
            Cleanup::Strip | Cleanup::Default => "strip",
            Cleanup::Scissors => "scissors",
        }
    }
}

/// The four sequencer verbs, carrying the spelling git uses in diagnostics.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Quit,
    Continue,
    Abort,
    Skip,
}

impl Verb {
    fn name(self) -> &'static str {
        match self {
            Verb::Quit => "--quit",
            Verb::Continue => "--continue",
            Verb::Abort => "--abort",
            Verb::Skip => "--skip",
        }
    }
}

/// Everything the command line established, in the shape the pipeline consumes.
#[derive(Default)]
struct Opts<'a> {
    specs: Vec<&'a str>,
    /// An option git's table does not contain. Kept rather than rejected, and
    /// only fatal once every revision has resolved (stage 5).
    leftover_unknown: bool,
    verb: Option<Verb>,
    record_origin: bool,
    allow_ff: bool,
    allow_empty: bool,
    allow_empty_message: bool,
    no_commit: bool,
    edit: bool,
    /// `opts->edit` as git's tri-state, kept alongside [`Opts::edit`] because
    /// `save_opts` writes the key for an explicit `--no-edit` and omits it when
    /// the option was never given. `-e` is refused before any pick runs, so
    /// `Some(true)` never reaches `sequencer/opts`.
    edit_given: Option<bool>,
    signoff: bool,
    mainline: Option<u32>,
    empty: Option<Empty>,
    /// The raw, unvalidated `--cleanup` value (last-wins; `None` after
    /// `--no-cleanup`). git stores the string and validates only this final
    /// value once parsing is over, so a bad mode overwritten by a later good one
    /// never errors.
    cleanup: Option<&'a str>,
    /// `--strategy=<name>`, last-wins (`OPT_STRING`); `None` after
    /// `--no-strategy`. `None`, `recursive` and `ort` all mean the built-in
    /// three-way merge; anything else is run as a `git merge-<name>` child.
    strategy: Option<&'a str>,
    /// `-X`/`--strategy-option` values in order (`OPT_STRVEC`, so they
    /// accumulate and `--no-strategy-option` clears the whole list).
    xopts: Vec<&'a str>,
    gpg_sign: bool,
    /// `--keep-redundant-commits` / `--no-keep-redundant-commits` on their own
    /// (`OPT_BOOL` on `opts->keep_redundant_commits`), kept apart from
    /// [`Opts::empty`] because `verify_opt_compatible` reads the two separately:
    /// `--no-keep-redundant-commits --quit` is fine while `--empty=stop --quit`
    /// is not.
    keep_redundant_flag: bool,
    /// Whether `--empty=<action>` was given at all — git's `empty_opt !=
    /// EMPTY_COMMIT_UNSPECIFIED`. Not implied by `--keep-redundant-commits`,
    /// which leaves `empty_opt` alone.
    empty_given: bool,
    /// `opts->allow_rerere_auto`: `Some(true)` for `--rerere-autoupdate`,
    /// `Some(false)` for `--no-rerere-autoupdate`, `None` for neither. Both
    /// spellings are no-ops for the pick itself and differ only here.
    rerere_auto: Option<bool>,
}

impl Opts<'_> {
    fn empty_action(&self) -> Empty {
        self.empty.unwrap_or(Empty::Stop)
    }

    /// `opts->keep_redundant_commits` as `run_sequencer` leaves it: the flag,
    /// or `--empty=keep`, which `builtin/revert.c` folds into it for a pick.
    fn keep_redundant_commits(&self) -> bool {
        self.keep_redundant_flag || self.empty == Some(Empty::Keep)
    }
}

/// Parse `--cleanup`'s value the way git's `get_cleanup_mode` does: an unknown
/// mode is fatal (128) on the spot, not a usage error.
fn parse_cleanup(value: &str) -> Result<Cleanup, ExitCode> {
    match value {
        "verbatim" => Ok(Cleanup::Verbatim),
        "whitespace" => Ok(Cleanup::Whitespace),
        "strip" => Ok(Cleanup::Strip),
        "scissors" => Ok(Cleanup::Scissors),
        "default" => Ok(Cleanup::Default),
        other => Err(fatal(&format!("Invalid cleanup mode {other}"))),
    }
}

fn parse_empty(value: &str) -> Result<Empty, ExitCode> {
    match value {
        "stop" => Ok(Empty::Stop),
        "drop" => Ok(Empty::Drop),
        "keep" => Ok(Empty::Keep),
        other => Err(opt_error(&format!("invalid value for '--empty': '{other}'"))),
    }
}

/// `-m`'s value must be a positive integer; git rejects everything else with one
/// message regardless of whether it was unparsable or zero.
///
/// git reads it with `strtol()`, which *skips leading whitespace* and then
/// requires the remainder of the string to be consumed. That is load-bearing:
/// `-m 1` arriving as a single argument gives the callback `" 1"`, which stock
/// git accepts as 1. A plain `str::parse` rejects it.
fn parse_mainline(value: &str) -> Result<u32, ExitCode> {
    let trimmed = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = trimmed.strip_prefix('+').unwrap_or(trimmed);
    match digits.parse::<u32>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(opt_error("option `mainline' expects a number greater than zero")),
    }
}

/// git's `OPT_CMDMODE` conflict check: the four sequencer verbs share one
/// `cmd` slot, so a *second, different* verb is rejected the moment
/// `parse_options` reaches it — before the post-parse `--cleanup` validation and
/// before anything else in the pipeline. Repeating the same verb is fine.
///
/// The message names the option being parsed first and the one already stored
/// second, which is why `--abort --skip` and `--skip --abort` print the pair in
/// opposite orders. `error()` + `return -1` prints no usage block here; the exit
/// status is 129.
fn set_verb(current: &mut Option<Verb>, new: Verb) -> Result<(), ExitCode> {
    match *current {
        Some(prev) if prev != new => Err(opt_error(&verb_conflict(new, prev))),
        _ => {
            *current = Some(new);
            Ok(())
        }
    }
}

/// The `error:` line [`set_verb`] prints, split out so its wording and argument
/// order can be pinned by a test.
fn verb_conflict(new: Verb, prev: Verb) -> String {
    format!(
        "options '{}' and '{}' cannot be used together",
        new.name(),
        prev.name()
    )
}

/// An option's value: either attached (`--opt=v`, `-Xv`) or the next argument.
/// Advances `i` when it consumes that next argument, like `parse_options` does.
fn take_value<'a>(
    attached: Option<&'a str>,
    args: &'a [String],
    i: &mut usize,
    name: &str,
) -> Result<&'a str, ExitCode> {
    match attached {
        Some(v) => Ok(v),
        None => {
            *i += 1;
            match args.get(*i) {
                Some(v) => Ok(v.as_str()),
                None => Err(opt_error(&format!("option `{name}' requires a value"))),
            }
        }
    }
}

/// Walk the command line once, left to right, exactly like `parse_options`.
///
/// Returns `Err(code)` for the errors that abort parsing immediately; unknown
/// options are recorded in `leftover_unknown` instead, so that a later bad
/// revision can outrank them.
fn parse<'a>(args: &'a [String]) -> Result<Opts<'a>, ExitCode> {
    let mut o = Opts::default();
    let mut only_specs = false;
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].as_str();
        if only_specs {
            o.specs.push(arg);
            i += 1;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): tested on the token as typed, ahead of the
        // `=<value>` split below, because it is a `strcmp` — `--help-all=x` is
        // an unknown option, not a help request. It renders `USAGE_FULL`, which
        // for the `cherry-pick`/`revert` table keeps the hidden `-r`.
        if arg == "--help-all" {
            return Err(super::show_usage(USAGE_ALL));
        }

        // Split `--name=value` once so both spellings share a match arm.
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (arg, None),
        };

        match name {
            "--" => only_specs = true,

            "--quit" => set_verb(&mut o.verb, Verb::Quit)?,
            "--continue" => set_verb(&mut o.verb, Verb::Continue)?,
            "--abort" => set_verb(&mut o.verb, Verb::Abort)?,
            "--skip" => set_verb(&mut o.verb, Verb::Skip)?,

            // Stored raw; the final value is validated post-parse, not here.
            "--cleanup" => {
                o.cleanup = Some(take_value(inline, args, &mut i, "cleanup")?);
            }
            "--no-cleanup" => o.cleanup = None,
            "--empty" => {
                o.empty = Some(parse_empty(take_value(inline, args, &mut i, "empty")?)?);
                o.empty_given = true;
            }
            "--mainline" => {
                o.mainline = Some(parse_mainline(take_value(inline, args, &mut i, "mainline")?)?);
            }
            "--no-mainline" => o.mainline = None,
            "--strategy" => {
                o.strategy = Some(take_value(inline, args, &mut i, "strategy")?);
            }
            "--no-strategy" => o.strategy = None,
            "--strategy-option" => {
                let v = take_value(inline, args, &mut i, "strategy-option")?;
                o.xopts.push(v);
            }
            // `OPT_STRVEC`'s unset arm is `strvec_clear()`, so the negation drops
            // every `-X` seen so far rather than merely the last one.
            "--no-strategy-option" => o.xopts.clear(),
            "--gpg-sign" => o.gpg_sign = true,
            "--no-gpg-sign" => o.gpg_sign = false,

            "--commit" => o.no_commit = false,
            "--no-commit" => o.no_commit = true,
            "--edit" => {
                o.edit = true;
                o.edit_given = Some(true);
            }
            "--no-edit" => {
                o.edit = false;
                o.edit_given = Some(false);
            }
            "--signoff" => o.signoff = true,
            "--no-signoff" => o.signoff = false,
            "--ff" => o.allow_ff = true,
            "--no-ff" => o.allow_ff = false,
            "--allow-empty" => o.allow_empty = true,
            "--no-allow-empty" => o.allow_empty = false,
            "--allow-empty-message" => o.allow_empty_message = true,
            "--no-allow-empty-message" => o.allow_empty_message = false,
            "--keep-redundant-commits" => {
                o.empty = Some(Empty::Keep);
                o.keep_redundant_flag = true;
            }
            "--no-keep-redundant-commits" => {
                o.empty = Some(Empty::Stop);
                o.keep_redundant_flag = false;
            }
            // rerere only ever participates in conflict resolution, and every
            // conflicted pick is refused below, so both spellings are no-ops for
            // the pick — but `verify_opt_compatible` still reads which was given.
            "--rerere-autoupdate" => o.rerere_auto = Some(true),
            "--no-rerere-autoupdate" => o.rerere_auto = Some(false),

            _ if name.starts_with("--") => o.leftover_unknown = true,

            // Short options, including clusters like `-xn` and attached values
            // like `-Xtheirs` / `-m1` / `-S<keyid>`. A non-ASCII argument cannot
            // be a cluster of git's short options, so it goes straight to the
            // unknown pile rather than being sliced at a non-char boundary.
            _ if name.len() > 1 && name.starts_with('-') && name.is_ascii() => {
                let bytes = name.as_bytes();
                let mut c = 1;
                while c < bytes.len() {
                    let rest = &name[c + 1..];
                    let attached = (!rest.is_empty()).then_some(rest);
                    match bytes[c] {
                        b'x' => o.record_origin = true,
                        b'n' => o.no_commit = true,
                        b'e' => {
                            o.edit = true;
                            o.edit_given = Some(true);
                        }
                        b's' => o.signoff = true,
                        b'm' => {
                            let v = take_value(attached, args, &mut i, "mainline")?;
                            o.mainline = Some(parse_mainline(v)?);
                            break;
                        }
                        b'X' => {
                            let v = take_value(attached, args, &mut i, "strategy-option")?;
                            o.xopts.push(v);
                            break;
                        }
                        // `-S` takes an *optional* key id, so a bare `-S` never
                        // reaches into the next argument.
                        b'S' => {
                            o.gpg_sign = true;
                            break;
                        }
                        // parse_options_step() tests `internal_help` inside the
                        // short-option loop and answers at once, on stdout —
                        // never through `usage()`, whose stderr belongs to
                        // rejections. `revert` shares this table.
                        b'h' => return Err(super::show_usage(USAGE)),
                        _ => {
                            o.leftover_unknown = true;
                            break;
                        }
                    }
                    c += 1;
                }
            }

            // Any other dashed argument: unknown, so kept for stage 5.
            _ if name.len() > 1 && name.starts_with('-') => o.leftover_unknown = true,

            _ => o.specs.push(arg),
        }
        i += 1;
    }

    Ok(o)
}

pub fn cherry_pick(args: &[String]) -> Result<ExitCode> {
    let opts = match parse(args) {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };

    // --- stage 1 (tail): validate the final `--cleanup` value -------------
    // git's `parse_args` stores `--cleanup` raw and runs `get_cleanup_mode` on
    // the final value right after option parsing — before the sequencer verbs,
    // before revision resolution, before the no-revision usage error. A bad mode
    // dies `fatal: Invalid cleanup mode <v>` (128) here and nowhere later.
    let cleanup: Option<Cleanup> = match opts.cleanup {
        Some(raw) => match parse_cleanup(raw) {
            Ok(mode) => Some(mode),
            Err(code) => return Ok(code),
        },
        None => None,
    };

    // --- stage 2: sequencer verbs ----------------------------------------
    if let Some(verb) = opts.verb {
        return handle_verb(verb, &opts);
    }

    // --- stage 3: nothing to pick ----------------------------------------
    if opts.specs.is_empty() {
        return Ok(usage());
    }

    let repo = gix::discover(".")?;
    // The whole sequence (tree build, commit, HEAD move, worktree update) is one
    // logical write; hold the coordinator lock across all of it, like `merge`.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // --- stage 4: resolve every revision, in order ------------------------
    //
    // git's `setup_revisions` walks the whole argument list and dies on the
    // *first* spec that fails to resolve, regardless of what comes after it. So a
    // bad revision outranks the unsupported-range bail below: `main..feature
    // does-not-exist` must report `bad revision 'does-not-exist'` (128), not the
    // range. Validate every spec first; only after the list is clean does the
    // range become the failure.
    let mut picks: Vec<ObjectId> = Vec::with_capacity(opts.specs.len());
    let mut range_spec: Option<&str> = None;
    for spec in &opts.specs {
        let parsed = match repo.rev_parse(*spec) {
            Ok(parsed) => parsed,
            Err(_) => return Ok(fatal(&format!("bad revision '{spec}'"))),
        };
        match parsed.single() {
            // A revision may name an annotated tag (`v0.2.0`); git peels every
            // commit-ish it is handed before replaying it.
            Some(id) => picks.push(id.object()?.peel_to_commit()?.id),
            // Both endpoints resolved, so this is a genuine range. Enumerating it
            // is unported, and silently picking one end would be wrong — but the
            // bail waits until every remaining spec has been validated, so a
            // later bad revision still wins.
            None => range_spec = range_spec.or(Some(*spec)),
        }
    }
    if let Some(spec) = range_spec {
        anyhow::bail!(
            "commit ranges are not supported; name each commit individually (`{spec}`)"
        );
    }
    // `add_pending_object()` sets `SEEN` on each object it queues, so the walk
    // hands `walk_revs_populate_todo` a given commit exactly once however many
    // times it was named: `git cherry-pick A A` replays `A` once and its todo
    // list has one line. The *operand* count still decides single-pick versus
    // sequence, though (`opts->revs->cmdline.nr`), so that is read separately.
    {
        let mut seen = HashSet::new();
        picks.retain(|id| seen.insert(*id));
    }

    // --- stage 5: options git kept but never consumed ----------------------
    if opts.leftover_unknown {
        return Ok(usage());
    }

    if opts.edit {
        anyhow::bail!("`-e`/`--edit` (editor mode) is not supported");
    }
    if opts.gpg_sign {
        anyhow::bail!("commit signing is not supported");
    }

    let head = repo.head()?;
    if head.is_unborn() {
        crate::git_fatal!("cannot cherry-pick onto an unborn branch");
    }
    let head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    drop(head);

    // --- stage 6: start the sequence ---------------------------------------
    //
    // `sequencer_pick_revisions` splits on the command line rather than on how
    // many commits it ends up replaying: exactly one plain `<commit>` operand
    // takes `single_pick`, which never touches `.git/sequencer` (git's own
    // comment: that is what makes it "possible to cherry-pick in the middle of a
    // cherry-pick sequence"). Anything else creates the directory and runs
    // `pick_commits`. Ranges bail above, so the operand count is the whole test.
    //
    // This runs *before* the dirty-worktree refusal below because git's does:
    // `create_seq_dir()`/`save_head()`/`save_opts()` are all in
    // `sequencer_pick_revisions`, and the refusal comes out of the first
    // `do_pick_commit` inside `pick_commits`. So a multi-commit pick that is
    // turned away for a dirty worktree still leaves the sequencer state behind,
    // and a *second* one is then refused as "already in progress".
    let sequence = opts.specs.len() > 1;
    let todo = build_todo(&repo, &picks)?;

    if sequence {
        let git_dir = repo.git_dir();
        // `create_seq_dir` refuses over a live sequence; `cmd_cherry_pick` turns
        // that `-1` into `die(_("cherry-pick failed"))`.
        if crate::sequencer::create(git_dir)?.is_err() {
            return Ok(sequencer_failed_tail());
        }
        crate::sequencer::save_head(git_dir, head_id)?;
        crate::sequencer::save_opts(git_dir, &saved_opts(&opts, cleanup))?;
        crate::sequencer::update_abort_safety_file(&repo)?;
    }

    // --- stage 7: git's `require_clean_work_tree` --------------------------
    if repo.is_dirty()? {
        eprintln!("error: your local changes would be overwritten by cherry-pick.");
        // `error_dirty_index` (sequencer.c) gates its one-line direction on
        // `advice.commitBeforeMerge`; the `error:` line above always prints.
        crate::advice::Advice::CommitBeforeMerge
            .advise_plain("commit your changes or stash them to proceed.");
        return Ok(sequencer_failed_tail());
    }

    // Committer identity, shared by every pick in the sequence.
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let committer_ident = format!("{} <{}>", committer.name, committer.email);

    let env = PickEnv {
        repo: &repo,
        opts: &opts,
        cleanup,
        committer,
        committer_ident: &committer_ident,
        should_interrupt: AtomicBool::new(false),
    };
    let mut state = PickState {
        // Index mirroring the current (clean) worktree; carried forward across
        // picks so filesystem stats are reused and a later `status` stays cheap.
        index: repo.index_or_load_from_head()?.into_owned(),
        // `-n`/`--no-commit` applies picks to the index and worktree without
        // committing; `pending_tree` accumulates the applied tree so a run of
        // picks squashes onto each other while HEAD stays put.
        pending_tree: None,
        head_id,
    };
    run_todo(&env, &mut state, &todo, 0, sequence)
}

/// `walk_revs_populate_todo`: one `pick <abbrev> <subject>` instruction per
/// commit, in the order they will be replayed.
///
/// The abbreviation is `short_commit_name()`, taken once here — the todo file
/// records what the abbreviation was when the sequence started, while the
/// diagnostics a stopped pick prints re-abbreviate against the repository as it
/// stands then.
fn build_todo(
    repo: &gix::Repository,
    picks: &[ObjectId],
) -> Result<Vec<crate::sequencer::TodoItem>> {
    picks
        .iter()
        .map(|id| {
            let commit = repo.find_commit(*id)?;
            let subject = gix::objs::commit::MessageRef::from_bytes(commit.message_raw()?)
                .summary()
                .to_str_lossy()
                .into_owned();
            Ok(crate::sequencer::TodoItem {
                action: crate::sequencer::Action::Pick,
                oid: *id,
                abbrev: id.attach(repo).shorten_or_id().to_string(),
                subject,
            })
        })
        .collect()
}

/// `save_opts`'s view of the command line: which options a `--continue` would
/// have to restore.
///
/// `--empty=drop` is git's `drop_redundant_commits` and `--empty=keep` folds
/// into `keep_redundant_commits`, which in turn "implies allow_empty"
/// (`builtin/revert.c`) — so `--empty=keep` alone writes both keys, exactly as
/// stock does.
fn saved_opts<'a>(opts: &'a Opts<'a>, cleanup: Option<Cleanup>) -> crate::sequencer::SavedOpts<'a> {
    // `--keep-redundant-commits` and `--empty=keep` both land on `Empty::Keep`
    // here, and git makes them equivalent too: `keep_redundant_commits |=
    // (empty_opt == KEEP_EMPTY_COMMIT)`.
    let keep_redundant = opts.empty == Some(Empty::Keep);
    crate::sequencer::SavedOpts {
        no_commit: opts.no_commit,
        edit: opts.edit_given,
        allow_empty: opts.allow_empty || keep_redundant,
        allow_empty_message: opts.allow_empty_message,
        drop_redundant_commits: opts.empty == Some(Empty::Drop),
        keep_redundant_commits: keep_redundant,
        signoff: opts.signoff,
        record_origin: opts.record_origin,
        allow_ff: opts.allow_ff,
        mainline: opts.mainline.unwrap_or(0),
        strategy: opts.strategy,
        gpg_sign: None,
        xopts: &opts.xopts,
        allow_rerere_auto: None,
        default_msg_cleanup: cleanup.map(Cleanup::name),
    }
}

/// Everything a pick reads that does not change between picks in a sequence.
struct PickEnv<'a> {
    repo: &'a gix::Repository,
    opts: &'a Opts<'a>,
    cleanup: Option<Cleanup>,
    committer: gix::actor::SignatureRef<'a>,
    committer_ident: &'a str,
    should_interrupt: AtomicBool,
}

/// What one pick hands to the next.
struct PickState {
    index: gix::index::File,
    pending_tree: Option<ObjectId>,
    head_id: ObjectId,
}

/// Whether a pick landed or stopped the sequence.
enum PickOutcome {
    Done,
    /// git printed the refusal itself; this is the status to exit with.
    Stopped(ExitCode),
}

/// `pick_commits`: replay `todo[start..]`.
///
/// When a sequence is live the todo list is rewritten at the *top* of every
/// iteration — keeping the instruction about to run, so a stop leaves the file
/// beginning with the pick that stopped — and the abort-safety marker is
/// refreshed at the bottom of each one, which is `do_pick_commit`'s `leave:`
/// label and therefore happens whether the pick landed or not.
fn run_todo(
    env: &PickEnv<'_>,
    state: &mut PickState,
    todo: &[crate::sequencer::TodoItem],
    start: usize,
    sequence: bool,
) -> Result<ExitCode> {
    let git_dir = env.repo.git_dir();
    for (i, item) in todo.iter().enumerate().skip(start) {
        if sequence {
            crate::sequencer::save_todo(git_dir, todo, i)?;
        }
        let outcome = pick_one(env, state, item);
        if sequence {
            crate::sequencer::update_abort_safety_file(env.repo)?;
        }
        match outcome? {
            PickOutcome::Done => {}
            PickOutcome::Stopped(code) => return Ok(code),
        }
    }
    // "Sequence of picks finished successfully; cleanup by removing the
    // .git/sequencer directory."
    if sequence {
        crate::sequencer::remove_state(git_dir);
    }
    Ok(ExitCode::SUCCESS)
}

/// `do_pick_commit` for one instruction.
fn pick_one(
    env: &PickEnv<'_>,
    state: &mut PickState,
    item: &crate::sequencer::TodoItem,
) -> Result<PickOutcome> {
    let PickEnv {
        repo,
        opts,
        cleanup,
        committer,
        committer_ident,
        should_interrupt,
    } = env;
    let (repo, opts, cleanup) = (*repo, *opts, *cleanup);
    let committer = *committer;
    let committer_ident: &str = committer_ident;
    let hash = repo.object_hash();
    let pick_id = item.oid;
    let head_id = state.head_id;
    let pick = repo.find_commit(pick_id)?;

    // --- pick the base (mainline) parent ------------------------------
    let parents: Vec<ObjectId> = pick.parent_ids().map(|id| id.detach()).collect();
    let base_id: Option<ObjectId> = if parents.is_empty() {
        // git's `do_pick_commit` tests `!commit->parents` *first*: a root
        // commit is replayed against the empty tree and `-m` is never
        // consulted, so `-m 1 <root>` is accepted rather than rejected.
        None
    } else {
        match opts.mainline {
            // `-m N` is legal on a non-merge as long as parent N exists, so a
            // plain commit accepts `-m 1` and behaves exactly as without it.
            Some(n) => match parents.get(n as usize - 1) {
                Some(id) => Some(*id),
                None => {
                    return Ok(PickOutcome::Stopped(sequencer_failed(&format!(
                        "commit {pick_id} does not have parent {n}"
                    ))));
                }
            },
            None => match parents.len() {
                1 => Some(parents[0]),
                _ => {
                    return Ok(PickOutcome::Stopped(sequencer_failed(&format!(
                        "commit {pick_id} is a merge but no -m option was given."
                    ))));
                }
            },
        }
    };

    let pick_tree = pick.tree_id()?.detach();
    let base_tree = match base_id {
        Some(id) => repo.find_commit(id)?.tree_id()?.detach(),
        None => ObjectId::empty_tree(hash),
    };
    let head_tree = match state.pending_tree {
        Some(t) => t,
        None => repo.find_commit(head_id)?.tree_id()?.detach(),
    };

    // `--ff`: HEAD is exactly the picked commit's parent, so replaying the
    // change is a fast-forward. git prints nothing in this case.
    if opts.allow_ff && base_id == Some(head_id) {
        advance_head(&repo, head_id, pick_id, "cherry-pick: fast-forward".into())?;
        state.index = update_clean_worktree(&repo, &state.index, pick_tree, &should_interrupt)?;
        state.head_id = pick_id;
        return Ok(PickOutcome::Done);
    }

    // --- three-way tree merge -----------------------------------------
    //
    // Labels come from `get_message()` in git's `sequencer.c`: *ours* is the
    // literal `HEAD`, *theirs* is `<abbrev> (<subject>)`, and the ancestor is
    // that same string prefixed with `parent of `. They are what ends up in
    // the `<<<<<<<` / `>>>>>>>` marker lines, so they are computed from the
    // picked commit's *own* message, before `-x`/`--signoff` rewrite it.
    let pick_subject = gix::objs::commit::MessageRef::from_bytes(pick.message_raw()?)
        .summary()
        .to_str_lossy()
        .into_owned();
    let pick_short = pick_id.attach(&repo).shorten_or_id().to_string();
    let other_label = format!("{pick_short} ({pick_subject})");
    let ancestor_label = format!("parent of {other_label}");

    // --- message -----------------------------------------------------
    //
    // Built before the merge, as `do_pick_commit` does: the strategy branch
    // below writes it to `MERGE_MSG` *before* it runs the merge program.
    let mut message: BString = pick.message_raw()?.to_owned();
    // Only an explicit `--cleanup` rewrites the picked message; without one
    // git carries it across untouched.
    if let Some(mode) = cleanup {
        message = cleanup_message(&message, mode);
    }
    if message.trim().is_empty() && !opts.allow_empty_message {
        crate::git_fatal!(
            "the commit message of {pick_short} is empty (use --allow-empty-message)"
        );
    }
    if message.last() != Some(&b'\n') {
        message.push(b'\n');
    }
    if opts.record_origin {
        if !has_conforming_footer(&message) {
            message.push(b'\n');
        }
        message.extend_from_slice(b"(cherry picked from commit ");
        message.extend_from_slice(pick_id.to_string().as_bytes());
        message.extend_from_slice(b")\n");
    }
    if opts.signoff {
        let trailer = format!("Signed-off-by: {committer_ident}\n");
        if !message.ends_with(trailer.as_bytes()) {
            if !has_conforming_footer(&message) {
                message.push(b'\n');
            }
            message.extend_from_slice(trailer.as_bytes());
        }
    }
    let subject = gix::objs::commit::MessageRef::from_bytes(message.as_bstr())
        .summary()
        .to_str_lossy()
        .into_owned();

    // --- the merge, chosen by strategy --------------------------------
    //
    // `do_pick_commit` splits on the strategy name and nothing else: no
    // strategy, `recursive` and `ort` all go to the in-process three-way
    // merge, every other name is run as a `git merge-<name>` child by
    // `try_merge_command`. Nothing validates the name first, which is why
    // `--strategy=nonexistent-strategy` reaches the child and fails there
    // with `git: 'merge-nonexistent-strategy' is not a git command`.
    let unresolved = gix::merge::tree::TreatAsUnresolved::git();
    let mut conflicted: Vec<BString> = Vec::new();
    let (tree_id, mut merge) = match opts.strategy {
        None | Some("recursive") | Some("ort") => {
            // git hands every `-X` to `parse_merge_opt()` here and *discards*
            // the result, so an unknown one is silently ignored rather than
            // fatal. `absorb`'s answer is dropped for the same reason.
            let mut strategy = super::merge_tree::StrategyOptions::default();
            for x in &opts.xopts {
                strategy.absorb(x);
            }
            let mut merge = repo.merge_trees(
                base_tree,
                head_tree,
                pick_tree,
                gix::merge::blob::builtin_driver::text::Labels {
                    ancestor: Some(BStr::new(ancestor_label.as_bytes())),
                    current: Some(BStr::new("HEAD")),
                    other: Some(BStr::new(other_label.as_bytes())),
                },
                strategy.apply(repo.tree_merge_options()?)?,
            )?;
            let tree_id = merge.tree.write()?.detach();

            // `merge_switch_to_result()` (merge-ort.c) records the merged tree
            // as `AUTO_MERGE` for every result it checks out. Only this
            // in-process arm goes through merge-ort: a `merge-<strategy>` child
            // never writes the file, which is what makes `--strategy=resolve`
            // leave none.
            crate::merge_apply::write_auto_merge(repo, tree_id)?;

            // git's merge-ort emits messages grouped per path: an
            // `Auto-merging` line for every attempted blob merge, then a
            // `CONFLICT (...)` line for the ones it could not resolve.
            // Identical changes on both sides resolve trivially and are
            // reported by neither, which is why picking a commit that is
            // already applied stays silent.
            for conflict in &merge.conflicts {
                let path = conflict.changes_in_resolution().0.location().to_owned();
                if conflict.content_merge().is_some() {
                    println!("Auto-merging {path}");
                }
                if !conflict.is_unresolved(unresolved) {
                    continue;
                }
                // merge-ort's `filemask == 6`: no ancestor stage means both
                // sides added the path, which it reports as `add/add` rather
                // than `content`.
                let kind = if conflict.entries()[0].is_none() {
                    "add/add"
                } else {
                    "content"
                };
                println!("CONFLICT ({kind}): Merge conflict in {path}");
                conflicted.push(path);
            }
            // `res |= write_message(ctx->message…, git_path_merge_msg(r), 0);`
            // — sequencer.c:2450, immediately after `do_recursive_merge()` and
            // before anything decides whether a commit follows. A pick that
            // commits has it consumed and unlinked again below; `--no-commit`
            // stops first, which is why stock leaves `.git/MERGE_MSG` holding
            // the picked message (and, under `-x`, its
            // `(cherry picked from commit …)` line) for the `git commit` the
            // user is expected to run next. The conflict arm rewrites it with
            // git's `# Conflicts:` block appended.
            std::fs::write(repo.git_dir().join("MERGE_MSG"), &message[..])?;
            (tree_id, Some(merge))
        }
        Some(strategy) => {
            match run_merge_strategy(
                &repo,
                strategy,
                &opts.xopts,
                base_id,
                head_id,
                pick_id,
                &message,
            )? {
                0 => {}
                res => {
                    // `if (res)` in `do_pick_commit`: the picked commit's own
                    // subject, then the resolution hints only when the child
                    // reported a conflict (status 1) rather than a refusal.
                    eprintln!("error: could not apply {pick_short}... {pick_subject}");
                    if res == 1 {
                        // Written just above the `if (res)` block, for
                        // `res == 0 || res == 1` and only when a commit is
                        // still intended. A successful pick would have it
                        // removed again by the commit, so it is only
                        // persisted on the stop.
                        if !opts.no_commit {
                            std::fs::write(
                                repo.git_dir().join("CHERRY_PICK_HEAD"),
                                format!("{pick_id}\n"),
                            )?;
                        }
                        merge_conflict_advice(&repo, opts.no_commit);
                    }
                    return Ok(PickOutcome::Stopped(ExitCode::from(res)));
                }
            }
            // The child wrote the result into the index and the worktree;
            // git re-reads the index and commits its tree from there.
            state.index = repo.open_index()?;
            (tree_from_index(&repo, &state.index)?, None)
        }
    };

    // The merged tree is the state of every path after the merge, conflict
    // markers included; the diffstat and the empty-result test both read it
    // back rather than tracking resolutions as they are made.
    let mut resolved: Vec<(BString, EntryMode, ObjectId)> = flatten(&repo, tree_id)?
        .into_iter()
        .map(|(path, (mode, id))| (path, mode, id))
        .collect();
    resolved.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    // --- stopped on conflict ------------------------------------------
    //
    // Checked before the empty-result guards, as git does: a conflicted pick
    // never reaches the point where emptiness would be decided.
    if let Some(merge) = merge.as_mut().filter(|_| !conflicted.is_empty()) {
        let mut new_index =
            update_clean_worktree(&repo, &state.index, tree_id, &should_interrupt)?;
        merge.index_changed_after_applying_conflicts(
            &mut new_index,
            unresolved,
            gix::merge::tree::apply_index_entries::RemovalMode::Prune,
        );
        new_index.write(Default::default())?;

        let git_dir = repo.git_dir();
        // `AUTO_MERGE` — the merge result with its conflict markers — was
        // already recorded by `merge_switch_to_result()` above.
        std::fs::write(git_dir.join("CHERRY_PICK_HEAD"), format!("{pick_id}\n"))?;

        // git's `append_conflicts_hint`: a blank line, then one commented
        // line per conflicted path, appended to the message it would have
        // committed.
        let mut merge_msg = message.clone();
        merge_msg.push(b'\n');
        merge_msg.extend_from_slice(b"# Conflicts:\n");
        for path in &conflicted {
            merge_msg.extend_from_slice(b"#\t");
            merge_msg.extend_from_slice(&path[..]);
            merge_msg.push(b'\n');
        }
        std::fs::write(git_dir.join("MERGE_MSG"), &merge_msg[..])?;

        eprintln!("error: could not apply {pick_short}... {pick_subject}");
        eprintln!("hint: After resolving the conflicts, mark them with");
        eprintln!("hint: \"git add/rm <pathspec>\", then run");
        eprintln!("hint: \"git cherry-pick --continue\".");
        eprintln!("hint: You can instead skip this commit with \"git cherry-pick --skip\".");
        eprintln!("hint: To abort and get back to the state before \"git cherry-pick\",");
        eprintln!("hint: run \"git cherry-pick --abort\".");
        return Ok(PickOutcome::Stopped(ExitCode::from(1)));
    }

    // --- `--no-commit`: stage the applied tree, do not commit ---------
    // git leaves the result in the index and worktree and never moves HEAD;
    // `pending_tree` carries it forward so a following pick squashes onto it.
    if opts.no_commit {
        state.index = update_clean_worktree(&repo, &state.index, tree_id, &should_interrupt)?;
        state.pending_tree = Some(tree_id);
        return Ok(PickOutcome::Done);
    }

    // --- empty-result guards -----------------------------------------
    if tree_id == head_tree {
        let initially_empty = pick_tree == base_tree;
        let action = opts.empty_action();
        let keep = action == Empty::Keep || (initially_empty && opts.allow_empty);
        if !keep {
            // `--empty` governs commits that *became* empty; an initially
            // empty one is only ever kept via `--allow-empty`/`--empty=keep`.
            if !initially_empty && action == Empty::Drop {
                // `allow_empty() == 2` deletes `CHERRY_PICK_HEAD` and unlinks
                // both `MERGE_MSG` and `AUTO_MERGE` before it reports the
                // drop, so a strategy child's `MERGE_MSG` does not survive.
                let git_dir = repo.git_dir();
                let _ = std::fs::remove_file(git_dir.join("CHERRY_PICK_HEAD"));
                let _ = std::fs::remove_file(git_dir.join("MERGE_MSG"));
                let _ = std::fs::remove_file(git_dir.join("AUTO_MERGE"));
                eprintln!(
                    "dropping {pick_id} {subject} -- patch contents already upstream"
                );
                return Ok(PickOutcome::Done);
            }
            return Ok(PickOutcome::Stopped(stop_empty(
                repo, pick_id, head_id, &message,
            )?));
        }
    }

    // --- write the commit, preserving the original author -------------
    let author = pick.author()?;
    let author_ident = format!("{} <{}>", author.name, author.email);
    let author_time = author.time()?;

    let commit = gix::objs::Commit {
        message: message.clone(),
        tree: tree_id,
        author: author.to_owned()?,
        committer: committer.to_owned()?,
        encoding: None,
        parents: std::iter::once(head_id).collect(),
        extra_headers: Default::default(),
    };
    let new_id = repo.write_object(&commit)?.detach();

    let reflog = gix::reference::log::message("cherry-pick", message.as_bstr(), 1);
    advance_head(&repo, head_id, new_id, reflog)?;
    state.index = update_clean_worktree(&repo, &state.index, tree_id, &should_interrupt)?;
    // The commit consumes `MERGE_MSG` — `do_commit()` passes it as the message
    // file and `remove_branch_state()` unlinks it — so a landed pick leaves
    // neither it nor `CHERRY_PICK_HEAD` behind, whichever arm wrote them.
    // The sequencer directory is untouched: it belongs to the whole sequence
    // rather than to one pick.
    {
        let git_dir = repo.git_dir();
        let _ = std::fs::remove_file(git_dir.join("MERGE_MSG"));
        if merge.is_none() {
            let _ = std::fs::remove_file(git_dir.join("CHERRY_PICK_HEAD"));
        }
    }

    // --- summary, matching git's `print_commit_summary` ----------------
    let branch_label = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "detached HEAD".to_string(),
    };
    println!("[{branch_label} {}] {subject}", new_id.attach(&repo).shorten_or_id());
    if author_ident != committer_ident {
        println!(" Author: {author_ident}");
    }
    {
        use gix::date::time::format;
        println!(" Date: {}", author_time.format_or_unix(format::DEFAULT));
    }
    print_diffstat(&repo, head_tree, tree_id, &resolved)?;

    state.head_id = new_id;
    Ok(PickOutcome::Done)
}

/// git's `try_merge_command` (`merge.c`): run the named merge strategy as a
/// child and return its exit status, which becomes the pick's.
///
/// The argument vector is built exactly as git builds it —
/// `merge-<strategy> [--<xopt>...] <merge base> -- <HEAD> <commit being replayed>`
/// — and every operand is a full object id, with the empty tree standing in for
/// the absent base of a root commit (`merge_argument`). `cmd.git_cmd = 1` means
/// the child is looked up as a git subcommand rather than on `PATH`, so this
/// re-executes *this* binary, the same way the ported `submodule` verbs do; the
/// strategies git ships as scripts (`merge-resolve`, `merge-octopus`) are ported
/// subcommands here.
///
/// `MERGE_MSG` is written first because the `else` arm of `do_pick_commit` does
/// exactly that, before the child runs: a strategy that stops mid-way leaves the
/// message behind for the eventual `--continue`.
fn run_merge_strategy(
    repo: &gix::Repository,
    strategy: &str,
    xopts: &[&str],
    base_id: Option<ObjectId>,
    head_id: ObjectId,
    pick_id: ObjectId,
    message: &BString,
) -> Result<u8> {
    std::fs::write(repo.git_dir().join("MERGE_MSG"), &message[..])?;

    let base = base_id.unwrap_or_else(|| ObjectId::empty_tree(repo.object_hash()));
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg(format!("merge-{strategy}"));
    for x in xopts {
        cmd.arg(format!("--{x}"));
    }
    cmd.arg(base.to_string())
        .arg("--")
        .arg(head_id.to_string())
        .arg(pick_id.to_string());

    let status = cmd.status()?;
    // `run_command` reports a signalled child as `128 + signal`; a plain exit
    // status passes through.
    Ok(match status.code() {
        Some(code) => code.clamp(0, 255) as u8,
        None => 128,
    })
}

/// `print_advice(r, show_hint = 1, opts)` for a stopped cherry-pick: the
/// `advice.mergeConflict` slot, so the `Disable this message with …` trailer is
/// appended when the user has never configured it.
fn merge_conflict_advice(repo: &gix::Repository, no_commit: bool) {
    let body = if no_commit {
        "after resolving the conflicts, mark the corrected paths\n\
         with 'git add <paths>' or 'git rm <paths>'"
    } else {
        "After resolving the conflicts, mark them with\n\
         \"git add/rm <pathspec>\", then run\n\
         \"git cherry-pick --continue\".\n\
         You can instead skip this commit with \"git cherry-pick --skip\".\n\
         To abort and get back to the state before \"git cherry-pick\",\n\
         run \"git cherry-pick --abort\"."
    };
    crate::advice::Advice::MergeConflict.advise_in(repo, body);
}

/// The bare `fatal: cherry-pick failed` tail, for call sites that already
/// printed their own `error:` line(s).
fn sequencer_failed_tail() -> ExitCode {
    eprintln!("fatal: cherry-pick failed");
    ExitCode::from(128)
}

/// Stage 2 of git's pipeline: a sequencer verb was given.
fn handle_verb(verb: Verb, opts: &Opts<'_>) -> Result<ExitCode> {
    // git's `verify_opt_compatible`, in its declaration order.
    for (name, given) in [
        ("--no-commit", opts.no_commit),
        ("--signoff", opts.signoff),
        ("--mainline", opts.mainline.is_some()),
        ("--strategy", opts.strategy.is_some()),
        ("--strategy-option", !opts.xopts.is_empty()),
        ("-x", opts.record_origin),
        ("--ff", opts.allow_ff),
        ("--rerere-autoupdate", opts.rerere_auto == Some(true)),
        ("--no-rerere-autoupdate", opts.rerere_auto == Some(false)),
        ("--keep-redundant-commits", opts.keep_redundant_commits()),
        ("--empty", opts.empty_given),
    ] {
        if given {
            return Ok(fatal(&format!(
                "cherry-pick: {name} cannot be used with {}",
                verb.name()
            )));
        }
    }

    // `run_sequencer`: a verb sets `opts->revs = NULL` and skips `setup_revisions`
    // entirely, so any operand left on the command line trips the `argc > 1` usage
    // check — before the verb is carried out, which is why `--skip <rev>` prints
    // usage rather than "no cherry-pick in progress".
    if !opts.specs.is_empty() {
        return Ok(usage());
    }

    let repo = gix::discover(".")?;
    let git_dir = repo.git_dir().to_owned();

    match verb {
        // `cmd == 'q'`: `sequencer_remove_state()` followed by
        // `remove_branch_state()`. It forgets the operation without touching the
        // index or the worktree, and succeeds even with nothing in progress.
        Verb::Quit => {
            crate::sequencer::remove_state(&git_dir);
            let _ = std::fs::remove_file(git_dir.join("CHERRY_PICK_HEAD"));
            let _ = std::fs::remove_file(git_dir.join("MERGE_MSG"));
            let _ = std::fs::remove_file(git_dir.join("AUTO_MERGE"));
            Ok(ExitCode::SUCCESS)
        }
        Verb::Continue => sequencer_continue(&repo),
        Verb::Abort => sequencer_rollback(&repo),
        Verb::Skip => sequencer_skip(&repo),
    }
}

/// `reset_merge()` (sequencer.c): `git reset --merge [<commit>]`.
///
/// The two-tree reset discards the staged pick and restores the paths it touched
/// while keeping unrelated local changes, and drops `CHERRY_PICK_HEAD` /
/// `REVERT_HEAD` / `MERGE_MSG` / `AUTO_MERGE` on the way through
/// `remove_branch_state()`. git runs it without `--quiet`; a merge reset prints
/// nothing either way, and the flag is what keeps the ported reset silent too.
fn reset_merge(to: Option<ObjectId>) -> Result<ExitCode> {
    let mut args = vec!["--merge".to_string(), "--quiet".to_string()];
    if let Some(oid) = to {
        args.push(oid.to_string());
    }
    super::reset(&args)
}

/// `sequencer_continue()`: finish the pick that stopped, then replay whatever
/// the todo list still holds.
///
/// With no todo file this is `continue_single_pick()` alone — the single-pick
/// fast path, which is also the "nothing in progress" refusal. With one, the
/// stopped instruction is committed first, the index is required to match the
/// new `HEAD`, and `pick_commits()` resumes at the *next* instruction (git's
/// `todo_list.current++`).
fn sequencer_continue(repo: &gix::Repository) -> Result<ExitCode> {
    let git_dir = repo.git_dir().to_owned();
    if !crate::sequencer::todo_path(&git_dir).exists() {
        return Ok(match continue_single_pick(repo, &git_dir)? {
            Ok(()) => ExitCode::SUCCESS,
            Err(code) => code,
        });
    }

    let todo = crate::sequencer::read_todo(repo, &git_dir)?;
    if git_dir.join("CHERRY_PICK_HEAD").exists() || git_dir.join("REVERT_HEAD").exists() {
        if let Err(code) = continue_single_pick(repo, &git_dir)? {
            return Ok(code);
        }
    }

    // `index_differs_from(r, "HEAD", NULL, 0)` → `error_dirty_index()`. Resuming
    // on top of staged-but-uncommitted work would fold it into the next pick.
    let index = repo.open_index()?;
    let head_tree = repo.head_commit()?.tree_id()?.detach();
    if tree_from_index(repo, &index)? != head_tree {
        eprintln!("error: your local changes would be overwritten by cherry-pick.");
        crate::advice::Advice::CommitBeforeMerge
            .advise_plain("commit your changes or stash them to proceed.");
        return Ok(sequencer_failed_tail());
    }

    resume_todo(repo, &git_dir, &todo, 1)
}

/// `pick_commits()` over a todo list read back from disk, with the options the
/// sequence recorded in `sequencer/opts`.
fn resume_todo(
    repo: &gix::Repository,
    git_dir: &std::path::Path,
    todo: &[crate::sequencer::TodoItem],
    start: usize,
) -> Result<ExitCode> {
    // `read_populate_opts()`: the command line that started the sequence, minus
    // everything `save_opts()` does not persist.
    let loaded = crate::sequencer::read_opts(git_dir);
    let xopts: Vec<&str> = loaded.xopts.iter().map(String::as_str).collect();
    let opts = Opts {
        no_commit: loaded.no_commit,
        edit: loaded.edit == Some(true),
        edit_given: loaded.edit,
        signoff: loaded.signoff,
        record_origin: loaded.record_origin,
        allow_ff: loaded.allow_ff,
        allow_empty: loaded.allow_empty,
        allow_empty_message: loaded.allow_empty_message,
        mainline: (loaded.mainline != 0).then_some(loaded.mainline),
        empty: if loaded.drop_redundant_commits {
            Some(Empty::Drop)
        } else if loaded.keep_redundant_commits {
            Some(Empty::Keep)
        } else {
            None
        },
        cleanup: loaded.default_msg_cleanup.as_deref(),
        strategy: loaded.strategy.as_deref(),
        xopts,
        gpg_sign: false,
        specs: Vec::new(),
        leftover_unknown: false,
        verb: None,
        // Reconstructed from `sequencer/opts`, which records the *effect* of the
        // original command line rather than its spelling. Only
        // `verify_opt_compatible` reads these three, and that ran when the
        // sequence started — a resumed pick never re-checks them.
        keep_redundant_flag: loaded.keep_redundant_commits,
        empty_given: false,
        rerere_auto: None,
    };
    let cleanup = match opts.cleanup {
        Some(raw) => match parse_cleanup(raw) {
            Ok(mode) => Some(mode),
            Err(code) => return Ok(code),
        },
        None => None,
    };

    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let committer_ident = format!("{} <{}>", committer.name, committer.email);
    let env = PickEnv {
        repo,
        opts: &opts,
        cleanup,
        committer,
        committer_ident: &committer_ident,
        should_interrupt: AtomicBool::new(false),
    };
    let mut state = PickState {
        index: repo.index_or_load_from_head()?.into_owned(),
        pending_tree: None,
        head_id: repo.head_id()?.detach(),
    };
    run_todo(&env, &mut state, todo, start, true)
}

/// `sequencer_rollback()`: rewind to the pre-sequence `HEAD` and drop the state.
///
/// Without `sequencer/head` there is no sequence, so this is
/// `rollback_single_pick()` — reset the one stopped pick away. With one, the
/// rewind only happens when [`crate::sequencer::rollback_is_safe`] agrees that
/// `HEAD` is still where the sequence left it; otherwise git warns and leaves
/// the history alone rather than discarding a hand-made commit.
fn sequencer_rollback(repo: &gix::Repository) -> Result<ExitCode> {
    let git_dir = repo.git_dir().to_owned();
    let head_file = crate::sequencer::head_path(&git_dir);
    let Ok(text) = std::fs::read_to_string(&head_file) else {
        return rollback_single_pick(repo, &git_dir);
    };
    let Some(oid) = text
        .lines()
        .next()
        .and_then(|line| ObjectId::from_hex(line.trim().as_bytes()).ok())
    else {
        eprintln!(
            "error: stored pre-cherry-pick HEAD file '{}' is corrupt",
            head_file.display()
        );
        return Ok(sequencer_failed_tail());
    };

    if crate::sequencer::rollback_is_safe(repo) {
        reset_merge(Some(oid))?;
    } else {
        // `warning()`, not an error: git declines the rewind and still succeeds.
        eprintln!("warning: You seem to have moved HEAD. Not rewinding, check your HEAD!");
    }
    crate::sequencer::remove_state(&git_dir);
    Ok(ExitCode::SUCCESS)
}

/// `rollback_single_pick()`: undo one stopped pick, or report that none is.
fn rollback_single_pick(
    repo: &gix::Repository,
    git_dir: &std::path::Path,
) -> Result<ExitCode> {
    if !git_dir.join("CHERRY_PICK_HEAD").exists() && !git_dir.join("REVERT_HEAD").exists() {
        return Ok(sequencer_failed("no cherry-pick or revert in progress"));
    }
    let head = repo.head_id()?.detach();
    reset_merge(Some(head))
}

/// `sequencer_skip()`: drop the stopped pick, then continue the sequence if one
/// is live.
///
/// The `rollback_is_safe()` check only runs when `CHERRY_PICK_HEAD` is already
/// gone, because that is the state a hand-made `git commit` leaves — and in that
/// state there is nothing left to skip, so git points at `--continue` instead.
fn sequencer_skip(repo: &gix::Repository) -> Result<ExitCode> {
    let git_dir = repo.git_dir().to_owned();
    if !git_dir.join("CHERRY_PICK_HEAD").exists() {
        if crate::sequencer::get_last_command(&git_dir) != Some(crate::sequencer::Action::Pick) {
            return Ok(sequencer_failed("no cherry-pick in progress"));
        }
        if !crate::sequencer::rollback_is_safe(repo) {
            eprintln!("error: there is nothing to skip");
            crate::advice::Advice::ResolveConflict.advise_plain(
                "have you committed already?\ntry \"git cherry-pick --continue\"",
            );
            return Ok(sequencer_failed_tail());
        }
    }
    // `skip_single_pick()` is `reset_merge(HEAD)`.
    let head = repo.head_id()?.detach();
    reset_merge(Some(head))?;
    if !crate::sequencer::dir(&git_dir).is_dir() {
        return Ok(ExitCode::SUCCESS);
    }
    sequencer_continue(repo)
}

/// Write a tree object from the stage-0 entries of the current index.
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
            .ok_or_else(|| anyhow::anyhow!("index entry `{path}` has an unrepresentable mode"))?;
        editor.upsert(path.split(|&b| b == b'/').map(|c| c.as_bstr()), mode.kind(), entry.id)?;
    }
    Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?)
}

/// `continue_single_pick()`: finish the pick that stopped.
///
/// git spawns `git commit --no-edit --cleanup=strip`, which recovers the message
/// and the author from `MERGE_MSG` and `CHERRY_PICK_HEAD`/`REVERT_HEAD`; this
/// does the same work in process. The index must be fully resolved, the worktree
/// already holds the resolution, and the state files go once the commit lands.
///
/// `Err(code)` is a refusal git already reported — including its "no cherry-pick
/// or revert in progress", which is what a `--continue` with nothing stopped
/// reduces to.
fn continue_single_pick(
    repo: &gix::Repository,
    git_dir: &std::path::Path,
) -> Result<std::result::Result<(), ExitCode>> {
    let stopped = ["CHERRY_PICK_HEAD", "REVERT_HEAD"]
        .into_iter()
        .find_map(|name| std::fs::read_to_string(git_dir.join(name)).ok());
    let Some(raw) = stopped else {
        return Ok(Err(sequencer_failed("no cherry-pick or revert in progress")));
    };
    let pick_id = ObjectId::from_hex(raw.trim().as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid CHERRY_PICK_HEAD: {e}"))?;
    let pick = repo.find_commit(pick_id)?;

    // The resolution must be complete: any conflict stage left blocks the
    // commit. git gets here through the child `git commit`, so the diagnosis is
    // literally that command's — the `U<TAB><path>` report on stdout included.
    let index = repo.open_index()?;
    if index.entries().iter().any(|e| e.stage_raw() != 0) {
        return Ok(Err(super::commit::die_resolve_conflict(&index)));
    }
    let tree_id = tree_from_index(repo, &index)?;
    let head_id = repo
        .head_id()
        .map_err(|_| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();

    // The message git commits is `MERGE_MSG` with the default cleanup applied
    // (comment lines dropped) — which is the picked message the stop recorded,
    // carrying any `-x`/`-s` trailer, plus whatever the user edited in.
    let merge_msg = std::fs::read_to_string(git_dir.join("MERGE_MSG"))
        .unwrap_or_else(|_| pick.message_raw().map(|m| m.to_string()).unwrap_or_default());
    let mut message: BString = clean_message(&merge_msg).into();

    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let author = pick.author()?;
    let commit = gix::objs::Commit {
        message: std::mem::take(&mut message),
        tree: tree_id,
        author: author.to_owned()?,
        committer: committer.to_owned()?,
        encoding: None,
        parents: std::iter::once(head_id).collect(),
        extra_headers: Default::default(),
    };
    let new_id = repo.write_object(&commit)?.detach();
    let reflog = gix::reference::log::message("cherry-pick", commit.message.as_bstr(), 1);
    advance_head(repo, head_id, new_id, reflog)?;
    // `sequencer_post_commit_cleanup()`, which is what the child `git commit`
    // runs on its way out: the pseudo-refs always go, the sequencer directory
    // only once the todo is down to the instruction just finished.
    let _ = std::fs::remove_file(git_dir.join("MERGE_MSG"));
    crate::sequencer::post_commit_cleanup(repo)?;

    // `print_commit_summary()`. `git commit` reaches it with
    // `author_date_is_interesting()` true here, because `prepare_to_commit()`
    // set `author_message = "CHERRY_PICK_HEAD"` to reuse the picked author — so
    // the ` Date:` line appears just as it does for a pick that did not stop.
    let subject = gix::objs::commit::MessageRef::from_bytes(commit.message.as_bstr())
        .summary()
        .to_str_lossy()
        .into_owned();
    let branch_label = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "detached HEAD".to_string(),
    };
    println!("[{branch_label} {}] {subject}", new_id.attach(repo).shorten_or_id());
    let author_ident = format!("{} <{}>", author.name, author.email);
    let committer_ident = format!("{} <{}>", committer.name, committer.email);
    if author_ident != committer_ident {
        println!(" Author: {author_ident}");
    }
    {
        use gix::date::time::format;
        println!(" Date: {}", author.time()?.format_or_unix(format::DEFAULT));
    }
    let mut resolved: Vec<(BString, EntryMode, ObjectId)> = flatten(repo, tree_id)?
        .into_iter()
        .map(|(path, (mode, id))| (path, mode, id))
        .collect();
    resolved.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let head_tree = repo.find_commit(head_id)?.tree_id()?.detach();
    print_diffstat(repo, head_tree, tree_id, &resolved)?;
    Ok(Ok(()))
}

/// git's default message cleanup: drop whole-line comments (`#…`) and collapse
/// trailing blank lines, ensuring a single trailing newline.
fn clean_message(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        if line.starts_with('#') {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if out.is_empty() {
        out.push('\n');
    }
    out
}

/// git's stopped-on-empty state: record `CHERRY_PICK_HEAD` and `MERGE_MSG`,
/// print the in-progress status block on stdout and the advice on stderr,
/// exit 1.
///
/// The worktree is necessarily clean here — the merged tree equalled `HEAD`'s —
/// so the status block's body is fixed rather than derived from a diff.
/// `AUTO_MERGE` is not written here: `merge_switch_to_result()` already recorded
/// it for every merge-ort result, and a pick that reached this point through a
/// `merge-<strategy>` child never went through merge-ort at all — which is why
/// stock's `.git` holds none after `--strategy=resolve`.
fn stop_empty(
    repo: &gix::Repository,
    pick_id: ObjectId,
    head_id: ObjectId,
    message: &BString,
) -> Result<ExitCode> {
    let git_dir = repo.git_dir();
    std::fs::write(git_dir.join("CHERRY_PICK_HEAD"), format!("{pick_id}\n"))?;
    std::fs::write(git_dir.join("MERGE_MSG"), &message[..])?;

    match repo.head_name()? {
        Some(name) => println!("On branch {}", name.shorten()),
        None => println!("HEAD detached at {}", head_id.attach(repo).shorten_or_id()),
    }
    // `wt_status_print` follows the header with the upstream relation and a
    // blank line; a branch with no upstream contributes neither.
    print!("{}", super::status::tracking_block(repo));
    println!(
        "You are currently cherry-picking commit {}.",
        pick_id.attach(repo).shorten_or_id()
    );
    println!("  (all conflicts fixed: run \"git cherry-pick --continue\")");
    println!("  (use \"git cherry-pick --skip\" to skip this patch)");
    println!("  (use \"git cherry-pick --abort\" to cancel the cherry-pick operation)");
    println!();
    println!("nothing to commit, working tree clean");

    eprintln!("The previous cherry-pick is now empty, possibly due to conflict resolution.");
    eprintln!("If you wish to commit it anyway, use:");
    eprintln!();
    eprintln!("    git commit --allow-empty");
    eprintln!();
    eprintln!("Otherwise, please use 'git cherry-pick --skip'");

    Ok(ExitCode::from(1))
}

/// git's `strbuf_stripspace` plus the per-mode extras: trailing whitespace goes,
/// runs of blank lines collapse to one, and leading/trailing blank lines are
/// dropped. `strip` additionally removes comment lines, `scissors` truncates at
/// the scissors marker, and `verbatim` returns the message untouched.
fn cleanup_message(message: &BString, mode: Cleanup) -> BString {
    if mode == Cleanup::Verbatim {
        return message.clone();
    }
    let strip_comments = mode == Cleanup::Strip;

    let mut out: Vec<u8> = Vec::with_capacity(message.len());
    let mut pending_blank = false;
    let mut seen_content = false;
    for line in message.split(|&b| b == b'\n') {
        // `# ------------------------ >8 ------------------------` and
        // everything after it is the scissors cut.
        if mode == Cleanup::Scissors && line.starts_with(b"# ") && line.contains_str(">8") {
            break;
        }
        if strip_comments && line.first() == Some(&b'#') {
            continue;
        }
        let trimmed = match line.iter().rposition(|b| !b.is_ascii_whitespace()) {
            Some(last) => &line[..=last],
            None => &[][..],
        };
        if trimmed.is_empty() {
            pending_blank = seen_content;
            continue;
        }
        if pending_blank {
            out.push(b'\n');
            pending_blank = false;
        }
        out.extend_from_slice(trimmed);
        out.push(b'\n');
        seen_content = true;
    }
    BString::from(out)
}

/// Flatten a tree into `path -> (mode, blob id)`, recursively, dropping entries
/// whose mode has no tree representation (which cannot occur for a real tree).
fn flatten(
    repo: &gix::Repository,
    tree: ObjectId,
) -> Result<HashMap<BString, (EntryMode, ObjectId)>> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    let mut out = HashMap::with_capacity(index.entries().len());
    for e in index.entries() {
        if let Some(mode) = e.mode.to_tree_entry_mode() {
            out.insert(e.path_in(backing).to_owned(), (mode, e.id));
        }
    }
    Ok(out)
}

/// Point `HEAD` (writing through to its branch when attached) at `new`, with
/// `reflog` as the reflog message, requiring `old` to still be the current tip.
fn advance_head(
    repo: &gix::Repository,
    old: ObjectId,
    new: ObjectId,
    reflog: BString,
) -> Result<()> {
    let name: FullName = "HEAD"
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: reflog,
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(old)),
            new: Target::Object(new),
        },
        name,
        deref: true,
    })?;
    Ok(())
}

/// git's short-stat plus the create/delete/mode-change block, for the diff from
/// `old_tree` to `new_tree`. Ported from `porcelain::commit`, which derives the
/// file count from the flattened entry sets (so a rename counts as a delete plus
/// a create) and the line counts from a rename-free tree diff.
fn print_diffstat(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
    new_entries: &[(BString, EntryMode, ObjectId)],
) -> Result<()> {
    let old_entries = flatten(repo, old_tree)?;
    let new_paths: HashSet<&BString> = new_entries.iter().map(|(p, _, _)| p).collect();

    let mut files_changed: u64 = 0;
    let mut summary: Vec<(BString, String)> = Vec::new();
    for (path, mode, id) in new_entries {
        match old_entries.get(path) {
            None => {
                files_changed += 1;
                summary.push((path.clone(), format!("create mode {} {path}", octal(*mode))));
            }
            Some((old_mode, old_id)) => {
                if old_id != id || old_mode != mode {
                    files_changed += 1;
                }
                if old_mode != mode {
                    summary.push((
                        path.clone(),
                        format!("mode change {} => {} {path}", octal(*old_mode), octal(*mode)),
                    ));
                }
            }
        }
    }
    for (path, (mode, _)) in &old_entries {
        if !new_paths.contains(path) {
            files_changed += 1;
            summary.push((path.clone(), format!("delete mode {} {path}", octal(*mode))));
        }
    }

    if files_changed == 0 {
        return Ok(());
    }

    let old = repo.find_tree(old_tree)?;
    let new = repo.find_tree(new_tree)?;
    let mut platform = old.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });
    let stats = platform.stats(&new)?;

    let (ins, del) = (stats.lines_added, stats.lines_removed);
    let mut line = format!(" {files_changed} file{} changed", plural(files_changed));
    if ins > 0 || del == 0 {
        line.push_str(&format!(", {ins} insertion{}(+)", plural(ins)));
    }
    if del > 0 || ins == 0 {
        line.push_str(&format!(", {del} deletion{}(-)", plural(del)));
    }
    println!("{line}");

    summary.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, l) in &summary {
        println!(" {l}");
    }
    Ok(())
}

/// git's `has_conforming_footer`: does the message end in a trailer block, so
/// that `-x` may append its line without inserting a blank line first?
///
/// The block is the last paragraph, and it only qualifies when it is not also
/// the first paragraph (a subject is never a footer) and every one of its lines
/// is a `token: value` trailer, an indented continuation, a `#` comment, or a
/// `(cherry picked from commit <id>)` line — the last form being what makes a
/// second `-x` append directly, as stock git does.
fn has_conforming_footer(msg: &[u8]) -> bool {
    // Drop trailing blank lines.
    let mut lines: Vec<&[u8]> = msg.split(|&b| b == b'\n').collect();
    while lines.last().is_some_and(|l| l.iter().all(u8::is_ascii_whitespace)) {
        lines.pop();
    }
    if lines.is_empty() {
        return false;
    }

    // Start of the last paragraph.
    let mut start = lines.len();
    while start > 0 && !lines[start - 1].iter().all(u8::is_ascii_whitespace) {
        start -= 1;
    }
    // No preceding blank line means this paragraph is the subject.
    if start == 0 {
        return false;
    }

    let mut saw_trailer = false;
    for line in &lines[start..] {
        if line.first().is_some_and(|b| b.is_ascii_whitespace()) || line.starts_with(b"#") {
            continue;
        }
        if line.starts_with(b"(cherry picked from commit ") && line.ends_with(b")") {
            saw_trailer = true;
            continue;
        }
        match line.iter().position(|&b| b == b':') {
            // A trailer token is non-empty and contains no whitespace.
            Some(sep)
                if sep > 0 && !line[..sep].iter().any(u8::is_ascii_whitespace) =>
            {
                saw_trailer = true;
            }
            _ => return false,
        }
    }
    saw_trailer
}

/// Move a clean worktree and its index from the state captured in `old` to
/// `new_tree_id`, writing only the files that changed, and return the index that
/// was persisted.
///
/// Verbatim port of `porcelain::merge`'s helper: added/modified files are
/// checked out via `gix-worktree-state`, removed files are deleted, and the new
/// index reuses prior stats for unchanged entries.
fn update_clean_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_tree_id: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<gix::index::File> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    let mut new_index = repo.index_from_tree(&new_tree_id)?;
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        None => false,
    });

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

    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

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

    new_index.remove_tree();
    new_index.write(Default::default())?;
    Ok(new_index)
}

/// The git-internal octal representation of a tree entry mode, e.g. `100644`.
fn octal(mode: EntryMode) -> String {
    let mut buf = [0u8; 6];
    mode.as_bytes(&mut buf).to_string()
}

/// `""` for a count of 1, `"s"` otherwise — for git's `file`/`files` etc.
fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> std::result::Result<Opts<'static>, ExitCode> {
        // Leak so the borrowed `Opts` can outlive the call; a test process is
        // short-lived and this keeps the parser's real lifetime signature.
        let owned: &'static [String] = Box::leak(
            args.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_boxed_slice(),
        );
        parse(owned)
    }

    /// `--strategy` is `OPT_STRING` (last wins, `--no-strategy` clears) while
    /// `-X`/`--strategy-option` is `OPT_STRVEC` (accumulates in order,
    /// `--no-strategy-option` clears the whole list). Both spellings of each
    /// feed the same slot, and `-X` takes its value attached or detached.
    #[test]
    fn strategy_slots_follow_their_parse_options_types() {
        let o = parse_args(&["--strategy=ort", "-Xtheirs", "--strategy", "resolve", "-X", "ours"])
            .unwrap_or_else(|_| panic!("well-formed options must parse"));
        assert_eq!(o.strategy, Some("resolve"));
        assert_eq!(o.xopts, vec!["theirs", "ours"]);

        let o = parse_args(&["--strategy=ort", "--no-strategy"])
            .unwrap_or_else(|_| panic!("well-formed options must parse"));
        assert_eq!(o.strategy, None);

        let o = parse_args(&["-Xtheirs", "--strategy-option=ours", "--no-strategy-option"])
            .unwrap_or_else(|_| panic!("well-formed options must parse"));
        assert!(o.xopts.is_empty());

        // An empty `--strategy=` value is kept as-is: git validates no strategy
        // name, so the empty one reaches `try_merge_command` as `merge-`.
        let o = parse_args(&["--strategy="])
            .unwrap_or_else(|_| panic!("well-formed options must parse"));
        assert_eq!(o.strategy, Some(""));
    }

    /// The four verbs share one `OPT_CMDMODE` slot: repeating one is fine, a
    /// second *different* one is rejected during parsing. Observed against stock
    /// git 2.55.0, which exits 129 for the conflict and reaches the "no
    /// cherry-pick in progress" path (128) for the repeat.
    #[test]
    fn cmdmode_verbs_conflict_only_when_they_differ() {
        assert!(parse_args(&["--skip", "--skip"]).is_ok());
        assert!(parse_args(&["--quit", "--quit", "--quit"]).is_ok());
        assert!(parse_args(&["--skip", "--abort"]).is_err());
        assert!(parse_args(&["--cleanup=default", "main", "-x", "--skip", "--abort"]).is_err());
    }

    /// The conflict names the option being parsed first and the one already
    /// stored second, so the two argument orders print different lines. Both
    /// strings are stock git 2.55.0's, verbatim.
    #[test]
    fn cmdmode_conflict_message_matches_git() {
        assert_eq!(
            verb_conflict(Verb::Abort, Verb::Skip),
            "options '--abort' and '--skip' cannot be used together"
        );
        assert_eq!(
            verb_conflict(Verb::Skip, Verb::Abort),
            "options '--skip' and '--abort' cannot be used together"
        );
        assert_eq!(
            verb_conflict(Verb::Continue, Verb::Quit),
            "options '--continue' and '--quit' cannot be used together"
        );
    }
}
