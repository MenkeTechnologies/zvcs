//! `git reset` — move `HEAD` (`--soft`/`--mixed`/`--hard`) and/or unstage paths.
//!
//! Served natively via the vendored gitoxide crates so tools on PATH observe the
//! same refs and index. Every ref/index/worktree mutation is serialized through
//! [`crate::lock::RepoLock`] and matches git's staging semantics.
//!
//! ## Supported forms
//!
//! * `reset --soft [<commit>]`  — move the current branch only (no index/worktree touch).
//! * `reset --hard [<commit>]`  — move the branch, then overwrite the index and worktree
//!   from the target tree (discarding local changes to tracked files). Prints
//!   `HEAD is now at <short> <summary>` unless `--quiet`.
//! * `reset [--mixed] [<commit>]` — move the branch and reset the index to the target
//!   tree, leaving the worktree, then refresh the index against the worktree and
//!   report what is still unstaged (see below).
//! * `reset [<commit>] [--] <paths>...` — reset the given pathspecs in the index back
//!   to the target tree's version (default `HEAD`), leaving the worktree. No `HEAD`
//!   move, no `ORIG_HEAD`, but the same index refresh and report.
//! * `reset --merge [<commit>]` / `reset --keep [<commit>]` — git's two-tree merge
//!   (`unpack-trees.c` `oneway_merge` / `twoway_merge`): move the branch and update
//!   the index and worktree toward the target, but preserve local changes to files
//!   the reset does not touch, and abort (exit 128, `error: Entry '<p>' not
//!   uptodate. Cannot merge.` + `fatal: Could not reset index file …`, HEAD
//!   unmoved) if a file that must change has un-committed local modifications.
//!
//! ## Resolving the target
//!
//! `cmd_reset()` (builtin/reset.c:405-425) has three exclusive arms and they do
//! not agree on what the operand may be:
//!
//! * **unborn** — `rev` is the word `HEAD` (typed or defaulted) and `HEAD` does
//!   not resolve. The target is the empty tree, and `reset_refs()` is skipped, so
//!   the index (and, for `--hard`, the worktree) is emptied while HEAD and
//!   `ORIG_HEAD` are left alone and no `HEAD is now at` line is printed. `--keep`
//!   is the one mode that cannot run: its two-tree merge wants HEAD's tree and
//!   reports `You do not have a valid HEAD.` instead.
//! * **whole tree** — `lookup_commit_reference()`, so the operand must peel to a
//!   *commit*; one that does not gets `error: object <operand> is a <kind>, not a
//!   commit` ahead of `fatal: Could not parse object '<rev>'.`
//! * **pathspec** — `repo_parse_tree_indirect()`, so the operand only has to peel
//!   to a *tree*: `git reset <tree> -- <path>` is the documented way to load
//!   paths out of a tree, and `HEAD^{tree}` works here where it is an error
//!   above. Its peel is handed a NULL name and so fails *silently*, leaving
//!   `Could not parse object` as the only line.
//!
//! ## The `Unstaged changes after reset:` report
//!
//! `cmd_reset()` (builtin/reset.c) ends a `MIXED` reset — which includes the path
//! form, since `reset_type` defaults to `MIXED` — by calling `refresh_index()` with
//! `REFRESH_IN_PORCELAIN` and the header `Unstaged changes after reset:`. That walks
//! every index entry, `lstat`s it, and prints `<status>\t<path>` for the ones that
//! disagree with the worktree, emitting the header lazily before the first hit
//! (`show_file()`, read-cache.c). Paths are written raw — `refresh_index` does not
//! quote them. Entries that only looked stale get their stat data refreshed instead.
//! Here the walk is `Repository::status()` restricted to the index↔worktree pass,
//! whose `EntryStatus` maps 1:1 onto git's `modified`/`deleted`/`typechange` formats,
//! and whose `NeedsUpdate` carries exactly the refreshed stat git would store.
//! `--quiet` and `--no-refresh` suppress the report, as does a bare repository.
//!
//! ## `--intent-to-add` / `-N` and `--pathspec-from-file`
//!
//! `-N` (MIXED only) is git's `update_index_from_diff()` intent-to-add path: any
//! index entry the reset would drop because it is absent from the target tree is
//! kept as an intent-to-add stub instead — mode `100644`, the empty-blob object id,
//! `CE_INTENT_TO_ADD` set — so the removed path stays tracked and re-appears in
//! `git diff`. Entries present in the target tree reset to it as usual. `-N` with a
//! non-MIXED mode dies `the option '-N' requires '--mixed'`.
//!
//! `--pathspec-from-file[=<file>]` / `--pathspec-file-nul` (git's
//! `parse_pathspec_from_file()`) read the pathspec list from a file (or stdin for
//! `-`), NUL- or newline-separated; they feed the same path form as inline
//! pathspecs and reject being combined with inline pathspecs.
//!
//! ## `--recurse-submodules[=<bool>]`
//!
//! git's `option_parse_recurse_submodules_worktree_updater()`. The value is a
//! plain boolean (`git_parse_maybe_bool`), so `--recurse-submodules=on-demand` is
//! rejected with `fatal: bad recurse-submodules argument: on-demand` during the
//! parse, and `submodule.recurse` supplies the default when neither the flag nor
//! its `--no-` form is given. It rides only on the worktree-updating modes —
//! `--hard`, `--merge`, `--keep` — because git routes it through
//! `unpack_trees()`, which `--soft`/`--mixed` never run. Each active, initialized
//! submodule whose worktree HEAD differs from the gitlink the reset just recorded
//! is moved to it (the shared [`super::checkout::maybe_recurse_submodules`]).
//!
//! ## The interactive-hunk options
//!
//! `-U`/`--unified <n>`, `--inter-hunk-context <n>` and `--[no-]auto-advance`
//! configure git's hunk selector and nothing else, but they are still observable
//! without `--patch`: parse-options validates their values as `OPT_INTEGER`
//! (`k`/`m`/`g` suffixes, `int` range check), and `cmd_reset()` then refuses any
//! non-default value with `fatal: '--unified' cannot be negative` or `fatal: the
//! option '<x>' requires '--patch'`. Both are reproduced, in git's order — after
//! `parse_args()` verifies the leading positional, before every other
//! compatibility check. See [`PatchDiffOpts`], shared with `git checkout`.
//!
//! ## Deferred
//!
//! `--patch`/`-p` (interactive hunk selection) is unsupported.
//!
//! The index's **cache-tree extension** is not written. git primes it from the
//! target tree after a `--mixed`/`--hard` (`prime_cache_tree()`, reset.c:108-111)
//! and lets `unpack_trees()` update it for `--merge`/`--keep`; every index writer
//! in this port instead drops it (`gix::index::File::remove_tree`), so the
//! extension is absent rather than stale. It is a cache, so no command reads a
//! wrong answer out of it — but `git fsck` notices when the tree git would have
//! primed is itself absent from the odb, which is what an unborn `reset --hard`
//! or `--merge` does: stock leaves a cache tree naming the empty tree and reports
//! `missing tree 4b825dc…`, this port reports nothing. Writing it properly is a
//! whole-index feature, not a reset one.

use crate::optint;
use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResetMode {
    Soft,
    Mixed,
    Hard,
    Merge,
    Keep,
}

impl ResetMode {
    fn label(self) -> &'static str {
        match self {
            ResetMode::Soft => "soft",
            ResetMode::Mixed => "mixed",
            ResetMode::Hard => "hard",
            ResetMode::Merge => "merge",
            ResetMode::Keep => "keep",
        }
    }
}

