//! `git checkout` — the legacy combined verb: switch branches (or detach at a
//! commit) *and* restore paths, backed by the vendored gitoxide crates so tools
//! on PATH see the same `.git`, index, and worktree.
//!
//! Supported invocations (the common forms):
//! ```text
//!   * `git checkout <branch>`                 → switch to an existing local branch
//!   * `git checkout <commit-ish>`             → detach `HEAD` at a commit
//!   * `git checkout -d|--detach <rev>`        → detach even when `<rev>` is a branch
//!   * `git checkout -b <name> [<start>]`      → create branch at `<start>` (default HEAD), switch
//!   * `git checkout -B <name> [<start>]`      → create-or-reset branch at `<start>`, switch
//!   * `git checkout [<tree-ish>] -- <path>…`  → restore paths (index+worktree from `<tree-ish>`; worktree-only from index when no tree-ish)
//!   * `git checkout <path>…`                  → restore paths from the index (bare pathspec form)
//!   * `git checkout <remote-only-name>`         → DWIM (`--guess`, the default):
//!                                                create-and-track a local branch
//!                                                when exactly one remote has it.
//!   * `git checkout -f|--force [<commit-ish>]`  → git's `discard_changes`: the
//!                                                worktree and index are reset to
//!                                                the target tree through
//!                                                `reset_tree()`, throwing away
//!                                                modified, staged, conflicted and
//!                                                deleted tracked files instead of
//!                                                refusing or carrying them
//!   * `git checkout [-f]`                       → no ref moves; only the worktree
//!                                                reconciliation runs
//!   * `git checkout HEAD` / `git checkout @`    → likewise a no-op for every ref:
//!                                                there is no `refs/heads/HEAD` to
//!                                                switch to, so this is NOT a detach
//!   * `git checkout --no-overlay <tree-ish> -- <path>…` → also delete paths that
//!                                                match the pathspec but are absent
//!                                                from `<tree-ish>` (overlay mode,
//!                                                the default, never removes).
//!   * `git checkout --pathspec-from-file <file>` (`--pathspec-file-nul`) → read
//!                                                pathspecs from `<file>` (or stdin
//!                                                for `-`) instead of the argv.
//!   * `-q`/`--quiet` suppress the transition messages
//! ```
//!
//! Every transition message (`Switched to …`, `Already on …`, `Reset branch …`,
//! `HEAD is now at …`, `Previous HEAD position was …`, `Updated N path(s) …`,
//! and the `advice.detachedHead` block) goes to **stderr**, as in stock git —
//! `git checkout` writes nothing to stdout on success.
//!
//! A switch is gated the way `unpack_trees()` gates it: per path the two trees
//! disagree on, `verify_uptodate()` for one being rewritten and
//! `verify_absent()` for one being added, so local work on any other path is
//! carried across and listed afterwards by `show_local_changes()`
//! (`<letter>\t<path>` on stdout). A path both trees share keeps its index entry
//! untouched, staged content included.
//!
//! Deviations (honest, conservative — never corrupting):
//! ```text
//!   * Pathspecs match literal files and directory prefixes (and `.`); general
//!     glob magic is left to the shell.
//!   * `--ours`/`--theirs` write a conflicted path's stage-2/stage-3 blob into
//!     the worktree (index left conflicted), `--orphan` starts an unborn branch —
//!     all matching stock git.
//!   * Tracking follows `setup_tracking()`: `-t`/`--track` and `--no-track` decide
//!     outright, and otherwise `branch.autoSetupMerge` does — its default (`true`)
//!     configures the upstream whenever the start point is a remote-tracking branch,
//!     which is what `checkout -b feature origin/feature` relies on. `always` adds
//!     local start points, `simple` narrows it to a same-named remote branch, and
//!     `inherit`'s upstream-copying is not reproduced (it behaves as the default).
//!   * A switch ends with `report_tracking()`'s ahead/behind summary, the same block
//!     `status` prints; a branch created by `-b` reports only the upstream it just
//!     configured, as git does.
//!   * A bare `--` introduces no pathspec, so `checkout -B main origin/main --` is a
//!     branch reset rather than a path restore; a path after the separator is still
//!     `Cannot update paths and switch to branch` (exit 128).
//!   * `-m`/`--merge` on a *switch* is git 2.55's autostash path: when the
//!     two-way `unpack_trees()` refuses because local changes stand in the way,
//!     the changes are stashed (`autostash while switching to '<name>'`), the
//!     now-clean switch happens, and the stash is re-applied with a three-way
//!     merge — so they come back **unstaged**, a conflicting re-apply leaves the
//!     snapshot in `refs/stash` with conflict markers labelled `<name>`/`local`,
//!     and the run still exits 0. An *untracked* file in the way is not a local
//!     change `-m` can carry, so that refusal stands. The listing this path
//!     prints is a *second*, headed one (`The following paths have local
//!     changes:`) emitted **after** `update_refs_for_switch()` has announced the
//!     switch, not the one at the tail of `merge_working_tree()`.
//!     `--conflict=<style>` reaches the re-apply through
//!     [`crate::merge_apply::three_way_merge_styled`], the way git reaches it by
//!     pushing `merge.conflictStyle=<style>` as a config parameter around the
//!     `git stash apply`.
//!   * `-m`/`--merge` on *paths* is `checkout_merged()`: each pathspec-matched
//!     conflicted entry is re-merged from its three stages under git's
//!     `base`/`ours`/`theirs` labels — in any of the three conflict styles — and
//!     written back to the worktree, leaving the index stages alone. Without it
//!     a conflicted path is `error: path '<p>' is unmerged` (exit 1, nothing
//!     written); with `-f` it is a warning and the path is left as it is. Naming
//!     a tree to read from alongside `-m`, `--ours` or `--theirs` is upstream's
//!     `fatal: '--merge', '--ours', or '--theirs' cannot be used when checking
//!     out of a tree`.
//!   * `-p`/`--patch` runs the interactive hunk selector ([`super::add_patch`]),
//!     restoring the picked hunks into the index and the worktree.
//!   * `-U`/`--unified <n>`, `--inter-hunk-context <n>` and `--[no-]auto-advance`
//!     configure that hunk selector and nothing else, but are still observable
//!     without `--patch`: their values go through parse-options' `OPT_INTEGER`
//!     validation and `cmd_checkout()` then refuses any non-default one with
//!     `fatal: '--unified' cannot be negative` / `fatal: the option '<x>'
//!     requires '--patch'`, right after the parse and before any ref or pathspec
//!     is resolved. Shared with `git reset` — see [`super::reset::PatchDiffOpts`].
//!   * `--conflict <style>` is validated and implies `-m` (`if (conflict_style)
//!     opts->merge = 1`), and a later `--no-conflict` takes both back. With the
//!     option absent the style is `merge.conflictStyle`'s, which implies nothing
//!     on its own.
//! ```

use anyhow::{anyhow, bail, Result};
// Every `print!`/`println!` below goes through git's stdout buffer; see
// `crate::cstdio` and the `defer()` call in `checkout()`.
use crate::cstdio::{print, println};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU8;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::diff::blob::{Algorithm, InternedInput};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::bstr::ByteSlice;
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// `usage_with_options()` over `builtin/checkout.c`'s `checkout` option table.
const USAGE: &str = r"usage: git checkout [<options>] <branch>
   or: git checkout [<options>] [<branch>] -- <file>...

    -b <branch>           create and checkout a new branch
    -B <branch>           create/reset and checkout a branch
    -l                    create reflog for new branch
    --[no-]guess          second guess 'git checkout <no-such-branch>' (default)
    --[no-]overlay        use overlay mode (default)
    --[no-]auto-advance   auto advance to the next file when selecting hunks interactively
    -q, --[no-]quiet      suppress progress reporting
    --[no-]recurse-submodules[=<checkout>]
                          control recursive updating of submodules
    --[no-]progress       force progress reporting
    -m, --[no-]merge      perform a 3-way merge with the new branch
    --[no-]conflict <style>
                          conflict style (merge, diff3, or zdiff3)
    -d, --[no-]detach     detach HEAD at named commit
    -t, --[no-]track[=(direct|inherit)]
                          set branch tracking configuration
    -f, --[no-]force      force checkout (throw away local modifications)
    --[no-]orphan <new-branch>
                          new unborn branch
    --[no-]overwrite-ignore
                          update ignored files (default)
    --[no-]ignore-other-worktrees
                          do not check if another worktree is using this branch
    -2, --ours            checkout our version for unmerged files
    -3, --theirs          checkout their version for unmerged files
    -p, --[no-]patch      select hunks interactively
    -U, --unified <n>     generate diffs with <n> lines context
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    --[no-]ignore-skip-worktree-bits
                          do not limit pathspecs to sparse entries only
    --[no-]pathspec-from-file <file>
                          read pathspec from file
    --[no-]pathspec-file-nul
                          with --pathspec-from-file, pathspec elements are separated with NUL character

";

/// `cmd_checkout`'s option table, which git builds by concatenating four:
/// `checkout_options[]` (builtin/checkout.c:2096-2108), then
/// `add_common_options()` (:1767-1778), `add_common_switch_branch_options()`
/// (:1787-1802) and `add_checkout_path_options()` (:1811-1826). The order is the
/// order the usage block above lists them in, and it decides which two names an
/// `ambiguous option:` sentence reports.
///
/// `-b`, `-B` and `-l` are `OPT_STRING`/`OPT_BOOL` with a NULL `long_name`, so
/// `parse_long_opt()` skips them (parse-options.c:544-545) and they are absent
/// here. The five `PARSE_OPT_NONEG` entries are the two writeout-stage selectors
/// and the two diff-context integers.
pub(super) const LONG_OPTS: &[super::LongOpt] = {
    use super::{Arg, LongOpt};
    &[
        LongOpt { name: "guess", neg: true, arg: Arg::None },
        LongOpt { name: "overlay", neg: true, arg: Arg::None },
        LongOpt { name: "auto-advance", neg: true, arg: Arg::None },
        LongOpt { name: "quiet", neg: true, arg: Arg::None },
        LongOpt { name: "recurse-submodules", neg: true, arg: Arg::Optional },
        LongOpt { name: "progress", neg: true, arg: Arg::None },
        LongOpt { name: "merge", neg: true, arg: Arg::None },
        LongOpt { name: "conflict", neg: true, arg: Arg::Required },
        LongOpt { name: "detach", neg: true, arg: Arg::None },
        LongOpt { name: "track", neg: true, arg: Arg::Optional },
        LongOpt { name: "force", neg: true, arg: Arg::None },
        LongOpt { name: "orphan", neg: true, arg: Arg::Required },
        LongOpt { name: "overwrite-ignore", neg: true, arg: Arg::None },
        LongOpt { name: "ignore-other-worktrees", neg: true, arg: Arg::None },
        // `OPT_SET_INT_F(..., PARSE_OPT_NONEG)`.
        LongOpt { name: "ours", neg: false, arg: Arg::None },
        LongOpt { name: "theirs", neg: false, arg: Arg::None },
        LongOpt { name: "patch", neg: true, arg: Arg::None },
        // `OPT_DIFF_UNIFIED` / `OPT_DIFF_INTERHUNK_CONTEXT` are
        // `OPT_INTEGER_F(..., PARSE_OPT_NONEG)` (parse-options.h:627-628).
        LongOpt { name: "unified", neg: false, arg: Arg::Required },
        LongOpt { name: "inter-hunk-context", neg: false, arg: Arg::Required },
        LongOpt { name: "ignore-skip-worktree-bits", neg: true, arg: Arg::None },
        LongOpt { name: "pathspec-from-file", neg: true, arg: Arg::Required },
        LongOpt { name: "pathspec-file-nul", neg: true, arg: Arg::None },
    ]
};

