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
//! `label`, `reset`, `merge` and `update-ref` parse and round-trip through
//! `--edit-todo`, but executing one is refused with a message naming the reason
//! (a refused instruction leaves the rebase resumable, so `--abort` still
//! works). They need the merge-topology rebuild `make_script_with_merges()`
//! drives, which is also what `rebase.maxLabelLength` sizes — the one
//! `rebase.*` key still without a reader.
//!
//! ### What is NOT ported, and why
//!
//! * **No patch-id equivalence.** `sequencer_make_script()` sets
//!   `revs.cherry_mark`, which drops a to-be-rebased commit whose patch is
//!   already in `<upstream>` *before* the sheet is written, announcing it with
//!   `warning: skipped previously applied commit <abbrev>`. Deciding that needs
//!   a patch id per commit and nothing vendored computes one, so such a commit
//!   stays in the sheet and is dropped in the pick loop instead — by git's own
//!   `drop_redundant_commits`, with its `dropping <oid> <subject> -- patch
//!   contents already upstream` line. The visible difference is the step count:
//!   the sheet still holds the commit, so the progress line counts it.
//! * `-v`/`--verbose` past the up-to-date exit. `-v` prints the upstream diffstat
//!   *and* a second, post-replay diffstat the sequencer emits in verbose mode;
//!   only the first is ported, so `-v` is rejected with a message naming the
//!   reason rather than emitting half of git's output. Plain `--stat` (and
//!   `rebase.stat=true`, which sets the same bit and nothing else) is ported.
//! * `--update-refs`, and the `label`/`reset`/`merge`/`update-ref` instructions
//!   it generates: it still selects the merge backend and still raises git's
//!   apply-backend incompatibility errors, but `make_script_with_merges()` and
//!   the four executors are missing.
//! * `--rebase-merges` over a range that *contains* a merge, for the same
//!   reason — it is refused by name rather than flattening the history it was
//!   asked to keep. Over a linear range the instruction sheet is picks either
//!   way, so it replays exactly as git does; only git's step count differs,
//!   since its sheet also carries the `label onto`/`reset onto` pair.
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
//! (both of which raise git's apply-backend incompatibility errors).
//!
//! Read by the instruction sheet in [`super::rebase_todo`]:
//! `rebase.instructionFormat` (the oneline on each line),
//! `rebase.abbreviateCommands` (one-letter spellings) and
//! `rebase.missingCommitsCheck` (what happens when the user deletes a line).
//!
//! `rebase.maxLabelLength` has no reader: it sizes the labels
//! `make_script_with_merges()` mints, and `--rebase-merges` is not ported.
//!
//! `--signoff`/`--trailer` are *not* refused up front, and *not* refused at all
//! when the todo is empty: like git they only set `REBASE_FORCE`, so an
//! up-to-date range takes the noop / fast-forward finish (git rewrites nothing
//! there — `git rebase --signoff HEAD` leaves the tip untouched), and a missing
//! upstream (stdout, exit 1) or an invalid upstream/onto (`fatal:`, exit 128)
//! still reports git's own diagnostic in git's order, since resolution runs
//! before the message-rewrite refusal.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use super::rebase_todo as todo;