/// The three options `git reset`, `git checkout` and `git add` share for
/// configuring the interactive hunk selector (`add-patch.c`'s `add_p_opt`):
/// `-U`/`--unified <n>`, `--inter-hunk-context <n>` and `--[no-]auto-advance`.
///
/// None of them can change a byte of output on their own — they only take effect
/// once `--patch` runs the hunk selector — but they are *not* inert: after
/// parsing, git refuses the whole command when one of them was set to anything
/// but its default and patch mode is off. That refusal, and parse-options'
/// `OPT_INTEGER` value validation, are the entire observable behavior of these
/// options outside `--patch`, and both are reproduced here.
///
/// Shared by [`reset`] and [`super::checkout::checkout`] so the two parse and
/// diagnose them identically.
#[derive(Clone, Copy)]
pub(super) struct PatchDiffOpts {
    /// `-U`/`--unified <n>` — git's `add_p_opt.context`. `-1` means "unset";
    /// anything below `-1` is git's `cannot be negative` fatal.
    unified: i32,
    /// `--inter-hunk-context <n>` — git's `add_p_opt.interhunkcontext`, same
    /// `-1` sentinel.
    inter_hunk_context: i32,
    /// `--[no-]auto-advance` — git's `add_p_opt.auto_advance`, on by default, so
    /// only `--no-auto-advance` is ever observable outside `--patch`.
    auto_advance: bool,
    /// Whether this command's option table carries `OPT_ADD_AUTO_ADVANCE` at all.
    /// `git commit` does not (only `OPT_DIFF_UNIFIED` and
    /// `OPT_DIFF_INTERHUNK_CONTEXT`), so there `--auto-advance` must stay an
    /// unknown option rather than a silently accepted toggle.
    has_auto_advance: bool,
    /// Set while a value-taking option has consumed its flag but not yet its
    /// value: `Some((<name>, <short?>))`. parse-options takes the *next* argv
    /// element verbatim, whatever it looks like.
    pending: Option<(&'static str, bool)>,
}

impl Default for PatchDiffOpts {
    fn default() -> Self {
        Self {
            unified: -1,
            inter_hunk_context: -1,
            auto_advance: true,
            has_auto_advance: true,
            pending: None,
        }
    }
}

impl PatchDiffOpts {
    /// The same options without `--[no-]auto-advance`, for `git commit`, whose
    /// option table stops at `OPT_DIFF_UNIFIED` / `OPT_DIFF_INTERHUNK_CONTEXT`.
    pub(super) fn without_auto_advance() -> Self {
        Self { has_auto_advance: false, ..Self::default() }
    }

    /// True while a separate value is owed, so the caller must hand the next
    /// token to [`Self::take_arg`] even after a `--` end-of-options marker.
    pub(super) fn awaiting_value(&self) -> bool {
        self.pending.is_some()
    }