pub fn checkout(args: &[String]) -> Result<ExitCode> {
    // git writes `show_local_changes()` / `report_tracking()` to stdout and
    // `Switched to branch '<b>'` to stderr (builtin/checkout.c). Off a terminal
    // stdio holds the stdout half until `exit()`, so a caller capturing both
    // sees the stderr line first; see `crate::cstdio`.
    crate::cstdio::defer();
    // Every ref this moves carries a reflog line, and git writes those with an
    // identity it synthesizes from the OS when `user.*` is unset — only a
    // `commit` with nothing determinable is refused. Without this a bare runner,
    // a container or a `sudo` shell cannot switch branches at all, and a
    // recursive submodule walk aborts on the first one it reaches.
    let mut repo = gix::discover(".")?;
    crate::ensure_reflog_identity(&mut repo);

    // `cmd_checkout()` special-cases the exact command line `git checkout -b
    // <branch>` — argv checked literally, so `-B` and any extra option fall out
    // of it — and gives it `git switch -c`'s behaviour by setting
    // `only_merge_on_switching_branches`. With no start-point that makes
    // `switch_branches()` skip `merge_working_tree()` altogether, which is why
    // this one spelling prints no local-changes listing.
    let only_merge_on_switching_branches = args.len() == 2 && args[0] == "-b";

    // --- Argument classification -------------------------------------------
    // `new_branch` is Some((name, reset_if_exists)) for -b / -B.
    // `-b <name>` and `-B <name>`, kept in the two slots git keeps them in
    // (`opts->new_branch` / `opts->new_branch_force`) so both being set is
    // detectable; folded into `new_branch` once that check has run.
    let mut new_branch_create: Option<String> = None;
    let mut new_branch_force: Option<String> = None;
    let mut detach = false;
    let mut quiet = false;
    // `-f`/`--force` → git's `opts->discard_changes`.
    let mut force = false;
    // `-t`/`--track` vs `--no-track`; `None` leaves the decision to
    // `branch.autoSetupMerge`, which is how `checkout -b x origin/x` gets its upstream.
    let mut track: Option<bool> = None;
    let mut orphan: Option<String> = None;
    // Which conflict stage `--ours`/`--theirs` writes out (2 = ours, 3 = theirs);
    // the last of the two flags wins, exactly like git's `opts.writeout_stage`.
    let mut writeout_stage: Option<u8> = None;
    let mut pre: Vec<&str> = Vec::new(); // positionals before `--`
    let mut post: Vec<&str> = Vec::new(); // pathspecs after `--`
    let mut has_dashdash = false;
    // `None` = fall back to `checkout.guess` (default on); `Some(b)` = `--[no-]guess`.
    let mut guess_flag: Option<bool> = None;
    // Overlay (default) never removes paths; `--no-overlay` deletes paths that
    // match the pathspec but are absent from the source tree. Tri-state, because
    // git's `opts->overlay_mode` starts at -1 and `checkout_branch()` refuses any
    // explicit setting: `if (opts->overlay_mode != -1) die("'%s' cannot be used
    // with switching branches", "--[no]-overlay")` (builtin/checkout.c:1671).
    let mut overlay_mode: Option<bool> = None;
    // `-l` (`opts->new_branch_log`), refused by `checkout_paths()`:
    // `if (opts->new_branch_log) die("'%s' cannot be used with updating paths", "-l")`
    // (builtin/checkout.c:533). Branch reflogs are always written here, so the
    // flag has no effect beyond that refusal.
    let mut new_branch_log = false;
    let mut pathspec_from_file: Option<String> = None;
    let mut pathspec_file_nul = false;
    // `--recurse-submodules[=<pathspec>]` / `--no-recurse-submodules`. `None` =
    // fall back to `submodule.recurse` config; `Some(b)` = explicit flag. After a
    // switch, each initialized submodule's worktree is moved to the gitlink the
    // superproject now records (git's `submodule_move_head` via the worktree updater).
    let mut recurse_submodules: Option<bool> = None;
    // `-U`/`--unified`, `--inter-hunk-context`, `--[no-]auto-advance`: the
    // interactive-hunk-selector options, shared verbatim with `git reset` and
    // refused after the loop exactly as git refuses them without `--patch`.
    let mut patch_opts = super::reset::PatchDiffOpts::default();
    // `-p`/`--patch`: hand the paths to the interactive hunk selector instead of
    // restoring them wholesale.
    let mut patch_mode = false;
    // `-m`/`--merge` → git's `opts->merge`, and `--conflict=<style>`, which
    // implies it (`if (conflict_style) { opts->merge = 1; … }`).
    let mut merge = false;
    let mut conflict_style: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let orig = args[i].as_str();
        // Respell the token the way `parse_long_opt()` reads it, so any
        // unambiguous prefix lands on the arm its full spelling lands on. Two
        // positions are exempt because parse-options never looks them up: the
        // argument owed to a `-U`/`--unified`/`--inter-hunk-context`, and
        // everything past `--`.
        let resolved;
        let a: &str = if patch_opts.awaiting_value() || has_dashdash {
            orig
        } else {
            resolved = match super::canonical_long(orig, LONG_OPTS) {
                super::Long::Name(name) => name,
                super::Long::Ambiguous(first, second) => {
                    return Ok(super::ambiguous_option(orig, &first, &second, USAGE))
                }
            };
            resolved.as_ref()
        };
        // A `-U`/`--unified`/`--inter-hunk-context` value still owed is taken
        // verbatim even past `--`, the way parse-options takes it; outside that,
        // these options are only recognised before `--`.
        if patch_opts.awaiting_value() || !has_dashdash {
            match patch_opts.take_arg(a) {
                Err(code) => return Ok(code),
                Ok(true) => {
                    i += 1;
                    continue;
                }
                Ok(false) => {}
            }
        }
        if has_dashdash {
            post.push(orig);
            i += 1;
            continue;
        }
        // Long options that take a value, in `--opt=value` or `--opt value` form.
        // `--conflict` implies `-m` (`if (conflict_style) opts->merge = 1;`) and
        // names the style the three-way markers are written in.
        if a == "--conflict" || a.starts_with("--conflict=") {
            let val = match a.strip_prefix("--conflict=") {
                Some(v) => v.to_string(),
                None => {
                    i += 1;
                    super::value_at(args, i, a)?.to_string()
                }
            };
            if !matches!(val.as_str(), "merge" | "diff3" | "zdiff3") {
                eprintln!("error: unknown style '{val}' given for '--conflict'");
                return Ok(ExitCode::from(129));
            }
            conflict_style = Some(val);
            i += 1;
            continue;
        }
        // `--track=(direct|inherit)`: the optional-value form of `-t`/`--track`
        // (`git`'s `parse_opt_tracking_mode`). `direct` is the default explicit
        // tracking already implemented here; `inherit` needs upstream-inheritance
        // substrate that is not vendored, so it errors honestly rather than
        // silently behaving like `direct`. An unknown value is git's 129.
        if let Some(val) = a.strip_prefix("--track=") {
            match val {
                "direct" => track = Some(true),
                "inherit" => bail!(
                    "--track=inherit is not supported (upstream-inheritance tracking not implemented)"
                ),
                _ => {
                    eprintln!("error: option `--track' expects \"direct\" or \"inherit\"");
                    return Ok(ExitCode::from(129));
                }
            }
            i += 1;
            continue;
        }
        if a == "--pathspec-from-file" || a.starts_with("--pathspec-from-file=") {
            let val = match a.strip_prefix("--pathspec-from-file=") {
                Some(v) => v.to_string(),
                None => {
                    i += 1;
                    super::value_at(args, i, a)?.to_string()
                }
            };
            pathspec_from_file = Some(val);
            i += 1;
            continue;
        }
        match a {
            "--" => has_dashdash = true,
            // parse_options_step()'s `internal_help`: the block on stdout at
            // 129, with no `error:` line — a help request is not a rejection.
            // `--help-all` reaches the same renderer with USAGE_FULL, which this
            // table renders identically: it has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => return Ok(super::show_usage(USAGE)),
            // Tracked apart, because git counts the three *pointers* it fills:
            // `if ((!!opts->new_branch + !!opts->new_branch_force +
            // !!opts->new_orphan_branch) > 1) die("options '-b', '-B', and
            // '--orphan' cannot be used together")` (builtin/checkout.c:1926).
            // Collapsing `-b`/`-B` into one slot loses that count, and `-b x -B y`
            // then silently creates `y`.
            "-b" | "-B" => {
                let name = super::value_at(args, i + 1, a)?.to_string();
                if a == "-B" {
                    new_branch_force = Some(name);
                } else {
                    new_branch_create = Some(name);
                }
                i += 1;
            }
            "--orphan" => {
                orphan = Some(super::value_at(args, i + 1, a)?.to_string());
                i += 1;
            }
            // `-d` is git's short form of `--detach` (OPT_BOOL('d', "detach")).
            "-d" | "--detach" => detach = true,
            "--no-detach" => detach = false,
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            // git's `-f` sets `opts->discard_changes`, which routes the switch
            // through `reset_tree()` instead of the 2-way merge: local changes
            // are thrown away rather than carried or refused.
            "-f" | "--force" => force = true,
            "--no-force" => force = false,
            "-t" | "--track" => track = Some(true),
            "--no-track" => track = Some(false),
            "--ours" | "-2" => writeout_stage = Some(2),
            "--theirs" | "-3" => writeout_stage = Some(3),
            // `opts->merge`: carry local changes across a switch the two-way
            // `unpack_trees()` would otherwise refuse, by stashing them and
            // merging them back afterwards. With a clean worktree it changes
            // nothing.
            "-m" | "--merge" => merge = true,
            "--no-merge" => merge = false,
            // `--no-conflict` NULLs the style string. It does not clear
            // `opts->merge`, because that was set when `--conflict` was seen and
            // nothing sets it back.
            "--no-conflict" => conflict_style = None,
            // The branch reflog is always written here (`RefLog::AndReference`), so
            // `-l` only survives to be refused alongside a pathspec.
            "-l" => new_branch_log = true,
            "--guess" => guess_flag = Some(true),
            "--no-guess" => guess_flag = Some(false),
            "--overlay" => overlay_mode = Some(true),
            "--no-overlay" => overlay_mode = Some(false),
            "--pathspec-file-nul" => pathspec_file_nul = true,
            // The unset sense of the three value-carrying entries: git NULLs the
            // `OPT_STRING`/`OPT_FILENAME` pointer and clears the `OPT_BOOL`, so
            // each is "as if never given".
            "--no-orphan" => orphan = None,
            "--no-pathspec-from-file" => pathspec_from_file = None,
            "--no-pathspec-file-nul" => pathspec_file_nul = false,
            // Accepted no-ops: progress is discarded, ignored-file overwrite is the
            // default, and the other-worktree / skip-worktree checks git guards
            // against are not enforced here, so toggling them changes nothing.
            "--progress" | "--no-progress" => {}
            "--overwrite-ignore" | "--no-overwrite-ignore" => {}
            "--ignore-other-worktrees" | "--no-ignore-other-worktrees" => {}
            "--ignore-skip-worktree-bits" | "--no-ignore-skip-worktree-bits" => {}
            "-p" | "--patch" => patch_mode = true,
            "--no-patch" => patch_mode = false,
            "--recurse-submodules" => recurse_submodules = Some(true),
            "--no-recurse-submodules" => recurse_submodules = Some(false),
            // `--recurse-submodules=<value>` limits which submodules move; this
            // port recurses into all active ones rather than honoring the pathspec.
            // The value is still validated: `option_parse_update_submodules()`
            // (submodule.c) takes only a boolean, and anything else is
            // `bad recurse-submodules argument: <value>` at 128.
            _ if a.starts_with("--recurse-submodules=") => {
                let val = &a["--recurse-submodules=".len()..];
                match crate::optint::maybe_bool(val) {
                    Some(on) => recurse_submodules = Some(on),
                    None => crate::git_fatal!("bad recurse-submodules argument: {val}"),
                }
            }
            // Every name the table carries is dispatched above, so anything left
            // with a dash is unknown to stock git too and gets git's own
            // refusal — the `error:` line and the usage block on stderr, at 129.
            _ if a.starts_with('-') && a.len() > 1 => {
                return Ok(super::unknown_option(a, USAGE))
            }
            _ => pre.push(orig),
        }
        i += 1;
    }

    if let Err(code) = patch_opts.finish() {
        return Ok(code);
    }
    // `if (conflict_style) { opts->merge = 1; git_xmerge_config(…); }`, run after
    // the whole command line has been parsed — which is why a `--no-conflict`
    // that NULLs the style again also takes the implied `-m` with it. Without
    // the option the style is whatever `merge.conflictStyle` already said, and
    // that alone implies nothing.
    if conflict_style.is_some() {
        merge = true;
    }
    let conflict_style = conflict_style
        .or_else(|| {
            repo.config_snapshot()
                .string("merge.conflictStyle")
                .map(|v| v.to_string())
        })
        .unwrap_or_else(|| "merge".to_string());
    // git collects the hunk-selector options into `add_p_opt` and refuses them
    // right after parse-options, before any ref or pathspec is resolved — so a
    // `-U 3` alongside an unknown branch reports the option, not the branch
    // (verified against git 2.55.0).
    if let Some(code) = patch_opts.require_patch(patch_mode) {
        return Ok(code);
    }

    // `if ((!!opts->new_branch + !!opts->new_branch_force + !!opts->new_orphan_branch) > 1)
    //     die(_("options '-%c', '-%c', and '%s' cannot be used together"),
    //         cb_option, toupper(cb_option), "--orphan");`
    // (builtin/checkout.c:1926). `cb_option` is 'b' for `checkout`, so the three
    // names are spelled `-b`, `-B` and `--orphan` here.
    if (new_branch_create.is_some() as u8)
        + (new_branch_force.is_some() as u8)
        + (orphan.is_some() as u8)
        > 1
    {
        crate::git_fatal!("options '-b', '-B', and '--orphan' cannot be used together");
    }
    // The three slots collapse into one only once the count above has run, in
    // git's own precedence: `if (new_branch_force) new_branch = new_branch_force;
    // if (new_orphan_branch) new_branch = new_orphan_branch;` (checkout.c:1957-1962).
    let new_branch = match (new_branch_create, new_branch_force) {
        (_, Some(name)) => Some((name, true)),
        (Some(name), None) => Some((name, false)),
        (None, None) => None,
    };

    // `if (opts->overlay_mode == 1 && opts->patch_mode)
    //     die(_("options '%s' and '%s' cannot be used together"), "-p", "--overlay");`
    // (builtin/checkout.c:1931). Only the *set* sense collides: `-p --no-overlay`
    // is a supported combination.
    if overlay_mode == Some(true) && patch_mode {
        crate::git_fatal!("options '-p' and '--overlay' cannot be used together");
    }

    // `-p`: `git checkout -p [<tree-ish>] [--] [<pathspec>...]` selects hunks to
    // restore into BOTH the index and the worktree (git's `ADD_P_CHECKOUT`). The
    // exact patch mode depends on the source: the index when no tree-ish is
    // given, `HEAD` verbatim, and any other tree-ish resolved to its hex oid —
    // `checkout_paths()` does the same substitution because `diff-index` cannot
    // take an `<a>...<b>` range.
    // `if (opts->pathspec_from_file) { … if (opts->patch_mode) die(…) }`
    // (builtin/checkout.c:2043) runs in `cmd_checkout()` itself, ahead of both
    // halves' own option gates, so it is checked before them here too.
    if patch_mode && pathspec_from_file.is_some() {
        crate::git_fatal!("options '--pathspec-from-file' and '--patch' cannot be used together");
    }

    // ```c
    // /* --track without -c/-C/-b/-B/--orphan should DWIM */
    // if (opts->track != BRANCH_TRACK_UNSPECIFIED && !opts->new_branch) {
    //         const char *argv0 = argv[0];
    //         if (!argc || !strcmp(argv0, "--"))
    //                 die(_("--track needs a branch name"));
    //         skip_prefix(argv0, "refs/", &argv0);
    //         skip_prefix(argv0, "remotes/", &argv0);
    //         argv0 = strchr(argv0, '/');
    //         if (!argv0 || !argv0[1])
    //                 die(_("missing branch name; try -%c"), cb_option);
    //         opts->new_branch = argv0 + 1;
    // }
    // ```
    // (builtin/checkout.c:1964-1975.) It runs in `cmd_checkout()` before either
    // half's option gates, so `--track` with no usable start-point is refused
    // ahead of everything, and the branch it derives is visible to the
    // `--detach`/pathspec checks below. `--no-track` reaches it too: git tests
    // `!= BRANCH_TRACK_UNSPECIFIED`, and `--no-track` is `BRANCH_TRACK_NEVER`.
    let new_branch = match new_branch {
        Some(nb) => Some(nb),
        // `opts->new_branch` has already absorbed `--orphan` by the time the DWIM
        // block runs (`if (opts->new_orphan_branch) opts->new_branch =
        // opts->new_orphan_branch;`, checkout.c:1961-1962, immediately above it),
        // so `--orphan` suppresses the DWIM and reaches `'--orphan' cannot be
        // used with '-t'` in `checkout_branch()` instead.
        None if track.is_some() && orphan.is_none() => {
            // `argv[0]` after parse-options, which keeps the `--` for `checkout`.
            let argv0 = if has_dashdash && pre.is_empty() {
                Some("--")
            } else {
                pre.first().copied()
            };
            let Some(argv0) = argv0 else {
                crate::git_fatal!("--track needs a branch name");
            };
            if argv0 == "--" {
                crate::git_fatal!("--track needs a branch name");
            }
            let stem = argv0.strip_prefix("refs/").unwrap_or(argv0);
            let stem = stem.strip_prefix("remotes/").unwrap_or(stem);
            match stem.split_once('/') {
                Some((_, rest)) if !rest.is_empty() => Some((rest.to_string(), false)),
                _ => crate::git_fatal!("missing branch name; try -b"),
            }
        }
        None => None,
    };

    // `opts->pathspec.nr != 0`: whatever `parse_branchname_arg()` left behind
    // once it took its 0-or-1 leading ref. It is the single switch git's own
    // `cmd_checkout()` tail hangs on — `checkout_paths()` when there is a
    // pathspec, `checkout_branch()` when there is not — and the two halves
    // refuse *different* option combinations, so the gates below need it too.
    //
    // With `--` the split is literal. Without one, the leading positional is the
    // ref whenever it names a branch, resolves as a rev, is the `HEAD`/`@`
    // spelling, or DWIMs to a unique remote branch; anything else is a pathspec,
    // as is every positional after a `-b`/`-B`/`--orphan`/`-t` start-point.
    let path_op = if pathspec_from_file.is_some() {
        true
    } else if has_dashdash {
        !post.is_empty()
    } else if new_branch.is_some() || orphan.is_some() || track == Some(true) {
        // The one positional those forms accept is their start-point.
        pre.len() > 1
    } else {
        match pre.len() {
            0 => false,
            1 => {
                let spec = pre[0];
                let is_ref = matches!(spec, "HEAD" | "@")
                    || repo
                        .try_find_reference(format!("refs/heads/{spec}").as_str())
                        .ok()
                        .flatten()
                        .is_some()
                    || crate::objname::resolve_quiet(&repo, spec).is_some()
                    || matches!(unique_remote_branch(&repo, spec), Ok(Dwim::One(_)));
                !is_ref
            }
            _ => true,
        }
    };

    // `opts->overlay_mode` resolved for the path forms: git's default is on
    // (`-1` behaves as overlay), and only `--no-overlay` turns it off.
    let overlay = overlay_mode.unwrap_or(true);

    if path_op {
        // `checkout_paths()`'s own refusals, in its order (builtin/checkout.c:530-551).
        if track.is_some() {
            crate::git_fatal!("'--track' cannot be used with updating paths");
        }
        if new_branch_log {
            crate::git_fatal!("'-l' cannot be used with updating paths");
        }
        if merge && patch_mode {
            crate::git_fatal!("options '--merge' and '--patch' cannot be used together");
        }
        // `if (opts->force_detach) die("git checkout: --detach does not take a
        // path argument '%s'")` (builtin/checkout.c:2031). Reported against the
        // first pathspec, and reached before `checkout_paths()`'s own
        // `'--detach' cannot be used with updating paths`.
        if detach {
            let first = if has_dashdash { post.first() } else { pre.first() };
            if let Some(first) = first {
                crate::git_fatal!("git checkout: --detach does not take a path argument '{first}'");
            }
        }
        // `if (1 < !!opts->writeout_stage + !!opts->force + !!opts->merge)`
        // (builtin/checkout.c:2054) — one message for all three pairings.
        if (writeout_stage.is_some() as u8) + (force as u8) + (merge as u8) > 1 {
            crate::git_fatal!(
                "git checkout: --ours/--theirs, --force and --merge are incompatible when\nchecking out of the index."
            );
        }
    } else {
        // `checkout_branch()`'s refusals (builtin/checkout.c:1667-1699).
        if patch_mode {
            crate::git_fatal!("'--patch' cannot be used with switching branches");
        }
        if overlay_mode.is_some() {
            crate::git_fatal!("'--[no]-overlay' cannot be used with switching branches");
        }
        if writeout_stage.is_some() {
            // `noop_switch`: no ref named, nothing created, no `--detach`.
            let noop_switch = pre.is_empty() && new_branch.is_none() && !detach;
            if noop_switch {
                crate::git_fatal!("'--ours/--theirs' needs the paths to check out");
            }
            crate::git_fatal!("'--ours/--theirs' cannot be used with switching branches");
        }
        if force && merge {
            crate::git_fatal!("'-f' cannot be used with '-m'");
        }
        // `opts->new_branch` here is the merged slot, so `--orphan` is covered.
        if detach && (new_branch.is_some() || orphan.is_some()) {
            crate::git_fatal!("'--detach' cannot be used with '-b/-B/--orphan'");
        }
        // `else if (opts->force_detach) { if (track != …UNSPECIFIED) die("'--detach'
        // cannot be used with '-t'") }` has no counterpart here: `--track` without
        // a created branch already died in the DWIM block above, and with one
        // `--detach` collides at `-b/-B/--orphan` first.
        if orphan.is_some() && track.is_some() {
            crate::git_fatal!("'--orphan' cannot be used with '-t'");
        }
    }


    if patch_mode {
        // Without `--`, a leading positional is the tree-ish only when it
        // resolves as a revision; otherwise every positional is a pathspec.
        let (rev, specs): (Option<&str>, &[&str]) = if has_dashdash {
            match pre.len() {
                0 => (None, post.as_slice()),
                1 => (Some(pre[0]), post.as_slice()),
                _ => crate::git_fatal!("only one <tree-ish> may precede `--`"),
            }
        } else if !pre.is_empty() && crate::objname::resolve_quiet(&repo, pre[0]).is_some() {
            (Some(pre[0]), &pre[1..])
        } else {
            (None, pre.as_slice())
        };
        let revision = match rev {
            None | Some("HEAD") => rev.map(str::to_string),
            Some(r) => {
                // `parse_branchname_arg()` runs before `--patch` hands off to
                // `add-interactive`, so a name that does not resolve, or resolves
                // to an id this repository has no tree for, is reported here —
                // not by the revision parser, whose error named a vendored
                // `src/ported/…` path and exited 1.
                let Some(id) = crate::objname::resolve(&repo, r) else {
                    crate::git_fatal!("invalid reference: {r}");
                };
                classify_tree_ish(&repo, id)?;
                Some(id.to_string())
            }
        };
        let specs: Vec<String> = specs.iter().map(|s| s.to_string()).collect();
        return super::add_patch::run(
            &repo,
            super::add_patch::Mode::Checkout,
            revision.as_deref(),
            patch_opts.to_interactive(false),
            &specs,
        );
    }

    // Resolve submodule recursion: explicit flag wins, else `submodule.recurse`.
    let recurse_submodules = recurse_submodules
        .unwrap_or_else(|| repo.config_snapshot().boolean("submodule.recurse") == Some(true));

    // `if (opts->source_tree && (opts->merge || opts->writeout_stage))`
    // (builtin/checkout.c): the three-way and stage-picking forms read the
    // *index*, so naming a tree to read from instead is refused before anything
    // is resolved. `opts->source_tree` is set only when a tree-ish operand
    // precedes pathspecs, which is exactly this shape.
    let source_tree_with_paths = if has_dashdash {
        pre.len() == 1 && !post.is_empty()
    } else {
        pre.len() > 1 && crate::objname::resolve_quiet(&repo, pre[0]).is_some()
    };
    if source_tree_with_paths
        && new_branch.is_none()
        && orphan.is_none()
        && (merge || writeout_stage.is_some())
    {
        crate::git_fatal!(
            "'--merge', '--ours', or '--theirs' cannot be used when checking out of a tree"
        );
    }

    // --- Dispatch -----------------------------------------------------------
    // `--pathspec-from-file`: pathspecs come from the file (or stdin for `-`),
    // never the command line. A single positional may still precede them as the
    // `<tree-ish>` source; anything else is git's incompatibility error.
    if let Some(file) = pathspec_from_file {
        if has_dashdash || !post.is_empty() {
            crate::git_fatal!("--pathspec-from-file is incompatible with pathspec arguments");
        }
        if new_branch.is_some() || orphan.is_some() || writeout_stage.is_some() {
            crate::git_fatal!("--pathspec-from-file cannot be combined with branch creation or --ours/--theirs");
        }
        let specs = super::commit::read_pathspec_file(&file, pathspec_file_nul)?;
        let refs: Vec<&str> = specs.iter().map(String::as_str).collect();
        return match pre.len() {
            0 => restore_from_index(&repo, &refs, false, quiet, merge_opt(merge, &conflict_style, ""), force),
            // `--pathspec-from-file` rejects a `--` above, so this is the bare
            // form: stock reports `Updated N paths from <tree>` here.
            1 => restore_from_tree(&repo, pre[0], &refs, overlay, true, quiet),
            _ => crate::git_fatal!("only one <tree-ish> may precede pathspecs"),
        };
    }

    // `--orphan <name> [<start>]`: start an unborn branch off `<start>`'s tree.
    if let Some(name) = orphan {
        let start = pre.first().copied().unwrap_or("HEAD");
        return orphan_checkout(&repo, &name, start, quiet, force);
    }

    // `--ours`/`--theirs <path>…`: write one conflict side into the worktree.
    if let Some(stage) = writeout_stage {
        let paths = if has_dashdash { &post } else { &pre };
        if paths.is_empty() {
            eprintln!("fatal: '--ours/--theirs' needs the paths to check out");
            return Ok(ExitCode::from(128));
        }
        return restore_conflict_stage(&repo, paths, stage, !has_dashdash, quiet);
    }

    if let Some((name, reset)) = new_branch {
        // A bare `--` introduces no pathspec at all — `git checkout -B main origin/main --`
        // is a plain branch creation, which is how the JetBrains client spells it. Only a
        // path *after* the separator (or before it, without one) is a path restore.
        // `parse_branchname_arg()` takes the leading operand as the start-point
        // only when it resolves; whatever is left is `opts->pathspec`.
        let start_resolved = pre
            .first()
            .map(|p| crate::objname::resolve_quiet(&repo, p).is_some())
            .unwrap_or(false);
        let remaining: &[&str] = if has_dashdash {
            &post
        } else if start_resolved {
            &pre[1..]
        } else {
            &pre
        };
        if !remaining.is_empty() {
            // ```c
            // /* Try to give more helpful suggestion. new_branch && argc > 1 will be caught later. */
            // if (opts->new_branch && argc == 1 && !new_branch_info.commit)
            //         die(_("'%s' is not a commit and a branch '%s' cannot be created from it"),
            //             argv[0], opts->new_branch);
            // ```
            // (builtin/checkout.c:2024-2027, then `checkout_paths()`'s
            // `die(_("Cannot update paths and switch to branch '%s' at the same
            // time."), opts->new_branch)` at :551.) The friendlier wording needs
            // *both* conditions: exactly one operand left, and no start-point
            // resolved for it to have been. `git checkout -b o master -- f.txt`
            // has a start-point, so it gets the blunt one.
            if remaining.len() == 1 && !start_resolved {
                crate::git_fatal!(
                    "'{}' is not a commit and a branch '{name}' cannot be created from it",
                    remaining[0]
                );
            }
            eprintln!(
                "fatal: Cannot update paths and switch to branch '{name}' at the same time."
            );
            return Ok(ExitCode::from(128));
        }
        let start = pre.first().copied().unwrap_or("HEAD");
        // On an UNBORN HEAD (a fresh `git init`, or a clone of an empty repo)
        // there is no commit for the default start-point to resolve to, and git
        // still succeeds: `-b`/`-B` with no explicit start just re-points the
        // unborn HEAD at the new name. Resolving "HEAD" here instead would fail
        // with a rev-spec parse error and make `checkout -B main` unusable in an
        // empty repository.
        if pre.is_empty() && repo.head()?.is_unborn() {
            let full: gix::refs::FullName = format!("refs/heads/{name}").try_into()?;
            repo.edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!("checkout: moving to {name}").into(),
                    },
                    expected: PreviousValue::Any,
                    new: Target::Symbolic(full),
                },
                name: "HEAD".try_into()?,
                deref: false,
            })?;
            if !quiet {
                eprintln!("Switched to a new branch '{name}'");
            }
            return Ok(ExitCode::SUCCESS);
        }
        return create_and_switch(
            &repo,
            &name,
            reset,
            start,
            quiet,
            track,
            !only_merge_on_switching_branches,
            merge_opt(merge, &conflict_style, &name),
        );
    }

    // `-t`/`--no-track` without `-b`/`-B`/`--orphan` no longer reaches here: the
    // DWIM block above has already turned it into a branch creation, or refused.

    if has_dashdash {
        if post.is_empty() {
            crate::git_fatal!("you must specify path(s) to restore");
        }
        return match pre.len() {
            0 => restore_from_index(&repo, &post, false, quiet, merge_opt(merge, &conflict_style, ""), force),
            // Reached only under `has_dashdash`, and stock stays silent for the
            // `--` form even though it updates the same paths.
            1 => restore_from_tree(&repo, pre[0], &post, overlay, false, quiet),
            _ => crate::git_fatal!("only one <tree-ish> may precede `--`"),
        };
    }

    // No `--`, no -b/-B.
    if pre.is_empty() {
        // `git checkout --detach` with no revision detaches at the CURRENT HEAD:
        // git resolves the missing argument to HEAD rather than erroring
        // (builtin/checkout.c, `opts->force_detach && !argc`). The worktree is
        // already at that commit, so this is a ref-only move.
        if detach {
            let head = repo
                .head_id()
                .map_err(|_| anyhow::anyhow!("you are on a branch yet to be born"))?
                .detach();
            let commit = head.attach(&repo).object()?.peel_to_commit()?;
            let code = detached_checkout(&repo, "HEAD", commit, quiet, true, force, merge_opt(merge, &conflict_style, "HEAD"))?;
            maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
            return Ok(code);
        }
        // `git checkout` with neither a ref nor a pathspec is not an error:
        // `switch_branches()` names the missing branch "HEAD", takes the current
        // commit, and `update_refs_for_switch()` then does nothing to any ref.
        // Only the worktree reconciliation happens.
        let code = checkout_head_in_place(&repo, quiet, force)?;
        maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
        return Ok(code);
    }

    // Single positional: prefer ref interpretation (branch → switch; else rev →
    // detach); fall back to a bare path restore from the index.
    if pre.len() == 1 {
        let spec = pre[0];
        // `parse_branchname_arg()` resolves through `get_oid_mb()`, so
        // `get_oid_basic()`'s `core.warnAmbiguousRefs` warning is emitted here,
        // ahead of anything the checkout itself prints. `--quiet` does not
        // suppress it — stock warns under `git checkout -q` too.
        super::rev_parse::warn_ambiguous_refname(&repo, spec, false);
        // A revspec like `HEAD~3` is not a valid ref *name* (`~` is rejected by
        // ref validation), so treat a lookup error as "not a branch" and let the
        // `rev_parse_single` path below resolve and detach-checkout it.
        let is_branch = repo
            .try_find_reference(format!("refs/heads/{spec}").as_str())
            .ok()
            .flatten()
            .is_some();
        if is_branch && !detach {
            let code =
                switch_to_branch_opts(&repo, spec, quiet, force, None, merge_opt(merge, &conflict_style, spec))?;
            maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
            return Ok(code);
        }
        // `git checkout HEAD` (and its `@` spelling) is not a detach: with no
        // `refs/heads/HEAD` to resolve, `new_branch_info->path` stays NULL while
        // its name is "HEAD", and `update_refs_for_switch()`'s first arm leaves
        // every ref alone. Only the worktree reconciliation runs.
        if !detach && matches!(spec, "HEAD" | "@") && !is_branch {
            let code = checkout_head_in_place(&repo, quiet, force)?;
            maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
            return Ok(code);
        }
        // `get_oid_mb()`, not a bare `rev_parse_single()`: a full-length hex name
        // *is* the object id, decoded without consulting the odb, so an id this
        // repository does not have still resolves here and reaches the classifier
        // below — which reports the missing object the way git does. Resolving it
        // through the odb instead made `git checkout <sha-from-an-email>` fall
        // through to the pathspec branch and report "did not match any file(s)".
        // Quiet: `parse_branchname_arg()` put this same operand through
        // `get_oid_mb()` above, which is the one resolution git warns for. This is
        // that resolution's result being used, not a second visit to
        // `get_oid_basic()` — warning again made `git checkout --detach <ambiguous>`
        // print two `refname … is ambiguous` lines where git prints one.
        if let Some(id) = crate::objname::resolve_quiet(&repo, spec) {
            let commit = match classify_tree_ish(&repo, id)? {
                TreeIsh::Commit(commit) => commit,
                // A tree is a legitimate `source_tree`, so `parse_branchname_arg()`
                // accepts it; with no paths to restore from it, `checkout_branch()`
                // is what refuses, by name.
                TreeIsh::Tree(_) => {
                    crate::git_fatal!("Cannot switch branch to a non-commit '{spec}'")
                }
            };
            let code =
                detached_checkout(&repo, spec, commit, quiet, detach, force, merge_opt(merge, &conflict_style, spec))?;
            maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
            return Ok(code);
        }
        // DWIM (`--guess`, default on via `checkout.guess`): a bare name that is
        // not a local ref and does not resolve as a rev, but names a branch on
        // exactly one remote, becomes `-b <name> --track <remote>/<name>` — git's
        // `dwim_new_local_branch` path in `builtin/checkout.c`.
        if !detach {
            let guess = guess_flag.unwrap_or_else(|| {
                repo.config_snapshot().boolean("checkout.guess") != Some(false)
            });
            if guess {
                match unique_remote_branch(&repo, spec)? {
                    Dwim::One(remote_short) => {
                        let code =
                            create_and_switch(
                            &repo, spec, false, &remote_short, quiet, Some(true), true,
                            merge_opt(merge, &conflict_style, spec),
                        )?;
                        maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
                        return Ok(code);
                    }
                    Dwim::Many { count } => {
                        crate::advice::ambiguous_remote_branch_name(&repo, "checkout");
                        eprintln!(
                            "fatal: '{spec}' matched multiple ({count}) remote tracking branches"
                        );
                        return Ok(ExitCode::from(128));
                    }
                    Dwim::None => {}
                }
            }
        }
        // Not a ref/rev — treat as a path restore from the index (bare form).
        return restore_from_index(&repo, &pre, true, quiet, merge_opt(merge, &conflict_style, ""), force);
    }

    // Multiple positionals, no `--`: if the first resolves to a tree-ish it is the
    // source and the rest are paths; otherwise all are paths from the index.
    if crate::objname::resolve_quiet(&repo, pre[0]).is_some() {
        return restore_from_tree(&repo, pre[0], &pre[1..], overlay, true, quiet);
    }
    restore_from_index(&repo, &pre, true, quiet, merge_opt(merge, &conflict_style, ""), force)
}