/// git's `builtin/rebase.c` usage block, reproduced verbatim (git 2.55.0) so the
/// `-h` and unknown-option paths are byte-identical.
const USAGE: &str = "\
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
                        "drop" | "keep" | "stop" => {}
                        "ask" => eprintln!(
                            "warning: --empty=ask is deprecated; use '--empty=stop' instead."
                        ),
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
    if !trailers.is_empty() {
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
        // The apply backend's `.git/rebase-apply` state is written by `git am`,
        // which is not ported; only the merge backend's `rebase-merge` resumes.
        if apply_in_progress {
            bail!(
                "unsupported flag {:?} for an apply-backend rebase (only merge-backend \
                 .git/rebase-merge state is resumable)",
                m.flag()
            );
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
        bail!("cannot rebase an unborn branch");
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
            match tracking {
                Some(Ok(name)) => {
                    if fork_point < 0 {
                        fork_point = 1;
                    }
                    name.shorten().to_string()
                }
                Some(Err(e)) => bail!("{e}"),
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
    let upstream_oid = if root {
        None
    } else {
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
            let is_branch = repo
                .try_find_reference(&format!("refs/heads/{requested}"))
                .ok()
                .flatten()
                .is_some();
            if !is_branch && peel_to_commit(&repo, requested).is_none() {
                die!("no such branch/commit '{requested}'");
            }
            let current = branch.as_ref().map(|b| b.shorten().to_string());
            // `options.switch_to = argv[0]` (builtin/rebase.c:1698) is set for any
            // `<branch>` argument, the current one included: when the rebase turns out
            // to be a no-op, `checkout_up_to_date()` still records the checkout.
            if is_branch && current.as_deref() == Some(requested.as_str()) {
                switch_to = Some(requested.clone());
            }
            if current.as_deref() != Some(requested.as_str()) {
                if !is_branch {
                    // git only switches to a *branch*; a bare commit-ish is
                    // resolved as the thing to rebase, with `HEAD` left alone.
                    die!("no such branch/commit '{requested}'");
                }
                // Everything below rebases `<branch>`, so its tip stands in for
                // `HEAD` right away. The worktree switch itself waits until after
                // `require_clean_work_tree()`, which git runs first — a dirty tree
                // must produce git's refusal, not the checkout's.
                head_oid = peel_to_commit(&repo, requested)
                    .ok_or_else(|| anyhow!("no such branch/commit '{requested}'"))?;
                branch = Some(FullName::try_from(format!("refs/heads/{requested}"))?);
                switch_to = Some(requested.clone());
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
        let base = match (peel_to_commit(&repo, left), peel_to_commit(&repo, right)) {
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
                }
                super::checkout::record_head_move(
                    &repo,
                    Some(head_oid),
                    Some(head_oid),
                    &message,
                );
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
    if !ok_to_skip_pre_rebase
        && !crate::hooks::run(&repo, "pre-rebase", &[onto_spec.as_str(), branch_name.as_str()], None)?
    {
        return Ok(ExitCode::from(1));
    }

    // `--stat` (and `rebase.stat=true`): the diffstat of what changed upstream,
    // `diff_tree_oid(merge_base, onto)` with DIFF_FORMAT_DIFFSTAT|DIFF_FORMAT_SUMMARY
    // on stdout. A null merge base (unrelated histories) diffs against the empty
    // tree. Rendering is delegated to the `diff` porcelain, which drives the same
    // machinery; the one deviation is git's `detect_rename`, which this port has
    // nowhere, so a rename renders as a delete plus an add.
    //
    // `-v` additionally sets REBASE_VERBOSE, which prefixes a `Changes from <a> to
    // <b>:` line *and* makes the sequencer emit a second, post-replay diffstat of
    // its own. That second one is not ported, so `-v` past this point is still
    // refused rather than answered with half of git's output.
    if flags & DIFFSTAT != 0 {
        if flags & VERBOSE != 0 {
            bail!("unsupported flag \"-v\"/\"--verbose\" (the sequencer's post-replay diffstat is not ported)");
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
    for info in repo.rev_walk([head_oid]).with_hidden(hidden).all()? {
        replay_range.push(info?.id);
    }
    // `sequencer_make_script()` walks with `revs.max_parents = 1` unless
    // `--rebase-merges` asked for the merge structure to be recreated: a merge
    // commit has no single patch to replay, and `pick` refuses one outright. git
    // therefore leaves merges out of the instruction sheet, which is what makes
    // `git rebase --onto <base> <upstream>` work on a branch that contains one.
    let todo_range: Vec<ObjectId> = if rebase_merges == 1 {
        // `make_script_with_merges()` writes `label`/`reset`/`merge` instructions
        // to rebuild the branch topology, and the four executors that run them are
        // not ported. Over a linear range the sheet is picks either way, so that
        // works; a merge in the range needs the real thing, and replaying it as a
        // pick would flatten history the user asked to keep.
        if let Some(merge) = replay_range.iter().find(|id| {
            repo.find_commit(**id)
                .map(|c| c.parent_ids().count() > 1)
                .unwrap_or(false)
        }) {
            let short = gix::prelude::ObjectIdExt::attach(*merge, &repo).shorten_or_id();
            bail!(
                "--rebase-merges over a merge commit ({short}) is not supported \
                 (recreating the topology needs the label/reset/merge instructions \
                 `make_script_with_merges()` writes, which are not ported); rebase \
                 without it to flatten, or replay the branches individually"
            );
        }
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
    let exact_replay = plan.is_some();
    let plan = plan.unwrap_or_default();

    if exact_replay {
        // `--signoff`/`--trailer` rewrite the *message* of every picked commit
        // (a Signed-off-by / custom trailer). The exact replay reproduces commit
        // metadata — committer, and the author date under `--ignore-date` — but
        // not message trailers, so a range that actually picks commits is refused
        // rather than replayed without the trailer. An empty todo (handled by the
        // noop / fast-forward finishes below) signs nothing, so it needs no guard;
        // that is why `git rebase --signoff HEAD` is accepted and only a non-empty
        // range is refused here.
        if gpg_sign_on {
            return Err(refuse_gpg_sign());
        }
        if signoff {
            bail!("unsupported flag \"--signoff\" (rewriting commit messages requires commit replay)");
        }
        if !trailers.is_empty() {
            bail!("unsupported flag \"--trailer\" (rewriting commit messages requires commit replay)");
        }
        // `git format-patch` emits nothing for a commit that changes no tree, so
        // the apply backend stops at one with `Patch is empty.` and leaves a
        // half-finished `.git/rebase-apply` behind. Reproducing that interrupted
        // state is out of scope. The merge backend keeps such commits — its picks
        // are trees, not patches — and needs no guard.
        if apply_backend && plan.iter().any(|s| s.tree == s.parent_tree) {
            bail!(
                "replaying an empty commit with the apply backend is not ported: `git am` stops \
                 with `Patch is empty.` and leaves a .git/rebase-apply state directory behind"
            );
        }
    } else if apply_backend {
        // The apply backend detaches first and only then notices it merely
        // fast-forwarded. Deciding here keeps a refused rebase from mutating
        // anything.
        if branch_base != Some(head_oid) {
            bail!(
                "replaying {} commit(s) with the apply backend needs `git am`-style patch \
                 application, which is not ported",
                replay_range.len()
            );
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
                rerere_autoupdate,
            },
            onto_spec: &onto_spec,
            upstream: upstream_oid,
            exec: &exec,
            autosquash: autosquash == 1,
            keep_empty,
            interactive: flags & INTERACTIVE_EXPLICIT != 0,
            autostash: autostash_oid,
            old_index: &old_index,
        });
    }

    if apply_backend && flags & NO_QUIET != 0 {
        println!("First, rewinding head to replay your work on top of it...");
    }

    // git writes ORIG_HEAD only once it commits to actually rebasing. It is a
    // pseudo-ref, so no reflog is created for it (gix applies git's own
    // `should_autocreate_reflog` rule).
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "rebase".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(head_oid),
        },
        name: full_name("ORIG_HEAD")?,
        deref: false,
    })?;

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

            let new = repo
                .write_object(&gix::objs::Commit {
                    message: step.message.clone(),
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
            tip = new;
        }
    } else if apply_backend {
        println!("Fast-forwarded {branch_name} to {onto_spec}.");
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

/// True when `name` names a hook git would actually run.
#[allow(dead_code)] // port helper retained for the hook-dispatch path
fn hook_is_runnable(repo: &gix::Repository, name: &str) -> bool {
    let path = repo.common_dir().join("hooks").join(name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
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
    /// `opts.allow_ff` — false under `-f`/`--no-ff`. Recorded as the presence of
    /// a `no-ff` marker file so a resumed rebase keeps re-committing rather than
    /// fast-forwarding a pick whose parent is already the tip.
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
    /// `opts.allow_rerere_auto` — what [`Sequencer::stop_for_conflict`] passes to
    /// `repo_rerere()`. `Some(true)` stages a replayed resolution, `Some(false)`
    /// leaves it unstaged, `None` defers to `rerere.autoupdate`. git records it
    /// as the one-liner `$state_dir/allow_rerere_autoupdate`, holding the flag
    /// as spelled, and writes no file at all when it was never given.
    rerere_autoupdate: Option<bool>,
}

fn rebase_merge_dir(repo: &gix::Repository) -> std::path::PathBuf {
    repo.git_dir().join("rebase-merge")
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
    marker(&dir, "no-ff", !st.allow_ff)?;
    marker(&dir, "quiet", st.quiet)?;
    marker(&dir, "verbose", st.verbose)?;
    marker(&dir, "reschedule-failed-exec", st.reschedule_failed_exec)?;
    marker(&dir, "no-reschedule-failed-exec", !st.reschedule_failed_exec)?;
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
        allow_ff: !dir.join("no-ff").exists(),
        quiet: dir.join("quiet").exists(),
        verbose: dir.join("verbose").exists(),
        reschedule_failed_exec: dir.join("reschedule-failed-exec").exists(),
        // `read_oneliner(…, READ_ONELINER_SKIP_IF_EMPTY)` followed by an exact
        // match on the flag as spelled: anything else leaves the option unset.
        rerere_autoupdate: match read("allow_rerere_autoupdate").as_deref() {
            Ok("--rerere-autoupdate") => Some(true),
            Ok("--no-rerere-autoupdate") => Some(false),
            _ => None,
        },
    })
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
/// re-attach `HEAD`, and drop the state directory.
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

    if st.head_name != "detached HEAD" {
        let name = full_name(&st.head_name)?;
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("{} (abort): updating HEAD", reflog_action()).into(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(st.orig_head),
            },
            name: name.clone(),
            deref: false,
        })?;
        set_head(repo, Target::Symbolic(name), &format!("{} (abort): returning", reflog_action()))?;
    } else {
        set_head(repo, Target::Object(st.orig_head), &format!("{} (abort)", reflog_action()))?;
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

    new_index.remove_tree();
    new_index.write(Default::default())?;
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
    autostash: Option<ObjectId>,
    old_index: &'a gix::index::File,
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
    let script = todo::make_script(repo, &start.range, start.keep_empty, abbreviate)?;

    // `init_basic_state()`: the state directory exists before the editor runs,
    // so an interrupted edit is still an in-progress rebase.
    write_basic_state(repo, &start.state)?;
    if let Some(oid) = start.autostash {
        std::fs::write(dir.join("autostash"), format!("{oid}\n"))?;
    }

    let (mut list, ok) = todo::List::parse(repo, &script, false);
    if !ok {
        bail!("generated an unusable todo list");
    }

    // `complete_action()`.
    if list.items.is_empty() {
        list.items.push(todo::Item::new(todo::Cmd::Noop));
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
        EditOutcome::Rejected => Ok(ExitCode::from(1)),
    }
}

/// What the editor round-trip produced.
enum EditOutcome {
    Ok(todo::List),
    /// The user emptied the sheet on the initial edit: git aborts the rebase.
    NothingToDo,
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
    if interactive {
        todo::launch_sequence_editor(repo, &todo_path)?;
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
/// pseudo-ref, so no reflog is created for it (gix applies git's own
/// `should_autocreate_reflog` rule).
fn write_orig_head(repo: &gix::Repository, head: ObjectId) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "rebase".into(),
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
    } else if let Some(code) = seq.commit_staged_changes()? {
        return Ok(code);
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
        let env = self.author_env();
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
        Ok(Sequencer {
            repo,
            st,
            committer,
            should_interrupt: AtomicBool::new(false),
            autostash: read_autostash(repo),
            index,
            done_nr: 0,
            total_nr: 0,
            fixup_count: 0,
            fixups: Vec::new(),
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
            let _ = std::fs::remove_file(dir.join("author-script"));

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
                    // `is_final_fixup()`: the last member of a fixup/squash
                    // chain is the one that cleans the combined message up.
                    let next_is_fixup = list
                        .items
                        .get(i + 1)
                        .is_some_and(|n| n.cmd.is_fixup());
                    let final_fixup = item.cmd.is_fixup() && !next_is_fixup;
                    self.pick_one_commit(&item, final_fixup)?
                }
                todo::Cmd::Exec => self.do_exec(&item)?,
                todo::Cmd::Noop | todo::Cmd::Drop | todo::Cmd::Comment => Step::Next,
                todo::Cmd::Label | todo::Cmd::Reset | todo::Cmd::Merge => {
                    self.term_clear_line();
                    bail!(
                        "unsupported todo command {:?} (`label`/`reset`/`merge` rebuild a merge \
                         topology, which is not ported; the rebase is still resumable with \
                         `git rebase --abort`)",
                        item.cmd.name()
                    )
                }
                todo::Cmd::UpdateRef => {
                    self.term_clear_line();
                    bail!(
                        "unsupported todo command \"update-ref\" (refs pointing into the rebased \
                         range are not tracked; the rebase is still resumable with \
                         `git rebase --abort`)"
                    )
                }
                todo::Cmd::Invalid => {
                    self.term_clear_line();
                    eprintln!("error: please fix this using 'git rebase --edit-todo'.");
                    return Ok(ExitCode::from(1));
                }
                todo::Cmd::Revert => {
                    self.term_clear_line();
                    bail!("unsupported todo command \"revert\" (only `git revert` produces it)")
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
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .env_remove("GIT_CHERRY_PICK_HELP")
            .status()
            .map_err(|e| anyhow!("cannot run 'sh -c': {e}"))?;
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

        // `CREATE_ROOT_COMMIT`: while the tip is still the stand-in `<onto>` that
        // `--root` without `--onto` minted, a pick becomes a new *root* commit
        // rather than that stand-in's child.
        let create_root = item.cmd.is_pick_or_similar() && Some(head) == self.st.squash_onto;
        if create_root && item.cmd.is_fixup() {
            bail!("cannot fixup root commit");
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
        let applied = crate::merge_apply::three_way_merge(
            repo,
            base_tree,
            head_tree,
            ctree,
            &self.index,
            labels,
            &self.should_interrupt,
        )?;
        self.index = applied.index;
        self.index.write(Default::default())?;

        if !applied.conflicts.is_empty() {
            return self.stop_for_conflict(item, oid, &short, &subject, &message);
        }

        let fast_forward = self.st.allow_ff
            && !item.cmd.is_fixup()
            && match parent {
                Some(p) => p == head,
                None => create_root,
            };
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
        if !item.cmd.is_fixup() && applied.tree_id == head_tree {
            let originally_empty = todo::is_original_commit_empty(repo, &commit)?;
            if !originally_empty {
                self.term_clear_line();
                let subject = commit
                    .message()
                    .map(|m| m.summary().to_string())
                    .unwrap_or_default();
                eprintln!(
                    "dropping {} {subject} -- patch contents already upstream",
                    oid.to_hex()
                );
                return Ok(Step::Next);
            }
        }

        write_author_script(repo, &commit)?;

        if item.cmd.is_fixup() {
            return self.commit_fixup(item, &commit, applied.tree_id, final_fixup);
        }

        let author = commit.author()?.to_owned()?;
        let parents = if create_root {
            Default::default()
        } else {
            std::iter::once(head).collect()
        };
        let new = repo
            .write_object(&gix::objs::Commit {
                message: message.clone(),
                tree: applied.tree_id,
                author,
                committer: self.committer.clone(),
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

        match item.cmd {
            todo::Cmd::Reword => self.reword(),
            todo::Cmd::Edit => self.stop_for_edit(new, &short, item),
            _ => Ok(Step::Next),
        }
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
        std::fs::write(dir.join("stopped-sha"), format!("{at}\n"))?;
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
        let author = head_commit.author()?.to_owned()?;

        if !final_fixup {
            // Mid-chain: no editor, the message is the running combination.
            let message = std::fs::read(dir.join("message-squash"))?;
            let cleaned = super::stripspace::strip_space(&message, None);
            let new = repo
                .write_object(&gix::objs::Commit {
                    message: cleaned.into(),
                    tree,
                    author,
                    committer: self.committer.clone(),
                    encoding: None,
                    parents: parents.into_iter().collect(),
                    extra_headers: Default::default(),
                })?
                .detach();
            set_head(repo, Target::Object(new), &format!("{} (fixup)", reflog_action()))?;
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
            let new = repo
                .write_object(&gix::objs::Commit {
                    message: cleaned.into(),
                    tree,
                    author,
                    committer: self.committer.clone(),
                    encoding: None,
                    parents: parents.into_iter().collect(),
                    extra_headers: Default::default(),
                })?
                .detach();
            set_head(repo, Target::Object(new), &format!("{} (fixup)", reflog_action()))?;
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
            let rest = &body[commented_len..];
            buf.extend_from_slice(rest);
            if replaces && !self.seen_squash() {
                // `fixup -C` outside a squash chain replaces the message
                // outright; the chain's end takes this alone, minus the blank
                // lines the commented-out subject left behind.
                std::fs::write(dir.join("message-fixup"), todo::skip_blank_lines(rest))?;
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
    ) -> Result<Step> {
        let dir = self.dir();
        std::fs::write(dir.join("stopped-sha"), format!("{oid}\n"))?;
        // The message `--continue` will commit. A conflicted fixup/squash
        // commits the running combination instead of the picked commit's own
        // message (`error_failed_squash()`).
        if item.cmd.is_fixup() && dir.join("message-squash").exists() {
            std::fs::copy(dir.join("message-squash"), dir.join("message"))?;
        } else {
            std::fs::write(dir.join("message"), message)?;
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
        let _ = std::fs::remove_dir_all(rebase_merge_dir(repo));
        if let Some(oid) = self.autostash {
            crate::porcelain::stash::apply_autostash(repo, oid, self.st.quiet)?;
        }
        super::maintenance::run_auto_maintenance(repo, self.st.quiet)?;
        if !self.st.quiet {
            self.term_clear_line();
            eprintln!("Successfully rebased and updated {label}.");
        }
        Ok(ExitCode::SUCCESS)
    }
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
    new_index.remove_tree();
    new_index.write(Default::default())?;

    Ok(())
}