    /// Feed one argv token. `Ok(true)` means it belonged to these options and was
    /// consumed; `Ok(false)` means the caller keeps parsing it; `Err(code)` is the
    /// exit code of a parse-options diagnostic already written to stderr.
    pub(super) fn take_arg(&mut self, arg: &str) -> std::result::Result<bool, ExitCode> {
        if let Some((name, short)) = self.pending.take() {
            self.store(name, short, Some(arg))?;
            return Ok(true);
        }
        match arg {
            "-U" => self.pending = Some(("unified", true)),
            "--unified" => self.pending = Some(("unified", false)),
            "--inter-hunk-context" => self.pending = Some(("inter-hunk-context", false)),
            "--auto-advance" if self.has_auto_advance => self.auto_advance = true,
            "--no-auto-advance" if self.has_auto_advance => self.auto_advance = false,
            // Sticky value forms.
            _ if arg.starts_with("-U") && !arg.starts_with("--") => {
                self.store("unified", true, Some(&arg[2..]))?;
            }
            _ if arg.starts_with("--unified=") => {
                self.store("unified", false, Some(&arg["--unified=".len()..]))?;
            }
            _ if arg.starts_with("--inter-hunk-context=") => {
                self.store(
                    "inter-hunk-context",
                    false,
                    Some(&arg["--inter-hunk-context=".len()..]),
                )?;
            }
            // `--[no-]auto-advance` is a pure toggle; a `=value` is a usage error.
            _ if self.has_auto_advance && arg.starts_with("--auto-advance=") => {
                eprintln!("error: option `auto-advance' takes no value");
                return Err(ExitCode::from(129));
            }
            _ if self.has_auto_advance && arg.starts_with("--no-auto-advance=") => {
                eprintln!("error: option `no-auto-advance' takes no value");
                return Err(ExitCode::from(129));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// End of argv with a value still owed: parse-options' `requires a value`.
    pub(super) fn finish(&self) -> std::result::Result<(), ExitCode> {
        match self.pending {
            Some((name, short)) => {
                let label = if short {
                    optint::short_opt('U')
                } else {
                    optint::long_opt(name)
                };
                eprintln!("error: {label} requires a value");
                Err(ExitCode::from(129))
            }
            None => Ok(()),
        }
    }

    /// Validate and record one `-U`/`--inter-hunk-context` value through
    /// [`optint::integer`], which is `OPTION_INTEGER` — the diagnostics come out
    /// in git's own order: an absent value, then an empty one, then one the
    /// base-0 + `k`/`m`/`g` grammar rejects, then one outside the `int` range.
    fn store(
        &mut self,
        name: &'static str,
        short: bool,
        value: Option<&str>,
    ) -> std::result::Result<(), ExitCode> {
        let label = if short {
            optint::short_opt('U')
        } else {
            optint::long_opt(name)
        };
        let Some(raw) = value else {
            eprintln!("error: {label} requires a value");
            return Err(ExitCode::from(129));
        };
        // `OPTION_INTEGER` over an `int`: the empty value, the base-0 + k/m/g
        // grammar and the range clause all come with git's own wording.
        let narrowed = match optint::integer(&label, raw) {
            Ok(v) => v as i32,
            Err(e) => {
                eprintln!("error: {}", e.message());
                return Err(ExitCode::from(129));
            }
        };
        if name == "unified" {
            self.unified = narrowed;
        } else {
            self.inter_hunk_context = narrowed;
        }
        Ok(())
    }

    /// git's post-parse refusal block, in git's fixed order (verified against git
    /// 2.55.0): both `cannot be negative` checks first, then the three
    /// `requires '--patch'` checks. `Some(code)` means the diagnostic has been
    /// written and the command must exit with it.
    ///
    /// `patch` is whether `--patch` is in effect; this port never enters the hunk
    /// selector, so callers pass `false` and every non-default value is refused
    /// exactly as stock git refuses it.
    pub(super) fn require_patch(&self, patch: bool) -> Option<ExitCode> {
        self.require_patch_named(patch, "--patch")
    }

    /// [`Self::require_patch`] with the name git cites for "patch mode" in this
    /// command: `--patch` for `reset`/`checkout`, `--interactive/--patch` for
    /// `add` and `commit`, whose `-i` reaches the same hunk selector.
    pub(super) fn require_patch_named(&self, patch: bool, what: &str) -> Option<ExitCode> {
        // `?` would be exactly backwards here: `reject_negative()` returning
        // `None` means *no* negative value was given, which is the case that has
        // to fall through to the `requires` half rather than end the check.
        if let Some(code) = self.reject_negative() {
            return Some(code);
        }
        self.require_patch_only(patch, what)
    }

    /// The `cannot be negative` half of [`Self::require_patch_named`], for
    /// `git commit`, which runs it at the top of `prepare_index()` — several
    /// pathspec refusals ahead of the `requires` half.
    pub(super) fn reject_negative(&self) -> Option<ExitCode> {
        if self.unified < -1 {
            eprintln!("fatal: '--unified' cannot be negative");
            return Some(ExitCode::from(128));
        }
        if self.inter_hunk_context < -1 {
            eprintln!("fatal: '--inter-hunk-context' cannot be negative");
            return Some(ExitCode::from(128));
        }
        None
    }

    /// The `requires '<what>'` half of [`Self::require_patch_named`].
    pub(super) fn require_patch_only(&self, patch: bool, what: &str) -> Option<ExitCode> {
        if patch {
            return None;
        }
        let opt = if self.unified != -1 {
            "--unified"
        } else if self.inter_hunk_context != -1 {
            "--inter-hunk-context"
        } else if !self.auto_advance {
            "--no-auto-advance"
        } else {
            return None;
        };
        eprintln!("fatal: the option '{opt}' requires '{what}'");
        Some(ExitCode::from(128))
    }

    /// The values, once validated, as the hunk selector's [`Options`].
    ///
    /// [`Options`]: super::add_patch::Options
    pub(super) fn to_interactive(self, disallow_edit: bool) -> super::add_patch::Options {
        super::add_patch::Options {
            context: self.unified,
            interhunk: self.inter_hunk_context,
            auto_advance: self.auto_advance,
            disallow_edit,
        }
    }
}

/// `cmd_reset()`'s `struct option options[]` (builtin/reset.c), in table order, as
/// [`super::resolve_long`] reads it.
///
/// The five mode selectors (`--mixed`, `--soft`, `--hard`, `--merge`, `--keep`)
/// are `OPT_SET_INT_F ... PARSE_OPT_NONEG`, and `--unified` /
/// `--inter-hunk-context` are `PARSE_OPT_NONEG` too, so none of the seven has a
/// `--no-` spelling. `no-refresh` is an entry spelled with its own `no-`, which
/// parse-options reads as the *unset* sense of `refresh`.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "quiet",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "no-refresh",                  neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "mixed",                       neg: false, arg: super::Arg::None },
    super::LongOpt { name: "soft",                        neg: false, arg: super::Arg::None },
    super::LongOpt { name: "hard",                        neg: false, arg: super::Arg::None },
    super::LongOpt { name: "merge",                       neg: false, arg: super::Arg::None },
    super::LongOpt { name: "keep",                        neg: false, arg: super::Arg::None },
    super::LongOpt { name: "recurse-submodules",          neg: true,  arg: super::Arg::Optional },
    super::LongOpt { name: "patch",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "auto-advance",                neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "unified",                     neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "inter-hunk-context",          neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "intent-to-add",               neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "pathspec-from-file",          neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "pathspec-file-nul",           neg: true,  arg: super::Arg::None },
];

/// `git reset -h` / the block parse-options prints on a bad option, verbatim.
const USAGE: &str = "\
usage: git reset [--mixed | --soft | --hard | --merge | --keep] [-q] [<commit>]
   or: git reset [-q] [<tree-ish>] [--] <pathspec>...
   or: git reset [-q] [--pathspec-from-file [--pathspec-file-nul]] [<tree-ish>]
   or: git reset --patch [<tree-ish>] [--] [<pathspec>...]

    -q, --[no-]quiet      be quiet, only report errors
    --no-refresh          skip refreshing the index after reset
    --refresh             opposite of --no-refresh
    --mixed               reset HEAD and index
    --soft                reset only HEAD
    --hard                reset HEAD, index and working tree
    --merge               reset HEAD, index and working tree
    --keep                reset HEAD but keep local changes
    --[no-]recurse-submodules[=<reset>]
                          control recursive updating of submodules
    -p, --[no-]patch      select hunks interactively
    --[no-]auto-advance   auto advance to the next file when selecting hunks interactively
    -U, --unified <n>     generate diffs with <n> lines context
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    -N, --[no-]intent-to-add
                          record only the fact that removed paths will be added later
    --[no-]pathspec-from-file <file>
                          read pathspec from file
    --[no-]pathspec-file-nul
                          with --pathspec-from-file, pathspec elements are separated with NUL character

";

/// `fatal: ambiguous argument ...` — `die()` from `verify_filename()` (setup.c) when a
/// leading positional is neither a revision nor an existing worktree path.
fn ambiguous_argument(arg: &str) -> ExitCode {
    eprintln!("fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.");
    eprintln!("Use '--' to separate paths from revisions, like this:");
    eprintln!("'git <command> [<revision>...] -- [<file>...]'");
    ExitCode::from(128)
}

pub fn reset(args: &[String]) -> Result<ExitCode> {
    // Every `reset` that moves HEAD writes a reflog line, and the reflog writer
    // (`log_ref_write_fd()`, refs/files-backend.c:1940-41) fills a missing
    // committer with `git_committer_info(0)` — flag 0, so `fmt_ident()` runs
    // *non-strict* and synthesizes the ident from the account and host instead
    // of dying. Only object-writing commands pass `IDENT_STRICT` and refuse.
    // `checkout`/`branch`/`fetch`/… already fill this gap; `reset` did not, so
    // it failed on any machine with no `user.name`/`user.email`.
    let mut repo = gix::discover(".")?;
    crate::ensure_reflog_identity(&mut repo);
    let repo = repo;

    // ---- 1. Parse flags, honoring the `--` paths separator. ----
    let mut mode: Option<ResetMode> = None;
    let mut quiet = false;
    let mut refresh = true;
    let mut saw_dd = false;
    let mut intent_to_add = false;
    let mut pathspec_from_file: Option<String> = None;
    let mut pathspec_file_nul = false;
    let mut take_pff_value = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    // `-U`/`--unified`, `--inter-hunk-context`, `--[no-]auto-advance`: the
    // interactive-hunk-selector options, refused after the argument split below
    // exactly as git refuses them without `--patch`.
    let mut patch_opts = PatchDiffOpts::default();
    // `-p`/`--patch`: unstage hunks interactively instead of whole paths.
    let mut patch_mode = false;
    // `--recurse-submodules[=<bool>]` / `--no-recurse-submodules`. `None` = fall
    // back to `submodule.recurse`; `Some(b)` = explicit flag. Only the
    // worktree-updating modes (`--hard`, `--merge`, `--keep`) can move a
    // submodule, matching git's `unpack_trees()` submodule updater.
    let mut recurse_submodules: Option<bool> = None;

    for typed in args {
        let a = typed;
        // `--pathspec-from-file <file>` (separate-argument form): parse-options
        // consumes the very next token as the value regardless of what it looks like.
        if take_pff_value {
            pathspec_from_file = Some(a.clone());
            take_pff_value = false;
            continue;
        }
        // Likewise for a `-U`/`--unified`/`--inter-hunk-context` value still owed —
        // and precisely because it is a value, it is never resolved as an option name.
        if patch_opts.awaiting_value() {
            match patch_opts.take_arg(a) {
                Err(code) => return Ok(code),
                Ok(true) => continue,
                Ok(false) => {}
            }
        }
        if saw_dd {
            paths.push(a.clone());
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt() and so ahead of the unknown-option refusal
        // below: the name never abbreviates and never takes an `=<value>`. This
        // table has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the
        // same block `-h` prints.
        if a == "--help-all" {
            return Ok(super::show_usage(USAGE));
        }
        // Respell a unique abbreviation as the name it resolves to, ahead of both
        // the shared value-option handler and the match below, so `--intent-to`
        // reaches the same arm as `--intent-to-add`.
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
        match patch_opts.take_arg(a) {
            Err(code) => return Ok(code),
            Ok(true) => continue,
            Ok(false) => {}
        }
        match a {
            "--" => saw_dd = true,
            "--soft" => mode = Some(ResetMode::Soft),
            "--mixed" => mode = Some(ResetMode::Mixed),
            "--hard" => mode = Some(ResetMode::Hard),
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            "--refresh" => refresh = true,
            "--no-refresh" => refresh = false,
            "--merge" => mode = Some(ResetMode::Merge),
            "--keep" => mode = Some(ResetMode::Keep),
            "-p" | "--patch" => patch_mode = true,
            "--no-patch" => patch_mode = false,
            "-N" | "--intent-to-add" => intent_to_add = true,
            "--no-intent-to-add" => intent_to_add = false,
            "--pathspec-from-file" => take_pff_value = true,
            "--no-pathspec-from-file" => pathspec_from_file = None,
            "--pathspec-file-nul" => pathspec_file_nul = true,
            "--no-pathspec-file-nul" => pathspec_file_nul = false,
            s if s.starts_with("--pathspec-from-file=") => {
                pathspec_from_file = Some(s["--pathspec-from-file=".len()..].to_string());
            }
            // `--recurse-submodules[=<bool>]`: git's
            // `option_parse_recurse_submodules_worktree_updater()`, whose value is a
            // plain boolean (`parse_update_recurse_submodules_arg()` →
            // `git_parse_maybe_bool`), so `on-demand` is as invalid as any other
            // non-boolean and dies inside the parse loop.
            "--recurse-submodules" => recurse_submodules = Some(true),
            "--no-recurse-submodules" => recurse_submodules = Some(false),
            s if s.starts_with("--recurse-submodules=") => {
                let val = &s["--recurse-submodules=".len()..];
                match optint::maybe_bool(val) {
                    Some(b) => recurse_submodules = Some(b),
                    None => {
                        eprintln!("fatal: bad recurse-submodules argument: {val}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &other[2..]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // parse_options_step()'s `internal_help` check: `-h` answers on
            // stdout at 129, with no `error:` line — it is not a rejection.
            "-h" => return Ok(super::show_usage(USAGE)),
            other if other.starts_with('-') && other != "-" => {
                let sw = other.chars().nth(1).unwrap_or('-');
                eprintln!("error: unknown switch `{sw}'");
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // A non-option argument is handed back unchanged by the resolver, so the
            // argv slice itself is pushed and the operand keeps `args`' lifetime.
            _ => positionals.push(typed),
        }
    }

    // parse-options rejects a dangling value-taking option with exit 129.
    if take_pff_value {
        eprintln!("error: option `pathspec-from-file' requires a value");
        return Ok(ExitCode::from(129));
    }
    if let Err(code) = patch_opts.finish() {
        return Ok(code);
    }

    // An explicit `--[no-]recurse-submodules` wins over `submodule.recurse`, which
    // git reads into the same `config_update_recurse_submodules` slot.
    let recurse_submodules = recurse_submodules
        .unwrap_or_else(|| repo.config_snapshot().boolean("submodule.recurse") == Some(true));

    // ---- 2. Split positionals into an optional <commit> and pathspecs. ----
    // With `--`, a lone token before it is the commit; everything after is a path.
    // Without `--`, git takes the first positional as <commit> iff it resolves as a
    // revision; otherwise it must name an existing worktree path (`verify_filename()`),
    // and the remainder are pathspecs that go unverified.
    let mut commit_spec: Option<&str> = None;
    let mut unverified: Option<&str> = None;
    if saw_dd {
        match positionals.as_slice() {
            [] => {}
            [c] => commit_spec = Some(*c),
            _ => crate::git_fatal!("too many revisions given before `--`"),
        }
    } else if let Some((first, rest)) = positionals.split_first() {
        // `parse_args()` asks `repo_get_oid_{committish,treeish}()`, which reach
        // `get_oid_basic()` — and its first branch decodes a full-length hex name
        // *without consulting the odb* (see [`crate::objname`]). So a well-formed
        // but absent id is a <rev> here, not a path: git goes on to die with
        // `Could not parse object` below rather than treating the token as a
        // filename. `rev_parse_single()` alone resolves through the odb and would
        // misfile it as a pathspec, producing `ambiguous argument` instead.
        if crate::objname::resolve(&repo, first).is_some() {
            commit_spec = Some(*first);
            paths.extend(rest.iter().map(|s| s.to_string()));
        } else {
            unverified = Some(*first);
            paths.extend(positionals.iter().map(|s| s.to_string()));
        }
    }

    // `check_filename()` is a bare `lstat` probe: a tracked-but-deleted path fails it
    // just like a typo'd revision does, and both die before anything is touched.
    if let Some(first) = unverified {
        if std::fs::symlink_metadata(first).is_err() {
            return Ok(ambiguous_argument(first));
        }
    }

    // git collects the hunk-selector options into `add_p_opt` and refuses them
    // here — after `parse_args()` has verified the leading positional (so a bad
    // revision still reports `ambiguous argument` first) and before every other
    // compatibility check, including `Cannot do soft reset with paths.` and
    // `the option '-N' requires '--mixed'` (verified against git 2.55.0).
    if let Some(code) = patch_opts.require_patch(patch_mode) {
        return Ok(code);
    }

    // `-p`: `git reset -p [<tree-ish>] [--] [<pathspec>...]` picks hunks to
    // unstage — `ADD_P_RESET`, whose diff is `diff-index --cached <rev>` and
    // whose apply is `apply -R --cached`. `<rev>` defaults to HEAD; any other
    // tree-ish flips the mode to the forward `patch_mode_reset_nothead`.
    if patch_mode {
        if mode.is_some() {
            eprintln!("fatal: options '--patch' and '--{{hard,mixed,soft}}' cannot be used together");
            return Ok(ExitCode::from(128));
        }
        let revision = commit_spec.unwrap_or("HEAD").to_string();
        return super::add_patch::run(
            &repo,
            super::add_patch::Mode::Reset,
            Some(&revision),
            patch_opts.to_interactive(false),
            &paths,
        );
    }

    // `parse_pathspec_from_file()` (builtin/reset.c): a NUL separator needs the file
    // option; the file list and inline pathspecs are mutually exclusive; then the
    // file/stdin is split into pathspecs that join the path form.
    if pathspec_file_nul && pathspec_from_file.is_none() {
        eprintln!("fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'");
        return Ok(ExitCode::from(128));
    }
    if let Some(f) = pathspec_from_file {
        if !paths.is_empty() {
            eprintln!("fatal: '--pathspec-from-file' and pathspec arguments cannot be used together");
            return Ok(ExitCode::from(128));
        }
        let data = if f == "-" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
            buf
        } else {
            std::fs::read(&f)?
        };
        let sep = if pathspec_file_nul { b'\0' } else { b'\n' };
        for part in data.split(|&c| c == sep) {
            let mut s = part;
            if !pathspec_file_nul && s.last() == Some(&b'\r') {
                s = &s[..s.len() - 1];
            }
            if s.is_empty() {
                continue;
            }
            paths.push(String::from_utf8_lossy(s).into_owned());
        }
    }

    let with_paths = !paths.is_empty();
    if with_paths {
        if let Some(m @ (ResetMode::Soft | ResetMode::Hard | ResetMode::Merge | ResetMode::Keep)) =
            mode
        {
            eprintln!("fatal: Cannot do {} reset with paths.", m.label());
            return Ok(ExitCode::from(128));
        }
        if mode == Some(ResetMode::Mixed) {
            eprintln!("warning: --mixed with paths is deprecated; use 'git reset -- <paths>' instead.");
        }
    }

    let mode = mode.unwrap_or(ResetMode::Mixed);

    // Bare-repository refusals, in `cmd_reset()`'s order (builtin/reset.c:470-478).
    //
    // * `setup_work_tree()` runs for every mode but SOFT, and for MIXED only
    //   when a work tree exists — so in a bare repository `--hard`, `--merge`
    //   and `--keep` die there with the generic work-tree message.
    // * MIXED (including the default and the pathspec form, both of which have
    //   already defaulted to MIXED above) reaches the explicit
    //   `is_bare_repository()` check and dies naming the mode.
    // * `--soft` needs neither, and is the one mode that works in a bare repo.
    //
    // Both precede the `-N` check below, which is why `git reset -N --hard` in
    // a bare repository reports the work tree and not the option.
    if repo.workdir().is_none() {
        match mode {
            ResetMode::Soft => {}
            ResetMode::Mixed => {
                crate::git_fatal!("{} reset is not allowed in a bare repository", mode.label())
            }
            ResetMode::Hard | ResetMode::Merge | ResetMode::Keep => {
                crate::git_fatal!("this operation must be run in a work tree")
            }
        }
    }

    // `-N` rides only on a MIXED reset (the with-paths guard above already fired for
    // the non-MIXED path form, so this catches the whole-tree `--soft/--hard/… -N`).
    if intent_to_add && mode != ResetMode::Mixed {
        eprintln!("fatal: the option '-N' requires '--mixed'");
        return Ok(ExitCode::from(128));
    }

    let reflog_spec = commit_spec.unwrap_or("HEAD");
    // `cmd_reset()` (builtin/reset.c:405-425) picks the target in three exclusive
    // arms, and the arm decides both what `<rev>` is allowed to *be* and what is
    // printed when it is not:
    //
    // ```c
    // unborn = !strcmp(rev, "HEAD") && repo_get_oid(the_repository, "HEAD", &unused);
    // if (unborn) {
    //         /* reset on unborn branch: treat as reset to empty tree */
    //         oidcpy(&oid, the_repository->hash_algo->empty_tree);
    // } else if (!pathspec.nr && !patch_mode) {
    //         struct commit *commit;
    //         if (repo_get_oid_committish(the_repository, rev, &oid))
    //                 die(_("Failed to resolve '%s' as a valid revision."), rev);
    //         commit = lookup_commit_reference(the_repository, &oid);
    //         if (!commit)
    //                 die(_("Could not parse object '%s'."), rev);
    //         oidcpy(&oid, &commit->object.oid);
    // } else {
    //         struct tree *tree;
    //         if (repo_get_oid_treeish(the_repository, rev, &oid))
    //                 die(_("Failed to resolve '%s' as a valid tree."), rev);
    //         tree = repo_parse_tree_indirect(the_repository, &oid);
    //         if (!tree)
    //                 die(_("Could not parse object '%s'."), rev);
    //         oidcpy(&oid, &tree->object.oid);
    // }
    // ```
    //
    // The pathspec arm wants a *tree*, not a commit: `git reset <tree> -- <path>`
    // is the documented way to load paths out of a tree, so `HEAD^{tree}` there
    // succeeds where the same operand in the whole-tree arm is an error. Peeling
    // to a commit in both arms turns the working form into a failure.
    //
    // Each `die()` above is preceded by whatever the helper already printed, and
    // the two helpers differ: `lookup_commit_reference()` reports a present
    // object of the wrong type with `error: object %s is a %s, not a %s`, while
    // `repo_parse_tree_indirect()` passes a NULL name into `repo_peel_to_type()`
    // and so fails *silently* — which is why a blob operand prints two lines in
    // the whole-tree form and one in the pathspec form.
    let unborn = reflog_spec == "HEAD" && crate::objname::resolve(&repo, "HEAD").is_none();

    // The commit `HEAD` is moved to, present only in the whole-tree arm: the
    // pathspec form never moves a ref, and an unborn branch has no commit to
    // move to (`!pathspec.nr && !unborn` gates `reset_refs()` below).
    let mut head_commit: Option<gix::Commit<'_>> = None;
    let target_tree = if unborn {
        gix::ObjectId::empty_tree(repo.object_hash())
    } else {
        // The lookup can only fail for a spec that stood before a `--`, since
        // without one the split above already resolved it. `get_oid_basic()`
        // decodes a full-length hex name without consulting the odb (see
        // [`crate::objname`]), so a well-formed but absent id gets past here and
        // falls into the peel's `Could not parse object` instead.
        let target_id = match crate::objname::resolve(&repo, reflog_spec) {
            Some(id) => id,
            None => crate::git_fatal!(
                "Failed to resolve '{reflog_spec}' as a valid {}.",
                if with_paths { "tree" } else { "revision" }
            ),
        };
        if with_paths {
            // `repo_parse_tree_indirect()` is `repo_peel_to_type(r, NULL, 0, obj,
            // OBJ_TREE)`: tags dereference to their target, commits to their
            // tree, a tree is the answer, and a blob is a silent NULL.
            let peeled = repo.find_object(target_id).ok().and_then(|o| o.peel_to_tree().ok());
            match peeled {
                Some(tree) => tree.id,
                None => crate::git_fatal!("Could not parse object '{reflog_spec}'."),
            }
        } else {
            match crate::objname::lookup_commit_reference(&repo, target_id) {
                crate::objname::CommitRef::Commit(id) => {
                    let commit = repo.find_object(id)?.into_commit();
                    let tree = commit.tree_id()?.detach();
                    head_commit = Some(commit);
                    tree
                }
                // `error: object %s is a %s, not a %s` names the *operand* id and
                // the *peeled* type, so a tag of a tree reports the tag's id and
                // the word `tree`; [`crate::objname::CommitRef`] already carries
                // that pairing.
                other => {
                    if let Some(note) = other.type_error() {
                        eprintln!("error: {note}");
                    }
                    crate::git_fatal!("Could not parse object '{reflog_spec}'.")
                }
            }
        }
    };

    // Serialize the whole read-modify-write; held for the rest of the function.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Capture the pre-reset index (worktree stats + tracked set) before mutating.
    let old_index = repo.index_or_load_from_head_or_empty()?.into_owned();

    // ```c
    // if (reset_type == SOFT || reset_type == KEEP)
    //         die_if_unmerged_cache(reset_type);
    // ```
    // (builtin/reset.c:486.) Neither mode can collapse the stages an unfinished merge left, so
    // both refuse while `MERGE_HEAD` is there or the index still holds one — naming the mode.
    if matches!(mode, ResetMode::Soft | ResetMode::Keep) {
        let merging = repo.git_dir().join("MERGE_HEAD").exists();
        let unmerged = old_index
            .entries()
            .iter()
            .any(|e| e.stage() != gix::index::entry::Stage::Unconflicted);
        if merging || unmerged {
            crate::git_fatal!("Cannot do a {} reset in the middle of a merge.", mode.label());
        }
    }

    // ---- 3. Path form: reset the named index entries only; no HEAD move. ----
    if with_paths {
        let mut index = pathspec_index(&repo, &old_index, target_tree, &paths, intent_to_add)?;
        finish_mixed(&repo, &old_index, &mut index, quiet, refresh)?;
        return Ok(ExitCode::SUCCESS);
    }

    // ---- 4. Whole-tree form. ----
    // `--merge`/`--keep` run git's two-tree merge (`unpack-trees.c`), which may
    // abort on local changes. git does not move HEAD when it aborts, so the merge
    // is computed and applied *before* any ref is touched.
    if matches!(mode, ResetMode::Merge | ResetMode::Keep) {
        // `reset_index()`'s KEEP arm needs HEAD's tree as the merge's first side
        // and reports before the caller's `die()` when there is none
        // (builtin/reset.c:97-100):
        //
        // ```c
        // if (reset_type == KEEP) {
        //         struct object_id head_oid;
        //         if (repo_get_oid(the_repository, "HEAD", &head_oid))
        //                 return error(_("You do not have a valid HEAD."));
        // ```
        //
        // MERGE takes no such side, so an unborn branch reaches `unpack_trees()`
        // and fails (or not) on the worktree state alone.
        let head_tree = match repo.head_id() {
            Ok(h) => h.object()?.peel_to_commit()?.tree_id()?.detach(),
            Err(_) if mode == ResetMode::Keep => {
                eprintln!("error: You do not have a valid HEAD.");
                eprintln!("fatal: Could not reset index file to revision '{reflog_spec}'.");
                return Ok(ExitCode::from(128));
            }
            Err(_) => gix::ObjectId::empty_tree(repo.object_hash()),
        };
        let should_interrupt = AtomicBool::new(false);
        let applied = reset_two_tree(
            &repo,
            &old_index,
            head_tree,
            target_tree,
            mode == ResetMode::Keep,
            &should_interrupt,
        )?;
        if !applied {
            // git's `reset_index` failure: `fatal:` line, exit 128, HEAD untouched.
            // The message names `rev` — the spec as typed — not the id it resolved
            // to (`die(_("Could not reset index file to revision '%s'."), rev)`).
            eprintln!("fatal: Could not reset index file to revision '{reflog_spec}'.");
            return Ok(ExitCode::from(128));
        }
        // ```c
        // err = reset_index(ref, &oid, reset_type, quiet);
        // if (reset_type == KEEP && !err)
        //         err = reset_index(ref, &oid, MIXED, quiet);
        // ```
        // (builtin/reset.c:522-524.) `--keep` resets the index a *second* time, as a plain
        // one-way MIXED pass, so an entry in neither tree — a newly staged file — is unstaged
        // rather than carried across by `twoway_merge`'s keep-the-current-entry case. Without it
        // `git reset --keep` left `A staged.txt` staged where stock leaves it untracked.
        if mode == ResetMode::Keep {
            let current = repo.open_index()?;
            let mut index = reset_index_to_tree(&repo, &current, target_tree, false)?;
            // That second pass is `reset_index()`, not `read_from_tree()`, so it ends in
            // `prime_cache_tree(the_repository, index, tree)` (builtin/reset.c:120-127): the
            // index it writes carries a cache-tree built from the target tree itself.
            index.prime_cache_tree(&repo.objects, &target_tree)?;
            crate::index_racy::write(&repo, &mut index)?;
        }
        // `if (!pathspec.nr && !unborn)`: an unborn branch has no ref to move and
        // no previous HEAD to save, so `reset_refs()` is skipped outright.
        if let Some(commit) = &head_commit {
            if let Ok(prev) = repo.head_id() {
                set_orig_head(&repo, prev.detach())?;
            }
            move_head(&repo, commit.id, reflog_spec)?;
        }
        remove_branch_state(&repo);
        super::checkout::maybe_recurse_submodules(&repo, recurse_submodules, true)?;
        // No `HEAD is now at` here: `cmd_reset()` gates `print_new_head_line()`
        // on `reset_type == HARD`, so `--merge` and `--keep` move the branch in
        // silence even though they touch the worktree.
        return Ok(ExitCode::SUCCESS);
    }

    // soft/mixed/hard: `reset_refs()` records the pre-reset HEAD in ORIG_HEAD
    // before moving HEAD, and `remove_branch_state()` drops any in-progress
    // merge/cherry-pick/revert state. `reset_refs()` is gated on `!unborn`, so an
    // unborn branch keeps both HEAD and ORIG_HEAD as they were; `remove_branch_state()`
    // is not gated and runs for every whole-tree reset.
    if let Some(commit) = &head_commit {
        if let Ok(prev) = repo.head_id() {
            set_orig_head(&repo, prev.detach())?;
        }
        move_head(&repo, commit.id, reflog_spec)?;
    }
    remove_branch_state(&repo);

    match mode {
        ResetMode::Soft => {}
        ResetMode::Mixed => {
            let mut index = reset_index_to_tree(&repo, &old_index, target_tree, intent_to_add)?;
            finish_mixed(&repo, &old_index, &mut index, quiet, refresh)?;
        }
        ResetMode::Hard => {
            let should_interrupt = AtomicBool::new(false);
            reset_worktree_hard(&repo, &old_index, target_tree, &should_interrupt)?;
            // `--recurse-submodules` only reaches the worktree-updating modes: git
            // routes it through `unpack_trees()`, which `--soft`/`--mixed` never run.
            // The move itself is silent in git, hence the unconditional quiet flag.
            super::checkout::maybe_recurse_submodules(&repo, recurse_submodules, true)?;
            // `print_new_head_line()` sits inside the same `!unborn` guard as
            // `reset_refs()`, so an unborn `--hard` empties the worktree silently.
            if !quiet {
                if let Some(commit) = &head_commit {
                    let summary = commit.message()?.summary().into_owned();
                    println!("HEAD is now at {} {}", commit.short_id()?, summary);
                }
            }
        }
        // Handled above, before the ref move.
        ResetMode::Merge | ResetMode::Keep => unreachable!(),
    }

    Ok(ExitCode::SUCCESS)
}

/// Point `HEAD` (or the branch it references) at `target`, writing a reflog entry
/// `reset: moving to <spec>` on both refs, exactly as `git reset` does.
fn move_head(repo: &gix::Repository, target: ObjectId, spec: &str) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("reset: moving to {spec}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(target),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: true,
    })?;
    Ok(())
}

/// Point `ORIG_HEAD` at `id`, as `reset_refs()` does before `HEAD` moves.
///
/// Shared with [`crate::porcelain::stash`]: `do_push_stash()` clears the
/// worktree by running `git reset --hard`, so a push inherits this side effect
/// rather than having its own.
pub(crate) fn set_orig_head(repo: &gix::Repository, id: ObjectId) -> Result<()> {
    let name: FullName = "ORIG_HEAD"
        .try_into()
        .map_err(|e| anyhow!("invalid ref name ORIG_HEAD: {e}"))?;
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

/// The state files `remove_branch_state()` (branch.c) unlinks after a whole-tree
/// reset.
///
/// `remove_branch_state()` opens with `sequencer_post_commit_cleanup()`, which
/// drops the pseudo-refs but takes `.git/sequencer` **only** when a pick was in
/// progress *and* `have_finished_the_last_pick()` — a todo list of one line.
/// Removing it unconditionally would break `git cherry-pick --skip`, which is
/// `git reset --merge HEAD` followed by resuming the very todo list this would
/// have deleted.
fn remove_branch_state(repo: &gix::Repository) {
    let git_dir = repo.git_dir();
    let _ = crate::sequencer::post_commit_cleanup(repo);
    for name in ["MERGE_HEAD", "MERGE_RR", "MERGE_MSG", "MERGE_MODE", "SQUASH_MSG"] {
        let _ = std::fs::remove_file(git_dir.join(name));
    }
}

/// `REFRESH_INDEX_DELAY_WARNING_IN_MS` (builtin/reset.c): two seconds.
const REFRESH_INDEX_DELAY_WARNING_IN_MS: u64 = 2 * 1000;

/// Close out a `MIXED` reset: refresh the index against the worktree, report what is
/// still unstaged, then persist. `--quiet`, `--no-refresh` and bare repositories skip
/// the refresh, exactly as `cmd_reset()` does.
fn finish_mixed(
    repo: &gix::Repository,
    old_index: &gix::index::File,
    index: &mut gix::index::File,
    quiet: bool,
    refresh: bool,
) -> Result<()> {
    if !quiet && refresh && repo.workdir().is_some() {
        // `cmd_reset` times the refresh and, past two seconds, points at the
        // `--no-refresh` that would have skipped it.
        let t0 = std::time::Instant::now();
        refresh_index_report(repo, index)?;
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        if elapsed_ms > REFRESH_INDEX_DELAY_WARNING_IN_MS
            && crate::advice::Advice::ResetNoRefresh.enabled_in(repo)
        {
            crate::advice::Advice::ResetNoRefresh.advise_plain_in(
                repo,
                &format!(
                    "It took {:.2} seconds to refresh the index after reset.  You can use\n\
                     '--no-refresh' to avoid this.",
                    elapsed_ms as f64 / 1000.0
                ),
            );
        }
    }
    // A `--mixed` reset never repairs: `cmd_reset()` routes it through
    // `read_from_tree()` (builtin/reset.c:494), which stages the differences entry by
    // entry and so invalidates only the paths that actually moved.
    super::write_tree::carry_cache_tree_invalidating_changes(repo, old_index, index);
    crate::index_racy::write(repo, index)?;
    Ok(())
}

/// `refresh_index(..., REFRESH_IN_PORCELAIN, "Unstaged changes after reset:")`.
///
/// Prints one `<status>\t<path>` line per index entry that disagrees with the
/// worktree, under a header emitted lazily before the first line, and folds the
/// refreshed stat data of merely-stale entries back into `index`. Paths are written
/// as raw bytes because `refresh_index` does no quoting.
fn refresh_index_report(repo: &gix::Repository, index: &mut gix::index::File) -> Result<()> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change as Wt, EntryStatus};

    let mut changed: Vec<(BString, &'static str)> = Vec::new();
    let mut fresh: HashMap<BString, Stat> = HashMap::new();

    let iter = repo
        .status(gix::progress::Discard)?
        .index(gix::worktree::IndexPersistedOrInMemory::InMemory(index.clone()))
        .untracked_files(gix::status::UntrackedFiles::None)
        .index_worktree_options_mut(|opts| opts.dirwalk_options = None)
        .into_index_worktree_iter(Vec::new())?;

    for item in iter {
        if let Item::Modification { rela_path, status, .. } = item? {
            // read-cache.c picks the format string in this order: deleted, then
            // intent-to-add, then typechange, then modified; unmerged entries are
            // reported as `U` because reset does not pass REFRESH_UNMERGED.
            let code = match status {
                EntryStatus::Change(Wt::Removed) => "D",
                EntryStatus::IntentToAdd => "A",
                EntryStatus::Change(Wt::Type { .. }) => "T",
                EntryStatus::Change(Wt::Modification { .. })
                | EntryStatus::Change(Wt::SubmoduleModification(_)) => "M",
                EntryStatus::Conflict { .. } => "U",
                EntryStatus::NeedsUpdate(stat) => {
                    fresh.insert(rela_path, stat);
                    continue;
                }
            };
            changed.push((rela_path, code));
        }
    }

    // git walks the index, which is sorted by path; the status iterator is not.
    changed.sort_by(|a, b| a.0.cmp(&b.0));
    if !changed.is_empty() {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(b"Unstaged changes after reset:\n")?;
        for (path, code) in &changed {
            out.write_all(code.as_bytes())?;
            out.write_all(b"\t")?;
            out.write_all(&path[..])?;
            out.write_all(b"\n")?;
        }
        out.flush()?;
    }

    if !fresh.is_empty() {
        let backing = index.path_backing().to_owned();
        for e in index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = fresh.get(&path) {
                e.stat = *stat;
            }
        }
    }

    Ok(())
}

/// git's `set_object_name_for_intent_to_add_entry()` (read-cache.c:704), which
/// `update_index_from_diff()` runs for every `-N` stub it makes:
///
/// ```c
/// struct object_id oid;
/// if (odb_write_object(the_repository->objects, "", 0, OBJ_BLOB, &oid))
///         die(_("cannot create an empty blob in the object database"));
/// oidcpy(&ce->oid, &oid);
/// ```
///
/// The id is always the well-known empty-blob hash, but the call *writes* it:
/// a repository whose reset produced an intent-to-add entry has the empty blob
/// loose in its odb afterwards. Naming the id without writing it leaves the
/// index referring to an object `git fsck` reports as `missing blob e69de29…`,
/// which is what stock and zvcs disagreed on for every `-N` reset that dropped
/// a path. Called lazily, so a `-N` reset that creates no stub writes nothing.
fn empty_blob_for_intent_to_add(repo: &gix::Repository) -> Result<ObjectId> {
    Ok(repo.write_blob(b"")?.detach())
}

/// Build the `--mixed` index: `tree` verbatim, but preserving worktree stats for
/// entries whose id and mode are unchanged so the following refresh does not have to
/// re-hash every file and the index isn't spuriously reported as fully modified.
///
/// With `intent_to_add`, every old-index path absent from `tree` — which a mixed
/// reset would otherwise drop — is re-added as git's intent-to-add stub instead
/// (`update_index_from_diff()`, the `!is_in_reset_tree` branch): mode `100644`, the
/// empty-blob id, `CE_INTENT_TO_ADD` set, and a zeroed stat so it is never up to date.
fn reset_index_to_tree(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    intent_to_add: bool,
) -> Result<gix::index::File> {
    let mut new_index = repo.index_from_tree(&tree)?;

    // ```c
    // repo_read_index_unmerged(the_repository);
    // ```
    //
    // (`reset_index()`, builtin/reset.c:99.) Every unmerged entry is replaced by a stage-0
    // marker through `add_index_entry()`, whose `remove_index_entry_at()` records the
    // displaced stages in the resolve-undo extension (read-cache.c:1370-1371, 3404-3431).
    // The index this port builds comes from the tree instead, so the records are collected
    // from the old index and carried across — without them a `git checkout --merge <path>`
    // after the reset has nothing to put the conflict back from.
    {
        let mut collapsed = old.clone();
        collapsed.remove_entries(|_, _, e| e.stage_raw() != 0);
        if let Some(records) = collapsed.remove_resolve_undo() {
            new_index.set_resolve_undo(records);
        }
    }

    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    let tree_paths: HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }

    if intent_to_add {
        let mut ita: Option<ObjectId> = None;
        let mut added: HashSet<BString> = HashSet::new();
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing).to_owned();
            if !tree_paths.contains(&path) && added.insert(path.clone()) {
                let id = match ita {
                    Some(id) => id,
                    None => *ita.insert(empty_blob_for_intent_to_add(repo)?),
                };
                // EXTENDED must accompany INTENT_TO_ADD: the writer upgrades to index
                // V3 and emits the extended-flags word only when EXTENDED is set —
                // without it the i-t-a bit (>0xffff) is truncated away on write.
                new_index.dangerously_push_entry(
                    Stat::default(),
                    id,
                    Flags::INTENT_TO_ADD | Flags::EXTENDED,
                    Mode::FILE,
                    BStr::new(&path),
                );
            }
        }
        new_index.sort_entries();
    }

    Ok(new_index)
}

/// `--hard`: overwrite the worktree and index from `tree`, discarding local changes
/// to tracked files and deleting files the reset removes. Untracked files are left
/// untouched, matching `git reset --hard`.
fn reset_worktree_hard(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("hard reset not allowed in a bare repository"))?
        .to_owned();