/// What an object name means to the checkout family once it has been resolved
/// to an id — the two outcomes `parse_branchname_arg()` distinguishes.
pub(super) enum TreeIsh<'repo> {
    /// `lookup_commit_reference_gently()` found a commit; an annotated tag peels
    /// through to the commit it points at.
    Commit(gix::Commit<'repo>),
    /// Not a commit, but `parse_tree_indirect()` reached a tree. git keeps this
    /// as `source_tree`: it can be restored *from*, but not switched *to*.
    Tree(ObjectId),
}

impl TreeIsh<'_> {
    /// `*source_tree` as `parse_branchname_arg()` sets it: a commit contributes
    /// `repo_get_commit_tree()`, a tree is already one.
    pub(super) fn source_tree(self) -> Result<ObjectId> {
        Ok(match self {
            TreeIsh::Commit(commit) => commit.tree_id()?.detach(),
            TreeIsh::Tree(id) => id,
        })
    }
}

/// `parse_branchname_arg()`'s tail: classify an id the way `git checkout`,
/// `git switch` and `git restore` all classify one.
///
/// ```c
/// new_branch_info->commit = lookup_commit_reference_gently(the_repository, rev, 1);
/// if (!new_branch_info->commit) {
///         *source_tree = parse_tree_indirect(rev);
///         if (!*source_tree)
///                 die(_("unable to read tree (%s)"), oid_to_hex(rev));
/// }
/// ```
///
/// This is where a *missing* object is finally noticed. `get_oid_basic()` hands
/// back a full-length hex name without ever asking the odb whether the object
/// exists, so every command in this family resolves an absent id successfully
/// and then dies here — which is why `unable to read tree` is what stock prints
/// for `git checkout <sha-that-is-not-in-this-repo>`, and why an id naming a
/// blob prints exactly the same thing.
///
/// The message renders `oid_to_hex(rev)`, the *decoded* id rather than the
/// spelling, so an uppercase name is echoed back lowercase.
pub(super) fn classify_tree_ish(repo: &gix::Repository, id: ObjectId) -> Result<TreeIsh<'_>> {
    let Ok(object) = repo.find_object(id) else {
        crate::git_fatal!("unable to read tree ({id})");
    };
    if let Ok(commit) = object.clone().peel_to_commit() {
        return Ok(TreeIsh::Commit(commit));
    }
    match object.peel_to_tree() {
        Ok(tree) => Ok(TreeIsh::Tree(tree.id)),
        Err(_) => crate::git_fatal!("unable to read tree ({id})"),
    }
}

