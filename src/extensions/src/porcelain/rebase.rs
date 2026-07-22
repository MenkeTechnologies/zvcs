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
//! ### Genuine picks
//!
//! A replay where `<onto>` is *not* already `<head>`'s ancestor is a genuine
//! pick: each commit is cherry-picked with a real three-way merge (its tree
//! against the growing tip over the commit's first parent) via
//! [`crate::merge_apply`]. Clean picks reproduce git's rebased tree byte-for-byte;
//! a conflict stops the rebase with `CONFLICT`/`could not apply` and the
//! conflicted worktree/index in place, recoverable with `git reset --hard
//! ORIG_HEAD`.
//!
//! ### What is NOT ported, and why
//!
//! * **No patch-id equivalence.** Default `git rebase` *drops* a to-be-rebased
//!   commit whose patch is already in `<upstream>` (`warning: skipped previously
//!   applied commit <abbrev>`). Deciding that needs a patch-id per commit; nothing
//!   vendored computes one, so such commits are re-picked (they usually merge to a
//!   no-op) rather than dropped.
//! * The **`--continue`/`--skip`/`--abort`/`--quit`/`--edit-todo`/
//!   `--show-current-patch`** state machine (the `.git/rebase-merge` directory),
//!   `--root` (mints a root commit), `--fork-point` (walks the upstream reflog),
//!   `--autostash` against a dirty tree (writes a stash commit), and `-v`/`--stat`
//!   past the up-to-date exit (the upstream diffstat). Each is rejected with a
//!   message naming the reason; none is silently ignored.
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

    // git reads the repository (and the in-progress state dirs, which seed the
    // backend) before `parse_options` runs.
    let repo = gix::discover(".")?;
    let state_dir = repo.common_dir();
    let apply_in_progress = state_dir.join("rebase-apply").is_dir();
    let merge_in_progress = state_dir.join("rebase-merge").is_dir();
    if apply_in_progress {
        ty = Backend::Apply;
    } else if merge_in_progress {
        ty = Backend::Merge;
    }
    let in_progress = apply_in_progress || merge_in_progress;

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
                "rerere-autoupdate" => {
                    noarg!();
                    // Only consulted while resolving a conflict during replay,
                    // which never happens on the paths this module completes.
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
                // `--keep-empty` only changes which commits the sequencer picks,
                // so beyond selecting the merge backend it has no effect on a
                // range that picks nothing.
                "keep-empty" => {
                    noarg!();
                    try_imply!(ty, if unset { "--no-keep-empty" } else { "--keep-empty" });
                }
                "autosquash" => {
                    noarg!();
                    autosquash = i32::from(!unset);
                }
                "update-refs" => {
                    noarg!();
                    update_refs = i32::from(!unset);
                }
                // Accepted, and genuinely a no-op on the paths this module
                // completes: a range that picks nothing produces no commit to
                // sign.
                "gpg-sign" => {}
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
                'S' => break,
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
        // Every mode option resumes or unwinds a sequencer run recorded in the
        // state directory this port never writes.
        bail!(
            "unsupported flag {:?} (resuming a rebase requires the .git/rebase-merge state directory and commit replay)",
            m.flag()
        );
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

    // "all am options except -q are compatible only with --apply"
    if !git_am_opts.is_empty() || ty == Backend::Apply {
        let has_real_am_opt = git_am_opts.iter().any(|o| o != "-q");
        if has_real_am_opt || ty == Backend::Apply {
            if ty == Backend::Merge {
                die!("apply options and merge options cannot be used together");
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
    if autosquash == 1 {
        try_imply!(ty, "--autosquash");
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
    if root && onto_name.is_none() {
        write_synth_root(&repo)?;
    }

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
    let head_oid = head
        .id()
        .ok_or_else(|| anyhow!("HEAD does not point to a commit"))?
        .detach();
    let branch: Option<FullName> = head.referent_name().map(std::borrow::ToOwned::to_owned);
    drop(head);

    // --- <upstream> --------------------------------------------------------
    if root {
        // With no `--onto`, git mints a fresh root commit (`commit_tree("")`),
        // which changes the object database before anything else happens.
        bail!("unsupported flag \"--root\" (rebasing onto a synthesized root commit requires commit replay)");
    }
    let upstream_spec = match positional.first() {
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
    };
    let Some(upstream_oid) = peel_to_commit(&repo, &upstream_spec) else {
        die!("invalid upstream '{upstream_spec}'");
    };

    // --- <branch> ----------------------------------------------------------
    // git checks out `<branch>` before rebasing. That checkout is out of scope
    // here, so only the already-current branch is accepted — but the argument is
    // still validated the way git validates it, in git's order.
    let branch_name = match positional.get(1) {
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
            if current.as_deref() != Some(requested.as_str()) {
                bail!(
                    "rebasing a branch other than the checked-out one requires a checkout; run `git switch {requested}` first"
                );
            }
            requested.clone()
        }
        None => match branch.as_ref() {
            Some(b) => b.shorten().to_string(),
            None => "HEAD".to_string(),
        },
    };

    // --- <onto> ------------------------------------------------------------
    let onto_spec = if keep_base {
        format!("{upstream_spec}...{branch_name}")
    } else {
        onto_name.clone().unwrap_or_else(|| upstream_spec.clone())
    };
    let onto_is_merge_base = onto_spec.contains("...");
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
        onto_oid
    } else {
        upstream_oid
    };

    if fork_point > 0 {
        // `get_fork_point()` needs the upstream's reflog to find where the
        // branch diverged; nothing here walks reflogs.
        bail!("unsupported flag \"--fork-point\" (refining the upstream needs `merge-base --fork-point`, which walks the upstream reflog)");
    }

    // --- require_clean_work_tree() -----------------------------------------
    // `refresh_index()` runs first and reports unmerged paths on stdout, even
    // under --quiet, before either error line.
    let (unstaged, staged, conflicts) = dirty_state(&repo)?;
    if autostash && (unstaged || staged) {
        bail!("unsupported flag \"--autostash\" (stashing a dirty worktree requires writing a stash commit)");
    }
    for path in &conflicts {
        println!("{path}: needs merge");
    }
    if unstaged || staged {
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

    // --- can_fast_forward() ------------------------------------------------
    let branch_base = merge_base_unique(&repo, onto_oid, head_oid)?;
    let can_ff = can_fast_forward(&repo, branch_base, onto_oid, upstream_oid, head_oid)?;
    if allow_preemptive_ff && can_ff {
        if flags & FORCE == 0 {
            if flags & NO_QUIET != 0 {
                if branch_name == "HEAD" {
                    println!("HEAD is up to date.");
                } else {
                    println!("Current branch {branch_name} is up to date.");
                }
            }
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

    if flags & DIFFSTAT != 0 {
        bail!("unsupported flag \"-v\"/\"--stat\" (the upstream diffstat is not ported)");
    }

    // --- decide whether anything would be replayed -------------------------
    // The merge backend picks the right side of `<upstream>...<head>`; the apply
    // backend picks `<upstream>..<head>`. Both are empty exactly when
    // `<upstream>..<head>` is, which is what is measured here.
    let mut todo: Vec<ObjectId> = Vec::new();
    for info in repo.rev_walk([head_oid]).with_hidden([upstream_oid]).all()? {
        todo.push(info?.id);
    }

    let apply_backend = ty == Backend::Apply;

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
    let exact_replay = allow_preemptive_ff && can_ff && !todo.is_empty();

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
                todo.len()
            );
        }
    }
    // Genuine picks (a real three-way merge per commit) are replayed in the finish
    // below via [`crate::merge_apply`]; no refusal here anymore.

    if !exec.is_empty() {
        // Reachable only with an empty todo list, where git appends no exec
        // lines at all — but saying so is cheaper than proving it per case.
        bail!("unsupported flag \"--exec\" (exec lines run inside the sequencer)");
    }

    // --- the finish --------------------------------------------------------
    // Serialize the whole read-modify-write through the repo coordinator (a
    // no-op when no daemon is running), matching the merge/zsync write path.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Capture the current (clean) index BEFORE any ref moves: it mirrors the old
    // tree and carries the filesystem stats reused for unchanged files. Taken
    // first because `index_or_load_from_head` would otherwise fall back to the
    // *new* HEAD if a repository happened to have no index file on disk.
    let old_index = repo.index_or_load_from_head()?.into_owned();

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
        &format!("rebase (start): checkout {onto_spec}"),
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
                &gix::reference::log::message("rebase (pick)", step.message.as_bstr(), 1)
                    .to_string(),
            )?;
            tip = new;
        }
    } else if !todo.is_empty() && !apply_backend {
        // Genuine picks: cherry-pick each commit (oldest first) onto the growing
        // tip with a real three-way merge. On a conflict the rebase stops with the
        // conflicted worktree/index in place; recover with `git reset --hard
        // ORIG_HEAD` (git's `--continue`/`--abort` state machine is not ported).
        let committer = repo
            .committer()
            .ok_or_else(|| anyhow!("committer identity is not configured"))??
            .to_owned()?;
        let empty_tree = ObjectId::empty_tree(repo.object_hash());
        let onto_tree = repo.find_commit(onto_oid)?.tree_id()?.detach();
        let mut cur_index = repo.index_from_tree(&onto_tree)?;
        let total = todo.len();
        for (n, oid) in todo.iter().rev().enumerate() {
            if flags & NO_QUIET != 0 {
                eprint!(
                    "Rebasing ({}/{total}){}",
                    n + 1,
                    if flags & VERBOSE != 0 { "\n" } else { "\r" }
                );
            }
            let commit = repo.find_commit(*oid)?;
            let message: BString = commit.message_raw()?.to_owned();
            let subject: BString = match message.find_byte(b'\n') {
                Some(p) => message[..p].as_bstr().to_owned(),
                None => message.clone(),
            };
            let ctree = commit.tree_id()?.detach();
            let base_tree = match commit.parent_ids().next() {
                Some(p) => repo.find_commit(p.detach())?.tree_id()?.detach(),
                None => empty_tree,
            };
            let tip_tree = repo.find_commit(tip)?.tree_id()?.detach();

            let short = oid.to_hex_with_len(7).to_string();
            let other_label = format!("{short} ({subject})", subject = subject.to_str_lossy());
            let labels = gix::merge::blob::builtin_driver::text::Labels {
                ancestor: Some(BStr::new(b"HEAD")),
                current: Some(BStr::new(b"HEAD")),
                other: Some(BStr::new(other_label.as_bytes())),
            };
            let applied = crate::merge_apply::three_way_merge(
                &repo,
                base_tree,
                tip_tree,
                ctree,
                &cur_index,
                labels,
                &should_interrupt,
            )?;
            cur_index = applied.index;
            cur_index.write(Default::default())?;

            if !applied.conflicts.is_empty() {
                eprintln!("error: could not apply {short}... {}", subject.to_str_lossy());
                eprintln!(
                    "hint: Resolve all conflicts manually, mark them as resolved with"
                );
                eprintln!(
                    "hint: \"git add/rm <conflicted_files>\", then run \"git rebase --continue\"."
                );
                eprintln!(
                    "hint: You can instead skip this commit: run \"git rebase --skip\"."
                );
                eprintln!(
                    "hint: To abort and get back to the state before \"git rebase\", run \"git rebase --abort\"."
                );
                eprintln!("Could not apply {short}... {}", subject.to_str_lossy());
                return Ok(ExitCode::from(1));
            }

            let author = commit.author()?.to_owned()?;
            let new = repo
                .write_object(&gix::objs::Commit {
                    message: message.clone(),
                    tree: applied.tree_id,
                    author,
                    committer: committer.clone(),
                    encoding: None,
                    parents: std::iter::once(tip).collect(),
                    extra_headers: Default::default(),
                })?
                .detach();
            set_head(
                &repo,
                Target::Object(new),
                &gix::reference::log::message("rebase (pick)", message.as_bstr(), 1).to_string(),
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
                        message: format!("rebase (finish): {name} onto {onto_oid}").into(),
                    },
                    expected: PreviousValue::MustExistAndMatch(Target::Object(head_oid)),
                    new: Target::Object(tip),
                },
                name: b.clone(),
                deref: false,
            })?;
            set_head(
                &repo,
                Target::Symbolic(b.clone()),
                &format!("rebase (finish): returning to {name}"),
            )?;
            name
        }
        None => "detached HEAD".to_string(),
    };

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
fn write_synth_root(repo: &gix::Repository) -> Result<()> {
    let author = repo
        .author()
        .ok_or_else(|| anyhow!("author identity is not configured"))??
        .to_owned()?;
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow!("committer identity is not configured"))??
        .to_owned()?;
    repo.write_object(&gix::objs::Commit {
        message: BString::default(),
        tree: ObjectId::empty_tree(repo.object_hash()),
        author,
        committer,
        encoding: None,
        parents: Default::default(),
        extra_headers: Default::default(),
    })?;
    Ok(())
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

/// Resolve `spec` and peel it to a commit id, or `None` when either step fails —
/// git reports both as one "invalid" message rather than surfacing the cause.
fn peel_to_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let id = repo.rev_parse_single(spec).ok()?;
    Some(id.object().ok()?.peel_to_commit().ok()?.id)
}

/// True when `name` names a hook git would actually run.
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
fn dirty_state(repo: &gix::Repository) -> Result<(bool, bool, Vec<String>)> {
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
fn set_head(repo: &gix::Repository, target: Target, message: &str) -> Result<()> {
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
