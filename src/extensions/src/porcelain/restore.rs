//! `git restore` — restore worktree (and/or `--staged` index) files from a source.
//!
//! Backed natively by the vendored gitoxide crates so tools on PATH observe the
//! same staged index. Supported invocation forms (matching stock `git restore`):
//!
//!   * `git restore <pathspec>...`                    worktree ← index (default)
//!   * `git restore --source=<tree> <pathspec>...`    worktree ← <tree>
//!   * `git restore --staged <pathspec>...`           index    ← HEAD (unstage)
//!   * `git restore --staged --source=<tree> ...`     index    ← <tree>
//!   * `git restore --staged --worktree [-s <tree>]`  both     ← HEAD (or <tree>)
//!   * `git restore --ours/--theirs <pathspec>...`    worktree ← unmerged stage 2/3
//!   * `git restore --merge [--conflict=<style>] ...` worktree ← recreated conflict
//!   * `git restore --overlay ...`                    keep target files absent in source
//!   * `git restore --pathspec-from-file=<f> ...`     read pathspecs from a file/stdin
//!   * `git restore --recurse-submodules <pathspec>`  also restore matched submodule worktrees
//!
//! `--staged`/`--worktree` are git's `opts->checkout_index`/`checkout_worktree`,
//! which start as *tri-state* defaults (`-1` off / `-2` on) and collapse to 0/1
//! only after the whole command line has been read: naming either flag in either
//! sense turns the other off, so `git restore --no-worktree <path>` leaves both
//! targets off and is refused with `neither '--staged' or '--worktree' is
//! specified` rather than silently doing nothing.
//!
//! The default restore source is the index for `--worktree`, and `HEAD` when
//! `--staged` is given (either alone or combined). Restore is no-overlay by
//! default: a path present in the target but not the source is removed; with
//! `--overlay` such files are kept. `--ours`/`--theirs` pick the stage-2/stage-3
//! blob of an unmerged path; `--merge`/`--conflict` recreate the 3-way conflict
//! (with markers) in the worktree. With `--recurse-submodules`, any matched,
//! active submodule whose gitlink appears in the restore source has its worktree
//! reset to the recorded commit (local modifications overwritten, submodule HEAD
//! detached), matching git-restore(1). `-p`/`--patch` runs the interactive hunk
//! selector ([`super::add_patch`]) against whichever of the index / worktree the
//! `--staged` / `--worktree` flags select.

use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU8;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::diff::blob::{Algorithm, InternedInput};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::merge::blob::builtin_driver::text::{Conflict, ConflictStyle, Labels, Options};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

use super::{Arg, LongOpt};

/// `cmd_restore()`'s option table (builtin/checkout.c:2187), in the order
/// `parse_options_concat()` builds it: `restore_options[]`, then
/// `add_common_options()`, then `add_checkout_path_options()`.
///
/// `--ours`/`--theirs` are `PARSE_OPT_NONEG` `OPT_SET_INT_F`s and `--unified` /
/// `--inter-hunk-context` are `PARSE_OPT_NONEG` too, so none of the four has a
/// `--no-` spelling. `--auto-advance` belongs to `checkout_options[]` alone and is
/// deliberately absent here, exactly as `git restore` rejects it.
const LONG_OPTS: &[LongOpt] = &[
    // restore_options[] (builtin/checkout.c:2187)
    LongOpt { name: "source",                      neg: true,  arg: Arg::Required },
    LongOpt { name: "staged",                      neg: true,  arg: Arg::None },
    LongOpt { name: "worktree",                    neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-unmerged",             neg: true,  arg: Arg::None },
    LongOpt { name: "overlay",                     neg: true,  arg: Arg::None },
    // add_common_options() (builtin/checkout.c:1767)
    LongOpt { name: "quiet",                       neg: true,  arg: Arg::None },
    LongOpt { name: "recurse-submodules",          neg: true,  arg: Arg::Optional },
    LongOpt { name: "progress",                    neg: true,  arg: Arg::None },
    LongOpt { name: "merge",                       neg: true,  arg: Arg::None },
    LongOpt { name: "conflict",                    neg: true,  arg: Arg::Required },
    // add_checkout_path_options() (builtin/checkout.c:1811)
    LongOpt { name: "ours",                        neg: false, arg: Arg::None },
    LongOpt { name: "theirs",                      neg: false, arg: Arg::None },
    LongOpt { name: "patch",                       neg: true,  arg: Arg::None },
    LongOpt { name: "unified",                     neg: false, arg: Arg::Required },
    LongOpt { name: "inter-hunk-context",          neg: false, arg: Arg::Required },
    LongOpt { name: "ignore-skip-worktree-bits",   neg: true,  arg: Arg::None },
    LongOpt { name: "pathspec-from-file",          neg: true,  arg: Arg::Required },
    LongOpt { name: "pathspec-file-nul",           neg: true,  arg: Arg::None },
];
/// `usage_with_options()` over `builtin/checkout.c`'s `restore` option table.
const USAGE: &str = r"usage: git restore [<options>] [--source=<branch>] <file>...

    -s, --[no-]source <tree-ish>
                          which tree-ish to checkout from
    -S, --[no-]staged     restore the index
    -W, --[no-]worktree   restore the working tree (default)
    --[no-]ignore-unmerged
                          ignore unmerged entries
    --[no-]overlay        use overlay mode
    -q, --[no-]quiet      suppress progress reporting
    --[no-]recurse-submodules[=<checkout>]
                          control recursive updating of submodules
    --[no-]progress       force progress reporting
    -m, --[no-]merge      perform a 3-way merge with the new branch
    --[no-]conflict <style>
                          conflict style (merge, diff3, or zdiff3)
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

/// True if `path` matches any of the (repo-root-relative, slash-separated)
/// pathspecs. A spec matches its own exact path, or any path under it as a
/// directory prefix. `match_all` (a `.` or empty spec) matches everything.
fn path_matches(path: &BStr, match_all: bool, specs: &super::log::PathspecMatcher) -> bool {
    match_all || specs.matches(path.as_ref())
}

/// Which unmerged stage a conflict-resolution flag selects.
#[derive(Copy, Clone, PartialEq)]
enum Pick {
    Ours,
    Theirs,
}

/// Resolve a (possibly subdirectory-relative) pathspec to a repo-root-relative,
/// slash-separated path. Returns `Ok(None)` when the spec designates the whole
/// tree (a `.`/empty at the worktree root), `Ok(Some(path))` for a concrete
/// path, and `Err(())` when the spec escapes the worktree.
fn resolve_spec(prefix: &[String], wd: &Path, raw: &str) -> Result<Option<String>, ()> {
    // Absolute pathspec: resolve lexically against the worktree root.
    if raw.starts_with('/') {
        let wds = wd.to_string_lossy();
        if raw == wds {
            return Ok(None);
        }
        return match raw.strip_prefix(&*wds).and_then(|r| r.strip_prefix('/')) {
            Some("") => Ok(None),
            Some(rest) => Ok(Some(rest.trim_end_matches('/').to_string())),
            None => Err(()),
        };
    }
    let mut comps: Vec<&str> = prefix.iter().map(String::as_str).collect();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if comps.pop().is_none() {
                    return Err(());
                }
            }
            other => comps.push(other),
        }
    }
    if comps.is_empty() {
        Ok(None)
    } else {
        Ok(Some(comps.join("/")))
    }
}