/// After a switch that changed `HEAD`, move each active, initialized submodule's
/// worktree to the commit the superproject now records for it — git's
/// `--recurse-submodules` worktree updater (`submodule_move_head` in submodule.c).
///
/// Implemented by re-executing this binary's own `checkout <gitlink>` inside each
/// submodule, so the move stays faithful (dirty-worktree refusal, nested
/// recursion) without duplicating checkout logic. Uninitialized submodules (no
/// repo of their own) are skipped, exactly like git.
pub(super) fn maybe_recurse_submodules(
    repo: &gix::Repository,
    recurse: bool,
    quiet: bool,
) -> Result<()> {
    if !recurse {
        return Ok(());
    }
    // Re-open the repository so the index reflects the checkout that just ran: the
    // `repo` handed in was opened before the worktree/index update and its cached
    // index snapshot still records the OLD submodule gitlinks, which would make
    // `index_id()` match `head_id()` and skip every move.
    let repo = gix::open(repo.git_dir())?;
    let Some(subs) = repo.submodules()? else {
        return Ok(());
    };
    let Some(workdir) = repo.workdir() else {
        return Ok(());
    };
    let exe = std::env::current_exe()?;
    for sm in subs {
        if !sm.is_active().unwrap_or(false) {
            continue;
        }
        // The gitlink the just-checked-out superproject index records for this path
        // (both `index_id` and gix's `head_id` are the *superproject's* view — they
        // match after a checkout, so they can't tell us whether the submodule needs
        // moving; the submodule's actual worktree HEAD is what to compare against).
        let Ok(Some(target)) = sm.index_id() else {
            continue;
        };
        // Only recurse into an initialized submodule (one with its own repo).
        let Some(sub_repo) = sm.open().ok().flatten() else {
            continue;
        };
        // The submodule's ACTUAL checked-out commit. Already at the recorded gitlink
        // → nothing to move.
        let actual = sub_repo.head_id().ok().map(|id| id.detach());
        if actual == Some(target) {
            continue;
        }
        let Ok(rel) = sm.path() else {
            continue;
        };
        let sub_path = workdir.join(gix::path::from_bstr(rel.as_bstr()));
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("-C")
            .arg(&sub_path)
            .arg("checkout")
            .arg("--recurse-submodules");
        if quiet {
            cmd.arg("-q");
        }
        cmd.arg(target.to_string());
        // `start_command()`'s `fflush(NULL)` (run-command.c:743).
        crate::cstdio::before_spawn();
        let _ = cmd.status();
    }
    Ok(())
}