    // The full target index; checking it all out overwrites every tracked file with
    // the tree version (thus discarding worktree modifications) and back-fills fresh
    // stats onto the entries, yielding a clean index after the write.
    let mut new_index = repo.index_from_tree(&tree)?;

    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut new_index,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    // Remove files tracked before the reset but absent from the target tree.
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

    // `unpack_trees()` ends with `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`

    // (unpack-trees.c:2088-2092), so the index git leaves here carries a cache-tree.

    super::write_tree::rebuild_cache_tree(repo, &mut new_index);
    crate::index_racy::write(repo, &mut new_index)?;
    Ok(())
}

/// The per-path outcome of the two-tree merge.
enum Act {
    /// Leave the index and worktree entry untouched (preserving local changes).
    Keep,
    /// Remove the path from the index and worktree.
    Delete,
    /// Set the index and worktree to the target version.
    Update,
    /// Local changes would be lost — abort the whole reset.
    Conflict,
}

/// `oneway_merge` (`--merge`): compare the index entry `i` to the target `t`.
fn classify_merge(t: Option<&(Mode, ObjectId)>, i: Option<&(Mode, ObjectId)>) -> Act {
    match (i, t) {
        (Some(_), None) => Act::Delete,
        (None, None) => Act::Keep,
        (Some(iv), Some(tv)) if iv == tv => Act::Keep,
        (_, Some(_)) => Act::Update,
    }
}

/// `twoway_merge` (`--keep`): compare HEAD `h`, target `t` and index `i`. Keeps
/// files unchanged between HEAD and target (or already at target), updates files
/// that changed only when the index still matches HEAD, and rejects everything
/// else (staged divergence).
fn classify_keep(
    h: Option<&(Mode, ObjectId)>,
    t: Option<&(Mode, ObjectId)>,
    i: Option<&(Mode, ObjectId)>,
) -> Act {
    match i {
        Some(iv) => {
            if h == t || Some(iv) == t {
                Act::Keep
            } else if t.is_none() && h == Some(iv) {
                Act::Delete
            } else if t.is_some() && h == Some(iv) {
                Act::Update
            } else {
                Act::Conflict
            }
        }
        None => match (h, t) {
            (_, None) => Act::Keep,
            (Some(hv), Some(_)) if Some(hv) == t => Act::Keep, // staged deletion kept
            (Some(_), Some(_)) => Act::Conflict,
            (None, Some(_)) => Act::Update,
        },
    }
}

/// `--merge` / `--keep`: git's two-tree merge (`unpack-trees.c` `oneway_merge` /
/// `twoway_merge`). Updates the index and worktree toward `target_tree` while
/// preserving local changes to files the reset does not touch, and aborts —
/// writing nothing and leaving HEAD in place — if a file that must change has
/// un-committed local modifications.
fn reset_two_tree(
    repo: &gix::Repository,
    old: &gix::index::File,
    head_tree: ObjectId,
    target_tree: ObjectId,
    keep: bool,
    should_interrupt: &AtomicBool,
) -> Result<bool> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| {
            anyhow!(
                "{} reset not allowed in a bare repository",
                if keep { "keep" } else { "merge" }
            )
        })?
        .to_owned();

    let target = tree_map(repo, target_tree)?;
    let head = if keep {
        tree_map(repo, head_tree)?
    } else {
        HashMap::new()
    };
    // `repo_read_index_unmerged()`, which `reset_index()` runs before
    // `unpack_trees()`: every unmerged path collapses to a single stage-0 entry
    // carrying a null object id and the `CE_CONFLICTED` flag. The null id is
    // what makes it compare unequal to the target, and the flag is what makes
    // `merged_entry()`/`deleted_entry()` skip `verify_uptodate()` for it — which
    // is why `git reset --merge` succeeds over a conflicted index while the same
    // worktree state would abort a clean one.
    let mut index = index_entry_map(old);
    let unmerged = unmerged_paths(old);
    for (path, mode) in &unmerged {
        index.insert(path.clone(), (*mode, ObjectId::null(repo.object_hash())));
    }

    let mut all: BTreeSet<BString> = BTreeSet::new();
    all.extend(index.keys().cloned());
    all.extend(target.keys().cloned());
    all.extend(head.keys().cloned());

    let mut updates: Vec<(BString, Mode, ObjectId)> = Vec::new();
    let mut deletes: Vec<BString> = Vec::new();
    // Each conflict carries git's per-entry reason: a worktree that no longer
    // matches the index is "not uptodate"; a staged divergence "would be
    // overwritten by merge" (unpack-trees.c `ERRORMSG`).
    let mut conflicts: BTreeSet<(BString, &'static str)> = BTreeSet::new();

    for path in &all {
        let i = index.get(path);
        let t = target.get(path);
        let act = if keep {
            classify_keep(head.get(path), t, i)
        } else {
            classify_merge(t, i)
        };
        // The `CE_CONFLICTED` marker `read_index_unmerged()` left behind: git
        // skips the uptodate check for it outright, so the half-merged worktree
        // file is overwritten rather than protected.
        let conflicted = unmerged.contains_key(path);
        match act {
            Act::Keep => {}
            Act::Delete => {
                if conflicted || worktree_uptodate(repo, BStr::new(path), i.map(|(_, o)| *o)) {
                    deletes.push(path.clone());
                } else {
                    conflicts.insert((path.clone(), "not uptodate"));
                }
            }
            Act::Update => {
                let (tm, to) = *t.expect("update implies a target entry");
                let clean = conflicted
                    || match i {
                        Some((_, io)) => worktree_uptodate(repo, BStr::new(path), Some(*io)),
                        None => worktree_absent_or_matches(repo, BStr::new(path), to),
                    };
                if clean {
                    updates.push((path.clone(), tm, to));
                } else {
                    conflicts.insert((path.clone(), "not uptodate"));
                }
            }
            Act::Conflict => {
                conflicts.insert((path.clone(), "would be overwritten by merge"));
            }
        }
    }

    // git's `unpack_trees` prints one `error:` line per conflicting entry, then the
    // caller (`reset_index`) prints the `fatal:` line and exits 128. Nothing is
    // written and HEAD is not moved.
    if !conflicts.is_empty() {
        for (path, reason) in &conflicts {
            eprintln!("error: Entry '{path}' {reason}. Cannot merge.");
        }
        return Ok(false);
    }

    // No conflicts: apply. Start from the old index so kept paths retain their
    // existing entry (and thus any staged content), then apply updates/deletes.
    let mut new_index = old.clone();
    let changed: HashSet<BString> = updates
        .iter()
        .map(|(p, _, _)| p.clone())
        .chain(deletes.iter().cloned())
        .collect();
    // Stage 1/2/3 entries never survive: `read_index_unmerged()` already
    // replaced them with the marker that has just been resolved one way or the
    // other, so the result is a stage-0-only index either way.
    new_index.remove_entries(|_, path, entry| {
        changed.contains(path) || entry.stage_raw() != 0
    });
    for (p, mode, oid) in &updates {
        new_index.dangerously_push_entry(Stat::default(), *oid, Flags::empty(), *mode, BStr::new(p));
    }
    new_index.sort_entries();

    // `--merge` and `--keep` are both `unpack_trees()` in git, and it opens with
    // `resolve_undo_clear_index()` (unpack-trees.c) — a two-tree reset discards
    // the resolve-undo record rather than adding to it.
    //
    // This port reaches the same result by *mutating* the old index, and the
    // mutation runs through `remove_entries()`, which is exactly where git
    // *records* resolve-undo from (`remove_index_entry_at()`,
    // read-cache.c:1370-1371). So the shapes collide: at index level a
    // conflict-resolving `add` and a two-tree reset do the same thing, and only
    // the caller knows which one it is. Clearing here is that knowledge — without
    // it `git reset --merge` after a resolved conflict writes a REUC stock git
    // does not have (measured: stock 0 records, this port 3).
    new_index.remove_resolve_undo();

    // Write the changed files to the worktree by checking out a filtered copy that
    // holds only the updated entries — kept files (with their local changes) are
    // never touched.
    if !updates.is_empty() {
        let upd: HashSet<BString> = updates.iter().map(|(p, _, _)| p.clone()).collect();
        let mut wt = new_index.clone();
        wt.remove_entries(|_, path, _| !upd.contains(path));
        let mut opts =
            repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
        opts.destination_is_initially_empty = false;
        opts.overwrite_existing = true;
        let odb = repo.objects.clone().into_arc()?;
        let discard_files = gix::progress::Discard;
        let discard_bytes = gix::progress::Discard;
        crate::worktree::checkout_subset(
            &mut wt,
            workdir.as_path(),
            odb,
            &discard_files,
            &discard_bytes,
            should_interrupt,
            opts,
        )?;
        // Copy the fresh stats back onto the persisted index so the just-written
        // files are not reported modified before the next refresh.
        // The id and mode ride with the stat: a stat is only true of the entry naming the content
        // it was measured from. Stamping one on an entry that names a different blob claims the
        // worktree matches the index when it does not, and the difference then disappears from
        // `status`, `diff` and `add`.
        let stat_map: HashMap<BString, (ObjectId, Mode, Stat)> = {
            let backing = wt.path_backing();
            wt.entries()
                .iter()
                .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode, e.stat)))
                .collect()
        };
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            if let Some((_, _, stat)) = stat_map
                .get(e.path_in(&backing))
                .filter(|(id, mode, _)| *id == e.id && *mode == e.mode)
            {
                e.stat = *stat;
            }
        }
    }

    for p in &deletes {
        if let Some(full) = repo.workdir_path(BStr::new(p)) {
            let _ = std::fs::remove_file(full);
        }
    }

    // `unpack_trees()` ends with `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`

    // (unpack-trees.c:2088-2092), so the index git leaves here carries a cache-tree.

    super::write_tree::rebuild_cache_tree(repo, &mut new_index);
    crate::index_racy::write(repo, &mut new_index)?;
    Ok(true)
}