/// Perform a 3-way text merge of the three unmerged stages and return the
/// merged bytes (with conflict markers, using git's `ours`/`base`/`theirs`
/// labels). A missing stage is treated as empty content.
fn three_way_merge(
    repo: &gix::Repository,
    base: Option<ObjectId>,
    ours: Option<ObjectId>,
    theirs: Option<ObjectId>,
    style: ConflictStyle,
) -> Result<Vec<u8>> {
    let load = |o: Option<ObjectId>| -> Result<Vec<u8>> {
        Ok(match o {
            Some(id) => repo.find_object(id)?.detach().data,
            None => Vec::new(),
        })
    };
    let base_b = load(base)?;
    let our_b = load(ours)?;
    let their_b = load(theirs)?;

    let mut input = InternedInput::new(our_b.as_slice(), their_b.as_slice());
    let mut out = Vec::new();
    let opts = Options {
        diff_algorithm: Algorithm::Myers,
        conflict: Conflict::Keep {
            style,
            marker_size: NonZeroU8::new(7).expect("7 != 0"),
        },
        // `merge-ll.c`'s level, which is what every caller but `merge-file` uses.
        ..Options::default()
    };
    // The free 3-way text merge is re-exported as the value `builtin_driver::text`
    // (a function that shares its name with the `text` module), so it is invoked
    // by its full path rather than a `text::merge` alias.
    gix::merge::blob::builtin_driver::text(
        &mut out,
        &mut input,
        Labels {
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

/// Reset a submodule's worktree to `commit`, overwriting local modifications,
/// and detach its `HEAD` there — the behavior `git restore --recurse-submodules`
/// applies to each matched active submodule (git-restore(1), `submodule_move_head`
/// in git's `builtin/checkout.c`).
///
/// The recorded commit is peeled to its tree, unpacked into an index, and checked
/// out over the existing worktree with overwrite; files tracked before but absent
/// in the target tree are deleted, the submodule index is rewritten with fresh
/// stats, and `HEAD` is repointed to the detached commit.
fn restore_submodule_worktree(
    sm_repo: &gix::Repository,
    commit: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let sm_workdir = match sm_repo.workdir() {
        Some(w) => w.to_owned(),
        // A bare submodule checkout has no worktree to restore.
        None => return Ok(()),
    };
    let tree_id = sm_repo.find_object(commit)?.peel_to_tree()?.id;

    // Target index (all target-tree entries) — the write target and deletion set.
    let mut target_index = sm_repo.index_from_tree(&tree_id)?;
    let new_paths: HashSet<BString> = {
        let b = target_index.path_backing();
        target_index.entries().iter().map(|e| e.path_in(b).to_owned()).collect()
    };

    // Files tracked in the submodule's current index but gone from the target.
    let old_paths: Vec<BString> = match sm_repo.open_index() {
        Ok(idx) => {
            let b = idx.path_backing();
            idx.entries().iter().map(|e| e.path_in(b).to_owned()).collect()
        }
        Err(_) => Vec::new(),
    };

    // Check out the full target index over the existing worktree (a separate copy
    // is passed since `checkout` takes the index's path backing out).
    let mut subset = target_index.clone();
    let mut opts =
        sm_repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = sm_repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        sm_workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    // Remove files that the target tree no longer tracks.
    for p in &old_paths {
        if !new_paths.contains(p) {
            if let Some(full) = sm_repo.workdir_path(BStr::new(p)) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    // Copy the fresh checkout stats into the target index before persisting it.
    let mut fresh: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let b = subset.path_backing();
        for e in subset.entries() {
            fresh.insert(e.path_in(b).to_owned(), e.stat);
        }
    }
    {
        let b = target_index.path_backing().to_owned();
        for e in target_index.entries_mut() {
            if let Some(stat) = fresh.get(&e.path_in(&b).to_owned()) {
                e.stat = *stat;
            }
        }
    }
    // `unpack_trees()` leaves a repaired cache-tree behind (unpack-trees.c:2088-2092).
    super::write_tree::rebuild_cache_tree(sm_repo, &mut target_index);
    // The *submodule's* index, so the submodule repository's settings decide the
    // trailer — which is what git gets too, since it moves a submodule HEAD by
    // running the plumbing inside the submodule and the command-line `-c`
    // overrides reach that child through the environment.
    target_index.write(crate::config::index_write_options(sm_repo))?;

    // Detach the submodule HEAD at the restored commit (git detaches here).
    sm_repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("restore: moving to {commit}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(commit),
        },
        name: "HEAD".try_into().map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

pub fn restore(args: &[String]) -> Result<ExitCode> {
    // --- Argument parsing ---------------------------------------------------
    // git's `opts->checkout_index` / `opts->checkout_worktree`, which start at
    // `-1` ("default off") and `-2` ("default on") for `restore` and resolve to
    // 0/1 only after the whole command line has been read:
    //
    // ```c
    // if (opts->checkout_index >= 0 || opts->checkout_worktree >= 0) {
    //         if (opts->checkout_index < 0)    opts->checkout_index = 0;
    //         if (opts->checkout_worktree < 0) opts->checkout_worktree = 0;
    // } else {
    //         if (opts->checkout_index < 0)    opts->checkout_index = -opts->checkout_index - 1;
    //         if (opts->checkout_worktree < 0) opts->checkout_worktree = -opts->checkout_worktree - 1;
    // }
    // ```
    // (builtin/checkout.c:1933-1943.) Naming *either* flag in *either* sense
    // switches the other one off, which is why `--no-worktree` alone leaves both
    // targets off and is refused rather than silently doing nothing. A plain
    // `bool` pair cannot express that: it cannot tell "not mentioned" from
    // "explicitly off".
    let mut staged: Option<bool> = None;
    let mut worktree: Option<bool> = None;
    let mut source: Option<String> = None;
    let mut pathspecs: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    let mut overlay = false;
    let mut pick: Option<Pick> = None;
    let mut merge_flag = false;
    let mut conflict_style: Option<ConflictStyle> = None;
    let mut ignore_unmerged = false;
    let mut pathspec_from_file: Option<String> = None;
    let mut pathspec_file_nul = false;
    let mut recurse_submodules = false;

    // Parse a `--conflict` style value; git errors with exit 129 on unknown.
    let parse_conflict = |v: &str| -> Option<ConflictStyle> {
        match v {
            "merge" => Some(ConflictStyle::Merge),
            "diff3" => Some(ConflictStyle::Diff3),
            "zdiff3" => Some(ConflictStyle::ZealousDiff3),
            _ => None,
        }
    };

    // `-U`/`--unified` and `--inter-hunk-context`: the interactive hunk selector's
    // options, parsed and diagnosed by the same code `git reset` and `git checkout`
    // use. `--[no-]auto-advance` is *not* among them — `OPT_ADD_AUTO_ADVANCE` sits in
    // `checkout_options[]` alone (builtin/checkout.c:2100), not in the
    // `add_checkout_path_options()` block `cmd_restore()` concatenates, so stock
    // answers `git restore --auto-advance` with `unknown option`.
    let mut patch_opts = super::reset::PatchDiffOpts::without_auto_advance();
    let mut patch_mode = false;

    let mut i = 0;
    while i < args.len() {
        // A value still owed to `-U`/`--inter-hunk-context` is taken verbatim,
        // even past `--`, exactly as parse-options takes it — and precisely because
        // it is a value, it is never resolved as an option name.
        if patch_opts.awaiting_value() {
            match patch_opts.take_arg(&args[i]) {
                Err(code) => return Ok(code),
                Ok(true) => {
                    i += 1;
                    continue;
                }
                Ok(false) => {}
            }
        }
        if after_dashdash {
            pathspecs.push(args[i].clone());
            i += 1;
            continue;
        }
        // Respell a unique abbreviation as the name it resolves to, ahead of both
        // the shared value-option handler and the match below, so `--ignore-unm`
        // reaches the same arm as `--ignore-unmerged`.
        let canonical;
        let a = match super::canonical_long(&args[i], LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(&args[i], &first, &second, USAGE))
            }
        };
        match patch_opts.take_arg(a) {
            Err(code) => return Ok(code),
            Ok(true) => {
                i += 1;
                continue;
            }
            Ok(false) => {}
        }
        match a {
            "--" => after_dashdash = true,
            // parse_options_step()'s `internal_help`: the block on stdout at
            // 129, with no `error:` line — a help request is not a rejection.
            // `--help-all` reaches the same renderer with USAGE_FULL, which this
            // table renders identically: it has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => return Ok(super::show_usage(USAGE)),
            "--staged" | "-S" => staged = Some(true),
            // Every `--no-<x>` below is parse-options' unset for that entry:
            // an `OPT_BOOL` writes 0, an `OPT_STRING`/`OPT_FILENAME` writes NULL,
            // and `parse_opt_conflict()` (builtin/checkout.c:1750) sets
            // `conflict_style = -1`. None of them is a gap in this port; they are
            // the other half of options it already implements.
            "--no-staged" => staged = Some(false),
            "--worktree" | "-W" => worktree = Some(true),
            "--no-worktree" => worktree = Some(false),
            "-s" | "--source" => {
                i += 1;
                match args.get(i) {
                    Some(v) => source = Some(v.clone()),
                    None => {
                        // git: short flags are "switch `s'", long are "option `source'".
                        if a == "-s" {
                            eprintln!("error: switch `s' requires a value");
                        } else {
                            eprintln!("error: option `source' requires a value");
                        }
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            "--overlay" => overlay = true,
            "--no-overlay" => overlay = false,
            "--ours" | "-2" => pick = Some(Pick::Ours),
            "--theirs" | "-3" => pick = Some(Pick::Theirs),
            "-m" | "--merge" => merge_flag = true,
            "--no-merge" => merge_flag = false,
            "--conflict" => {
                i += 1;
                match args.get(i) {
                    Some(v) => match parse_conflict(v) {
                        Some(s) => conflict_style = Some(s),
                        None => {
                            eprintln!("error: unknown conflict style '{v}'");
                            return Ok(ExitCode::from(129));
                        }
                    },
                    None => {
                        eprintln!("error: option `conflict' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            "--ignore-unmerged" => ignore_unmerged = true,
            "--no-ignore-unmerged" => ignore_unmerged = false,
            "--pathspec-file-nul" => pathspec_file_nul = true,
            "--no-pathspec-file-nul" => pathspec_file_nul = false,
            "--no-pathspec-from-file" => pathspec_from_file = None,
            "--no-source" => source = None,
            "--no-conflict" => conflict_style = None,
            "--pathspec-from-file" => {
                i += 1;
                match args.get(i) {
                    Some(v) => pathspec_from_file = Some(v.clone()),
                    None => {
                        eprintln!("error: option `pathspec-from-file' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // Accepted no-ops: quiet/progress/default no-recurse/diff-context knobs
            // (context knobs only affect interactive `--patch`, unsupported here).
            "-q" | "--quiet" | "--no-quiet" | "--progress" | "--no-progress"
            | "--ignore-skip-worktree-bits" | "--no-ignore-skip-worktree-bits" => {}
            "-p" | "--patch" => patch_mode = true,
            "--no-patch" => patch_mode = false,
            "--recurse-submodules" => recurse_submodules = true,
            "--no-recurse-submodules" => recurse_submodules = false,
            s if s.starts_with("--source=") => source = Some(s["--source=".len()..].to_string()),
            s if s.starts_with("--conflict=") => {
                let v = &s["--conflict=".len()..];
                match parse_conflict(v) {
                    Some(style) => conflict_style = Some(style),
                    None => {
                        eprintln!("error: unknown conflict style '{v}'");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if s.starts_with("--pathspec-from-file=") => {
                pathspec_from_file = Some(s["--pathspec-from-file=".len()..].to_string());
            }
            s if s.starts_with("-s") && s.len() > 2 => source = Some(s[2..].to_string()),
            // `PARSE_OPT_UNKNOWN` (parse-options.c:1210-1224): the `error:` line and
            // then the whole usage block, both on stderr, exit 129. That block is
            // what separates this from a bad option *value*, which returns
            // `PARSE_OPT_ERROR` and exits 129 with the one line alone — which is why
            // the value refusals above print no block. A short option is named by
            // its letter (`unknown switch \`Z'`), a long one by its body.
            s if s.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &s[2..]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with('-') && s != "-" => {
                eprintln!("error: unknown switch `{}'", &s[1..2]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // A non-option argument is handed back unchanged by the resolver.
            _ => pathspecs.push(a.to_string()),
        }
        i += 1;
    }

    let merge_active = merge_flag || conflict_style.is_some();
    let conflict_mode = pick.is_some() || merge_active;

    // --- Pathspec-from-file -------------------------------------------------
    if pathspec_file_nul && pathspec_from_file.is_none() {
        eprintln!("fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'");
        return Ok(ExitCode::from(128));
    }
    if let Some(f) = pathspec_from_file.clone() {
        if !pathspecs.is_empty() {
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
            pathspecs.push(String::from_utf8_lossy(s).into_owned());
        }
    }

    // --- Incompatible-flag combinations (git's fatal/exit-128 diagnostics) --
    // `if (opts->ignore_unmerged && opts->merge) die(_("options '%s' and '%s'
    // cannot be used together"), opts->ignore_unmerged_opt, "-m");`
    // (builtin/checkout.c:547). `restore` never sets `ignore_unmerged_opt` to
    // anything but `--ignore-unmerged` — the `--force` spelling belongs to
    // `checkout`, which has no `--ignore-unmerged`.
    if ignore_unmerged && merge_active {
        eprintln!("fatal: options '--ignore-unmerged' and '-m' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    // The tri-state resolution quoted above, then
    // `if (!opts->checkout_worktree && !opts->checkout_index)
    //      die(_("neither '%s' or '%s' is specified"), "--staged", "--worktree");`
    // (builtin/checkout.c:554).
    let (staged, worktree) = if staged.is_some() || worktree.is_some() {
        (staged.unwrap_or(false), worktree.unwrap_or(false))
    } else {
        (false, true)
    };
    if !worktree && !staged {
        eprintln!("fatal: neither '--staged' or '--worktree' is specified");
        return Ok(ExitCode::from(128));
    }
    if pick.is_some() && staged {
        eprintln!("fatal: '--ours' or '--theirs' cannot be used with --staged");
        return Ok(ExitCode::from(128));
    }
    if merge_active && staged {
        eprintln!("fatal: '--merge' or '--conflict' cannot be used with --staged");
        return Ok(ExitCode::from(128));
    }
    if conflict_mode && source.is_some() {
        eprintln!("fatal: '--merge', '--ours', or '--theirs' cannot be used when checking out of a tree");
        return Ok(ExitCode::from(128));
    }

    if let Err(code) = patch_opts.finish() {
        return Ok(code);
    }
    if let Some(code) = patch_opts.require_patch(patch_mode) {
        return Ok(code);
    }

    // `-p`: hand the paths to the interactive hunk selector. The patch mode
    // follows git's `checkout_paths()` mapping of the two targets:
    // `--worktree` alone is `ADD_P_WORKTREE`, `--staged` alone is `ADD_P_RESET`
    // (index only), and both together are `ADD_P_CHECKOUT`. `--source` supplies
    // the revision — resolved to a hex oid unless it is literally `HEAD`, since
    // `diff-index` cannot take an `<a>...<b>` range.
    if patch_mode {
        if !worktree && source.is_none() {
            eprintln!("fatal: '--worktree' must be used when '--source' is not specified");
            return Ok(ExitCode::from(128));
        }
        if pathspec_from_file.is_some() {
            eprintln!("fatal: options '--pathspec-from-file' and '--patch' cannot be used together");
            return Ok(ExitCode::from(128));
        }
        if merge_active {
            eprintln!("fatal: options '--merge' and '--patch' cannot be used together");
            return Ok(ExitCode::from(128));
        }
        if ignore_unmerged {
            eprintln!("fatal: '--ignore-unmerged' cannot be used with updating paths");
            return Ok(ExitCode::from(128));
        }
        let repo = gix::discover(".")?;
        let revision = match source.as_deref() {
            None | Some("HEAD") => source.clone(),
            Some(r) => Some(repo.rev_parse_single(r)?.detach().to_string()),
        };
        let mode = match (staged, worktree) {
            (true, true) => super::add_patch::Mode::Checkout,
            (true, false) => super::add_patch::Mode::Reset,
            _ => super::add_patch::Mode::Worktree,
        };
        return super::add_patch::run(
            &repo,
            mode,
            revision.as_deref(),
            patch_opts.to_interactive(false),
            &pathspecs,
        );
    }

    if pathspecs.is_empty() {
        eprintln!("fatal: you must specify path(s) to restore");
        return Ok(ExitCode::from(128));
    }

    // --- Repository + lock --------------------------------------------------
    let repo = gix::discover(".")?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| crate::fatal::need_work_tree())?
        .to_owned();
    let cwd = std::env::current_dir()?;
    // Pathspecs given relative to the current directory are resolved against the
    // worktree root using this prefix, so `git restore` works from any subdir.
    let wd_c = workdir.canonicalize().unwrap_or_else(|_| workdir.clone());
    let cwd_c = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
    let prefix_components: Vec<String> = cwd_c
        .strip_prefix(&wd_c)
        .ok()
        .map(|rel| {
            rel.components()
                .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // Normalize pathspecs: `.`/empty (at root) restores everything; the rest are
    // resolved to repo-root-relative paths. A spec escaping the worktree is fatal.
    let mut match_all = false;
    let mut specs: Vec<(String, Vec<u8>)> = Vec::new();
    for p in &pathspecs {
        match resolve_spec(&prefix_components, &wd_c, p) {
            Ok(None) => match_all = true,
            Ok(Some(rel)) => specs.push((p.clone(), rel.into_bytes())),
            Err(()) => {
                eprintln!("fatal: {p}: '{p}' is outside repository at '{}'", wd_c.display());
                return Ok(ExitCode::from(128));
            }
        }
    }
    // The pathspec set, parsed once by the shared engine — from the RAW specs, not
    // the ones `resolve_spec` already made repo-root-relative: the engine applies
    // the repository prefix itself, and feeding it resolved specs would apply it
    // twice, so a spec given from a subdirectory would match nothing.
    let raw_specs: Vec<String> = specs.iter().map(|(raw, _)| raw.clone()).collect();
    let spec_set = super::log::PathspecMatcher::new(&repo, &raw_specs)?;

    // Serialize the whole read-modify-write through the repo coordinator so a
    // concurrent zvcs writer can't race `index.lock`. Held for the function.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // --- Resolve the restore source ----------------------------------------
    // `source_tree_id == None` means "the current index" (default worktree src).
    let source_tree_id: Option<ObjectId> = match &source {
        // A `--source` that names nothing is git's own fatal, with the name
        // quoted. Propagating the revision parser's error instead put a Rust type
        // name and a path inside `src/ported/` in front of the user, and exited 1
        // where git exits 128.
        //
        // A full-length hex `--source` is the object id itself — `get_oid_basic()`
        // never asks the odb — so an absent one resolves here and is reported by
        // the tree lookup as `unable to read tree`, not as a name that would not
        // resolve.
        Some(rev) => {
            let Some(id) = crate::objname::resolve(&repo, rev.as_str()) else {
                crate::git_fatal!("could not resolve '{rev}'");
            };
            Some(super::checkout::classify_tree_ish(&repo, id)?.source_tree()?)
        }
        None if staged => Some(repo.head_tree_id_or_empty()?.detach()),
        None => None,
    };
    let source_is_index = source_tree_id.is_none();

    // The real worktree index — the write target for `--staged`, and the read
    // source in the default worktree case.
    let mut cur = repo.open_index()?;

    // The source, materialized as an index (a tree is unpacked into one).
    let source_index: gix::index::File = match &source_tree_id {
        Some(tid) => repo.index_from_tree(tid)?,
        None => cur.clone(),
    };

    // path -> (id, mode, flags, stat) for the source; used for staged writes.
    let mut source_map: HashMap<BString, (ObjectId, Mode, Flags, Stat)> = HashMap::new();
    {
        let b = source_index.path_backing();
        for e in source_index.entries() {
            source_map.insert(e.path_in(b).to_owned(), (e.id, e.mode, e.flags, e.stat));
        }
    }

    // Current index: stage blobs per path (index 0..=3) plus the path set.
    let mut stage_blobs: HashMap<BString, [Option<(ObjectId, Mode)>; 4]> = HashMap::new();
    let mut cur_paths: HashSet<BString> = HashSet::new();
    {
        let b = cur.path_backing();
        for e in cur.entries() {
            let p = e.path_in(b).to_owned();
            let s = e.stage_raw() as usize;
            stage_blobs.entry(p.clone()).or_insert([None, None, None, None])[s] = Some((e.id, e.mode));
            cur_paths.insert(p);
        }
    }
    let is_unmerged =
        |arr: &[Option<(ObjectId, Mode)>; 4]| arr[1].is_some() || arr[2].is_some() || arr[3].is_some();

    // Matched unmerged paths (sorted for git-identical diagnostic ordering).
    let mut unmerged_matched: Vec<BString> = Vec::new();
    for (p, arr) in &stage_blobs {
        if is_unmerged(arr) && path_matches(BStr::new(p), match_all, &spec_set) {
            unmerged_matched.push(p.clone());
        }
    }
    unmerged_matched.sort();

    // Validate every explicit pathspec matches something git knows about (the
    // union of source and index paths), mirroring git's pathspec error (exit 1).
    if !match_all {
        // `PS_IGNORE_SKIP_WORKTREE`: a path the sparse-checkout definition keeps out
        // of the worktree cannot be matched by a pathspec, so naming one is git's
        // "did not match" rather than a restore of a file that should not be there.
        let sparse: std::collections::HashSet<BString> = {
            let index = repo.index_or_empty()?;
            let backing = index.path_backing();
            index
                .entries()
                .iter()
                .filter(|e| e.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE))
                .map(|e| e.path_in(backing).to_owned())
                .collect()
        };
        for (raw, spec) in &specs {
            // Each spec is checked on its own: git names the one that matched nothing.
            let single = super::log::PathspecMatcher::new(&repo, std::slice::from_ref(raw))?;
            let hit = source_map
                .keys()
                .chain(cur_paths.iter())
                .filter(|p| !sparse.contains(&BString::from(p.to_vec())))
                .any(|p| path_matches(BStr::new(p), false, &single));
            if !hit {
                eprintln!("error: pathspec '{raw}' did not match any file(s) known to git");
                return Ok(ExitCode::from(1));
            }
        }
    }

    // Unmerged handling for the pure worktree-from-index restore: without a
    // conflict-resolution flag such a path is an error (exit 1), unless
    // `--ignore-unmerged` downgrades it to a skipped warning.
    if source_is_index && worktree && !conflict_mode && !unmerged_matched.is_empty() {
        if ignore_unmerged {
            for p in &unmerged_matched {
                eprintln!("warning: path '{p}' is unmerged");
            }
        } else {
            for p in &unmerged_matched {
                eprintln!("error: path '{p}' is unmerged");
            }
            return Ok(ExitCode::from(1));
        }
    }

    // Conflict-resolution targets for the worktree: the resolved stage-0 blob
    // for each matched unmerged path (or removal when the chosen side deleted it).
    let mut resolved_entries: Vec<(BString, ObjectId, Mode)> = Vec::new();
    let mut resolved_remove: HashSet<BString> = HashSet::new();
    if conflict_mode {
        for p in &unmerged_matched {
            let arr = &stage_blobs[p];
            if merge_active {
                let (ours, theirs) = (arr[2], arr[3]);
                if ours.is_none() && theirs.is_none() {
                    resolved_remove.insert(p.clone());
                    continue;
                }
                let mode = ours.or(theirs).map(|(_, m)| m).expect("one side present");
                let merged = three_way_merge(
                    &repo,
                    arr[1].map(|(id, _)| id),
                    ours.map(|(id, _)| id),
                    theirs.map(|(id, _)| id),
                    conflict_style.unwrap_or(ConflictStyle::Merge),
                )?;
                let id = repo.write_blob(&merged)?.detach();
                resolved_entries.push((p.clone(), id, mode));
            } else {
                let want = match pick {
                    Some(Pick::Ours) => arr[2],
                    _ => arr[3],
                };
                match want {
                    Some((id, mode)) => resolved_entries.push((p.clone(), id, mode)),
                    None => {
                        resolved_remove.insert(p.clone());
                    }
                }
            }
        }
    }

    // --- Classify each matched path relative to source vs. index -----------
    // updates: present in both  → overwrite index entry (staged) / rewrite file
    // inserts: source only      → add to index (staged)
    // removals: index only      → drop from index (staged) / delete file (wt)
    let mut updates: Vec<(BString, ObjectId, Mode, Stat)> = Vec::new();
    let mut inserts: Vec<(BString, ObjectId, Mode, Flags, Stat)> = Vec::new();
    let mut removals: HashSet<BString> = HashSet::new();

    let mut candidates: HashSet<&BString> = HashSet::new();
    candidates.extend(source_map.keys());
    candidates.extend(cur_paths.iter());
    for path in candidates {
        if !path_matches(BStr::new(path), match_all, &spec_set) {
            continue;
        }
        match (source_map.get(path), cur_paths.contains(path)) {
            (Some((id, mode, _flags, stat)), true) => {
                updates.push((path.clone(), *id, *mode, *stat));
            }
            (Some((id, mode, flags, stat)), false) => {
                inserts.push((path.clone(), *id, *mode, *flags, *stat));
            }
            (None, true) => {
                removals.insert(path.clone());
            }
            (None, false) => {}
        }
    }

    // --- Apply staged (index) mutations ------------------------------------
    // Every path that goes through one of git's two entry-mutating calls below is
    // collected here, because `checkout_paths()` invalidates exactly those and
    // repairs nothing (see the cache-tree note at the index write).
    let mut invalidated: Vec<BString> = Vec::new();
    if staged {
        // Resolve unmerged matched paths: drop all their stage entries so the
        // source (a tree) can re-add a single stage-0 entry below.
        //
        // In git the same thing happens inside `add_index_entry()`: a stage-0 entry
        // "will always replace all non-merged entries" (read-cache.c:1273-1283), and
        // the `cache_tree_invalidate_path()` at read-cache.c:1259-1260 has already run.
        if !unmerged_matched.is_empty() {
            let um: HashSet<BString> = unmerged_matched.iter().cloned().collect();
            cur.remove_entries(|_, p, e| e.stage_raw() != 0 && um.contains(&p.to_owned()));
            invalidated.extend(unmerged_matched.iter().cloned());
        }
        let mut need_sort = false;
        for (path, id, mode, stat) in &updates {
            match cur.entry_index_by_path(BStr::new(path)) {
                Ok(idx) => {
                    let e = &mut cur.entries_mut()[idx];
                    // `update_some()` leaves the old entry in place — and so never
                    // reaches `add_index_entry()` — when the tree names the same blob
                    // in the same mode and the entry is not intent-to-add
                    // (builtin/checkout.c:214-229). Only the other case invalidates.
                    if e.id != *id || e.mode != *mode || e.flags.contains(Flags::INTENT_TO_ADD) {
                        invalidated.push(path.clone());
                    }
                    e.id = *id;
                    e.mode = *mode;
                    e.stat = *stat;
                }
                Err(_) => {
                    cur.dangerously_push_entry(*stat, *id, Flags::empty(), *mode, BStr::new(path));
                    invalidated.push(path.clone());
                    need_sort = true;
                }
            }
        }
        if !removals.is_empty() {
            cur.remove_entries(|_, p, _| removals.contains(&p.to_owned()));
        }
        for (path, id, mode, flags, stat) in &inserts {
            cur.dangerously_push_entry(*stat, *id, *flags, *mode, BStr::new(path));
            invalidated.push(path.clone());
        }
        if !inserts.is_empty() || need_sort {
            cur.sort_entries();
        }
    }
    // `remove_marked_cache_entries(the_repository->index, 1)` runs on both arms of
    // `checkout_paths()` — the worktree one (builtin/checkout.c:490) and the
    // index-only one (:689) — and `invalidate` is 1, so each dropped path takes its
    // ancestors down with it (read-cache.c:611-616). The set is empty unless a
    // `--source` tree was named, which is the only way an entry gets `CE_REMOVE`
    // here (builtin/checkout.c:430-436).
    invalidated.extend(removals.iter().cloned());

    // --- Apply worktree checkout -------------------------------------------
    let mut fresh_stats: HashMap<BString, Stat> = HashMap::new();
    if worktree {
        let should_interrupt = AtomicBool::new(false);

        // Subset of the source restricted to matched stage-0 entries, plus any
        // conflict-resolved entries; checked out over the existing worktree.
        let mut subset = source_index.clone();
        subset.remove_entries(|_, p, e| e.stage_raw() != 0 || !path_matches(p, match_all, &spec_set));
        for (path, id, mode) in &resolved_entries {
            subset.dangerously_push_entry(Stat::default(), *id, Flags::empty(), *mode, BStr::new(path));
        }
        if !resolved_entries.is_empty() {
            subset.sort_entries();
        }

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
            &should_interrupt,
            opts,
        )?;

        // Capture the fresh filesystem stats produced by the checkout.
        {
            let b = subset.path_backing();
            for e in subset.entries() {
                fresh_stats.insert(e.path_in(b).to_owned(), e.stat);
            }
        }

        // No-overlay: delete worktree files present before but absent in source.
        // `--overlay` suppresses these; conflict-resolution deletes (a side that
        // removed the file) are applied regardless of overlay.
        if !overlay {
            for path in &removals {
                if let Some(full) = repo.workdir_path(BStr::new(path)) {
                    let _ = std::fs::remove_file(full);
                }
            }
        }
        for path in &resolved_remove {
            if let Some(full) = repo.workdir_path(BStr::new(path)) {
                let _ = std::fs::remove_file(full);
            }
        }

        // --- Recurse into matched submodules --------------------------------
        // git-restore(1): when the restore location includes the working tree
        // and `--recurse-submodules` is given, every matched *active* submodule
        // has its worktree reset to the commit recorded in the superproject
        // (the restore source), overwriting local modifications and detaching
        // the submodule HEAD. Without the flag submodule worktrees are left
        // untouched. A submodule whose gitlink is absent from the source, is
        // inactive, or is uninitialized (no checked-out repo) is skipped.
        if recurse_submodules {
            if let Some(subs) = repo.submodules()? {
                for sm in subs {
                    let sm_path = sm.path()?;
                    if !path_matches(BStr::new(&sm_path), match_all, &spec_set) {
                        continue;
                    }
                    // Target commit = the gitlink recorded in the restore source.
                    let target = match source_map.get(&sm_path) {
                        Some((id, _, _, _)) => *id,
                        None => continue,
                    };
                    if !sm.is_active().unwrap_or(false) {
                        continue;
                    }
                    let sm_repo = match sm.open()? {
                        Some(r) => r,
                        None => continue,
                    };
                    restore_submodule_worktree(&sm_repo, target, &should_interrupt)?;
                }
            }
        }
    }

    // --- Persist the index --------------------------------------------------
    // Written when the index itself changed (--staged), or when the default
    // worktree restore refreshed stats so a later status stays clean. A pure
    // `--source` worktree restore leaves the index untouched (content now
    // differs from it, which git reflects as an unstaged modification).
    // Conflict-resolution (--ours/--theirs/--merge) leaves the unmerged stages
    // intact: only matched clean stage-0 entries get their stats refreshed.
    let index_write_needed = staged || (worktree && source_is_index);
    if index_write_needed {
        if worktree {
            // Only refresh stage-0 entries; unmerged stages (1..3) left for a
            // conflict-resolution restore must keep their recorded stats intact.
            for (e, p) in cur.entries_mut_with_paths() {
                if e.stage_raw() != 0 {
                    continue;
                }
                if let Some(stat) = fresh_stats.get(&p.to_owned()) {
                    e.stat = *stat;
                }
            }
        }
        // `restore` is **not** an `unpack_trees()` verb, and repairing here wrote a
        // *fully valid* cache-tree where git leaves a partly invalidated one —
        // `restore --staged staged.txt` over the one staged addition brought the index
        // back to `HEAD`'s tree, that tree is already in the odb, so the repair
        // re-validated the root git had just marked `-1` (19 bytes longer than stock's).
        //
        // Path-restricted checkout goes through `checkout_paths()`
        // (builtin/checkout.c:517-719), which stages one entry at a time —
        // `update_some()` ends in `add_index_entry(..., ADD_CACHE_OK_TO_ADD |
        // ADD_CACHE_OK_TO_REPLACE)` (builtin/checkout.c:231-232) and
        // `remove_marked_cache_entries(the_repository->index, 1)` drops the rest
        // (:490, :689) — and then writes with a plain `write_locked_index()` (:701).
        // The `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)` in this
        // file belongs to `merge_working_tree()` (:924-925), the *branch-switching*
        // path, which `restore` never takes.
        //
        // `invalidated` is exactly the set of paths that went through one of those two
        // calls; each takes its ancestors with it (`cache_tree_invalidate_path()`,
        // cache-tree.c:113-157). A pure `--worktree` restore from the index mutates no
        // entry at all — `checkout_entry()` only refreshes stat data
        // (`update_ce_after_write()`, entry.c:270-280) — so the set is empty and the
        // extension is carried over untouched, which is what stock leaves too.
        for path in &invalidated {
            cur.invalidate_path_in_tree(path.as_ref());
        }
        super::write_tree::prepare_offset_table(&repo, &mut cur);
        // `do_write_index()` takes `skip_hash` from the settings block for every
        // index it writes (read-cache.c:2830-2831), so a `--staged` restore leaves
        // the same trailer an `add` or an `update-index` would have.
        cur.write(crate::config::index_write_options(&repo))?;
    }

    Ok(ExitCode::SUCCESS)
}