/// git's "nothing to do" ref case: `new_branch_info->name` is "HEAD" and there is
/// no branch path to move to, so `update_refs_for_switch()` touches no ref and
/// prints nothing. Reached by `git checkout` with no arguments and by
/// `git checkout HEAD`.
///
/// Only `merge_working_tree()` has an effect: forced, it resets the worktree and
/// index to `HEAD`'s tree; unforced, the 2-way merge against an identical tree is
/// a no-op and all that remains is the local-changes listing.
fn checkout_head_in_place(repo: &gix::Repository, quiet: bool, force: bool) -> Result<ExitCode> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    if force {
        let tree = repo
            .head_tree_id()
            .map_err(|_| anyhow!("You are on a branch yet to be born"))?
            .detach();
        reset_worktree_to_tree(repo, tree)?;
    } else {
        show_local_changes("HEAD", quiet)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Switch `HEAD` to an existing local branch `spec`, updating the worktree when
/// the target tree differs from the current one.
///
/// `force` is git's `opts->discard_changes`: it replaces the clean-worktree
/// requirement with `reset_tree()`, which is also why an already-current branch
/// still does work — `merge_working_tree()` runs before
/// `update_refs_for_switch()` decides there is no ref to move.
/// Shared with `rebase <upstream> <branch>`, which checks the branch out before
/// replaying onto it — quietly, since git's rebase prints no switch message.
pub(crate) fn switch_to_branch(
    repo: &gix::Repository,
    spec: &str,
    quiet: bool,
    force: bool,
    // The reflog line to write instead of `checkout: moving from <a> to <b>`. git's
    // rebase passes `<reflog action>: checkout <branch>` here, which is what its
    // `options.switch_to` checkout records.
    reflog_message: Option<&str>,
) -> Result<ExitCode> {
    switch_to_branch_opts(repo, spec, quiet, force, reflog_message, None)
}

/// [`switch_to_branch`] with `opts->merge` made explicit — the spelling
/// `git checkout -m <branch>` reaches, and the only one that can stash.
pub(crate) fn switch_to_branch_opts(
    repo: &gix::Repository,
    spec: &str,
    quiet: bool,
    force: bool,
    reflog_message: Option<&str>,
    merge: Option<MergeOpt<'_>>,
) -> Result<ExitCode> {
    // Already on it → the branch `HEAD` points at does not change, but git still
    // goes through `refs_update_symref("HEAD", ...)`, so the move is reflogged
    // ("checkout: moving from main to main") before "Already on 'x'" is printed.
    if let Some(cur) = repo.head_name()? {
        if cur.shorten() == spec {
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            let head_id = repo.head_id().ok().map(|id| id.detach());
            if force {
                let tree = head_tree_or_empty(repo)?;
                reset_worktree_to_tree(repo, tree)?;
            } else {
                // `switch_branch_doing_nothing_is_ok`: the switch is a no-op, but
                // `merge_working_tree()` still runs, so the local changes are
                // still listed — before `Already on '<branch>'`.
                show_local_changes(spec, quiet)?;
            }
            let branch_full: FullName = format!("refs/heads/{spec}")
                .try_into()
                .map_err(|e| anyhow!("invalid branch name '{spec}': {e}"))?;
            set_head_symbolic(
                repo,
                branch_full,
                reflog_message.unwrap_or(&format!("checkout: moving from {spec} to {spec}")),
                head_id,
                head_id,
            )?;
            if !quiet {
                eprintln!("Already on '{spec}'");
            }
            return Ok(ExitCode::SUCCESS);
        }
    }

    let commit = repo.rev_parse_single(spec)?.object()?.peel_to_commit()?;
    let target_tree = commit.tree_id()?.detach();

    let head = repo.head()?;
    let old_detached = head.is_detached();
    // `orphaned_commit_warning()` is gated on `old_branch_info.commit`
    // (checkout.c:1252), so a `HEAD` that peels to no commit reports nothing —
    // and, in particular, is never handed to `describe()`, which would fail on it.
    let old_commit = peeled_head_commit(repo, &head);
    let old_id = head.id().map(|i| i.detach());
    let old_label = head_label(repo, &head);
    let cur_tree = head_tree_or_empty(repo)?;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut autostashed = false;
    if force {
        reset_worktree_to_tree(repo, target_tree)?;
    } else if target_tree != cur_tree {
        match move_worktree(repo, cur_tree, target_tree, merge)? {
            Moved::Refused(code) => return Ok(code),
            Moved::Autostashed => autostashed = true,
            Moved::Clean => {}
        }
    }
    // The tail of `merge_working_tree()`: `!opts->discard_changes && !opts->quiet`.
    // An autostashed switch skips it — its listing is the headed one below.
    if !force && !autostashed {
        show_local_changes(&commit.id.to_string(), quiet)?;
    }

    let branch_full: FullName = format!("refs/heads/{spec}")
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{spec}': {e}"))?;
    set_head_symbolic(
        repo,
        branch_full,
        reflog_message.unwrap_or(&format!("checkout: moving from {old_label} to {spec}")),
        old_id,
        Some(commit.id),
    )?;

    if autostashed && !quiet {
        println!("The following paths have local changes:");
        show_local_changes(&commit.id.to_string(), quiet)?;
    }

    if !quiet {
        // git only reports the abandoned detached position when it actually
        // moves (checkout.c: `!old->path && old->commit != new->commit`).
        if old_detached {
            if let Some(id) = old_commit.filter(|id| *id != commit.id) {
                let (abbrev, summary) = describe(repo, id)?;
                eprintln!("Previous HEAD position was {abbrev} {summary}");
            }
        }
        eprintln!("Switched to branch '{spec}'");
        // `report_tracking()`: the ahead/behind summary for a branch with an upstream,
        // the same block `status` prints under its header.
        print_tracking_status(repo);
    }
    Ok(ExitCode::SUCCESS)
}


/// Detach `HEAD` at `commit`, updating the worktree when the target tree differs.
/// `force_detach` is true for an explicit `--detach`, which suppresses the
/// `advice.detachedHead` block just as git's `opts->force_detach` does.
fn detached_checkout(
    repo: &gix::Repository,
    spec: &str,
    commit: gix::Commit<'_>,
    quiet: bool,
    force_detach: bool,
    force: bool,
    merge: Option<MergeOpt<'_>>,
) -> Result<ExitCode> {
    let target_id = commit.id;
    let target_tree = commit.tree_id()?.detach();

    let head = repo.head()?;
    let old_detached = head.is_detached();
    // `orphaned_commit_warning()` is gated on `old_branch_info.commit`
    // (checkout.c:1252), so a `HEAD` that peels to no commit reports nothing —
    // and, in particular, is never handed to `describe()`, which would fail on it.
    let old_commit = peeled_head_commit(repo, &head);
    let old_id = head.id().map(|i| i.detach());
    let old_label = head_label(repo, &head);
    let cur_tree = head_tree_or_empty(repo)?;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut autostashed = false;
    if force {
        reset_worktree_to_tree(repo, target_tree)?;
    } else {
        if target_tree != cur_tree {
            match move_worktree(repo, cur_tree, target_tree, merge)? {
                Moved::Refused(code) => return Ok(code),
                Moved::Autostashed => autostashed = true,
                Moved::Clean => {}
            }
        }
        if !autostashed {
            show_local_changes(&target_id.to_string(), quiet)?;
        }
    }

    set_head_detached(
        repo,
        target_id,
        &format!("checkout: moving from {old_label} to {spec}"),
        old_id,
    )?;

    if autostashed && !quiet {
        println!("The following paths have local changes:");
        show_local_changes(&target_id.to_string(), quiet)?;
    }

    if !quiet {
        if old_detached {
            if let (Some(old), true) = (old_commit, old_commit != Some(target_id)) {
                let (abbrev, summary) = describe(repo, old)?;
                eprintln!("Previous HEAD position was {abbrev} {summary}");
            }
        } else if !force_detach
            && repo.config_snapshot().boolean("advice.detachedHead") != Some(false)
        {
            // Leaving an attached HEAD without an explicit --detach: git warns.
            print_detached_head_advice(spec);
        }
        let (abbrev, summary) = describe(repo, target_id)?;
        eprintln!("HEAD is now at {abbrev} {summary}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The `advice.detachedHead` block git prints when a bare `git checkout <commit>`
/// moves off a branch, verbatim (git 2.55.0, `builtin/checkout.c`).
fn print_detached_head_advice(spec: &str) {
    eprintln!("Note: switching to '{spec}'.\n");
    eprintln!(
        "You are in 'detached HEAD' state. You can look around, make experimental\n\
         changes and commit them, and you can discard any commits you make in this\n\
         state without impacting any branches by switching back to a branch.\n\
         \n\
         If you want to create a new branch to retain commits you create, you may\n\
         do so (now or later) by using -c with the switch command. Example:\n\
         \n\
         \x20 git switch -c <new-branch-name>\n\
         \n\
         Or undo this operation with:\n\
         \n\
         \x20 git switch -\n\
         \n\
         Turn off this advice by setting config variable advice.detachedHead to false\n"
    );
}

/// Create (`-b`) or create-or-reset (`-B`) `refs/heads/<name>` at `start`, then
/// switch `HEAD` to it, updating the worktree when the tree changes.
fn create_and_switch(
    repo: &gix::Repository,
    name: &str,
    reset: bool,
    start: &str,
    quiet: bool,
    track: Option<bool>,
    merge_worktree: bool,
    merge: Option<MergeOpt<'_>>,
) -> Result<ExitCode> {
    let full = format!("refs/heads/{name}");
    if !super::branch::valid_branch_name(name) {
        // `validate_branchname()` pairs the die with the refSyntax advice:
        //
        //     int code = die_message(_("'%s' is not a valid branch name"), name);
        //     advise_if_enabled(ADVICE_REF_SYNTAX, _("See 'git help check-ref-format'"));
        //     exit(code);
        eprintln!("fatal: '{name}' is not a valid branch name");
        crate::advice::Advice::RefSyntax.advise_in(repo, "See 'git help check-ref-format'");
        return Ok(ExitCode::from(128));
    }

    // `-t`: resolve the upstream before any mutation, so a bad start-point fails
    // exactly like git — branch untouched, HEAD unmoved. Without an explicit flag
    // `setup_tracking()` consults `branch.autoSetupMerge`, whose default (`true`) sets
    // the upstream whenever the start point is a remote-tracking branch.
    let track_info = match track {
        Some(true) => match resolve_tracking(repo, start)? {
            Some(info) => Some(info),
            None => {
                eprintln!(
                    "fatal: cannot set up tracking information; starting point '{start}' is not a branch"
                );
                return Ok(ExitCode::from(128));
            }
        },
        Some(false) => None,
        None => auto_tracking(repo, name, start)?,
    };

    // `parse_branchname_arg()` classifies the start-point before `create_branch()`
    // gets it: an id this repository does not have — which a full-length hex name
    // resolves to without the odb ever being asked — is `unable to read tree`, and
    // a tree is the family's non-commit refusal. Only a name that resolves to
    // nothing at all reaches `create_branch()`'s own wording.
    let Some(start_oid) = crate::objname::resolve(repo, start) else {
        crate::git_fatal!(
            "'{start}' is not a commit and a branch '{name}' cannot be created from it"
        );
    };
    let commit = match classify_tree_ish(repo, start_oid)? {
        TreeIsh::Commit(commit) => commit,
        TreeIsh::Tree(_) => crate::git_fatal!("Cannot switch branch to a non-commit '{start}'"),
    };

    // `create_branch()` hands the start-point to `dwim_branch_start()`
    // (branch.c:539-594), which resolves it a *second* time and then DWIMs it —
    // so the name reaches `get_oid_basic()` twice and warns twice, and more than
    // one matching ref is fatal before anything is created:
    //
    // ```c
    // if (repo_get_oid_mb(r, start_name, &oid)) { … die(_("not a valid object name: '%s'"), start_name); }
    //
    // switch (repo_dwim_ref(r, start_name, strlen(start_name), &oid, &real_ref, 0)) {
    // case 0: … break;
    // case 1: … break;
    // default:
    //         die(_("ambiguous object name: '%s'"), start_name);
    // }
    // ```
    crate::objname::warn_ambiguous_refname(repo, start);
    if super::rev_parse::dwim_ref_matches(repo, start).len() > 1 {
        crate::git_fatal!("ambiguous object name: '{start}'");
    }

    let start_id = commit.id;
    let target_tree = commit.tree_id()?.detach();

    let head = repo.head()?;
    let old_detached = head.is_detached();
    // `orphaned_commit_warning()` is gated on `old_branch_info.commit`
    // (checkout.c:1252), so a `HEAD` that peels to no commit reports nothing —
    // and, in particular, is never handed to `describe()`, which would fail on it.
    let old_commit = peeled_head_commit(repo, &head);
    let old_id = head.id().map(|i| i.detach());
    let old_label = head_label(repo, &head);
    // Whether HEAD is already attached to the branch we're (re)creating.
    let already_on = head
        .referent_name()
        .map(|n| n.shorten() == name)
        .unwrap_or(false);
    let cur_tree = head_tree_or_empty(repo)?;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let existed = repo.try_find_reference(full.as_str())?.is_some();
    if existed && !reset {
        crate::git_fatal!("a branch named '{name}' already exists");
    }

    let mut autostashed = false;
    if target_tree != cur_tree {
        match move_worktree(repo, cur_tree, target_tree, merge)? {
            Moved::Refused(code) => return Ok(code),
            Moved::Autostashed => autostashed = true,
            Moved::Clean => {}
        }
    }
    // `merge_working_tree()` ends here, and its last act is the listing of the
    // local changes carried onto the new branch — before `update_refs_for_switch()`
    // announces the switch. `only_merge_on_switching_branches` skips the whole
    // function, listing included.
    if merge_worktree && !autostashed {
        show_local_changes(&start_id.to_string(), quiet)?;
    }

    let branch_full: FullName = full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{name}': {e}"))?;
    // Create fresh, or force-move an existing branch for -B.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("branch: Created from {start}").into(),
            },
            expected: if existed {
                PreviousValue::Any
            } else {
                PreviousValue::MustNotExist
            },
            new: Target::Object(start_id),
        },
        name: branch_full.clone(),
        deref: false,
    })?;
    set_head_symbolic(
        repo,
        branch_full,
        &format!("checkout: moving from {old_label} to {name}"),
        old_id,
        Some(start_id),
    )?;

    // `-t`: persist branch.<name>.remote / .merge (lock already held above; the
    // per-thread RepoLock is reentrant, so config.rs-style locking isn't needed).
    if let Some(info) = &track_info {
        write_tracking_config(repo, name, info)?;
    }

    if !quiet {
        // Reset-in-place (-B on the current branch) prints only "Reset branch".
        if existed && already_on {
            eprintln!("Reset branch '{name}'");
        } else {
            if old_detached {
                if let Some(id) = old_commit.filter(|id| *id != start_id) {
                    let (abbrev, summary) = describe(repo, id)?;
                    eprintln!("Previous HEAD position was {abbrev} {summary}");
                }
            }
            if existed {
                eprintln!("Switched to and reset branch '{name}'");
            } else {
                eprintln!("Switched to a new branch '{name}'");
            }
        }
        // git prints the tracking confirmation to stdout, after the stderr
        // transition line, and only when not quiet.
        if let Some(info) = &track_info {
            println!("branch '{name}' set up to track '{}'.", info.display);
        }
        // `report_tracking()` follows for a branch that already existed; a brand-new
        // one has nothing to report beyond the upstream just configured.
        if existed {
            print_tracking_status(repo);
        }
    }
    if autostashed && !quiet {
        println!("The following paths have local changes:");
        show_local_changes(&start_id.to_string(), quiet)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `git checkout --orphan <name> [<start>]`: point `HEAD` at an unborn branch
/// `<name>` whose worktree/index come from `<start>`'s tree. The ref is not
/// created (git materializes it only at the first commit) and no reflog entry is
/// written, matching stock git.
fn orphan_checkout(
    repo: &gix::Repository,
    name: &str,
    start: &str,
    quiet: bool,
    // `opts->discard_changes`. `merge_working_tree()` routes a forced checkout
    // through `reset_tree()` instead of the two-way merge, so local changes are
    // thrown away rather than carried — and its closing listing is skipped.
    force: bool,
) -> Result<ExitCode> {
    // git resolves the start-point before anything else: a bad one aborts here.
    // Resolution is `get_oid_mb()`'s, so a full-length hex name is the id itself
    // and a missing object is `parse_branchname_arg()`'s `unable to read tree`
    // rather than this function's wording.
    let commit = match crate::objname::resolve(repo, start) {
        Some(id) => match classify_tree_ish(repo, id)? {
            TreeIsh::Commit(commit) => commit,
            TreeIsh::Tree(_) => {
                crate::git_fatal!("Cannot switch branch to a non-commit '{start}'")
            }
        },
        None => {
            eprintln!(
                "fatal: '{start}' is not a commit and a branch '{name}' cannot be created from it"
            );
            return Ok(ExitCode::from(128));
        }
    };

    let full = format!("refs/heads/{name}");
    if !super::branch::valid_branch_name(name) {
        eprintln!("fatal: '{name}' is not a valid branch name");
        crate::advice::Advice::RefSyntax.advise_in(repo, "See 'git help check-ref-format'");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if repo.try_find_reference(full.as_str())?.is_some() {
        eprintln!("fatal: a branch named '{name}' already exists");
        return Ok(ExitCode::from(128));
    }

    let start_commit = commit.id;
    let target_tree = commit.tree_id()?.detach();
    let cur_tree = head_tree_or_empty(repo)?;
    if force {
        reset_worktree_to_tree(repo, target_tree)?;
    } else if target_tree != cur_tree {
        if let Some(code) = ensure_clean(repo, cur_tree, target_tree)? {
            return Ok(code);
        }
        update_worktree_to_tree(repo, cur_tree, target_tree)?;
    }

    // The tail of `merge_working_tree()`, which `--orphan` runs like every other
    // switch: `if (!opts->discard_changes && !opts->quiet && new_branch_info->commit)
    // show_local_changes(&new_branch_info->commit->object, &opts->diff_options);`
    // (builtin/checkout.c:930-931). `git checkout --orphan` keeps its start-point
    // commit — only `git switch --orphan`, whose `orphan_from_empty_tree` leaves
    // `new_branch_info->commit` NULL, prints nothing — and the call is outside
    // the two-way merge, so it runs even when the trees were identical and
    // nothing moved.
    if !force {
        show_local_changes(&start_commit.to_string(), quiet)?;
    }

    // Write HEAD as a plain symref to the (not-yet-existing) branch. No ref is
    // created and no reflog line is appended — git's exact orphan behavior.
    let head_path = repo.git_dir().join("HEAD");
    std::fs::write(&head_path, format!("ref: {full}\n"))?;

    if !quiet {
        eprintln!("Switched to a new branch '{name}'");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git checkout --ours|--theirs <path>…`: write one side of a conflict into the
/// worktree. `stage` is 2 (ours) or 3 (theirs). A non-conflicted path falls back
/// to its stage-0 blob; a conflicted path missing the requested side errors with
/// git's `path 'X' does not have our/their version` and exits 1. The index is
/// left untouched (a conflicted path stays conflicted).
fn restore_conflict_stage(
    repo: &gix::Repository,
    paths: &[&str],
    stage: u8,
    bare: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let index = repo.open_index()?;
    let matched = match match_paths(&index, paths) {
        Ok(m) => m,
        Err(spec) => {
            eprintln!("error: pathspec '{spec}' did not match any file(s) known to git");
            return Ok(ExitCode::from(1));
        }
    };
    let mset: HashSet<BString> = matched.iter().cloned().collect();

    // Which stages each matched path carries (index 0..=3).
    let mut have: HashMap<BString, [bool; 4]> = HashMap::new();
    {
        let backing = index.path_backing();
        for e in index.entries() {
            let p = e.path_in(backing).to_owned();
            if mset.contains(&p) {
                let s = (e.stage_raw() as usize).min(3);
                have.entry(p).or_insert([false; 4])[s] = true;
            }
        }
    }

    let side = if stage == 2 { "our" } else { "their" };
    let mut keep: HashMap<BString, u32> = HashMap::new();
    let mut had_error = false;
    for p in &matched {
        let flags = have.get(p).copied().unwrap_or([false; 4]);
        let chosen = if flags[0] {
            Some(0u32) // not conflicted → the single indexed blob
        } else if flags[stage as usize] {
            Some(stage as u32)
        } else {
            None
        };
        match chosen {
            Some(st) => {
                keep.insert(p.clone(), st);
            }
            None => {
                let pb: &[u8] = p.as_ref();
                eprintln!(
                    "error: path '{}' does not have {side} version",
                    String::from_utf8_lossy(pb)
                );
                had_error = true;
            }
        }
    }

    if !keep.is_empty() {
        // Build a stage-0 view holding exactly the chosen entries and check it out;
        // the real index is never rewritten, so conflicts survive.
        let mut subset = repo.open_index()?;
        subset.remove_entries(|_, path, e| match keep.get(&path.to_owned()) {
            Some(&st) => e.stage_raw() != st,
            None => true,
        });
        for e in subset.entries_mut() {
            e.flags.remove(Flags::STAGE_MASK);
        }
        let should_interrupt = AtomicBool::new(false);
        checkout_subset(repo, &mut subset, &should_interrupt)?;
    }

    if had_error {
        return Ok(ExitCode::from(1));
    }
    if bare && !quiet {
        let n = keep.len();
        eprintln!(
            "Updated {n} path{} from the index",
            if n == 1 { "" } else { "s" }
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Upstream a `-t`/`--track` start-point resolves to.
struct TrackInfo {
    /// `branch.<name>.remote`: `"."` for a local start-point, else the remote name.
    remote: String,
    /// `branch.<name>.merge`, always `refs/heads/<branch>`.
    merge: String,
    /// Upstream short name shown in the "set up to track" line.
    display: String,
    /// For `-t` without `-b`: the local branch name DWIM'd from the start-point
    /// (`Some` only for a remote-tracking start; a local one can't DWIM a name).
    dwim_name: Option<String>,
}

/// Classify a `-t` start-point as a trackable branch. Returns `None` when it is
/// neither a local branch nor a remote-tracking branch of a configured remote —
/// the caller turns that into git's "is not a branch" / "missing branch name".
fn resolve_tracking(repo: &gix::Repository, start: &str) -> Result<Option<TrackInfo>> {
    // A start point is often a revision expression rather than a branch name
    // (`git checkout -b topic HEAD~2`, or a raw object name — which is how the
    // JetBrains client spells "branch from this commit"). `refs/heads/HEAD~2` is
    // not a well-formed refname, so the lookup fails to *parse* rather than
    // failing to find; either way it names no branch, and nothing is tracked.
    let branch_ref = |suffix: &str| -> Option<gix::Reference<'_>> {
        repo.try_find_reference(suffix).ok().flatten()
    };
    if branch_ref(format!("refs/heads/{start}").as_str()).is_some() {
        return Ok(Some(TrackInfo {
            remote: ".".into(),
            merge: format!("refs/heads/{start}"),
            display: start.into(),
            dwim_name: None,
        }));
    }
    if branch_ref(format!("refs/remotes/{start}").as_str()).is_some() {
        // Remote names carry no '/', so the first component is the remote.
        if let Some((remote, rest)) = start.split_once('/') {
            if !rest.is_empty()
                && repo
                    .remote_names()
                    .iter()
                    .any(|n| n.to_str_lossy() == remote)
            {
                return Ok(Some(TrackInfo {
                    remote: remote.into(),
                    merge: format!("refs/heads/{rest}"),
                    display: start.into(),
                    dwim_name: Some(rest.into()),
                }));
            }
        }
    }
    Ok(None)
}

/// Persist `branch.<name>.remote` / `branch.<name>.merge` into the repo-local
/// config. The caller already holds the reentrant `RepoLock`.
fn write_tracking_config(repo: &gix::Repository, name: &str, info: &TrackInfo) -> Result<bool> {
    // Whether this changes anything: `install_branch_config()` announces the upstream it
    // *set*, so re-stating the same one (a `-B` onto a branch that already tracks it)
    // says nothing and leaves the ordinary tracking status to speak instead.
    let unchanged = {
        let snap = repo.config_snapshot();
        snap.string(&format!("branch.{name}.remote")).map(|v| v.to_string()) == Some(info.remote.clone())
            && snap.string(&format!("branch.{name}.merge")).map(|v| v.to_string())
                == Some(info.merge.clone())
    };
    let path = repo.common_dir().join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)?;
    file.set_raw_value_by("branch", Some(gix::bstr::BStr::new(name)), "remote", info.remote.as_str())?;
    file.set_raw_value_by("branch", Some(gix::bstr::BStr::new(name)), "merge", info.merge.as_str())?;
    let bytes = file.to_bstring();
    let tmp = path.with_extension("zvcs-tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(!unchanged)
}

// --- DWIM (`--guess`) ------------------------------------------------------

/// Result of resolving a bare `<name>` against the remote-tracking namespace.
enum Dwim {
    /// Exactly one remote has the branch; its short name (`<remote>/<name>`).
    One(String),
    /// More than one remote has it — ambiguous (unless `checkout.defaultRemote`).
    Many { count: usize },
    /// No remote has it.
    None,
}

/// Find the remote-tracking branch a bare `<name>` should DWIM to: `refs/remotes/
/// <remote>/<name>` across every configured remote. `checkout.defaultRemote`
/// disambiguates a multi-remote match. Mirrors `switch`'s identical resolver.
fn unique_remote_branch(repo: &gix::Repository, name: &str) -> Result<Dwim> {
    let mut matches: Vec<String> = Vec::new();
    for remote in repo.remote_names() {
        let remote = remote.to_str_lossy();
        let full = format!("refs/remotes/{remote}/{name}");
        if repo.try_find_reference(full.as_str())?.is_some() {
            matches.push(remote.into_owned());
        }
    }
    matches.sort();
    match matches.len() {
        0 => Ok(Dwim::None),
        1 => Ok(Dwim::One(format!("{}/{name}", matches[0]))),
        n => {
            if let Some(def) = repo.config_snapshot().string("checkout.defaultRemote") {
                let def = def.to_str_lossy().into_owned();
                if matches.contains(&def) {
                    return Ok(Dwim::One(format!("{def}/{name}")));
                }
            }
            Ok(Dwim::Many { count: n })
        }
    }
}

/// `nr_checkouts` as `checkout_entry()` (entry.c) counts it: the number of
/// entries whose file it actually rewrites.
///
/// The counter is not "paths the pathspec matched" — `checkout_entry_ca()` gives
/// up before writing when the worktree file already matches the entry:
///
/// ```c
/// if (!check_path(path.buf, path.len, &st, state->base_dir_len)) {
///         unsigned changed = ie_match_stat(state->istate, ce, &st,
///                                          CE_MATCH_IGNORE_VALID | CE_MATCH_IGNORE_SKIP_WORKTREE);
///         …
///         if (!changed)
///                 return 0;
/// ```
///
/// and only `write_entry()`, past that gate, does `if (nr_checkouts) (*nr_checkouts)++`.
/// So `git checkout <path>` on an untouched worktree is stock's
/// `Updated 0 paths from the index`, and the decision is `ie_match_stat()`'s —
/// **stat** data, not content: a bare `touch` that leaves the bytes alone still
/// counts, because the mtime moved.
///
/// `entries` are the entries `checkout_worktree()` would hand to `checkout_entry()`,
/// which is why the tree form has to build them first (see [`tree_effective_entries`]).
fn nr_checkouts<'a>(
    repo: &gix::Repository,
    ctx: &super::read_tree::StatCtx,
    entries: impl Iterator<Item = (&'a BString, &'a gix::index::Entry)>,
) -> usize {
    entries
        .filter(|(path, entry)| {
            // `check_path()` failing (the file is gone) is not the `!changed`
            // early return — git falls through and writes, so it counts.
            ctx.probe(repo, entry, BStr::new(path.as_slice())) != super::read_tree::Probe::Uptodate
        })
        .count()
}

/// The entries `read_tree_some()` leaves in the index for a `<tree-ish>` path
/// checkout, which is what the counter above has to be asked about.
///
/// `update_some()` (builtin/checkout.c) does not blindly replace: an entry the
/// tree agrees with keeps its **existing stat data**, and only that is what makes
/// `git checkout HEAD^{tree} <clean-path>` report `Updated 0 paths`:
///
/// ```c
/// pos = index_name_pos(the_repository->index, ce->name, ce->ce_namelen);
/// if (pos >= 0) {
///         struct cache_entry *old = the_repository->index->cache[pos];
///         if (ce->ce_mode == old->ce_mode &&
///             !ce_intent_to_add(old) &&
///             oideq(&ce->oid, &old->oid)) {
///                 old->ce_flags |= CE_UPDATE;
///                 discard_cache_entry(ce);
///                 return 0;
///         }
/// }
/// add_index_entry(the_repository->index, ce, ADD_CACHE_OK_TO_ADD | ADD_CACHE_OK_TO_REPLACE);
/// ```
///
/// A replaced (or newly added) entry arrives with `make_empty_cache_entry()`'s
/// zeroed stat, which `ie_match_stat()` can never call clean — so it always
/// counts. Only the "leave the old entry in place" arm can report up to date.
fn tree_effective_entries(
    from_tree: &gix::index::File,
    current: &gix::index::File,
    matched: &[BString],
) -> Vec<(BString, gix::index::Entry)> {
    let cur_backing = current.path_backing();
    let mut cur: HashMap<BString, &gix::index::Entry> = HashMap::new();
    for e in current.entries().iter().filter(|e| e.stage_raw() == 0) {
        cur.insert(e.path_in(cur_backing).to_owned(), e);
    }
    let backing = from_tree.path_backing();
    let mut out = Vec::with_capacity(matched.len());
    for e in from_tree.entries() {
        let path = e.path_in(backing).to_owned();
        if !matched.contains(&path) {
            continue;
        }
        let mut eff = e.clone();
        match cur.get(&path) {
            Some(old)
                if old.mode == e.mode
                    && old.id == e.id
                    && !old.flags.contains(Flags::INTENT_TO_ADD) =>
            {
                eff.stat = old.stat;
            }
            // `make_empty_cache_entry()`: no stat data at all.
            _ => eff.stat = Stat::default(),
        }
        out.push((path, eff));
    }
    out
}

/// The conflicted entries among `matched`, as `[stage] -> (id, mode)` with index
/// 1/2/3 for base/ours/theirs — git's `ce_stage()` walk over the runs of equal
/// names. A path with a stage-0 entry is not conflicted and never appears here.
fn unmerged_stages(
    index: &gix::index::File,
    matched: &[BString],
) -> HashMap<BString, [Option<(ObjectId, Mode)>; 4]> {
    let want: HashSet<&BString> = matched.iter().collect();
    let backing = index.path_backing();
    let mut out: HashMap<BString, [Option<(ObjectId, Mode)>; 4]> = HashMap::new();
    for e in index.entries() {
        let stage = e.stage_raw() as usize;
        if stage == 0 || stage > 3 {
            continue;
        }
        let path = e.path_in(backing).to_owned();
        if !want.contains(&path) {
            continue;
        }
        out.entry(path).or_default()[stage] = Some((e.id, e.mode));
    }
    out
}

/// `checkout_merged()`'s `ll_merge()`: the three stages merged under git's
/// `base` / `ours` / `theirs` labels, in the requested conflict style. A missing
/// ancestor is the empty blob, which is what `read_mmblob()` of a null id gives.
fn merge_stages(
    repo: &gix::Repository,
    base: Option<ObjectId>,
    ours: ObjectId,
    theirs: ObjectId,
    style: &str,
) -> Result<Vec<u8>> {
    let load = |id: Option<ObjectId>| -> Result<Vec<u8>> {
        Ok(match id {
            Some(id) => repo.find_object(id)?.detach().data,
            None => Vec::new(),
        })
    };
    let base_b = load(base)?;
    let our_b = load(Some(ours))?;
    let their_b = load(Some(theirs))?;

    let mut input = InternedInput::new(our_b.as_slice(), their_b.as_slice());
    let mut out = Vec::new();
    let opts = gix::merge::blob::builtin_driver::text::Options {
        diff_algorithm: Algorithm::Myers,
        conflict: gix::merge::blob::builtin_driver::text::Conflict::Keep {
            style: match style {
                "diff3" => gix::merge::blob::builtin_driver::text::ConflictStyle::Diff3,
                "zdiff3" => gix::merge::blob::builtin_driver::text::ConflictStyle::ZealousDiff3,
                _ => gix::merge::blob::builtin_driver::text::ConflictStyle::Merge,
            },
            marker_size: NonZeroU8::new(7).expect("7 != 0"),
        },
        ..Default::default()
    };
    gix::merge::blob::builtin_driver::text(
        &mut out,
        &mut input,
        gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new("base")),
            current: Some(BStr::new("ours")),
            other: Some(BStr::new("theirs")),
        },
        our_b.as_slice(),
        base_b.as_slice(),
        their_b.as_slice(),
        opts,
    );
    Ok(out)
}

/// Restore `paths` in the worktree from the current index (index left unchanged;
/// only stat info is refreshed). `bare` is true for the no-`--` pathspec form,
/// which prints git's "Updated N path(s) from the index" confirmation.
fn restore_from_index(
    repo: &gix::Repository,
    paths: &[&str],
    bare: bool,
    quiet: bool,
    // `opts->merge`: an unmerged path is re-created as a conflicted file rather
    // than refused — `checkout_merged()`.
    merge: Option<MergeOpt<'_>>,
    // `opts->force`: the unmerged refusal becomes a warning, and the path is
    // then left alone (`checkout_paths()` has no branch that writes it).
    force: bool,
) -> Result<ExitCode> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut index = repo.open_index()?;
    let matched = match match_paths(&index, paths) {
        Ok(m) => m,
        Err(spec) => {
            eprintln!("error: pathspec '{spec}' did not match any file(s) known to git");
            return Ok(ExitCode::from(1));
        }
    };

    // `checkout_paths()`'s unmerged pass, which runs over the whole matched set
    // before anything is written — so a refusal leaves the worktree untouched.
    let unmerged = unmerged_stages(&index, &matched);
    let mut merged_blobs: HashMap<BString, ObjectId> = HashMap::new();
    if !unmerged.is_empty() {
        let mut had_error = false;
        for (path, stages) in &unmerged {
            let name = path.to_str_lossy();
            match (merge, force) {
                (Some(opt), _) => {
                    // `checkout_merged()`: `ll_merge()` of the three stages under
                    // the `base`/`ours`/`theirs` labels, written out as a blob.
                    let (Some(ours), Some(theirs)) = (stages[2], stages[3]) else {
                        eprintln!("error: path '{name}' does not have necessary versions");
                        had_error = true;
                        continue;
                    };
                    let content =
                        merge_stages(repo, stages[1].map(|(id, _)| id), ours.0, theirs.0, opt.style)?;
                    merged_blobs.insert(path.clone(), repo.write_blob(&content)?.detach());
                }
                (None, true) => eprintln!("warning: path '{name}' is unmerged"),
                (None, false) => {
                    eprintln!("error: path '{name}' is unmerged");
                    had_error = true;
                }
            }
        }
        if had_error {
            return Ok(ExitCode::from(1));
        }
    }

    // Counted before anything is written: afterwards every file matches its entry.
    let count = if bare && !quiet {
        let ctx = super::read_tree::StatCtx::new(repo, &index)?;
        let backing = index.path_backing();
        let mset: HashSet<&BString> = matched.iter().collect();
        let pairs: Vec<(BString, gix::index::Entry)> = index
            .entries()
            .iter()
            .filter(|e| e.stage_raw() == 0)
            .map(|e| (e.path_in(backing).to_owned(), e.clone()))
            .filter(|(p, _)| mset.contains(p))
            .collect();
        nr_checkouts(repo, &ctx, pairs.iter().map(|(p, e)| (p, e)))
    } else {
        0
    };

    let mut subset = repo.open_index()?;
    keep_only(&mut subset, &matched);
    // An unmerged path is written from the `checkout_merged()` result (git's
    // "phony cache entry": stage 2's mode carrying the merged blob), and one
    // that was only warned about is not written at all. Either way the real
    // index keeps its stages — `checkout_paths()` never resolves them.
    subset.remove_entries(|_, path, e| {
        let unmerged_here = unmerged.contains_key(path.as_bstr());
        // Stage 2 is the one `checkout_merged()` builds its transient entry from
        // (`if (stage == 2) mode = create_ce_mode(ce->ce_mode)`), so its *mode*
        // is what the merged content is written with.
        unmerged_here && (e.stage_raw() != 2 || !merged_blobs.contains_key(path.as_bstr()))
    });
    for e in subset.entries_mut() {
        e.flags.remove(Flags::STAGE_MASK);
    }
    {
        let backing = subset.path_backing().to_owned();
        for e in subset.entries_mut() {
            if let Some(id) = merged_blobs.get(e.path_in(&backing).as_bstr()) {
                e.id = *id;
            }
        }
    }
    let should_interrupt = AtomicBool::new(false);
    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Refresh stat info in the real index for the restored paths so a later
    // status stays cheap; content ids are unchanged. An unmerged path has no
    // stage-0 entry to refresh and its worktree file is a conflicted merge, so
    // it is left out.
    let fresh = stats_by_path(&subset);
    for path in matched.iter().filter(|p| !unmerged.contains_key(p.as_bstr())) {
        if let Ok(idx) = index.entry_index_by_path(BStr::new(path)) {
            if let Some((id, mode, stat)) = fresh.get(path) {
                let e = &mut index.entries_mut()[idx];
                e.id = *id;
                e.mode = *mode;
                e.stat = *stat;
            }
        }
    }
    index.remove_tree();
    index.write(Default::default())?;

    if bare && !quiet {
        eprintln!(
            "Updated {count} path{} from the index",
            if count == 1 { "" } else { "s" }
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Restore `paths` from `tree_ish` into both the index and the worktree
/// (matching stock `git checkout <tree-ish> -- <path>`). In overlay mode (the
/// default) paths absent from `tree_ish` are left untouched; with `overlay ==
/// false` a pathspec-matched path that exists in the current index but not in
/// `tree_ish` is deleted from both the worktree and the index, so the result
/// matches `tree_ish` exactly (git's `--no-overlay`).
/// `bare` is the no-`--` form, which reports through
/// `Updated N path(s) from <abbrev>` — where the abbreviation is of
/// `opts->source_tree`, i.e. the **tree** the operand peeled to and never the
/// operand's own id:
///
/// ```c
/// if (opts->source_tree)
///         fprintf_ln(stderr, Q_("Updated %d path from %s",
///                               "Updated %d paths from %s", nr_checkouts),
///                    nr_checkouts,
///                    repo_find_unique_abbrev(the_repository, &opts->source_tree->object.oid,
///                                            DEFAULT_ABBREV));
/// ```
///
/// so an annotated tag pointing at a tree reports the tree's id, not the tag's.
fn restore_from_tree(
    repo: &gix::Repository,
    tree_ish: &str,
    paths: &[&str],
    overlay: bool,
    bare: bool,
    quiet: bool,
) -> Result<ExitCode> {
    // `parse_branchname_arg()` resolves this through `get_oid_mb()` and dies with
    // `invalid reference` when nothing resolves. Propagating the revision parser's
    // own error instead put a Rust type name and a vendored `src/ported/…` path in
    // front of the user, and exited 1 where git exits 128.
    let Some(id) = crate::objname::resolve(repo, tree_ish) else {
        crate::git_fatal!("invalid reference: {tree_ish}");
    };
    let tree_id = classify_tree_ish(repo, id)?.source_tree()?;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut src = repo.index_from_tree(&tree_id)?;
    // `PS_IGNORE_SKIP_WORKTREE`: a path the sparse-checkout definition keeps out of
    // the worktree is not something a pathspec can match, so naming one is git's
    // "did not match any file(s) known to git" rather than a checkout of a file the
    // definition says should not be there.
    let sparse: HashSet<BString> = {
        let index = repo.open_index()?;
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .filter(|e| e.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE))
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    if !sparse.is_empty() {
        src.remove_entries(|_, path, _| sparse.contains(&path.to_owned()));
    }

    // Paths to write from the tree, and (no-overlay only) paths to delete.
    let (matched, to_remove) = if overlay {
        (
            match match_paths(&src, paths) {
                Ok(m) => m,
                Err(spec) => {
                    eprintln!(
                        "error: pathspec '{spec}' did not match any file(s) known to git"
                    );
                    return Ok(ExitCode::from(1));
                }
            },
            Vec::new(),
        )
    } else {
        let (tree_matched, tree_hit) = matches_in(&src, paths);
        let cur = repo.open_index()?;
        let (idx_matched, idx_hit) = matches_in(&cur, paths);
        // A pathspec must match in the tree or the index; else git's "did not match".
        if let Some(si) = (0..paths.len()).find(|&si| !tree_hit[si] && !idx_hit[si]) {
            bail!(
                "pathspec '{}' did not match any file(s) known to git",
                paths[si]
            );
        }
        let tset: HashSet<BString> = tree_matched.iter().cloned().collect();
        let rm: Vec<BString> = idx_matched.into_iter().filter(|p| !tset.contains(p)).collect();
        (tree_matched, rm)
    };

    // Counted before anything is written: afterwards every file matches its entry.
    let count = if bare && !quiet {
        let cur = repo.open_index()?;
        let ctx = super::read_tree::StatCtx::new(repo, &cur)?;
        let effective = tree_effective_entries(&src, &cur, &matched);
        nr_checkouts(repo, &ctx, effective.iter().map(|(p, e)| (p, e)))
    } else {
        0
    };

    let mut subset = repo.index_from_tree(&tree_id)?;
    keep_only(&mut subset, &matched);
    let should_interrupt = AtomicBool::new(false);
    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Fold the tree's blobs (with fresh checkout stats) into the real index.
    let fresh = stats_by_path(&subset);
    let mut index = repo.open_index()?;
    let mut pushed = false;
    for path in &matched {
        let Some((id, mode, stat)) = fresh.get(path) else {
            continue;
        };
        match index.entry_index_by_path(BStr::new(path)) {
            Ok(idx) => {
                let e = &mut index.entries_mut()[idx];
                e.id = *id;
                e.mode = *mode;
                e.stat = *stat;
            }
            Err(_) => {
                index.dangerously_push_entry(
                    *stat,
                    *id,
                    gix::index::entry::Flags::empty(),
                    *mode,
                    BStr::new(path),
                );
                pushed = true;
            }
        }
    }

    // No-overlay: delete pathspec-matched paths that the tree does not carry.
    if !to_remove.is_empty() {
        for path in &to_remove {
            if let Some(full) = repo.workdir_path(BStr::new(path)) {
                let _ = std::fs::remove_file(full);
            }
        }
        let rmset: HashSet<BString> = to_remove.into_iter().collect();
        index.remove_entries(|_, path, _| rmset.contains(&path.to_owned()));
    }

    if pushed {
        index.sort_entries();
    }
    index.remove_tree();
    index.write(Default::default())?;

    if bare && !quiet {
        eprintln!(
            "Updated {count} path{} from {}",
            if count == 1 { "" } else { "s" },
            tree_id.attach(repo).shorten_or_id()
        );
    }
    Ok(ExitCode::SUCCESS)
}

// --- Worktree / ref helpers ------------------------------------------------

/// `unpack_trees()` with `twoway_merge` over `HEAD -> <target>`: refuse the
/// switch only for the paths it would actually write over.
///
/// git does not ask whether the worktree is clean — it asks, per path the two
/// trees disagree on, whether the file there still matches the index
/// (`verify_uptodate()`) or, for a path being added, whether something untracked
/// is in the way (`verify_absent()`). Everything else is carried across
/// untouched, which is why `git checkout <branch>` works with unrelated local
/// edits in the tree and reports them as `M <path>` afterwards.
///
/// Returns the exit code when the switch is refused; the paths and the advice
/// are printed by [`crate::merge_guard::Clobber::report`].
pub(super) fn ensure_clean(
    repo: &gix::Repository,
    cur_tree: ObjectId,
    target_tree: ObjectId,
) -> Result<Option<ExitCode>> {
    match switch_gate(repo, cur_tree, target_tree, None)? {
        Gate::Clean => Ok(None),
        Gate::Refused(code) => Ok(Some(code)),
        Gate::Autostashed(_) => unreachable!("no -m was passed, so nothing is ever stashed"),
    }
}

/// `-m` / `--merge`, and the `--conflict=<style>` that implies it.
#[derive(Clone, Copy)]
pub(super) struct MergeOpt<'a> {
    /// The conflict style the re-applied changes are marked up with — `merge`,
    /// `diff3` or `zdiff3`, already resolved from `--conflict=<style>` or
    /// `merge.conflictStyle`. It reaches the stash re-apply through
    /// [`crate::merge_apply::three_way_merge_styled`], so all three styles write
    /// the markers stock writes.
    pub style: &'a str,
    /// The switch target **as spelled on the command line**: the stash message
    /// (`autostash while switching to '<name>'`) and the `ours` side of every
    /// conflict marker are written with it, so `git checkout -m <sha>` marks up
    /// with the id the user typed and not with a branch name. The pathspec form
    /// has no target and ignores it: `checkout_merged()` labels its three sides
    /// `base`/`ours`/`theirs` whatever the operands were.
    pub name: &'a str,
}

/// What the two-way `unpack_trees()` gate decided.
pub(super) enum Gate {
    /// Nothing in the way: check the target tree out as usual.
    Clean,
    /// `-m` took over: the local changes are in this stash-like commit and the
    /// worktree and index are back at `HEAD`, so the checkout can proceed. The
    /// commit is re-applied by [`apply_switch_autostash`] once the target tree
    /// is in place.
    Autostashed(ObjectId),
    /// Refused, with the exit code to return.
    Refused(ExitCode),
}

/// `merge_working_tree()`'s two-way `unpack_trees()` and, when it refuses and
/// `-m` was given, the autostash that replaces it.
///
/// git 2.55 no longer does the "real merge" in place. When the two-way unpack
/// fails, `-m` stashes the local changes (`autostash while switching to
/// '<name>'`), lets the now-clean switch happen, and re-applies the stash with a
/// three-way merge afterwards — which is why the local changes come back
/// *unstaged*, why a conflicting re-apply leaves the snapshot in `refs/stash`,
/// and why the run still exits 0. Only the *tracked* refusals are stashable: an
/// untracked file standing where the target tree has one is not a local change
/// git can carry, so `-m` does not apply and the refusal stands.
pub(super) fn switch_gate(
    repo: &gix::Repository,
    cur_tree: ObjectId,
    target_tree: ObjectId,
    merge: Option<MergeOpt<'_>>,
) -> Result<Gate> {
    let index = repo.index_or_load_from_head_or_empty()?;
    let clobber = crate::merge_guard::verify_two_way(repo, cur_tree, target_tree, &index)?;
    if clobber.is_empty() {
        return Ok(Gate::Clean);
    }
    let refuse = |clobber: &crate::merge_guard::Clobber| {
        clobber.report("checkout");
        Ok(Gate::Refused(ExitCode::from(1)))
    };
    let Some(opt) = merge else {
        return refuse(&clobber);
    };
    // `verify_absent()`'s two buckets: `-m` has nothing to stash for them, and
    // git leaves the same refusal in place.
    if !clobber.untracked_overwritten.is_empty() || !clobber.untracked_removed.is_empty() {
        return refuse(&clobber);
    }
    // `if (!old_branch_info->commit) return 1;` — there is no base to merge the
    // local changes against, so the two-way refusal is the whole answer.
    if repo.head_id().is_err() {
        return refuse(&clobber);
    }
    let stash = super::stash::create_autostash_msg(
        repo,
        &format!("autostash while switching to '{}'", opt.name),
    )?;
    Ok(Gate::Autostashed(stash))
}

/// `opts->merge` packaged for a switch: `None` when `-m` was not given, and
/// otherwise the conflict style plus the target **as the user spelled it**,
/// which is the name both the stash message and the `ours` conflict label carry.
fn merge_opt<'a>(merge: bool, style: &'a str, name: &'a str) -> Option<MergeOpt<'a>> {
    merge.then_some(MergeOpt { style, name })
}

/// `merge_working_tree()` as every switch in this file runs it: gate the move,
/// write the target tree out, and put back whatever `-m` had to stash to get
/// there. Returns the exit code of a refusal, or `None` when the worktree moved.
///
/// Called only when the two trees actually differ, which is the caller's own
/// `target_tree != cur_tree` test: an identical-tree switch has no path for
/// `twoway_merge()` to reject and nothing to carry.
pub(super) fn move_worktree(
    repo: &gix::Repository,
    cur_tree: ObjectId,
    target_tree: ObjectId,
    merge: Option<MergeOpt<'_>>,
) -> Result<Moved> {
    // The base label is the branch being *left*, so it is read before `HEAD`
    // moves — which, on every path here, is after this function returns.
    let base_label = old_head_label(repo)?;
    match switch_gate(repo, cur_tree, target_tree, merge)? {
        Gate::Refused(code) => Ok(Moved::Refused(code)),
        Gate::Clean => {
            update_worktree_to_tree(repo, cur_tree, target_tree)?;
            Ok(Moved::Clean)
        }
        Gate::Autostashed(stash) => {
            update_worktree_to_tree(repo, cur_tree, target_tree)?;
            let opt = merge.expect("only `-m` ever stashes");
            apply_switch_autostash(repo, stash, target_tree, opt, base_label.as_deref())?;
            Ok(Moved::Autostashed)
        }
    }
}

/// What [`move_worktree`] did, because the caller has to print differently for
/// the two outcomes.
///
/// ```c
/// if (do_merge) {
///         ret = merge_working_tree(…, opts->merge, &writeout_error);
///         if (ret == MERGE_WORKING_TREE_UNPACK_FAILED && opts->merge) {
///                 create_autostash_ref(…);  created_autostash = 1;
///                 ret = merge_working_tree(…, false, &writeout_error);
///         }
///         if (created_autostash) { … apply_autostash_ref(…); }
///         …
/// }
/// update_refs_for_switch(opts, &old_branch_info, new_branch_info);
/// if (created_autostash) {
///         discard_index(the_repository->index);
///         if (repo_read_index(the_repository) < 0) die(_("index file corrupt"));
///         if (!opts->quiet && new_branch_info->commit) {
///                 printf(_("The following paths have local changes:\n"));
///                 show_local_changes(&new_branch_info->commit->object, &opts->diff_options);
///         }
/// }
/// ```
/// (builtin/checkout.c:1215-1272.) The listing an autostashed switch prints is a
/// *second* one, headed and emitted **after** `update_refs_for_switch()` has
/// announced the switch — not the one at the tail of `merge_working_tree()`,
/// which the retry ran with `merge = false` and therefore against an index that
/// no longer holds the re-applied changes.
#[must_use]
pub(super) enum Moved {
    /// The two-way merge carried everything across; the caller prints the
    /// ordinary `merge_working_tree()` listing before it moves `HEAD`.
    Clean,
    /// `-m` stashed and re-applied; the caller prints the headed listing after
    /// it moves `HEAD`.
    Autostashed,
    /// The gate refused; this is the exit code, and nothing moved.
    Refused(ExitCode),
}

/// The second half of [`switch_gate`]'s `-m`: re-apply the stashed local changes
/// on top of the tree that was just checked out.
///
/// The three sides are the stash's own base (the tree `HEAD` held when it was
/// made), *ours* (the tree just checked out) and *theirs* (the stashed
/// worktree). A clean re-apply leaves the changes **unstaged** — the index goes
/// back to the target tree — and says `Applied autostash.`; a conflicting one
/// keeps the conflicted index, hands the snapshot to `refs/stash` so the wording
/// about `git stash pop` is true, and still lets the switch stand.
pub(super) fn apply_switch_autostash(
    repo: &gix::Repository,
    stash: ObjectId,
    ours_tree: ObjectId,
    opt: MergeOpt<'_>,
    base_label: Option<&str>,
) -> Result<()> {
    let commit = repo.find_commit(stash)?;
    let Some(parent) = commit.parent_ids().next() else {
        return Err(crate::fatal::die("autostash commit has no base"));
    };
    let base = repo.find_commit(parent.detach())?.tree_id()?.detach();
    let theirs = commit.tree_id()?.detach();
    let old_index = repo.index_or_load_from_head()?.into_owned();

    // `--label-ours` / `--label-theirs` / `--label-base`, which is how
    // `builtin/checkout.c` spells the sides to the `git stash apply` it runs.
    let labels = gix::merge::blob::builtin_driver::text::Labels {
        ancestor: base_label.map(|l| BStr::new(l.as_bytes())),
        current: Some(BStr::new(opt.name.as_bytes())),
        other: Some(BStr::new(b"local")),
    };
    let should_interrupt = AtomicBool::new(false);
    // The re-apply is `git stash apply --quiet`, so its own `Auto-merging` /
    // `CONFLICT (…)` block is suppressed and only the wording below is printed.
    // `--conflict=<style>` shapes the markers it leaves behind; the style was
    // validated at parse time, so an unknown one cannot reach here.
    let applied = crate::merge_apply::three_way_merge_styled(
        repo,
        base,
        ours_tree,
        theirs,
        &old_index,
        labels,
        &should_interrupt,
        false,
        crate::merge_apply::conflict_style(opt.style),
    )?;

    if applied.conflicts.is_empty() {
        // `stash apply` without `--index`: the restored changes come back
        // unstaged, so the index stays the target tree — which is exactly what
        // the checkout just wrote, stat data and all. Rebuilding it from the
        // tree here instead would drop that stat data and make the next
        // `diff-index` call it modified: `show_local_changes()` runs right after
        // this and would list every file in the tree.
        eprintln!("Applied autostash.");
    } else {
        let mut index = applied.index;
        index.write(Default::default())?;
        super::stash::store_commit(
            repo,
            stash,
            &format!("autostash while switching to '{}'", opt.name),
        )?;
        eprintln!("Your local changes are stashed, however applying them");
        eprintln!("resulted in conflicts.  You can either resolve the conflicts");
        eprintln!("and then discard the stash with \"git stash drop\", or, if you");
        eprintln!("do not want to resolve them now, run \"git reset --hard\" and");
        eprintln!("apply the local changes later by running \"git stash pop\".");
    }
    Ok(())
}

/// `o.ancestor`: the branch the switch is leaving, or its abbreviated commit id
/// when `HEAD` was already detached — the `|||||||` label of a diff3-style
/// conflict, and the base label the re-apply is given either way.
///
/// ```c
/// if (old_branch_info.name) {
///         stash_label_base = old_branch_info.name;
/// } else if (old_branch_info.commit) {
///         strbuf_add_unique_abbrev(&old_commit_shortname,
///                                  &old_branch_info.commit->object.oid, DEFAULT_ABBREV);
///         stash_label_base = old_commit_shortname.buf;
/// }
/// ```
/// (builtin/checkout.c:1205-1212.) `stash_label_base` stays NULL when `HEAD` has
/// neither — the same "gently" peel as [`head_label`] — and reaches
/// `apply_autostash_ref()` as an absent `--label-base`. Returning an error there
/// instead stopped every non-forced checkout out of a `HEAD` holding an id this
/// repository does not have, which is exactly the state a checkout is the way
/// out of. `name` is the *branch* name, so it is taken only under `refs/heads/`.
pub(super) fn old_head_label(repo: &gix::Repository) -> Result<Option<String>> {
    let head = repo.head()?;
    if let Some(name) = head.referent_name() {
        if let Some(short) = name.as_bstr().strip_prefix(b"refs/heads/") {
            return Ok(Some(short.to_str_lossy().into_owned()));
        }
    }
    Ok(peeled_head_commit(repo, &head).map(|id| id.attach(repo).shorten_or_id().to_string()))
}

/// Move a clean worktree and its index from the current state to `new_tree`,
/// writing only the files that changed (added/modified checked out, removed
/// deleted). Mirrors the file-level reconciliation used by `zsync`.
pub(super) fn update_worktree_to_tree(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
) -> Result<()> {
    let should_interrupt = AtomicBool::new(false);

    // `twoway_merge()` decides per path, and only for the paths the two trees
    // disagree on: everything else is `keep_entry()`d, index entry and worktree
    // file untouched. That is what carries a staged change to a file both
    // branches share across the switch — rebuilding the index from the target
    // tree instead would silently throw that work away.
    let old_flat = flatten_tree(repo, old_tree)?;
    let new_flat = flatten_tree(repo, new_tree)?;
    let touched: HashSet<BString> = old_flat
        .keys()
        .chain(new_flat.keys())
        .filter(|p| old_flat.get(*p) != new_flat.get(*p))
        .cloned()
        .collect();

    // The index this checkout starts from. `_or_empty` because the first
    // checkout of a repository has neither index nor `HEAD`: a freshly `init`ed
    // repo that has only fetched objects is exactly the state
    // `git init && git fetch <url> <sha> && git checkout <sha>` checks out from,
    // the sequence tree-sitter grammar fetchers use.
    let old = repo.index_or_load_from_head_or_empty()?.into_owned();
    let old_stats: HashMap<BString, (ObjectId, Mode, Stat)> = {
        let backing = old.path_backing();
        old.entries()
            .iter()
            .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode, e.stat)))
            .collect()
    };

    // `merged_entry()`: the touched paths the new tree has are written out.
    let mut subset = repo.index_from_tree(&new_tree)?;
    subset.remove_entries(|_, path, _| !touched.contains(&path.to_owned()));
    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // `deleted_entry()`: the touched paths it does not have are removed.
    for path in touched.iter().filter(|p| !new_flat.contains_key(*p)) {
        if let Some(full) = repo.workdir_path(path.as_bstr()) {
            let _ = std::fs::remove_file(full);
        }
    }

    // The index moves with the worktree, one path at a time: the touched entries
    // are replaced by the new tree's, the rest stay exactly as they were.
    let mut index = old;
    index.remove_entries(|_, path, _| touched.contains(&path.to_owned()));
    let subset_stats = stats_by_path(&subset);
    {
        let backing = subset.path_backing().to_owned();
        for e in subset.entries() {
            let path = e.path_in(&backing).to_owned();
            let stat = subset_stats
                .get(&path)
                .map(|(_, _, stat)| *stat)
                .or_else(|| {
                    old_stats
                        .get(&path)
                        .filter(|(oid, mode, _)| *oid == e.id && *mode == e.mode)
                        .map(|(_, _, stat)| *stat)
                })
                .unwrap_or(e.stat);
            index.dangerously_push_entry(stat, e.id, e.flags, e.mode, path.as_ref());
        }
    }
    index.sort_entries();
    index.remove_tree();
    index.write(Default::default())?;
    Ok(())
}