/// A tree flattened to `path -> (mode, oid)`, via a throwaway index built from it.
fn tree_map(repo: &gix::Repository, tree: ObjectId) -> Result<HashMap<BString, (Mode, ObjectId)>> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    Ok(index
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.mode, e.id)))
        .collect())
}

/// The stage-0 entries of `index` as `path -> (mode, oid)`.
/// The paths `repo_read_index_unmerged()` collapses, with the mode it keeps for
/// each — git copies `ce_mode` from the last stage it walks, so the highest
/// stage present wins.
fn unmerged_paths(index: &gix::index::File) -> HashMap<BString, Mode> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .filter(|e| e.stage_raw() != 0)
        .map(|e| (e.path_in(backing).to_owned(), e.mode))
        .collect()
}

fn index_entry_map(index: &gix::index::File) -> HashMap<BString, (Mode, ObjectId)> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .filter(|e| e.stage() == gix::index::entry::Stage::Unconflicted)
        .map(|e| (e.path_in(backing).to_owned(), (e.mode, e.id)))
        .collect()
}

/// Whether the worktree file at `path` still matches the index (`index_oid`), so
/// overwriting or removing it loses nothing. A missing file is up to date (git's
/// `verify_uptodate` returns 0 on `ENOENT`); an unreadable one is treated as
/// changed, so the reset errs on the side of aborting.
fn worktree_uptodate(repo: &gix::Repository, path: &BStr, index_oid: Option<ObjectId>) -> bool {
    let Some(full) = repo.workdir_path(path) else {
        return true;
    };
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(m) => m,
        Err(_) => return true,
    };
    let Some(oid) = index_oid else {
        return true;
    };
    blob_oid(repo, &full, &meta) == Some(oid)
}