/// A tree as `path -> (id, mode)`, through its index representation so nested
/// trees come out as slash-separated paths.
fn flatten_tree(
    repo: &gix::Repository,
    tree: ObjectId,
) -> Result<HashMap<BString, (ObjectId, Mode)>> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    Ok(index
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode)))
        .collect())
}

/// `git checkout -f`'s worktree update: git's `reset_tree()`, i.e. a one-tree
/// `unpack_trees` run with `reset = UNPACK_RESET_OVERWRITE_UNTRACKED` and
/// `update = 1`, driven by `oneway_merge`.
///
/// The difference from [`update_worktree_to_tree`] is the whole point of `-f`:
/// every safety check `verify_uptodate()` performs is skipped (`o->reset` returns
/// early from it), so a modified, staged, conflicted or *deleted* worktree file is
/// rewritten from the tree instead of blocking the switch. `oneway_merge` decides
/// per entry: an entry the tree leaves alone is still written when
/// `lstat()` fails or `ie_match_stat()` reports a change, and an entry the tree
/// changes — including a conflicted one, which `same()` never matches — is always
/// written and lands at stage 0.
pub(super) fn reset_worktree_to_tree(repo: &gix::Repository, new_tree: ObjectId) -> Result<()> {
    let should_interrupt = AtomicBool::new(false);

    let old = repo.index_or_load_from_head_or_empty()?.into_owned();
    let stat_ctx = super::read_tree::StatCtx::new(repo, &old)?;
    // Stage-0 entries only: a conflicted path has no stage-0 entry, so it never
    // matches `same(old, a)` and always ends up rewritten, which is what
    // `oneway_merge` does with a `CE_CONFLICTED` entry.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat, super::read_tree::Probe)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries().iter().filter(|e| e.stage_raw() == 0) {
            let path = e.path_in(backing);
            old_map.insert(
                path.to_owned(),
                (e.id, e.mode, e.stat, stat_ctx.probe(repo, e, path)),
            );
        }
    }

    let mut new_index = repo.index_from_tree(&new_tree)?;

    // What actually gets written: everything the tree changes, plus every
    // carried-forward entry whose worktree file is missing or stat-dirty.
    let mut subset = repo.index_from_tree(&new_tree)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _, probe)) => {
            *oid == entry.id && *mode == entry.mode && *probe == super::read_tree::Probe::Uptodate
        }
        None => false,
    });
    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Paths the tree drops: `deleted_entry()` removes them from the worktree too
    // (`verify_uptodate` is a no-op under `--reset`).
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

    // "Take the stat information from stage0, take the data from stage1": an entry
    // that was not rewritten keeps the stat cache it already had.
    let subset_stats = stats_by_path(&subset);
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some((_, _, stat)) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat, _)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }
    new_index.remove_tree();
    new_index.write(Default::default())?;
    // `remove_branch_state()`: a forced switch abandons any in-progress merge,
    // cherry-pick or revert, exactly as git's `switch_branches` does after the
    // worktree is reconciled.
    remove_branch_state(repo);
    Ok(())
}

/// git's `remove_branch_state()` (branch.c): the merge/sequencer state files a
/// switch invalidates, plus the `CHERRY_PICK_HEAD` / `REVERT_HEAD` that
/// `sequencer_post_commit_cleanup()` drops on its way through.
///
/// `AUTO_MERGE` is on the list because `git merge` writes it for the conflicted
/// tree and nothing else removes it: a forced checkout out of a conflicted state
/// that leaves it behind makes the next `git diff` compare against a stale tree.
/// `MERGE_AUTOSTASH` is saved rather than deleted by git and is left alone here.
fn remove_branch_state(repo: &gix::Repository) {
    for name in [
        "MERGE_HEAD",
        "MERGE_RR",
        "MERGE_MSG",
        "MERGE_MODE",
        "AUTO_MERGE",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        let _ = std::fs::remove_file(repo.git_dir().join(name));
    }
}

/// git's `show_local_changes()`: the `diff-index --name-status` listing a
/// non-forced checkout prints to **stdout** when it does not discard changes.
///
/// `rev` is what git passes as the diff's left side — `new_branch_info->commit`,
/// the commit being switched *to*, not `HEAD`. It runs from `merge_working_tree()`,
/// before `update_refs_for_switch()` moves `HEAD`, so diffing against `HEAD`
/// would name every path the two branches disagree about on top of the local
/// changes it is meant to list.
pub(super) fn show_local_changes(rev: &str, quiet: bool) -> Result<()> {
    if quiet {
        return Ok(());
    }
    let args = ["--name-status".to_string(), rev.to_string()];
    // git hands `show_local_changes()` the object it already resolved
    // (`add_pending_object(&rev, head, NULL)`), so no name is looked up a second
    // time. Reaching it through `diff-index`'s argument parsing would re-run
    // `get_oid()` on the same operand and repeat its ambiguity warning, which
    // stock prints exactly once per operand.
    let _quiet_ambiguity = crate::objname::AmbiguityWarnings::off();
    super::diff_index::diff_index(&args)?;
    Ok(())
}