/// Whether it is safe to create `path` from the target: no worktree file exists,
/// or the one that does already matches the target content (no untracked data lost).
fn worktree_absent_or_matches(repo: &gix::Repository, path: &BStr, target_oid: ObjectId) -> bool {
    let Some(full) = repo.workdir_path(path) else {
        return true;
    };
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(m) => m,
        Err(_) => return true,
    };
    blob_oid(repo, &full, &meta) == Some(target_oid)
}

/// The blob object id a worktree file would hash to (the link target for a
/// symlink), without writing it, for the up-to-date comparison.
fn blob_oid(
    repo: &gix::Repository,
    full: &std::path::Path,
    meta: &std::fs::Metadata,
) -> Option<ObjectId> {
    let data = if meta.file_type().is_symlink() {
        std::fs::read_link(full)
            .ok()?
            .into_os_string()
            .into_string()
            .ok()?
            .into_bytes()
    } else {
        std::fs::read(full).ok()?
    };
    gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &data).ok()
}

/// `git reset [<commit>] [--] <paths>` — build the index with the named entries (all
/// stages) reset to the target tree's version, or dropped if absent from the tree.
/// The worktree is untouched. A pathspec that matches nothing is not an error: git
/// only validates the *leading* positional, and that happens during setup.
///
/// With `intent_to_add`, a matched path that the reset would drop (present in the old
/// index but absent from the target tree) becomes an intent-to-add stub instead of
/// vanishing, matching `update_index_from_diff()`'s `!is_in_reset_tree` branch.
fn pathspec_index(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    paths: &[String],
    intent_to_add: bool,
) -> Result<gix::index::File> {
    // Desired versions for every path in the target tree.
    let target = repo.index_from_tree(&tree)?;
    let mut target_map: HashMap<BString, (Stat, ObjectId, Flags, Mode)> =
        HashMap::with_capacity(target.entries().len());
    {
        let backing = target.path_backing();
        for e in target.entries() {
            target_map.insert(e.path_in(backing).to_owned(), (e.stat, e.id, e.flags, e.mode));
        }
    }

    let mut index = old.clone();

    // Candidate paths = union of currently-tracked and target-tree paths.
    let mut candidates: BTreeSet<BString> = BTreeSet::new();
    {
        let backing = index.path_backing();
        for e in index.entries() {
            candidates.insert(e.path_in(backing).to_owned());
        }
    }
    for p in target_map.keys() {
        candidates.insert(p.clone());
    }

    // `parse_pathspec()` + `ce_path_match()`: the real matcher, magic and all. A prefix test was
    // enough for the plain `dir/file` shapes and silently ignored every other one — `:!nested/`
    // and `:(exclude)…` matched nothing (so `reset` reset nothing), `:(glob)a/**/*.txt` matched
    // only a literal path of that name, and `:(icase)` was case-sensitive. The pathspecs are also
    // taken relative to the current directory, which the engine handles through the repository's
    // own prefix.
    let mut search = repo.pathspec(
        true,
        paths.iter().map(String::as_bytes),
        false,
        old,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;
    let mut ops: HashSet<BString> = HashSet::new();
    for cand in &candidates {
        let matched = search
            .pattern_matching_relative_path(BStr::new(cand), Some(false))
            .is_some_and(|m| !m.is_excluded());
        if matched {
            ops.insert(cand.clone());
        }
    }

    if ops.is_empty() {
        return Ok(index);
    }

    // Drop every stage of each selected path, then re-add the tree version if any;
    // a `-N` path missing from the tree comes back as an intent-to-add stub instead.
    index.remove_entries(|_, path, _| ops.contains(&path.to_owned()));
    let mut ita: Option<ObjectId> = None;
    for path in &ops {
        if let Some((stat, id, flags, mode)) = target_map.get(path) {
            index.dangerously_push_entry(*stat, *id, *flags, *mode, BStr::new(path));
        } else if intent_to_add {
            let id = match ita {
                Some(id) => id,
                None => *ita.insert(empty_blob_for_intent_to_add(repo)?),
            };
            // EXTENDED must accompany INTENT_TO_ADD so the writer keeps the bit (V3).
            index.dangerously_push_entry(
                Stat::default(),
                id,
                Flags::INTENT_TO_ADD | Flags::EXTENDED,
                Mode::FILE,
                BStr::new(path),
            );
        }
    }
    index.sort_entries();

    Ok(index)
}