/// Check out the entries currently held in `index` into the worktree, overwriting
/// existing files (filters, mode and symlink handling applied by gitoxide).
fn checkout_subset(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    crate::worktree::checkout_subset(
        index,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        should_interrupt,
        opts,
    )?;
    Ok(())
}

/// Set `HEAD` to point symbolically at `branch` (attached), logging the move.
///
/// `from`/`to` are the object ids `HEAD` resolved to before and after; they exist
/// only for [`record_head_move`], which repairs the `.git/logs/HEAD` line the
/// vendored `gix-ref` cannot write correctly for a symbolic-target update.
fn set_head_symbolic(
    repo: &gix::Repository,
    branch: FullName,
    message: &str,
    from: Option<ObjectId>,
    to: Option<ObjectId>,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    append_head_log(repo, from, to, message);
    Ok(())
}

/// Make the last `.git/logs/HEAD` entry the one `refs_update_ref`/
/// `refs_update_symref` would have written for this move.
///
/// Two things the vendored `gix-ref` gets wrong for a `HEAD` update made with
/// `deref: false`, both visible in `git reflog`:
///
///  * a **symbolic** new target drops the reflog entry entirely
///    (`gix-ref/src/store/file/transaction/commit.rs`), so no line is written;
///  * `PreviousValue::Any` leaves the *old* field as the null id even when `HEAD`
///    resolved to a commit, because the previous value is a symref it does not
///    peel.
///
/// Rewriting the trailing line — or appending it when none was written — is
/// confined to the entry this command just made: the line is only replaced when
/// its message is the one passed in.
pub(super) fn record_head_move(
    repo: &gix::Repository,
    from: Option<ObjectId>,
    to: Option<ObjectId>,
    message: &str,
) {
    write_head_log(repo, from, to, message, true);
}

/// [`record_head_move`] without the replace-the-identical-tail rule, for the callers
/// that legitimately log the same message twice — a branch rename mirrors both the
/// removal and the creation of the branch `HEAD` points at.
pub(super) fn append_head_log(
    repo: &gix::Repository,
    from: Option<ObjectId>,
    to: Option<ObjectId>,
    message: &str,
) {
    write_head_log(repo, from, to, message, false);
}

fn write_head_log(
    repo: &gix::Repository,
    from: Option<ObjectId>,
    to: Option<ObjectId>,
    message: &str,
    dedup: bool,
) {
    // `log_all_ref_updates` is on by default for a repository with a worktree,
    // and git creates `logs/HEAD` on the first update there.
    let path = repo.git_dir().join("logs").join("HEAD");
    if !path.exists()
        && (repo.workdir().is_none()
            || repo.config_snapshot().boolean("core.logAllRefUpdates") == Some(false))
    {
        return;
    }
    let null = ObjectId::null(repo.object_hash());
    let now = gix::date::Time::now_local_or_utc().format_or_unix(gix::date::time::Format::Raw);
    let sig = match repo.committer() {
        Some(Ok(sig)) => sig,
        _ => gix::actor::SignatureRef {
            name: b"zvcs".as_bstr(),
            email: b"zvcs@localhost".as_bstr(),
            time: &now,
        },
    };
    let line = format!(
        "{} {} {} <{}> {}\t{}\n",
        from.unwrap_or(null),
        to.unwrap_or(null),
        sig.name,
        sig.email,
        sig.time,
        message
    );

    let mut body = std::fs::read_to_string(&path).unwrap_or_default();
    let tail_is_ours = dedup
        && body
            .lines()
            .next_back()
            .is_some_and(|last| last.ends_with(&format!("\t{message}")));
    if tail_is_ours {
        let cut = body.trim_end_matches('\n').rfind('\n').map_or(0, |i| i + 1);
        body.truncate(cut);
    }
    body.push_str(&line);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, body);
}

/// Detach `HEAD` at object `id`, logging the move.
fn set_head_detached(
    repo: &gix::Repository,
    id: ObjectId,
    message: &str,
    from: Option<ObjectId>,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    record_head_move(repo, from, Some(id), message);
    Ok(())
}

/// Human label for the current `HEAD` used in reflog "moving from …" messages.
///
/// `update_refs_for_switch()`:
///
/// ```c
/// old_desc = old_branch_info->name;
/// if (!old_desc && old_branch_info->commit)
///         old_desc = oid_to_hex(&old_branch_info->commit->object.oid);
/// ```
///
/// so a detached `HEAD` contributes the **full** id, not the abbreviation the
/// `Previous HEAD position was …` line carries.
fn head_label(repo: &gix::Repository, head: &gix::Head<'_>) -> String {
    // ```c
    // old_branch_info.path = refs_resolve_refdup(…, "HEAD", 0, &rev, &flag);
    // if (old_branch_info.path)
    //         old_branch_info.commit = lookup_commit_reference_gently(r, &rev, 1);
    // if (!(flag & REF_ISSYMREF))
    //         FREE_AND_NULL(old_branch_info.path);
    // if (old_branch_info.path) {
    //         const char *const prefix = "refs/heads/";
    //         const char *p;
    //         if (skip_prefix(old_branch_info.path, prefix, &p))
    //                 old_branch_info.name = xstrdup(p);
    // }
    // …
    // old_desc = old_branch_info->name;
    // if (!old_desc && old_branch_info->commit)
    //         old_desc = oid_to_hex(&old_branch_info->commit->object.oid);
    // strbuf_addf(&msg, "checkout: moving from %s to %s",
    //             old_desc ? old_desc : "(invalid)", new_branch_info->name);
    // ```
    // (builtin/checkout.c:1172-1185, 994-1000.) Three shapes, all measured
    // against git 2.55.0:
    //
    //   * `HEAD` symbolic under `refs/heads/` → the short branch name.
    //   * anything else that still peels to a commit — a detached `HEAD`, or a
    //     symref to `refs/tags/…`/`refs/foo/bar` — → the **peeled commit's** full
    //     hex. Not the name, and not `HEAD`'s raw target: detaching at an
    //     annotated tag's own id records the commit the tag points at.
    //   * `HEAD` peeling to no commit at all — a detached `HEAD` holding a blob's
    //     id, or an id this repository does not have → `(invalid)`.
    if let Some(name) = head.referent_name() {
        if let Some(short) = name.as_bstr().strip_prefix(b"refs/heads/") {
            return short.to_str_lossy().into_owned();
        }
    }
    match peeled_head_commit(repo, head) {
        Some(id) => id.to_string(),
        None => "(invalid)".to_string(),
    }
}

/// The tree the two-way merge starts from:
/// `old_commit_oid = old_branch_info->commit ? &…->object.oid : the_hash_algo->empty_tree;`
/// (builtin/checkout.c:904-906).
///
/// A `HEAD` that peels to no commit is the **empty tree**, not an error — the
/// same "gently" rule as [`peeled_head_commit`]. `head_tree_id_or_empty()` only
/// forgives an *unborn* `HEAD`, so a detached `HEAD` holding a blob's id or an
/// id this repository lacks reached the caller as a gix type error and stopped
/// the checkout that was the way out of it.
fn head_tree_or_empty(repo: &gix::Repository) -> Result<ObjectId> {
    let head = repo.head()?;
    Ok(match peeled_head_commit(repo, &head) {
        Some(id) => repo.find_object(id)?.peel_to_commit()?.tree_id()?.detach(),
        None => repo.empty_tree().id().detach(),
    })
}

/// `lookup_commit_reference_gently(the_repository, &rev, 1)` on whatever `HEAD`
/// resolves to: the commit it peels to, or `None` — never an error.
///
/// Everything `git checkout` reads out of the old `HEAD` goes through this
/// "gently" lookup, which is why stock can switch *away* from a `HEAD` holding a
/// blob's id or an id the object database does not have. Peeling strictly
/// instead turns that recoverable state into a wall the user cannot get past.
fn peeled_head_commit(repo: &gix::Repository, head: &gix::Head<'_>) -> Option<ObjectId> {
    let id = head.id()?;
    Some(repo.find_object(id).ok()?.peel_to_commit().ok()?.id)
}

/// Abbreviated hash + commit summary for `HEAD is now at …` / `Previous HEAD …`.
fn describe(repo: &gix::Repository, id: ObjectId) -> Result<(String, String)> {
    let abbrev = id.attach(repo).shorten_or_id().to_string();
    let commit = repo.find_object(id)?.peel_to_commit()?;
    let summary = commit.message()?.summary().into_owned().to_string();
    Ok((abbrev, summary))
}

// --- Pathspec / index helpers ----------------------------------------------

/// Collect the index entries (by path) matching every pathspec in `specs`.
/// Each spec must match at least one entry, else git's "did not match" error.
/// The matched paths, or `Err(spec)` naming the first pathspec that matched nothing —
/// git reports that on stderr as `error: …` and exits 1, so it is not an error value
/// the dispatcher would re-prefix.
fn match_paths<'a>(
    index: &gix::index::File,
    specs: &[&'a str],
) -> std::result::Result<Vec<BString>, &'a str> {
    let (matched, hit) = matches_in(index, specs);
    match hit.iter().position(|h| !h) {
        Some(si) => Err(specs[si]),
        None => Ok(matched),
    }
}

/// The entries of `index` matching any pathspec, plus a per-spec "did it match
/// anything" flag. Unlike [`match_paths`] this never fails, so callers that must
/// consider several indexes (e.g. no-overlay's tree ∪ index) can decide the
/// "did not match" error against their own union.
fn matches_in(index: &gix::index::File, specs: &[&str]) -> (Vec<BString>, Vec<bool>) {
    let mut matched: Vec<BString> = Vec::new();
    let mut seen: HashSet<BString> = HashSet::new();
    let mut hit = vec![false; specs.len()];

    // Normalised once, not per entry: `matches_in` is O(entries x specs).
    let norm: Vec<(String, bool)> = specs.iter().map(|s| normalize_spec(s)).collect();

    let backing = index.path_backing();
    for e in index.entries() {
        let path = e.path_in(backing);
        let bytes: &[u8] = path.as_ref();
        for (si, spec) in norm.iter().enumerate() {
            if spec_matches(bytes, spec) {
                hit[si] = true;
                let owned = path.to_owned();
                if seen.insert(owned.clone()) {
                    matched.push(owned);
                }
            }
        }
    }
    (matched, hit)
}

/// A pathspec reduced to what the matcher needs: the components it names, and
/// whether it ended at a directory boundary.
///
/// git normalises a pathspec before matching, so `sub/`, `sub//`, `sub/.` and
/// `sub/./` all name `sub`, and a `..` pops a component — `top.txt/..` names the
/// whole tree. What must survive normalisation is whether the spec *ended* on a
/// slash, a `.` or a `..`, because that makes it a directory spec: `top.txt/`
/// does not match the file `top.txt`, while a bare `top.txt` does.
///
/// A leading `/` is left alone. An absolute pathspec is resolved against the
/// worktree root rather than lexically, which this matcher does not model, and
/// reducing it here would silently turn `/abs` into a relative `abs`.
fn normalize_spec(spec: &str) -> (String, bool) {
    if spec.starts_with('/') {
        return (spec.to_string(), false);
    }
    let mut comps: Vec<&str> = Vec::new();
    let mut dir_only = false;
    for part in spec.split('/') {
        match part {
            "" | "." => dir_only = true,
            ".." => {
                comps.pop();
                dir_only = true;
            }
            other => {
                comps.push(other);
                dir_only = false;
            }
        }
    }
    (comps.join("/"), dir_only)
}

/// Whether `path` is matched by an already-normalised pathspec: an empty spec is
/// the whole tree, a directory spec matches only what lies under it, and anything
/// else matches itself or what lies under it.
fn spec_matches(path: &[u8], (spec, dir_only): &(String, bool)) -> bool {
    let s = spec.as_bytes();
    if s.is_empty() {
        return true;
    }
    if !dir_only && path == s {
        return true;
    }
    path.len() > s.len() && path.starts_with(s) && path[s.len()] == b'/'
}

/// Reduce `index` to only the entries whose path is in `keep`.
fn keep_only(index: &mut gix::index::File, keep: &[BString]) {
    index.remove_entries(|_, path, _| !keep.iter().any(|k| BStr::new(k) == path));
}

/// Map path → (id, mode, stat) for every entry of `index` (post-checkout stats).
fn stats_by_path(index: &gix::index::File) -> HashMap<BString, (ObjectId, Mode, Stat)> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode, e.stat)))
        .collect()
}

/// `setup_tracking()` with no explicit `--track`/`--no-track`: `branch.autoSetupMerge`
/// decides, and its default (`true`) means "track a remote-tracking start point".
///
/// * `false` — never.
/// * `true` (default) — when the start point is a remote-tracking branch.
/// * `always` — that, plus a local branch start point.
/// * `simple` — only a remote-tracking branch whose name matches the new branch's.
/// * `inherit` — copy the start branch's own upstream.
fn auto_tracking(repo: &gix::Repository, name: &str, start: &str) -> Result<Option<TrackInfo>> {
    let mode = repo
        .config_snapshot()
        .string("branch.autoSetupMerge")
        .map(|v| v.to_str_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| "true".into());
    if matches!(mode.as_str(), "false" | "no" | "off" | "0") {
        return Ok(None);
    }
    let Some(info) = resolve_tracking(repo, start)? else {
        return Ok(None);
    };
    // `remote == "."` is a local start point, which only `always` tracks.
    let is_remote = info.remote != ".";
    let keep = match mode.as_str() {
        "always" => true,
        "simple" => is_remote && info.dwim_name.as_deref() == Some(name),
        // `inherit` is about copying the *start branch's* upstream rather than pointing
        // at the start branch itself; that is not reproduced, so it behaves as the
        // default rather than guessing at a different upstream.
        _ => is_remote,
    };
    Ok(keep.then_some(info))
}

/// `report_tracking()`: the ahead/behind summary for the branch `HEAD` now points at,
/// printed after the switch line. Nothing at all when the branch has no upstream.
///
/// The text comes from the same renderer `status` uses, which appends the blank line
/// that separates it from the file lists there; a switch has nothing to separate from.
pub(crate) fn print_tracking_status(repo: &gix::Repository) {
    let block = super::status::tracking_block(repo);
    print!("{}", block.strip_suffix('\n').unwrap_or(&block));
}
