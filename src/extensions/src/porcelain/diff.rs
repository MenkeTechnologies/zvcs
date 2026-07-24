//! `git diff` — show changes between trees, the index and the worktree.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). Supported invocations:
//!
//! * `git diff`                       — index vs. worktree (unstaged changes)
//! * `git diff --cached [<rev>]`      — `<rev>`-tree (default `HEAD`) vs. the index (staged)
//! * `git diff --staged [<rev>]`      — alias of `--cached`
//! * `git diff <rev>`                 — `<rev>`-tree vs. the worktree
//! * `git diff <revA> <revB>`         — tree vs. tree (also `<revA>..<revB>`)
//!
//! Output formats follow `diff.c`'s model: `--raw`, `--numstat`, `--stat`,
//! `--shortstat`, `--name-only`, `--name-status` and the unified patch can be
//! combined, are emitted in git's fixed order (raw, numstat, stat, shortstat,
//! blank line, patch), and `--name-only`/`--name-status`/`-s` suppress every
//! other format exactly like `diff_setup_done()` does.
//!
//! Beyond the format selectors, these options are honored: `-R` (reverse, for
//! tree/tree and `--cached` pairs), `-z`, `--full-index`, `--abbrev[=<n>]`,
//! `--no-prefix`/`--default-prefix`/`--src-prefix=`/`--dst-prefix=`/`--line-prefix=`,
//! `--summary`, `--compact-summary`/`--no-compact-summary`, `--diff-filter=<...>`,
//! `--patch-with-raw`, `--patch-with-stat`, `--exit-code`, `--quiet`,
//! `--minimal`/`--diff-algorithm=<myers|minimal|histogram>`, and
//! merge-base ranges `<a>...<b>` (diffed as `merge-base(a,b)` against `b`).
//! Submodule/gitlink (`160000`) changes render as the short-format
//! `Subproject commit <oid>` diff for tree/tree and `--cached` pairs.
//!
//! ### Honest limitations (bailed on with a precise message, never faked)
//!
//! * Rename/copy detection is not performed. `--find-renames`/`-M`/`-C` are accepted
//!   (they change nothing on a history without renames) but a real rename still renders
//!   as a deletion plus an addition.
//! * `-R` on a worktree diff bails: the worktree "new" side has no object id to move
//!   onto the old side within this pipeline.
//! * A submodule change against the worktree (`git diff <rev>`) still bails — it needs
//!   the submodule's own repository to resolve the working HEAD.
//! * A type change (regular file ↔ symlink) in the worktree bails.
//! * The `patience` diff algorithm has no imara-diff equivalent and bails (the
//!   `--patience` alias and `diff.algorithm=patience` both surface the same error).
//! * `--line-prefix=<s>` is reproduced by a whole-buffer pass and so only tracks the
//!   newline-terminated formats; combining it with `-z` (NUL-separated records) is
//!   not byte-faithful.
//! * Hunk *section headings* (the text after the second `@@`, i.e. the enclosing function)
//!   are not emitted — gitoxide's unified-diff writer does not compute them.
//! * Magic pathspecs (`:(...)`) and glob pathspecs bail; literal path / directory-prefix
//!   filtering is supported.
//! * `git diff` on an unmerged path renders the combined (`--cc`) patch, and only that —
//!   the duplicate stage-2-vs-worktree pair the raw/name/stat formats also report is not
//!   given a `diff --git` section. `--cached` renders git's `* Unmerged path` line.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::pipeline::{Mode, WorktreeRoots};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, InternedInput, ResourceKind, UnifiedDiff};
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;

// ---------------------------------------------------------------------------
// output formats — mirrors DIFF_FORMAT_* in diff.h
// ---------------------------------------------------------------------------

const F_RAW: u32 = 1 << 0;
const F_NUMSTAT: u32 = 1 << 1;
const F_DIFFSTAT: u32 = 1 << 2;
const F_SHORTSTAT: u32 = 1 << 3;
const F_NAME: u32 = 1 << 4;
const F_NAME_STATUS: u32 = 1 << 5;
const F_PATCH: u32 = 1 << 6;
const F_NO_OUTPUT: u32 = 1 << 7;
const F_SUMMARY: u32 = 1 << 8;

/// The exact `git diff` usage stream, printed on a usage error (exit 129).
const USAGE: &str = "usage: git diff [<options>] [<commit>] [--] [<path>...]\n   or: git diff [<options>] --cached [--merge-base] [<commit>] [--] [<path>...]\n   or: git diff [<options>] [--merge-base] <commit> [<commit>...] <commit> [--] [<path>...]\n   or: git diff [<options>] <commit>...<commit> [--] [<path>...]\n   or: git diff [<options>] <blob> <blob>\n   or: git diff [<options>] --no-index [--] <path> <path> [<pathspec>...]\n\ncommon diff options:\n  -z            output diff-raw with lines terminated with NUL.\n  -p            output patch format.\n  -u            synonym for -p.\n  --patch-with-raw\n                output both a patch and the diff-raw format.\n  --stat        show diffstat instead of patch.\n  --numstat     show numeric diffstat instead of patch.\n  --patch-with-stat\n                output a patch and prepend its diffstat.\n  --name-only   show only names of changed files.\n  --name-status show names and status of changed files.\n  --full-index  show full object name on index lines.\n  --abbrev=<n>  abbreviate object names in diff-tree header and diff-raw.\n  -R            swap input file pairs.\n  -B            detect complete rewrites.\n  -M            detect renames.\n  -C            detect copies.\n  --find-copies-harder\n                try unchanged files as candidate for copy detection.\n  -l<n>         limit rename attempts up to <n> paths.\n  -O<file>      reorder diffs according to the <file>.\n  -S<string>    find filepair whose only one side contains the string.\n  --pickaxe-all\n                show all files diff when -S is used and hit is found.\n  -a  --text    treat all files as text.\n\n";

/// Print the usage stream and return git's usage-error exit code (129).
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Rendering options resolved from the command line and shared by every output
/// format (raw / name / patch). Mirrors the fields of `struct diff_options` that
/// affect byte-level formatting.
struct Render {
    /// Object-name abbreviation length for `--raw` and the patch `index` line.
    abbrev: usize,
    /// `--full-index`: emit the full object name on the patch `index` line.
    full_index: bool,
    /// `-z`: terminate `--raw`/`--name-only`/`--name-status` records with NUL and
    /// suppress path C-quoting.
    z: bool,
    /// The `a/` (source) path prefix; `b/` under `-R`, empty under `--no-prefix`.
    src_prefix: Vec<u8>,
    /// The `b/` (destination) path prefix.
    dst_prefix: Vec<u8>,
    hash_kind: gix::hash::Kind,
}

/// How lines are compared, mirroring xdiff's `XDF_*` whitespace flags.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Whitespace {
    Keep,
    /// `-w` / `--ignore-all-space`: every whitespace byte is ignored.
    IgnoreAll,
    /// `-b` / `--ignore-space-change`: runs of whitespace collapse to one space,
    /// trailing whitespace is ignored.
    IgnoreChange,
    /// `--ignore-space-at-eol`: only trailing whitespace is ignored.
    IgnoreAtEol,
}

/// The "new" side of a change.
enum NewSide {
    /// The path no longer exists (a deletion).
    Absent,
    /// A concrete object in the database (tree/index diffs).
    Blob(ObjectId, EntryKind),
    /// Content that must be read from the worktree at this path (worktree diffs).
    Worktree(EntryKind),
}

/// A single file-level change, normalized across all diff sources.
struct Delta {
    path: BString,
    /// `None` means the path did not exist before (an addition).
    old: Option<(ObjectId, EntryKind)>,
    new: NewSide,
    /// An unmerged (conflicted) index entry: rendered as status `U`, counted as
    /// zero changes by the stat formats, and never diffed through the blob pipeline.
    unmerged: bool,
    /// Stage 2 / stage 3 blobs, present only for the combined (`--cc`) patch of an
    /// unmerged worktree path.
    stages: Option<(ObjectId, ObjectId)>,
}

impl Delta {
    fn new_kind(&self) -> Option<EntryKind> {
        match self.new {
            NewSide::Absent => None,
            NewSide::Blob(_, k) | NewSide::Worktree(k) => Some(k),
        }
    }

    fn plain(path: BString, old: Option<(ObjectId, EntryKind)>, new: NewSide) -> Self {
        Delta {
            path,
            old,
            new,
            unmerged: false,
            stages: None,
        }
    }
}

/// Per-delta blob analysis: the new-side object id plus line counts and the
/// rendered hunks (only computed when a patch is actually requested).
struct Analysis {
    new_id: ObjectId,
    added: u32,
    deleted: u32,
    binary: bool,
    /// `None` when the two sides are byte-identical (e.g. a pure mode change).
    hunks: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

pub fn diff(args: &[String]) -> Result<ExitCode> {
    let mut cached = false;
    let mut ctx: u32 = 3;
    let mut ws = Whitespace::Keep;
    let mut fmt: u32 = 0;
    let mut trailing_paths: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    // Formatting / behavior options resolved below.
    let mut reverse = false;
    let mut z = false;
    let mut full_index = false;
    let mut want_exit_code = false;
    let mut quiet = false;
    let mut src_prefix: Vec<u8> = b"a/".to_vec();
    let mut dst_prefix: Vec<u8> = b"b/".to_vec();
    // `--line-prefix=<s>`: prepended to every emitted line (`diff_line_prefix()`).
    let mut line_prefix: Vec<u8> = Vec::new();
    // `--compact-summary`: annotate `--stat` names with create/delete/mode info.
    let mut compact_summary = false;
    let mut diff_filter: Option<Vec<u8>> = None;
    let mut algorithm: Option<gix::diff::blob::Algorithm> = None;
    // Default resolved from `core.abbrev` after repo discovery (see below);
    // `7` is only a placeholder until then. `--abbrev[=<n>]` overrides explicitly.
    let mut abbrev: usize = 7;
    let mut abbrev_explicit = false;
    // `diff.algorithm` default, applied after argument parsing so a `--minimal` /
    // `--histogram` / `--diff-algorithm=` flag always wins (git precedence).
    let mut config_algorithm: Option<ConfigAlgorithm> = None;

    // Revisions and pathspecs are classified in a single left-to-right pass, so an
    // invalid option value, an ambiguous positional, and any "too many operands"
    // error surface in git's own argument order — `setup_revisions()` is one pass,
    // and the earliest failing token is the one whose exit code git reports.
    let repo = gix::discover(".")?;

    // Config-provided defaults, overridden by the CLI flags parsed below (git's
    // precedence: diff.context < -U, diff.srcPrefix/dstPrefix/noPrefix < the
    // corresponding --*-prefix / --no-prefix flags).
    {
        let snap = repo.config_snapshot();
        if let Some(n) = snap.integer("diff.context") {
            if n >= 0 {
                ctx = n as u32;
            }
        }
        if snap.boolean("diff.noPrefix") == Some(true) {
            src_prefix.clear();
            dst_prefix.clear();
        } else {
            if let Some(p) = snap.string("diff.srcPrefix") {
                src_prefix = p.into();
            }
            if let Some(p) = snap.string("diff.dstPrefix") {
                dst_prefix = p.into();
            }
        }
        // `diff.algorithm` names the default algorithm. git validates it while
        // loading config — an unknown name is a hard error (exit 128) even when a
        // CLI flag would override it — so classify it eagerly here. `patience` is a
        // valid name git renders, but imara-diff has no patience variant, so it is
        // remembered as unrenderable and only rejected if actually used below.
        if let Some(name) = snap.string("diff.algorithm") {
            config_algorithm = Some(parse_config_algorithm(name.as_ref())?);
        }
    }

    let mut revs: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut in_rev_region = true;

    for a in args {
        if after_dashdash {
            trailing_paths.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => after_dashdash = true,
            "--cached" | "--staged" => cached = true,
            "--raw" => fmt |= F_RAW,
            "--numstat" => fmt |= F_NUMSTAT,
            "--shortstat" => fmt |= F_SHORTSTAT,
            "--stat" => fmt |= F_DIFFSTAT,
            "--name-only" => fmt |= F_NAME,
            "--name-status" => fmt |= F_NAME_STATUS,
            "-p" | "-u" | "--patch" => fmt |= F_PATCH,
            "-s" | "--no-patch" => fmt |= F_NO_OUTPUT,
            "--summary" => fmt |= F_SUMMARY,
            // `--compact-summary` (`diff_opt_compact_summary()`): sets the
            // stat-with-summary flag AND turns on `--stat`. `--no-compact-summary`
            // only clears the flag; it never touches the output format.
            "--compact-summary" => {
                compact_summary = true;
                fmt |= F_DIFFSTAT;
            }
            "--no-compact-summary" => compact_summary = false,
            // `--patch-with-raw` / `--patch-with-stat` request two formats at once.
            "--patch-with-raw" => fmt |= F_PATCH | F_RAW,
            "--patch-with-stat" => fmt |= F_PATCH | F_DIFFSTAT,
            "-w" | "--ignore-all-space" => ws = Whitespace::IgnoreAll,
            "-b" | "--ignore-space-change" => ws = Whitespace::IgnoreChange,
            "--ignore-space-at-eol" => ws = Whitespace::IgnoreAtEol,
            "-R" => reverse = true,
            "-z" => z = true,
            "--exit-code" => want_exit_code = true,
            "--quiet" => {
                quiet = true;
                want_exit_code = true;
            }
            "--full-index" => full_index = true,
            "--abbrev" => abbrev = 7,
            "--no-prefix" => {
                src_prefix.clear();
                dst_prefix.clear();
            }
            "--default-prefix" => {
                src_prefix = b"a/".to_vec();
                dst_prefix = b"b/".to_vec();
            }
            // Diff-algorithm selection. imara-diff has no `patience` variant, so
            // only the byte-reproducible algorithms are honored.
            "--minimal" => algorithm = Some(gix::diff::blob::Algorithm::MyersMinimal),
            "--myers" => algorithm = Some(gix::diff::blob::Algorithm::Myers),
            "--histogram" => algorithm = Some(gix::diff::blob::Algorithm::Histogram),
            // `--patience` aliases `--diff-algorithm=patience`; imara-diff has no
            // patience variant, so it bails identically to that flag.
            "--patience" => bail!("diff algorithm {:?} is not available", "patience"),
            // Accepted no-ops: these describe behavior zvcs already produces, or
            // (for rename detection) make no difference without renames present.
            "--no-renames" | "--no-color" | "--color=never" | "--ignore-blank-lines"
            | "--ignore-cr-at-eol" | "--find-renames" | "--find-copies" | "-M" | "-C"
            | "--rename-empty" | "--no-rename-empty" | "--text" | "-a"
            | "--indent-heuristic" | "--no-ext-diff" | "--ext-diff" | "--textconv"
            | "--no-textconv" | "--ita-invisible-in-index" | "--ita-visible-in-index" => {}
            s if s == "--ignore-submodules" || s.starts_with("--ignore-submodules=") => {}
            s if s.starts_with("--diff-filter=") => {
                diff_filter = Some(s["--diff-filter=".len()..].as_bytes().to_vec());
            }
            s if s.starts_with("--abbrev=") => {
                let raw = &s["--abbrev=".len()..];
                match raw.parse::<usize>() {
                    // git clamps `--abbrev` to the range [4, hexsz].
                    Ok(n) => {
                        abbrev = n.clamp(4, repo.object_hash().len_in_hex());
                        abbrev_explicit = true;
                    }
                    Err(_) => {
                        eprintln!("error: option `abbrev' expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if s.starts_with("--src-prefix=") => {
                src_prefix = s["--src-prefix=".len()..].as_bytes().to_vec();
            }
            s if s.starts_with("--dst-prefix=") => {
                dst_prefix = s["--dst-prefix=".len()..].as_bytes().to_vec();
            }
            s if s.starts_with("--line-prefix=") => {
                line_prefix = s["--line-prefix=".len()..].as_bytes().to_vec();
            }
            s if s.starts_with("--diff-algorithm=") => {
                match &s["--diff-algorithm=".len()..] {
                    "myers" | "default" => algorithm = Some(gix::diff::blob::Algorithm::Myers),
                    "minimal" => algorithm = Some(gix::diff::blob::Algorithm::MyersMinimal),
                    "histogram" => algorithm = Some(gix::diff::blob::Algorithm::Histogram),
                    other => bail!("diff algorithm {other:?} is not available"),
                }
            }
            s if s.starts_with("--stat=") || s.starts_with("--stat-") => fmt |= F_DIFFSTAT,
            s if s.starts_with("--find-renames=") || s.starts_with("--find-copies=") => {}
            s if s.starts_with("-M") || s.starts_with("-C") => {}
            // `-U` / `--unified[=<n>]`: git's `diff_opt_unified()` enables patch
            // output unconditionally, so any of these implies `-p` even alongside
            // `--raw`/`--stat`/`--numstat`. A bare `-U` / `--unified` keeps the
            // default context; an attached value is parsed with strtol semantics.
            "-U" | "--unified" => fmt |= F_PATCH,
            s if s.starts_with("-U") || s.starts_with("--unified=") => {
                let val = s.strip_prefix("--unified=").unwrap_or(&s[2..]);
                match parse_unified(val) {
                    UnifiedValue::Context(n) => {
                        ctx = n;
                        fmt |= F_PATCH;
                    }
                    UnifiedValue::NotNumeric => {
                        eprintln!("error: --unified expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                    UnifiedValue::Negative => {
                        eprintln!("error: --unified expects a non-negative integer");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if s.starts_with('-') => bail!("unsupported option {s:?}"),
            s => {
                // A positional is a revision while we are still in the revision
                // region, otherwise a pathspec. Once a positional is neither a
                // resolvable revision nor an existing path, git dies with the
                // "ambiguous argument" fatal (128) at exactly this point — before
                // any later option-value or operand-count check can fire.
                if in_rev_region {
                    if s.contains("...") && looks_like_range(s) {
                        // `A...B` diffs the merge-base of A and B against B, exactly
                        // like `git diff $(git merge-base A B) B`. Empty sides default
                        // to `HEAD`, mirroring `setup_revisions()`.
                        let (l, r) = s.split_once("...").expect("checked contains");
                        let left = if l.is_empty() { "HEAD" } else { l };
                        let right = if r.is_empty() { "HEAD" } else { r };
                        let lid = repo.rev_parse_single(left)?.object()?.peel_to_commit()?.id;
                        let rid = repo.rev_parse_single(right)?.object()?.peel_to_commit()?.id;
                        let base = repo.merge_base(lid, rid)?.detach();
                        revs.push(base.to_hex().to_string());
                        revs.push(right.to_string());
                        continue;
                    }
                    if s.contains("..") && looks_like_range(s) {
                        let (l, r) = s.split_once("..").expect("checked contains");
                        revs.push(if l.is_empty() { "HEAD".into() } else { l.into() });
                        revs.push(if r.is_empty() { "HEAD".into() } else { r.into() });
                        continue;
                    }
                    if repo.rev_parse_single(s).is_ok() {
                        revs.push(s.to_string());
                        continue;
                    }
                    if std::fs::symlink_metadata(s).is_err() {
                        eprintln!(
                            "fatal: ambiguous argument '{s}': unknown revision or path not in the working tree."
                        );
                        eprintln!("Use '--' to separate paths from revisions, like this:");
                        eprintln!("'git <command> [<revision>...] -- [<file>...]'");
                        return Ok(ExitCode::from(128));
                    }
                    in_rev_region = false;
                }
                paths.push(s.to_string());
            }
        }
    }
    paths.extend(trailing_paths);

    // Apply the `diff.algorithm` default only when no `--minimal`/`--histogram`/
    // `--diff-algorithm=` flag set the algorithm on the command line (git's
    // precedence). A `patience` default is git-valid but has no imara-diff
    // equivalent, so it bails exactly like `--diff-algorithm=patience` — but only
    // here, where it would actually be used, so an overriding flag is honored.
    if algorithm.is_none() {
        match config_algorithm {
            Some(ConfigAlgorithm::Use(a)) => algorithm = Some(a),
            Some(ConfigAlgorithm::Patience) => {
                bail!("diff algorithm {:?} is not available", "patience")
            }
            None => {}
        }
    }

    // `diff_setup_done()`: --name-only / --name-status / -s are mutually exclusive
    // and, when present, suppress every other output format.
    if (fmt & (F_NAME | F_NAME_STATUS | F_NO_OUTPUT)).count_ones() > 1 {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }
    if fmt & (F_NAME | F_NAME_STATUS | F_NO_OUTPUT) != 0 {
        fmt &= !(F_RAW | F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT | F_PATCH);
    }
    // `--name-only`/`--name-status` suppress `--summary`, but `-s` does not.
    if fmt & (F_NAME | F_NAME_STATUS) != 0 {
        fmt &= !F_SUMMARY;
    }
    if fmt == 0 {
        fmt = F_PATCH;
    }

    // `cmd_diff()` rejects `--cached`/`--staged` with two or more revisions as a
    // usage error (129), printing the full usage stream — this is checked after
    // `setup_revisions()`, so an earlier ambiguous positional (128) wins.
    if cached && revs.len() >= 2 {
        return Ok(usage_error());
    }

    for p in &paths {
        if p.starts_with(':') || p.bytes().any(|b| matches!(b, b'*' | b'?' | b'[')) {
            bail!("magic/glob pathspecs are not supported, got {p:?}");
        }
    }

    // Three or more revisions request a dense combined ("--cc") diff of the first
    // revision against the rest, exactly like `builtin_diff_combined()`.
    if !cached && revs.len() >= 3 {
        return combined_multi(&repo, &revs, &paths, fmt, ctx, &line_prefix);
    }

    // ---- collect the normalized change list -------------------------------
    let hash_kind = repo.object_hash();
    let mut deltas: Vec<Delta> = Vec::new();
    let mut worktree_mode = false;
    let mut cache;

    if cached {
        if revs.len() == 2 {
            bail!("--cached with two revisions is not supported");
        }
        collect_tree_index(&repo, revs.first(), &mut deltas)?;
        cache = repo.diff_resource_cache_for_tree_diff()?;
    } else if revs.len() == 2 {
        let old_tree = repo.rev_parse_single(revs[0].as_str())?.object()?.peel_to_tree()?;
        let new_tree = repo.rev_parse_single(revs[1].as_str())?.object()?.peel_to_tree()?;
        let changes =
            repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(gix::diff::Options::default()))?;
        for change in changes {
            collect_tree_change(change, &mut deltas)?;
        }
        cache = repo.diff_resource_cache_for_tree_diff()?;
    } else {
        let workdir = repo
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("this operation must be run in a work tree"))?
            .to_owned();
        if revs.len() == 1 {
            collect_tree_worktree(&repo, &revs[0], &paths, &mut deltas)?;
        } else {
            collect_index_worktree(&repo, &workdir, &paths, &mut deltas)?;
        }
        cache = repo.diff_resource_cache(
            Mode::ToGit,
            WorktreeRoots {
                old_root: None,
                new_root: Some(workdir.clone()),
            },
        )?;
        worktree_mode = true;
    }

    // For tree/index sources, apply literal pathspec filtering here (the worktree
    // iterators already filtered via `patterns`).
    if !worktree_mode && !paths.is_empty() {
        deltas.retain(|d| paths.iter().any(|p| path_matches(&d.path, p)));
    }

    // `-R`: swap the two sides of every pair. The worktree "new" side has no object
    // id to move onto the old side, so a reversed worktree diff genuinely cannot be
    // expressed through this pipeline.
    if reverse {
        if worktree_mode {
            bail!("-R (reverse) with a worktree diff is not supported");
        }
        std::mem::swap(&mut src_prefix, &mut dst_prefix);
        for d in &mut deltas {
            reverse_delta(d);
        }
    }

    // `--diff-filter`: keep only deltas whose status letter is selected.
    if let Some(filter) = &diff_filter {
        deltas.retain(|d| diff_filter_selected(filter, status_char(d)));
    }

    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));

    // ---- analyze every delta once -----------------------------------------
    // `--quiet`/`-s` produce no output, so the patch bodies are never needed.
    let workdir = repo.workdir().map(|p| p.to_owned());
    let want_patch = fmt & F_PATCH != 0 && !quiet;
    let mut analyses: Vec<Analysis> = Vec::with_capacity(deltas.len());
    if !quiet {
        for delta in &deltas {
            analyses.push(analyze(
                &mut cache,
                &repo.objects,
                delta,
                ctx,
                ws,
                hash_kind,
                workdir.as_deref(),
                want_patch,
                algorithm,
            )?);
        }
    }

    // With no explicit `--abbrev`, the `index` line honors `core.abbrev`
    // (git's DEFAULT_ABBREV / auto), not a hardcoded 7. `--full-index` still
    // wins at render time regardless of this length.
    if !abbrev_explicit {
        abbrev = crate::abbrev::configured_abbrev(&repo, repo.object_hash().len_in_hex());
    }

    let r = Render {
        abbrev,
        full_index,
        z,
        src_prefix,
        dst_prefix,
        hash_kind,
    };

    // ---- render, in `diff_flush()` order ----------------------------------
    // `diff_flush()` bails out before printing anything at all when the change
    // queue is empty, so even `--shortstat` stays silent on a clean tree.
    let mut out: Vec<u8> = Vec::new();
    let mut separator = false;
    if !quiet && !deltas.is_empty() {
        if fmt & (F_RAW | F_NAME | F_NAME_STATUS) != 0 {
            for delta in &deltas {
                if fmt & (F_RAW | F_NAME_STATUS) != 0 {
                    render_raw(&mut out, delta, fmt, &r);
                } else {
                    out.extend_from_slice(&name_field(&delta.path, r.z));
                    out.push(if r.z { 0 } else { b'\n' });
                }
            }
            separator = true;
        }

        if fmt & (F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT) != 0 {
            if fmt & F_NUMSTAT != 0 {
                render_numstat(&mut out, &deltas, &analyses);
            }
            if fmt & F_DIFFSTAT != 0 {
                render_stat(&mut out, &deltas, &analyses, compact_summary);
            }
            if fmt & F_SHORTSTAT != 0 {
                render_shortstat(&mut out, &deltas, &analyses);
            }
            separator = true;
        }

        if fmt & F_SUMMARY != 0 {
            render_summary(&mut out, &deltas);
            separator = true;
        }

        if fmt & F_PATCH != 0 {
            if separator {
                out.push(b'\n');
            }
            // `run_diff_files()` queues an unmerged path twice — once as the `U`
            // pair and once as the ordinary stage-2-vs-worktree modification — and
            // the raw/name/stat formats above print both. The patch format prints
            // only the combined (`--cc`) patch for such a path; the duplicate pair
            // contributes no `diff --git` section of its own.
            let unmerged: BTreeSet<&BString> =
                deltas.iter().filter(|d| d.unmerged).map(|d| &d.path).collect();
            for (delta, an) in deltas.iter().zip(&analyses) {
                if !delta.unmerged && unmerged.contains(&delta.path) {
                    continue;
                }
                render_patch(&mut out, &repo, delta, an, ctx, &r)?;
            }
        }
    }

    // `--line-prefix`: `diff_line_prefix()` prepends the string to every emitted
    // line, so a whole-buffer pass over the newline-terminated output reproduces it.
    let out = apply_line_prefix(out, &line_prefix);

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    // `--exit-code`/`--quiet`: exit 1 when any difference was reported.
    if want_exit_code && !deltas.is_empty() {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

/// Reverse (`-R`) one object-backed pair: the new side becomes the old side and
/// vice-versa. Worktree pairs are never reversed (rejected earlier).
fn reverse_delta(d: &mut Delta) {
    let new_as_old = match &d.new {
        NewSide::Blob(id, k) => Some((*id, *k)),
        NewSide::Absent => None,
        NewSide::Worktree(_) => return,
    };
    let old_as_new = match d.old {
        Some((id, k)) => NewSide::Blob(id, k),
        None => NewSide::Absent,
    };
    d.old = new_as_old;
    d.new = old_as_new;
    if let Some((a, b)) = d.stages {
        d.stages = Some((b, a));
    }
}

/// `--diff-filter`: an uppercase letter selects a status, its lowercase excludes it.
/// When only exclusions are given every other status is kept; when any inclusion is
/// present, unlisted statuses are dropped — matching `diff_opt_diff_filter()`.
fn diff_filter_selected(filter: &[u8], status: u8) -> bool {
    let up = status.to_ascii_uppercase();
    if filter.iter().any(|&f| f == up.to_ascii_lowercase()) {
        return false;
    }
    let has_include = filter.iter().any(|f| f.is_ascii_uppercase());
    if has_include {
        filter.iter().any(|&f| f == up)
    } else {
        true
    }
}

// ---------------------------------------------------------------------------
// change collection
// ---------------------------------------------------------------------------

/// `<tree>` vs. the index (`--cached`). gitoxide's index diff skips unmerged
/// entries, so those are re-added here the way `do_oneway_diff()` does: a single
/// `U` pair whose old side comes from the tree.
fn collect_tree_index(
    repo: &gix::Repository,
    spec: Option<&String>,
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let tree_id = tree_id_for(repo, spec)?;
    let index = repo.index_or_load_from_head()?;
    repo.tree_index_status(
        &tree_id,
        &index,
        None,
        gix::status::tree_index::TrackRenames::Disabled,
        |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
            collect_index_change(change, deltas);
            Ok(gix::diff::index::Action::Continue(()))
        },
    )?;

    let tree = repo.find_object(tree_id)?.peel_to_tree()?;
    for path in unmerged_paths(&index) {
        let old = tree_entry(&tree, &path)?;
        deltas.push(Delta {
            path,
            old,
            new: NewSide::Absent,
            unmerged: true,
            stages: None,
        });
    }
    Ok(())
}

/// `<tree>` vs. the worktree. Reproduces `diff-index`: start from the tree-to-index
/// difference, then let the index-to-worktree difference override the "new" side.
fn collect_tree_worktree(
    repo: &gix::Repository,
    spec: &str,
    paths: &[String],
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let tree_id = repo.rev_parse_single(spec)?.object()?.peel_to_tree()?.id;
    let patterns: Vec<BString> = paths.iter().map(|p| BString::from(p.as_str())).collect();

    // Path -> new side, in index order (the order `diff-index` reports in).
    let mut new_sides: BTreeMap<BString, NewSide> = BTreeMap::new();
    let mut gitlink: Option<BString> = None;

    let iter = repo
        .status(gix::progress::Discard)?
        .head_tree(tree_id)
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
        .index_worktree_options_mut(|o| {
            o.dirwalk_options = None; // exclude untracked files, matching `git diff`
            o.rewrites = None; // no rename detection
        })
        .into_iter(patterns)?;

    for item in iter {
        match item? {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                let deleted = matches!(change, ChangeRef::Deletion { .. });
                let (loc, _, entry_mode, oid) = change.fields();
                let (location, id) = (loc.to_owned(), oid.to_owned());
                match if deleted { None } else { index_mode_kind(entry_mode) } {
                    Some(EntryKind::Commit) => gitlink = Some(location),
                    Some(k) => {
                        new_sides.insert(location, NewSide::Blob(id, k));
                    }
                    None => {
                        new_sides.insert(location, NewSide::Absent);
                    }
                }
            }
            gix::status::Item::IndexWorktree(item) => {
                if let Some((path, new)) = worktree_new_side(item)? {
                    new_sides.insert(path, new);
                }
            }
        }
    }
    if let Some(p) = gitlink {
        bail!("submodule/gitlink change at {p:?} is not supported");
    }

    let tree = repo.find_object(tree_id)?.peel_to_tree()?;
    for (path, new) in new_sides {
        let old = tree_entry(&tree, &path)?;
        if matches!(old, Some((_, EntryKind::Commit))) {
            bail!("submodule/gitlink change at {path:?} is not supported");
        }
        // A path that neither existed in the tree nor exists now is not a change.
        if old.is_none() && matches!(new, NewSide::Absent) {
            continue;
        }
        // Unchanged content that only travelled through the index is not a change.
        if let (Some((oid, ok)), NewSide::Blob(nid, nk)) = (&old, &new) {
            if oid == nid && ok == nk {
                continue;
            }
        }
        deltas.push(Delta::plain(path, old, new));
    }
    Ok(())
}

/// The index vs. the worktree (plain `git diff`).
fn collect_index_worktree(
    repo: &gix::Repository,
    workdir: &std::path::Path,
    paths: &[String],
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let index = repo.index_or_empty()?;
    let conflicts = conflict_stages(&index);
    let patterns: Vec<BString> = paths.iter().map(|p| BString::from(p.as_str())).collect();
    let iter = repo
        .status(gix::progress::Discard)?
        .index_worktree_options_mut(|o| {
            o.dirwalk_options = None; // exclude untracked files, matching `git diff`
            o.rewrites = None; // no rename detection
        })
        .into_index_worktree_iter(patterns)?;

    let mut seen_conflicts: Vec<BString> = Vec::new();
    for item in iter {
        let item = item?;
        if let gix::status::index_worktree::Item::Modification {
            rela_path, status, ..
        } = &item
        {
            if matches!(
                status,
                gix::status::plumbing::index_as_worktree::EntryStatus::Conflict { .. }
            ) {
                if !seen_conflicts.contains(rela_path) {
                    seen_conflicts.push(rela_path.clone());
                }
                continue;
            }
        }
        if let Some((path, new)) = worktree_new_side(item)? {
            // A worktree entry with no index counterpart cannot happen here (the
            // dirwalk is off), so the old side is always the index entry.
            let entry = index
                .entry_by_path(path.as_bstr())
                .ok_or_else(|| anyhow::anyhow!("no index entry for {path:?}"))?;
            let old_kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
            deltas.push(Delta::plain(path, Some((entry.id, old_kind)), new));
        }
    }

    // `run_diff_files()` reports an unmerged path twice: once as the `U` pair, and
    // once as the ordinary stage-2-vs-worktree modification.
    for path in seen_conflicts {
        let stages = conflicts.get(&path);
        let wt_kind = worktree_kind(workdir, &path);
        deltas.push(Delta {
            path: path.clone(),
            old: None,
            new: match wt_kind {
                Some(k) => NewSide::Worktree(k),
                None => NewSide::Absent,
            },
            unmerged: true,
            stages: stages.map(|s| (s.ours.0, s.theirs.0)),
        });
        if let (Some(s), Some(k)) = (stages, wt_kind) {
            deltas.push(Delta::plain(path, Some((s.ours.0, s.ours.1)), NewSide::Worktree(k)));
        }
    }
    Ok(())
}

/// The "new" side an index-vs-worktree status item implies, or `None` when the
/// item carries no textual change.
fn worktree_new_side(
    item: gix::status::index_worktree::Item,
) -> Result<Option<(BString, NewSide)>> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let Item::Modification {
        entry,
        rela_path,
        status,
        ..
    } = item
    else {
        // Untracked/ignored entries never appear in `git diff` (the dirwalk is off),
        // and rename tracking is disabled.
        return Ok(None);
    };
    let old_kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
    if matches!(old_kind, EntryKind::Commit) {
        // Submodule content change; `git diff` renders this specially. Skip.
        return Ok(None);
    }
    Ok(match status {
        EntryStatus::Change(Change::Modification {
            executable_bit_changed,
            ..
        }) => {
            let new_kind = if executable_bit_changed {
                toggle_exec(old_kind)
            } else {
                old_kind
            };
            Some((rela_path, NewSide::Worktree(new_kind)))
        }
        EntryStatus::Change(Change::Removed) => Some((rela_path, NewSide::Absent)),
        EntryStatus::Change(Change::Type { .. }) => {
            bail!("type change at {rela_path:?} is not supported")
        }
        // A conflicted path still has worktree content; only `git diff` with no
        // revision treats it specially, and that caller intercepts it first.
        EntryStatus::Conflict { .. } => Some((rela_path, NewSide::Worktree(old_kind))),
        // Submodule content modification, intent-to-add, and stat-only refreshes
        // produce no textual diff.
        EntryStatus::Change(Change::SubmoduleModification(_))
        | EntryStatus::IntentToAdd
        | EntryStatus::NeedsUpdate(_) => None,
    })
}

/// The stage 2 ("ours") and stage 3 ("theirs") blobs of a conflicted path.
struct Stages {
    ours: (ObjectId, EntryKind),
    theirs: (ObjectId, EntryKind),
}

fn conflict_stages(index: &gix::index::State) -> BTreeMap<BString, Stages> {
    let mut per_path: BTreeMap<BString, [Option<(ObjectId, EntryKind)>; 2]> = BTreeMap::new();
    for entry in index.entries() {
        let slot = match entry.stage() {
            gix::index::entry::Stage::Ours => 0,
            gix::index::entry::Stage::Theirs => 1,
            _ => continue,
        };
        let kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
        per_path
            .entry(entry.path(index).to_owned())
            .or_default()[slot] = Some((entry.id, kind));
    }
    per_path
        .into_iter()
        .filter_map(|(path, [ours, theirs])| {
            Some((
                path,
                Stages {
                    ours: ours?,
                    theirs: theirs?,
                },
            ))
        })
        .collect()
}

/// Every path with at least one non-zero stage, in index order.
fn unmerged_paths(index: &gix::index::State) -> Vec<BString> {
    let mut out: Vec<BString> = Vec::new();
    for entry in index.entries() {
        if entry.stage() == gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let path = entry.path(index).to_owned();
        if out.last() != Some(&path) {
            out.push(path);
        }
    }
    out
}

fn worktree_kind(workdir: &std::path::Path, path: &BString) -> Option<EntryKind> {
    let full = workdir.join(gix::path::from_bstr(path.as_bstr()));
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if meta.is_symlink() {
        return Some(EntryKind::Link);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return Some(EntryKind::BlobExecutable);
        }
    }
    Some(EntryKind::Blob)
}

fn tree_entry(tree: &gix::Tree<'_>, path: &BString) -> Result<Option<(ObjectId, EntryKind)>> {
    let components: Vec<&[u8]> = path.as_slice().split(|b| *b == b'/').collect();
    let entry = tree.lookup_entry(components)?;
    Ok(entry.map(|e| (e.object_id(), e.mode().kind())))
}

/// A single revision spec into a tree id, defaulting to `HEAD^{tree}` (or the empty
/// tree if `HEAD` is unborn) when no spec is given.
fn tree_id_for(repo: &gix::Repository, spec: Option<&String>) -> Result<ObjectId> {
    Ok(match spec {
        Some(s) => repo.rev_parse_single(s.as_str())?.object()?.peel_to_tree()?.id,
        None => repo.head_tree_id_or_empty()?.detach(),
    })
}

/// `true` if a token looks like a revision range rather than a filename that merely
/// contains `..` (e.g. `../foo`). Ranges don't contain `/` and don't start with `.`.
fn looks_like_range(tok: &str) -> bool {
    !tok.starts_with('.') && !tok.contains('/')
}

/// A validated `diff.algorithm` config value: a renderable algorithm, or the
/// git-valid-but-unrenderable `patience`.
enum ConfigAlgorithm {
    Use(gix::diff::blob::Algorithm),
    Patience,
}

/// Parse a `diff.algorithm` config value the way git's config loader does:
/// case-insensitively, accepting `myers`/`default`, `minimal`, `histogram` and
/// `patience`. Any other name is a hard config error (git exits 128) — rendered
/// here as the same "not available" bail the `--diff-algorithm=` flag uses.
fn parse_config_algorithm(name: &gix::bstr::BStr) -> Result<ConfigAlgorithm> {
    use gix::diff::blob::Algorithm::{Histogram, Myers, MyersMinimal};
    let lower = name.to_ascii_lowercase();
    Ok(match lower.as_slice() {
        b"myers" | b"default" => ConfigAlgorithm::Use(Myers),
        b"minimal" => ConfigAlgorithm::Use(MyersMinimal),
        b"histogram" => ConfigAlgorithm::Use(Histogram),
        b"patience" => ConfigAlgorithm::Patience,
        _ => bail!("diff algorithm {:?} is not available", name.to_str_lossy()),
    })
}

/// The three outcomes of parsing a `-U`/`--unified` value, mirroring the two
/// distinct `error()` paths in git's `diff_opt_unified()`.
enum UnifiedValue {
    Context(u32),
    /// Trailing non-digit bytes (`*s != '\0'`) — "expects a numerical value".
    NotNumeric,
    /// A negative integer — "expects a non-negative integer".
    Negative,
}

/// Parse a `-U`/`--unified` value with git's `strtol(arg, &s, 10)` semantics:
/// leading whitespace and an optional sign are skipped, decimal digits are read,
/// and any trailing byte that is not part of the number (`*s != '\0'`) makes the
/// value non-numerical. An empty string yields context 0 (`strtol("")` performs no
/// conversion and leaves `*s` at the terminating NUL, which git accepts). Overflow
/// saturates rather than wrapping to a negative like git's `int` truncation would.
fn parse_unified(arg: &str) -> UnifiedValue {
    let b = arg.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = matches!(b.get(i), Some(b'-'));
    if matches!(b.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let digits_start = i;
    let mut val: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        val = val.saturating_mul(10).saturating_add((b[i] - b'0') as i64);
        i += 1;
    }
    // No digits consumed: strtol performs no conversion and leaves `s` at the
    // original pointer (offset 0), so anything but a wholly empty string is junk.
    let end = if i == digits_start { 0 } else { i };
    if end < b.len() {
        return UnifiedValue::NotNumeric;
    }
    if neg && val != 0 {
        return UnifiedValue::Negative;
    }
    UnifiedValue::Context(val.min(u32::MAX as i64) as u32)
}

/// Convert an index-entry mode into an [`EntryKind`], or `None` for tree entries.
fn index_mode_kind(mode: gix::index::entry::Mode) -> Option<EntryKind> {
    mode.to_tree_entry_mode().map(|m| m.kind())
}

/// Record a change from a tree-vs-index diff. Gitlink (`160000`) entries flow
/// through as `EntryKind::Commit` deltas, which `analyze()` renders as the
/// `Subproject commit <oid>` short-format submodule diff.
fn collect_index_change(change: gix::diff::index::ChangeRef<'_, '_>, deltas: &mut Vec<Delta>) {
    use gix::diff::index::ChangeRef;
    match change {
        ChangeRef::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if let Some(k) = index_mode_kind(entry_mode) {
                deltas.push(Delta::plain(
                    location.into_owned(),
                    None,
                    NewSide::Blob(id.into_owned(), k),
                ));
            }
        }
        ChangeRef::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if let Some(k) = index_mode_kind(entry_mode) {
                deltas.push(Delta::plain(
                    location.into_owned(),
                    Some((id.into_owned(), k)),
                    NewSide::Absent,
                ));
            }
        }
        ChangeRef::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => {
            let ok = index_mode_kind(previous_entry_mode);
            let nk = index_mode_kind(entry_mode);
            if let (Some(ok), Some(nk)) = (ok, nk) {
                deltas.push(Delta::plain(
                    location.into_owned(),
                    Some((previous_id.into_owned(), ok)),
                    NewSide::Blob(id.into_owned(), nk),
                ));
            }
        }
        // Rewrites are disabled, so this never fires; ignore defensively.
        ChangeRef::Rewrite { .. } => {}
    }
}

/// Record a change from a tree-vs-tree diff.
fn collect_tree_change(
    change: gix::object::tree::diff::ChangeDetached,
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    use gix::object::tree::diff::ChangeDetached;
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            // Gitlinks (`160000`) flow through as `EntryKind::Commit` and are rendered
            // by `analyze()` as a `Subproject commit` submodule diff.
            if !entry_mode.is_tree() {
                deltas.push(Delta::plain(location, None, NewSide::Blob(id, entry_mode.kind())));
            }
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if !entry_mode.is_tree() {
                deltas.push(Delta::plain(location, Some((id, entry_mode.kind())), NewSide::Absent));
            }
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            if !entry_mode.is_tree() {
                deltas.push(Delta::plain(
                    location,
                    Some((previous_id, previous_entry_mode.kind())),
                    NewSide::Blob(id, entry_mode.kind()),
                ));
            }
        }
        // Rewrites are disabled, so this never fires; ignore defensively.
        ChangeDetached::Rewrite { .. } => {}
    }
    Ok(())
}

/// The `-p`/`--patch` body for one commit: its tree diffed against `parent`'s
/// tree (the empty tree for a root commit), rendered as git's `diff --git` patch
/// with `ctx` lines of context. This runs the exact delta pipeline `git diff`'s
/// tree-vs-tree path uses — `collect_tree_change` → `analyze` → `render_patch` —
/// so `git log -p` and `git diff` produce byte-identical patches (same index-line
/// abbreviation, `a/`/`b/` prefixes, and hunk formatting). Merge commits are the
/// caller's concern: git shows no diff for them without `-m`/`-c`/`--cc`, so `log`
/// only invokes this for commits with a single parent (or none).
pub(crate) fn commit_patch(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
    ctx: u32,
) -> Result<Vec<u8>> {
    let new_tree = commit.tree()?;
    let old_tree = match parent {
        Some(pid) => Some(repo.find_object(pid)?.try_into_commit()?.tree()?),
        None => None,
    };

    let changes = repo.diff_tree_to_tree(
        old_tree.as_ref(),
        Some(&new_tree),
        Some(gix::diff::Options::default()),
    )?;
    let mut deltas: Vec<Delta> = Vec::new();
    for change in changes {
        collect_tree_change(change, &mut deltas)?;
    }
    // `diff_flush()` order: paths ascending. Tree diffs never produce unmerged
    // deltas, so the secondary key is inert here but kept for parity with `diff()`.
    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));

    let hash_kind = repo.object_hash();
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;
    let r = Render {
        // `git log -p`/`git show` honor core.abbrev on the index line, same as
        // `git diff` — resolved once here rather than hardcoded.
        abbrev: crate::abbrev::configured_abbrev(repo, hash_kind.len_in_hex()),
        full_index: false,
        z: false,
        src_prefix: b"a/".to_vec(),
        dst_prefix: b"b/".to_vec(),
        hash_kind,
    };

    let mut out: Vec<u8> = Vec::new();
    for delta in &deltas {
        // A worktree side never arises for a tree diff, so `workdir` is `None`.
        let an = analyze(
            &mut cache,
            &repo.objects,
            delta,
            ctx,
            Whitespace::Keep,
            hash_kind,
            None,
            true,
            None,
        )?;
        render_patch(&mut out, repo, delta, &an, ctx, &r)?;
    }
    Ok(out)
}

fn toggle_exec(k: EntryKind) -> EntryKind {
    match k {
        EntryKind::Blob => EntryKind::BlobExecutable,
        EntryKind::BlobExecutable => EntryKind::Blob,
        other => other,
    }
}

/// `true` if `path` equals `pat` or lives under the directory `pat`.
fn path_matches(path: &BString, pat: &str) -> bool {
    let pat = pat.trim_end_matches('/').as_bytes();
    let path = path.as_slice();
    path == pat || (path.len() > pat.len() && path.starts_with(pat) && path[pat.len()] == b'/')
}

// ---------------------------------------------------------------------------
// blob analysis
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn analyze(
    cache: &mut gix::diff::blob::Platform,
    objects: &gix::OdbHandle,
    delta: &Delta,
    ctx: u32,
    ws: Whitespace,
    hash_kind: gix::hash::Kind,
    workdir: Option<&std::path::Path>,
    want_patch: bool,
    algo_override: Option<gix::diff::blob::Algorithm>,
) -> Result<Analysis> {
    let null = hash_kind.null();
    if delta.unmerged {
        return Ok(Analysis {
            new_id: null,
            added: 0,
            deleted: 0,
            binary: false,
            hunks: None,
        });
    }

    // Submodule (gitlink) pairs cannot be read through the blob pipeline. git
    // renders them as a synthetic one-line `Subproject commit <oid>` blob per side,
    // so a modification counts as one insertion and one deletion.
    let old_commit = match delta.old {
        Some((id, EntryKind::Commit)) => Some(id),
        _ => None,
    };
    let new_commit = match &delta.new {
        NewSide::Blob(id, EntryKind::Commit) => Some(*id),
        _ => None,
    };
    if old_commit.is_some() || new_commit.is_some() {
        return analyze_gitlink(old_commit, new_commit, null, ctx, want_patch, algo_override);
    }

    let path = delta.path.as_bstr();
    let old_kind = delta.old.map(|(_, k)| k).unwrap_or(EntryKind::Blob);
    match delta.old {
        Some((id, k)) => cache.set_resource(id, k, path, ResourceKind::OldOrSource, objects)?,
        None => cache.set_resource(null, old_kind, path, ResourceKind::OldOrSource, objects)?,
    };
    match &delta.new {
        NewSide::Blob(id, k) => {
            cache.set_resource(*id, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        NewSide::Worktree(k) => {
            // With `new_root` set on the cache, a null id reads from the worktree by path.
            cache.set_resource(null, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        NewSide::Absent => {
            cache.set_resource(null, old_kind, path, ResourceKind::NewOrDestination, objects)?;
        }
    };

    let prep = cache.prepare_diff()?;

    let new_id: ObjectId = match &delta.new {
        NewSide::Absent => null,
        NewSide::Blob(id, _) => *id,
        NewSide::Worktree(_) => {
            if !prep.new.id.is_null() {
                prep.new.id.to_owned()
            } else if let Some(buf) = prep.new.data.as_slice() {
                gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, buf)?
            } else {
                // Binary worktree content: hash the raw file (filters not applied).
                let base = workdir.ok_or_else(|| anyhow::anyhow!("missing work tree"))?;
                let full = base.join(gix::path::from_bstr(path));
                let bytes = std::fs::read(&full)?;
                gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &bytes)?
            }
        }
    };

    match prep.operation {
        Operation::SourceOrDestinationIsBinary => Ok(Analysis {
            new_id,
            added: 0,
            deleted: 0,
            binary: true,
            hunks: None,
        }),
        Operation::ExternalCommand { .. } => {
            bail!("external diff drivers are not supported for {path:?}")
        }
        Operation::InternalDiff { algorithm } => {
            // `--minimal`/`--histogram`/`--diff-algorithm=` override the default.
            let algorithm = algo_override.unwrap_or(algorithm);
            let before: Vec<&[u8]> = byte_lines(prep.old.data.as_slice().unwrap_or_default());
            let after: Vec<&[u8]> = byte_lines(prep.new.data.as_slice().unwrap_or_default());
            let mut input: InternedInput<Vec<u8>> = InternedInput::default();
            input.update_before(before.iter().map(|l| normalize(l, ws)));
            input.update_after(after.iter().map(|l| normalize(l, ws)));

            let diff = diff_with_slider_heuristics(algorithm, &input);
            let added = diff.count_additions();
            let deleted = diff.count_removals();
            let hunks = if want_patch && (added != 0 || deleted != 0) {
                let sink = PatchSink {
                    buf: Vec::new(),
                    before: &before,
                    after: &after,
                };
                Some(
                    UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx))
                        .consume()?,
                )
            } else {
                None
            };
            Ok(Analysis {
                new_id,
                added,
                deleted,
                binary: false,
                hunks,
            })
        }
    }
}

/// Diff a submodule (gitlink) pair as git's `show_submodule_summary`-free short
/// format does: one `Subproject commit <full-oid>` line per present side. The new
/// object id on the `index` line is the new commit id (or null when removed).
fn analyze_gitlink(
    old_commit: Option<ObjectId>,
    new_commit: Option<ObjectId>,
    null: ObjectId,
    ctx: u32,
    want_patch: bool,
    algo_override: Option<gix::diff::blob::Algorithm>,
) -> Result<Analysis> {
    let line = |id: ObjectId| -> Vec<u8> {
        let mut v = b"Subproject commit ".to_vec();
        v.extend_from_slice(id.to_hex().to_string().as_bytes());
        v.push(b'\n');
        v
    };
    let before: Vec<Vec<u8>> = old_commit.map(|id| vec![line(id)]).unwrap_or_default();
    let after: Vec<Vec<u8>> = new_commit.map(|id| vec![line(id)]).unwrap_or_default();
    let before_r: Vec<&[u8]> = before.iter().map(|l| l.as_slice()).collect();
    let after_r: Vec<&[u8]> = after.iter().map(|l| l.as_slice()).collect();

    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before_r.iter().map(|l| l.to_vec()));
    input.update_after(after_r.iter().map(|l| l.to_vec()));
    let algorithm = algo_override.unwrap_or(gix::diff::blob::Algorithm::Myers);
    let diff = diff_with_slider_heuristics(algorithm, &input);
    let added = diff.count_additions();
    let deleted = diff.count_removals();
    let hunks = if want_patch && (added != 0 || deleted != 0) {
        let sink = PatchSink {
            buf: Vec::new(),
            before: &before_r,
            after: &after_r,
        };
        Some(UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx)).consume()?)
    } else {
        None
    };
    Ok(Analysis {
        new_id: new_commit.unwrap_or(null),
        added,
        deleted,
        binary: false,
        hunks,
    })
}

/// Split `data` into lines the way `imara_diff::sources::byte_lines` does: the
/// terminator stays attached, and a final line without one is still a line.
fn byte_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let len = rest.find_byte(b'\n').map_or(rest.len(), |i| i + 1);
        let (line, tail) = rest.split_at(len);
        out.push(line);
        rest = tail;
    }
    out
}

/// The form of a line used for *comparison* only; the original bytes are always
/// what gets printed.
fn normalize(line: &[u8], ws: Whitespace) -> Vec<u8> {
    let is_space = |b: u8| matches!(b, b' ' | b'\t' | b'\x0b' | b'\x0c' | b'\r' | b'\n');
    match ws {
        Whitespace::Keep => line.to_vec(),
        Whitespace::IgnoreAll => line.iter().copied().filter(|b| !is_space(*b)).collect(),
        Whitespace::IgnoreAtEol => {
            let end = line.iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
            line[..end].to_vec()
        }
        Whitespace::IgnoreChange => {
            let end = line.iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
            let mut out = Vec::with_capacity(end);
            let mut in_space = false;
            for &b in &line[..end] {
                if is_space(b) {
                    in_space = true;
                    continue;
                }
                if in_space {
                    out.push(b' ');
                    in_space = false;
                }
                out.push(b);
            }
            out
        }
    }
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

fn mode_octal(k: Option<EntryKind>) -> String {
    match k {
        None => "000000".to_string(),
        Some(k) => mode_str(k).to_string(),
    }
}

fn mode_str(k: EntryKind) -> &'static str {
    std::str::from_utf8(k.as_octal_str()).unwrap_or("100644")
}

/// `--raw` and `--name-status` (`diff_flush_raw()`).
fn render_raw(out: &mut Vec<u8>, delta: &Delta, fmt: u32, r: &Render) {
    let status = status_char(delta);
    if fmt & F_NAME_STATUS == 0 {
        let null = r.hash_kind.null().to_hex_with_len(r.abbrev).to_string();
        let old_hash = delta
            .old
            .map(|(id, _)| id.to_hex_with_len(r.abbrev).to_string())
            .unwrap_or_else(|| null.clone());
        // Worktree content has no object id yet, which git reports as all-zero.
        let new_hash = match (&delta.new, delta.unmerged) {
            (NewSide::Blob(id, _), false) => id.to_hex_with_len(r.abbrev).to_string(),
            _ => null,
        };
        push_str(out, ":");
        push_str(out, &mode_octal(delta.old.map(|(_, k)| k)));
        push_str(out, " ");
        push_str(out, &mode_octal(delta.new_kind()));
        push_str(out, " ");
        push_str(out, &old_hash);
        push_str(out, " ");
        push_str(out, &new_hash);
        push_str(out, " ");
    }
    out.push(status);
    // `-z`: the field / record separators become NUL and paths are not C-quoted.
    out.push(if r.z { 0 } else { b'\t' });
    out.extend_from_slice(&name_field(&delta.path, r.z));
    out.push(if r.z { 0 } else { b'\n' });
}

/// A path as a `--raw`/`--name-*` field: raw bytes under `-z`, otherwise C-quoted.
fn name_field(path: &BString, z: bool) -> Vec<u8> {
    if z {
        path.as_slice().to_vec()
    } else {
        quoted_name(path)
    }
}

/// `--summary` (`show_summary()` / `diff_summary_line()`): creation, deletion and
/// mode-change lines, one per delta in queue order.
fn render_summary(out: &mut Vec<u8>, deltas: &[Delta]) {
    for d in deltas {
        if d.unmerged {
            continue;
        }
        match (d.old, d.new_kind()) {
            (None, Some(nk)) => {
                push_str(out, " create mode ");
                push_str(out, mode_str(nk));
                out.push(b' ');
                out.extend_from_slice(&quoted_name(&d.path));
                out.push(b'\n');
            }
            (Some((_, ok)), None) => {
                push_str(out, " delete mode ");
                push_str(out, mode_str(ok));
                out.push(b' ');
                out.extend_from_slice(&quoted_name(&d.path));
                out.push(b'\n');
            }
            (Some((_, ok)), Some(nk)) if ok != nk => {
                push_str(out, " mode change ");
                push_str(out, mode_str(ok));
                push_str(out, " => ");
                push_str(out, mode_str(nk));
                out.push(b' ');
                out.extend_from_slice(&quoted_name(&d.path));
                out.push(b'\n');
            }
            _ => {}
        }
    }
}

/// `--name-status` letter for a delta.
fn status_char(d: &Delta) -> u8 {
    if d.unmerged {
        return b'U';
    }
    match (&d.old, &d.new) {
        (None, _) => b'A',
        (_, NewSide::Absent) => b'D',
        _ => b'M',
    }
}

/// `--numstat` (`show_numstat()`).
fn render_numstat(out: &mut Vec<u8>, deltas: &[Delta], analyses: &[Analysis]) {
    for (d, an) in deltas.iter().zip(analyses) {
        if an.binary {
            push_str(out, "-\t-\t");
        } else {
            push_str(out, &format!("{}\t{}\t", an.added, an.deleted));
        }
        out.extend_from_slice(&quoted_name(&d.path));
        out.push(b'\n');
    }
}

/// `--shortstat` (`show_shortstats()`).
fn render_shortstat(out: &mut Vec<u8>, deltas: &[Delta], analyses: &[Analysis]) {
    let (files, adds, dels) = stat_totals(deltas, analyses);
    stat_summary(out, files, adds, dels);
}

fn stat_totals(deltas: &[Delta], analyses: &[Analysis]) -> (u32, u32, u32) {
    let mut files = deltas.len() as u32;
    let (mut adds, mut dels) = (0u32, 0u32);
    for (d, an) in deltas.iter().zip(analyses) {
        if d.unmerged {
            files -= 1;
        } else if !an.binary {
            adds += an.added;
            dels += an.deleted;
        }
    }
    (files, adds, dels)
}

/// `print_stat_summary_inserts_deletes()`.
fn stat_summary(out: &mut Vec<u8>, files: u32, insertions: u32, deletions: u32) {
    if files == 0 {
        push_str(out, " 0 files changed\n");
        return;
    }
    push_str(
        out,
        &format!(" {files} file{} changed", if files == 1 { "" } else { "s" }),
    );
    if insertions != 0 || deletions == 0 {
        push_str(
            out,
            &format!(
                ", {insertions} insertion{}(+)",
                if insertions == 1 { "" } else { "s" }
            ),
        );
    }
    if deletions != 0 || insertions == 0 {
        push_str(
            out,
            &format!(
                ", {deletions} deletion{}(-)",
                if deletions == 1 { "" } else { "s" }
            ),
        );
    }
    out.push(b'\n');
}

fn decimal_width(n: u32) -> i64 {
    let mut w = 1i64;
    let mut n = n / 10;
    while n > 0 {
        w += 1;
        n /= 10;
    }
    w
}

/// `scale_linear()` from `diff.c`.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// `get_compact_summary()`: the parenthesized annotation `--compact-summary`
/// appends to a diffstat name. Mirrors `diff.c`'s status/mode ladder, in order:
/// creation (`new`/`new +x`/`new +l`), deletion (`gone`), then the symlink and
/// executable-bit mode transitions. Returns `None` when no annotation applies
/// (a content-only modification) so the name is printed bare.
fn compact_comment(d: &Delta) -> Option<&'static str> {
    // git computes the annotation from `p->one`/`p->two`; an unmerged pair has no
    // usable filespec modes here, so it carries no comment.
    if d.unmerged {
        return None;
    }
    let old = d.old.map(|(_, k)| k);
    let new = d.new_kind();
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
    let (ok, nk) = (old.expect("old present"), new.expect("new present"));
    let old_link = ok == EntryKind::Link;
    let new_link = nk == EntryKind::Link;
    if old_link && !new_link {
        Some("mode -l")
    } else if !old_link && new_link {
        Some("mode +l")
    } else if ok == EntryKind::Blob && nk == EntryKind::BlobExecutable {
        Some("mode +x")
    } else if ok == EntryKind::BlobExecutable && nk == EntryKind::Blob {
        Some("mode -x")
    } else {
        None
    }
}

/// The diffstat display name: the C-quoted path, plus the `--compact-summary`
/// annotation ` (<comment>)` when one applies (`fill_print_name()`).
fn stat_display_name(d: &Delta, compact: bool) -> Vec<u8> {
    let mut name = quoted_name(&d.path);
    if compact {
        if let Some(c) = compact_comment(d) {
            name.push(b' ');
            name.push(b'(');
            name.extend_from_slice(c.as_bytes());
            name.push(b')');
        }
    }
    name
}

/// `--stat` (`show_stats()`), with git's default 80-column budget.
fn render_stat(out: &mut Vec<u8>, deltas: &[Delta], analyses: &[Analysis], compact: bool) {
    let names: Vec<Vec<u8>> = deltas.iter().map(|d| stat_display_name(d, compact)).collect();

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    for (i, (d, an)) in deltas.iter().zip(analyses).enumerate() {
        let change = (an.added + an.deleted) as i64;
        max_len = max_len.max(names[i].len() as i64);
        if d.unmerged {
            bin_width = bin_width.max(8); // "Unmerged"
            continue;
        }
        if an.binary {
            let w = 14 + decimal_width(an.added) + decimal_width(an.deleted);
            bin_width = bin_width.max(w);
            number_width = 3;
            continue;
        }
        max_change = max_change.max(change);
    }

    // `width` is `options->stat_width ? options->stat_width : 80` for a plain `--stat`.
    let mut width: i64 = 80;
    number_width = number_width.max(decimal_width(max_change as u32));
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = if max_change + 4 > bin_width {
        max_change
    } else {
        bin_width - 4
    };
    let mut name_width = max_len;
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = (width * 3 / 8 - number_width - 6).max(6);
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    for (i, (d, an)) in deltas.iter().zip(analyses).enumerate() {
        let (added, deleted) = (an.added as i64, an.deleted as i64);
        // "scale" the filename: overlong names are truncated to "...<tail>".
        let full = &names[i];
        let (prefix, name): (&str, &[u8]) = if name_width < full.len() as i64 {
            let len = name_width - 3;
            let start = full.len() - len.max(0) as usize;
            let tail = &full[start..];
            let tail = match tail.iter().position(|b| *b == b'/') {
                Some(p) => &tail[p..],
                None => tail,
            };
            ("...", tail)
        } else {
            ("", full.as_slice())
        };
        let padding = (name_width - prefix.len() as i64 - name.len() as i64).max(0) as usize;

        push_str(out, " ");
        push_str(out, prefix);
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        push_str(out, " | ");

        if an.binary {
            push_str(out, &format!("{:>width$}", "Bin", width = number_width as usize));
            if added == 0 && deleted == 0 {
                out.push(b'\n');
                continue;
            }
            push_str(out, &format!(" {deleted} -> {added} bytes\n"));
            continue;
        }
        if d.unmerged {
            push_str(out, &format!("{:>width$}", "Unmerged", width = number_width as usize));
            out.push(b'\n');
            continue;
        }

        let (mut add, mut del) = (added, deleted);
        if graph_width <= max_change {
            let mut total = scale_linear(add + del, graph_width, max_change);
            if total < 2 && add > 0 && del > 0 {
                total = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = total - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = total - del;
            }
        }
        push_str(
            out,
            &format!("{:>width$}", added + deleted, width = number_width as usize),
        );
        if added + deleted != 0 {
            push_str(out, " ");
        }
        out.extend_from_slice(&b"+".repeat(add.max(0) as usize));
        out.extend_from_slice(&b"-".repeat(del.max(0) as usize));
        out.push(b'\n');
    }

    let (files, adds, dels) = stat_totals(deltas, analyses);
    stat_summary(out, files, adds, dels);
}

/// Render one delta as a `git diff` file section into `out`.
fn render_patch(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    delta: &Delta,
    an: &Analysis,
    ctx: u32,
    r: &Render,
) -> Result<()> {
    if delta.unmerged {
        return render_combined(out, repo, delta, ctx);
    }

    // The `index` line honors `--abbrev` / `--full-index`.
    let hlen = if r.full_index { r.hash_kind.len_in_hex() } else { r.abbrev };
    let null_hash = r.hash_kind.null().to_hex_with_len(hlen).to_string();
    let old_hash = delta
        .old
        .map(|(id, _)| id.to_hex_with_len(hlen).to_string())
        .unwrap_or_else(|| null_hash.clone());
    let new_hash = if matches!(delta.new, NewSide::Absent) {
        null_hash.clone()
    } else {
        an.new_id.to_hex_with_len(hlen).to_string()
    };
    let content_differs = old_hash != new_hash;
    let new_kind = delta.new_kind();

    push_str(out, "diff --git ");
    out.extend_from_slice(&quote_two(&r.src_prefix, &delta.path, &r.dst_prefix, &delta.path));
    out.push(b'\n');

    // File-creation / deletion / mode-change lines.
    match (delta.old, new_kind) {
        (None, Some(nk)) => {
            push_str(out, "new file mode ");
            push_str(out, mode_str(nk));
            out.push(b'\n');
        }
        (Some((_, ok)), None) => {
            push_str(out, "deleted file mode ");
            push_str(out, mode_str(ok));
            out.push(b'\n');
        }
        (Some((_, ok)), Some(nk)) if ok != nk => {
            push_str(out, "old mode ");
            push_str(out, mode_str(ok));
            push_str(out, "\nnew mode ");
            push_str(out, mode_str(nk));
            out.push(b'\n');
        }
        _ => {}
    }

    // The `index <old>..<new>[ <mode>]` line only appears when content differs.
    if content_differs {
        push_str(out, "index ");
        push_str(out, &old_hash);
        push_str(out, "..");
        push_str(out, &new_hash);
        // Trailing mode only for an unchanged-mode modification (not add/delete/mode-change).
        if let (Some((_, ok)), Some(nk)) = (delta.old, new_kind) {
            if ok == nk {
                out.push(b' ');
                push_str(out, mode_str(nk));
            }
        }
        out.push(b'\n');
    }

    let old_label = if delta.old.is_some() {
        quote_one(&r.src_prefix, &delta.path)
    } else {
        b"/dev/null".to_vec()
    };
    let new_label = if matches!(delta.new, NewSide::Absent) {
        b"/dev/null".to_vec()
    } else {
        quote_one(&r.dst_prefix, &delta.path)
    };

    if an.binary {
        push_str(out, "Binary files ");
        out.extend_from_slice(&old_label);
        push_str(out, " and ");
        out.extend_from_slice(&new_label);
        push_str(out, " differ\n");
    } else if let Some(hunks) = &an.hunks {
        emit_file_line(out, b"--- ", &old_label);
        emit_file_line(out, b"+++ ", &new_label);
        out.extend_from_slice(hunks);
    }
    Ok(())
}

/// `DIFF_SYMBOL_FILEPAIR_{MINUS,PLUS}`: a name containing a space gets a trailing
/// tab so the header stays unambiguously parseable.
fn emit_file_line(out: &mut Vec<u8>, lead: &[u8], label: &[u8]) {
    out.extend_from_slice(lead);
    out.extend_from_slice(label);
    if label.contains(&b' ') {
        out.push(b'\t');
    }
    out.push(b'\n');
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

/// `--line-prefix`: git's `emit_line_0()` writes `diff_line_prefix(o)` before every
/// emitted line, so prepending `prefix` at the buffer start and after each interior
/// newline reproduces it byte-for-byte for the newline-terminated formats (patch,
/// stat, summary, raw/name without `-z`). An empty buffer stays empty (git emits
/// nothing at all on a clean tree), and a trailing newline is not followed by a
/// dangling prefixed empty line.
fn apply_line_prefix(out: Vec<u8>, prefix: &[u8]) -> Vec<u8> {
    if prefix.is_empty() || out.is_empty() {
        return out;
    }
    let mut res = Vec::with_capacity(out.len() + prefix.len() * 2);
    res.extend_from_slice(prefix);
    for (i, &b) in out.iter().enumerate() {
        res.push(b);
        if b == b'\n' && i + 1 < out.len() {
            res.extend_from_slice(prefix);
        }
    }
    res
}

// ---------------------------------------------------------------------------
// path quoting (quote.c)
// ---------------------------------------------------------------------------

/// The escape character for `b`, or `None` if it can be emitted verbatim.
/// `Some(0)` means "octal-escape this byte".
fn cq_escape(b: u8) -> Option<u8> {
    match b {
        0x07 => Some(b'a'),
        0x08 => Some(b'b'),
        0x09 => Some(b't'),
        0x0a => Some(b'n'),
        0x0b => Some(b'v'),
        0x0c => Some(b'f'),
        0x0d => Some(b'r'),
        b'"' => Some(b'"'),
        b'\\' => Some(b'\\'),
        // Controls, DEL and (with the default `core.quotePath`) every high byte.
        0x00..=0x1f | 0x7f..=0xff => Some(0),
        _ => None,
    }
}

fn needs_quote(s: &[u8]) -> bool {
    s.iter().any(|b| cq_escape(*b).is_some())
}

/// The escaped body of `s`, without the surrounding double quotes.
fn cq_body(s: &[u8], out: &mut Vec<u8>) {
    for &b in s {
        match cq_escape(b) {
            None => out.push(b),
            Some(0) => {
                out.push(b'\\');
                out.push(((b >> 6) & 0o3) + b'0');
                out.push(((b >> 3) & 0o7) + b'0');
                out.push((b & 0o7) + b'0');
            }
            Some(c) => {
                out.push(b'\\');
                out.push(c);
            }
        }
    }
}

/// `write_name_quoted()`: the path, double-quoted and escaped only if needed.
fn quoted_name(path: &BString) -> Vec<u8> {
    let s = path.as_slice();
    if !needs_quote(s) {
        return s.to_vec();
    }
    let mut out = vec![b'"'];
    cq_body(s, &mut out);
    out.push(b'"');
    out
}

/// `quote_two_c_style()` for a single prefixed name (the `---`/`+++` lines).
fn quote_one(prefix: &[u8], path: &BString) -> Vec<u8> {
    let s = path.as_slice();
    if !needs_quote(prefix) && !needs_quote(s) {
        let mut out = prefix.to_vec();
        out.extend_from_slice(s);
        return out;
    }
    let mut out = vec![b'"'];
    cq_body(prefix, &mut out);
    cq_body(s, &mut out);
    out.push(b'"');
    out
}

/// The `diff --git <a> <b>` name pair.
fn quote_two(pa: &[u8], a: &BString, pb: &[u8], b: &BString) -> Vec<u8> {
    let mut out = quote_one(pa, a);
    out.push(b' ');
    out.extend_from_slice(&quote_one(pb, b));
    out
}

// ---------------------------------------------------------------------------
// combined ("--cc") diff for unmerged worktree paths
// ---------------------------------------------------------------------------

/// One line that a parent had but the merge result does not.
struct LostLine {
    line: Vec<u8>,
    /// Bit `n` set means parent `n` lost this line.
    parent_map: u32,
}

/// One line of the merge result, plus everything the parents lost in front of it.
/// Mirrors `struct sline` in `combine-diff.c`.
#[derive(Default)]
struct SLine {
    /// The line content without its terminator. Empty for the two trailer slots.
    bol: Vec<u8>,
    lost: Vec<LostLine>,
    /// Lines lost by the parent currently being processed, before coalescing.
    plost: Vec<Vec<u8>>,
    /// Bits `0..num_parent` mark parents that lack this line; bit `num_parent`
    /// is `mark` and bit `num_parent + 1` is `no_pre_delete`.
    flag: u32,
    /// Per-parent line number this sline starts at, filled by `combine_diff()`.
    p_lno: [u32; NUM_PARENT],
}

const NUM_PARENT: usize = 2;

/// Build the two-parent combined-diff `sline` table: the merge result plus, for
/// each parent, the lines that parent lost — coalesced and numbered exactly as
/// `combine_diff()` / `make_hunks()` do. Returns the table and the result line
/// count. Shared by the unmerged-worktree (`--cc`) and multi-revision paths.
fn build_combined_sline(result: &[u8], parents: &[Vec<u8>], ctx: u32) -> (Vec<SLine>, usize) {
    // Result lines, terminators stripped; a trailing incomplete line still counts.
    let mut cnt = result.iter().filter(|b| **b == b'\n').count();
    if !result.is_empty() && *result.last().expect("non-empty") != b'\n' {
        cnt += 1;
    }
    let mut sline: Vec<SLine> = (0..cnt + 2).map(|_| SLine::default()).collect();
    for (i, line) in byte_lines(result).into_iter().enumerate() {
        let end = line.len() - usize::from(line.last() == Some(&b'\n'));
        sline[i].bol = line[..end].to_vec();
    }

    let result_lines = byte_lines(result);
    for (n, parent) in parents.iter().enumerate() {
        let nmask = 1u32 << n;
        let before = byte_lines(parent);
        let mut input: InternedInput<Vec<u8>> = InternedInput::default();
        input.update_before(before.iter().map(|l| l.to_vec()));
        input.update_after(result_lines.iter().map(|l| l.to_vec()));
        // `xdi_diff_outf()` runs with git's default algorithm.
        let diff = diff_with_slider_heuristics(gix::diff::blob::Algorithm::Myers, &input);

        for hunk in diff.hunks() {
            // Removals hang off the result line that follows them, which for both
            // an empty and a non-empty "after" range is `after.start`.
            let bucket = hunk.after.start as usize;
            for i in hunk.before.clone() {
                let line = before[i as usize];
                let end = line.len() - usize::from(line.last() == Some(&b'\n'));
                sline[bucket].plost.push(line[..end].to_vec());
            }
            for i in hunk.after.clone() {
                sline[i as usize].flag |= nmask;
            }
        }

        // Assign per-parent line numbers, coalescing this parent's lost lines in.
        let mut p_lno: u32 = 1;
        for lno in 0..=cnt {
            sline[lno].p_lno[n] = p_lno;
            let fresh = std::mem::take(&mut sline[lno].plost);
            coalesce_lines(&mut sline[lno].lost, fresh, n as u32);
            for ll in &sline[lno].lost {
                if ll.parent_map & nmask != 0 {
                    p_lno += 1;
                }
            }
            if lno < cnt && sline[lno].flag & nmask == 0 {
                p_lno += 1;
            }
        }
        sline[cnt + 1].p_lno[n] = p_lno;
    }

    make_hunks(&mut sline, cnt, ctx);
    (sline, cnt)
}

/// `true` if any result line survived dense filtering, i.e. the combined diff has
/// at least one hunk to emit for this path.
fn sline_has_marks(sline: &[SLine], cnt: usize) -> bool {
    sline.iter().take(cnt + 1).any(|s| s.flag & MARK != 0)
}

/// The file path a tree-to-tree change touches, or `None` for a directory-level
/// (tree) change — gitoxide reports those too, and the combined diff only cares
/// about blob leaves.
fn change_blob_location(change: &gix::object::tree::diff::ChangeDetached) -> Option<BString> {
    use gix::object::tree::diff::ChangeDetached;
    match change {
        ChangeDetached::Addition { location, entry_mode, .. }
        | ChangeDetached::Deletion { location, entry_mode, .. } => {
            (!entry_mode.is_tree()).then(|| location.clone())
        }
        ChangeDetached::Modification {
            location,
            entry_mode,
            previous_entry_mode,
            ..
        } => (!entry_mode.is_tree() || !previous_entry_mode.is_tree()).then(|| location.clone()),
        // Rewrites are disabled on the options we pass, so this never fires.
        ChangeDetached::Rewrite { .. } => None,
    }
}

/// The blob at `path` in `tree`: its id, whether it exists, and its bytes (the id
/// is the null oid and the bytes are empty when the path is absent from the tree).
fn tree_blob(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    path: &BString,
) -> Result<(ObjectId, bool, Vec<u8>)> {
    match tree_entry(tree, path)? {
        Some((_, EntryKind::Commit)) => {
            bail!("submodule/gitlink change at {path:?} is not supported")
        }
        // A directory at this path contributes no blob content of its own.
        Some((_, EntryKind::Tree)) => Ok((repo.object_hash().null(), false, Vec::new())),
        Some((id, _)) => Ok((id, true, blob_bytes(repo, id)?)),
        None => Ok((repo.object_hash().null(), false, Vec::new())),
    }
}

/// `git diff <rev0> <rev1> [<rev2> ...]` with three or more revisions: a dense
/// combined ("--cc") diff of the first revision (the result) against every other
/// revision (its parents), mirroring `builtin_diff_combined()`. A path is shown
/// only when the result differs from every parent, exactly as dense combined-diff
/// filtering requires — so equal revisions produce no output at all.
fn combined_multi(
    repo: &gix::Repository,
    revs: &[String],
    paths: &[String],
    fmt: u32,
    ctx: u32,
    line_prefix: &[u8],
) -> Result<ExitCode> {
    // `-s` / `--no-patch` suppresses all output; the combined patch is the only
    // combined format zvcs renders, so every other format falls back to it.
    if fmt & F_NO_OUTPUT != 0 {
        return Ok(ExitCode::SUCCESS);
    }

    let result_tree = repo.rev_parse_single(revs[0].as_str())?.object()?.peel_to_tree()?;
    let mut parent_trees: Vec<gix::Tree<'_>> = Vec::with_capacity(revs.len() - 1);
    for r in &revs[1..] {
        parent_trees.push(repo.rev_parse_single(r.as_str())?.object()?.peel_to_tree()?);
    }

    let out = combined_trees_patch(repo, &result_tree, &parent_trees, paths, ctx)?;
    let out = apply_line_prefix(out, line_prefix);

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The dense combined diff (`diff --cc`) of `result_tree` against every parent
/// tree, returned as bytes. Shared by `git diff -c`/`--cc` and `git show` on a
/// merge commit. A path appears only where the result differs from *all*
/// parents (git's dense-combined elision).
pub(crate) fn combined_trees_patch(
    repo: &gix::Repository,
    result_tree: &gix::Tree<'_>,
    parent_trees: &[gix::Tree<'_>],
    paths: &[String],
    ctx: u32,
) -> Result<Vec<u8>> {
    // Candidate paths: everything that differs between the result and any parent.
    let mut cand: BTreeSet<BString> = BTreeSet::new();
    for pt in parent_trees {
        let changes = repo.diff_tree_to_tree(
            Some(pt),
            Some(result_tree),
            Some(gix::diff::Options::default()),
        )?;
        for change in changes {
            if let Some(loc) = change_blob_location(&change) {
                cand.insert(loc);
            }
        }
    }
    if !paths.is_empty() {
        cand.retain(|p| paths.iter().any(|x| path_matches(p, x)));
    }

    let null = repo.object_hash().null();
    let mut out: Vec<u8> = Vec::new();
    for path in &cand {
        let (res_id, res_present, res_bytes) = tree_blob(repo, result_tree, path)?;
        let mut parent_ids: Vec<ObjectId> = Vec::with_capacity(parent_trees.len());
        let mut parent_bytes: Vec<Vec<u8>> = Vec::with_capacity(parent_trees.len());
        for pt in parent_trees {
            let (pid, _present, pbytes) = tree_blob(repo, pt, path)?;
            parent_ids.push(pid);
            parent_bytes.push(pbytes);
        }

        // Dense combined diff shows a path only when the result differs from all
        // parents; matching any parent makes the change one-sided and elided.
        if parent_bytes.iter().any(|b| *b == res_bytes) {
            continue;
        }
        if parent_bytes.len() != NUM_PARENT {
            bail!("combined diff of more than two parents is not supported");
        }

        let (sline, cnt) = build_combined_sline(&res_bytes, &parent_bytes, ctx);
        if !sline_has_marks(&sline, cnt) {
            continue;
        }

        push_str(&mut out, "diff --cc ");
        out.extend_from_slice(&quoted_name(path));
        out.push(b'\n');
        push_str(&mut out, "index ");
        let abbrev = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex());
        for (i, pid) in parent_ids.iter().enumerate() {
            if i != 0 {
                out.push(b',');
            }
            push_str(&mut out, &pid.to_hex_with_len(abbrev).to_string());
        }
        push_str(&mut out, "..");
        let res_short = if res_present { res_id } else { null };
        push_str(&mut out, &res_short.to_hex_with_len(abbrev).to_string());
        out.push(b'\n');
        emit_file_line(&mut out, b"--- ", &quote_one(b"a/", path));
        emit_file_line(&mut out, b"+++ ", &quote_one(b"b/", path));
        dump_sline(&mut out, &sline, cnt, ctx);
    }
    Ok(out)
}

/// A combined diff of the two conflict stages against the working-tree file, as
/// `show_combined_diff()` renders it for `git diff` on a conflicted path.
///
/// Port of `show_patch_diff()` / `combine_diff()` / `make_hunks()` / `dump_sline()`
/// from `combine-diff.c`, specialized to the two-parent (stage 2 / stage 3) case.
fn render_combined(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    delta: &Delta,
    ctx: u32,
) -> Result<()> {
    let Some((ours, theirs)) = delta.stages else {
        // No stage 2/3 pair to combine (e.g. `--cached`): git prints the notice.
        push_str(out, "* Unmerged path ");
        out.extend_from_slice(&delta.path);
        out.push(b'\n');
        return Ok(());
    };
    let workdir = match repo.workdir() {
        Some(w) => w,
        None => {
            push_str(out, "* Unmerged path ");
            out.extend_from_slice(&delta.path);
            out.push(b'\n');
            return Ok(());
        }
    };
    let result = std::fs::read(workdir.join(gix::path::from_bstr(delta.path.as_bstr())))?;
    let parents = vec![blob_bytes(repo, ours)?, blob_bytes(repo, theirs)?];
    let (sline, cnt) = build_combined_sline(&result, &parents, ctx);

    // ---- header (`show_combined_header()`) --------------------------------
    push_str(out, "diff --cc ");
    out.extend_from_slice(&quoted_name(&delta.path));
    out.push(b'\n');
    push_str(out, "index ");
    let abbrev = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex());
    push_str(out, &ours.to_hex_with_len(abbrev).to_string());
    push_str(out, ",");
    push_str(out, &theirs.to_hex_with_len(abbrev).to_string());
    push_str(out, "..");
    // The result lives only in the worktree, so it has no object id.
    push_str(out, &repo.object_hash().null().to_hex_with_len(abbrev).to_string());
    out.push(b'\n');
    emit_file_line(out, b"--- ", &quote_one(b"a/", &delta.path));
    emit_file_line(out, b"+++ ", &quote_one(b"b/", &delta.path));

    dump_sline(out, &sline, cnt, ctx);
    Ok(())
}

fn blob_bytes(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    Ok(repo.find_object(id)?.detach().data)
}

/// `coalesce_lines()`: LCS-merge `fresh` (the lines parent `parent` lost) into the
/// already-merged `base`, so a line lost by several parents is shown once.
fn coalesce_lines(base: &mut Vec<LostLine>, fresh: Vec<Vec<u8>>, parent: u32) {
    if fresh.is_empty() {
        return;
    }
    if base.is_empty() {
        *base = fresh
            .into_iter()
            .map(|line| LostLine {
                line,
                parent_map: 1 << parent,
            })
            .collect();
        return;
    }
    let (n, m) = (base.len(), fresh.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    // 0 = BASE, 1 = NEW, 2 = MATCH — the same encoding `combine-diff.c` uses.
    let mut dir = vec![vec![0u8; m + 1]; n + 1];
    for d in dir.iter_mut() {
        d[0] = 0;
    }
    for j in 1..=m {
        dir[0][j] = 1;
    }
    for i in 1..=n {
        for j in 1..=m {
            if base[i - 1].line == fresh[j - 1] {
                lcs[i][j] = lcs[i - 1][j - 1] + 1;
                dir[i][j] = 2;
            } else if lcs[i][j - 1] >= lcs[i - 1][j] {
                lcs[i][j] = lcs[i][j - 1];
                dir[i][j] = 1;
            } else {
                lcs[i][j] = lcs[i - 1][j];
                dir[i][j] = 0;
            }
        }
    }
    let mut merged: Vec<LostLine> = Vec::with_capacity(n + m);
    let (mut i, mut j) = (n, m);
    while i != 0 || j != 0 {
        match dir[i][j] {
            2 => {
                let mut ll = std::mem::replace(
                    &mut base[i - 1],
                    LostLine {
                        line: Vec::new(),
                        parent_map: 0,
                    },
                );
                ll.parent_map |= 1 << parent;
                merged.push(ll);
                i -= 1;
                j -= 1;
            }
            1 => {
                merged.push(LostLine {
                    line: fresh[j - 1].clone(),
                    parent_map: 1 << parent,
                });
                j -= 1;
            }
            _ => {
                merged.push(std::mem::replace(
                    &mut base[i - 1],
                    LostLine {
                        line: Vec::new(),
                        parent_map: 0,
                    },
                ));
                i -= 1;
            }
        }
    }
    merged.reverse();
    *base = merged;
}

const ALL_MASK: u32 = (1 << NUM_PARENT) - 1;
const MARK: u32 = 1 << NUM_PARENT;
const NO_PRE_DELETE: u32 = 2 << NUM_PARENT;

fn interesting(sl: &SLine) -> bool {
    sl.flag & ALL_MASK != 0 || !sl.lost.is_empty()
}

/// `adjust_hunk_tail()`.
fn adjust_hunk_tail(sline: &[SLine], hunk_begin: usize, i: usize) -> usize {
    if hunk_begin + 1 <= i && sline[i - 1].flag & ALL_MASK == 0 {
        i - 1
    } else {
        i
    }
}

/// `find_next()`.
fn find_next(sline: &[SLine], i: usize, cnt: usize, look_for_uninteresting: bool) -> usize {
    let mut i = i;
    while i <= cnt {
        let marked = sline[i].flag & MARK != 0;
        if look_for_uninteresting != marked {
            return i;
        }
        i += 1;
    }
    i
}

/// `give_context()`.
fn give_context(sline: &mut [SLine], cnt: usize, context: usize) {
    let mut i = find_next(sline, 0, cnt, false);
    if cnt < i {
        return;
    }
    while i <= cnt {
        let mut j = i.saturating_sub(context);
        while j < i {
            if sline[j].flag & MARK == 0 {
                sline[j].flag |= NO_PRE_DELETE;
            }
            sline[j].flag |= MARK;
            j += 1;
        }
        loop {
            let mut j = find_next(sline, i, cnt, true);
            if cnt < j {
                return;
            }
            let k = find_next(sline, j, cnt, false);
            j = adjust_hunk_tail(sline, i, j);
            if k < j + context {
                while j < k {
                    sline[j].flag |= MARK;
                    j += 1;
                }
                i = k;
                continue;
            }
            i = k;
            let mut j2 = j;
            let end = (j + context).min(cnt + 1);
            while j2 < end {
                sline[j2].flag |= MARK;
                j2 += 1;
            }
            break;
        }
    }
}

/// `make_hunks()` with `dense` set, which is what `--cc` uses.
fn make_hunks(sline: &mut [SLine], cnt: usize, context: u32) {
    let context = context as usize;
    for sl in sline.iter_mut().take(cnt + 1) {
        if interesting(sl) {
            sl.flag |= MARK;
        } else {
            sl.flag &= !MARK;
        }
    }

    // Drop hunks whose every line differs from the same single set of parents:
    // those are changes only one side made, which `--cc` elides.
    let mut i = 0usize;
    while i <= cnt {
        while i <= cnt && sline[i].flag & MARK == 0 {
            i += 1;
        }
        if cnt < i {
            break;
        }
        let hunk_begin = i;
        let mut j = i + 1;
        while j <= cnt {
            if sline[j].flag & MARK == 0 {
                // Look past the gap: another marked line within `context` continues it.
                let mut la = adjust_hunk_tail(sline, hunk_begin, j);
                la = (la + context).min(cnt + 1);
                let mut contin = false;
                while la > 0 && j <= la - 1 {
                    la -= 1;
                    if sline[la].flag & MARK != 0 {
                        contin = true;
                        break;
                    }
                }
                if !contin {
                    break;
                }
                j = la;
            }
            j += 1;
        }
        let hunk_end = j;

        let mut same_diff: u32 = 0;
        let mut has_interesting = false;
        for sl in sline.iter().take(hunk_end).skip(i) {
            if has_interesting {
                break;
            }
            let this_diff = sl.flag & ALL_MASK;
            if this_diff != 0 {
                if same_diff == 0 {
                    same_diff = this_diff;
                } else if same_diff != this_diff {
                    has_interesting = true;
                    break;
                }
            }
            for ll in &sl.lost {
                if has_interesting {
                    break;
                }
                if same_diff == 0 {
                    same_diff = ll.parent_map;
                } else if same_diff != ll.parent_map {
                    has_interesting = true;
                }
            }
        }

        if !has_interesting && same_diff != ALL_MASK {
            for sl in sline.iter_mut().take(hunk_end).skip(hunk_begin) {
                sl.flag &= !MARK;
            }
        }
        i = hunk_end;
    }

    give_context(sline, cnt, context);
}

/// `dump_sline()`.
fn dump_sline(out: &mut Vec<u8>, sline: &[SLine], cnt: usize, context: u32) {
    let mut lno = 0usize;
    loop {
        while lno <= cnt && sline[lno].flag & MARK == 0 {
            lno += 1;
        }
        if cnt < lno {
            break;
        }
        let mut hunk_end = lno + 1;
        while hunk_end <= cnt && sline[hunk_end].flag & MARK != 0 {
            hunk_end += 1;
        }
        let mut rlines = hunk_end - lno;
        if cnt < hunk_end {
            rlines -= 1; // pointing at the last delete hunk
        }
        let mut null_context = 0usize;
        if context == 0 {
            for sl in sline.iter().take(hunk_end).skip(lno) {
                if sl.flag & (MARK - 1) == 0 {
                    null_context += 1;
                }
            }
            rlines = rlines.saturating_sub(null_context);
        }

        out.extend_from_slice(&b"@".repeat(NUM_PARENT + 1));
        for n in 0..NUM_PARENT {
            let l0 = sline[lno].p_lno[n];
            let l1 = sline[hunk_end].p_lno[n];
            push_str(
                out,
                &format!(" -{l0},{}", l1 as i64 - l0 as i64 - null_context as i64),
            );
        }
        push_str(out, &format!(" +{},{rlines} ", lno + 1));
        out.extend_from_slice(&b"@".repeat(NUM_PARENT + 1));
        out.push(b'\n');

        while lno < hunk_end {
            let sl = &sline[lno];
            lno += 1;
            if sl.flag & NO_PRE_DELETE == 0 {
                for ll in &sl.lost {
                    for n in 0..NUM_PARENT {
                        out.push(if ll.parent_map & (1 << n) != 0 { b'-' } else { b' ' });
                    }
                    out.extend_from_slice(&ll.line);
                    out.push(b'\n');
                }
            }
            if cnt < lno {
                break;
            }
            if sl.flag & (MARK - 1) == 0 && context == 0 {
                // Only there to hang lost lines in front of; not shown at -U0.
                continue;
            }
            for n in 0..NUM_PARENT {
                out.push(if sl.flag & (1 << n) != 0 { b'+' } else { b' ' });
            }
            out.extend_from_slice(&sl.bol);
            out.push(b'\n');
        }
    }
}

// ---------------------------------------------------------------------------
// unified-diff hunk sink
// ---------------------------------------------------------------------------

/// Format one side of a hunk header (`@@ -<here> +<here> @@`), omitting the length when
/// it is 1 and using the pre-hunk line number when it is 0, exactly like `git diff`.
fn fmt_range(start: u32, len: u32) -> String {
    match len {
        1 => format!("{start}"),
        0 => format!("{},0", start.saturating_sub(1)),
        _ => format!("{start},{len}"),
    }
}

/// A [`ConsumeHunk`] sink that renders unified-diff hunks into a byte buffer.
///
/// The tokens the differ compares may be whitespace-normalized (`-w` and friends),
/// so line *content* is taken from the original line tables instead, tracked by the
/// cursors the hunk header establishes.
struct PatchSink<'a> {
    buf: Vec<u8>,
    before: &'a [&'a [u8]],
    after: &'a [&'a [u8]],
}

impl ConsumeHunk for PatchSink<'_> {
    type Out = Vec<u8>;

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        self.buf.extend_from_slice(b"@@ -");
        self.buf.extend_from_slice(fmt_range(header.before_hunk_start, header.before_hunk_len).as_bytes());
        self.buf.extend_from_slice(b" +");
        self.buf.extend_from_slice(fmt_range(header.after_hunk_start, header.after_hunk_len).as_bytes());
        self.buf.extend_from_slice(b" @@\n");

        let mut bi = header.before_hunk_start.saturating_sub(1) as usize;
        let mut ai = header.after_hunk_start.saturating_sub(1) as usize;
        for (kind, fallback) in lines {
            let (marker, content): (u8, &[u8]) = match kind {
                DiffLineKind::Context => {
                    let c = self.before.get(bi).copied().unwrap_or(*fallback);
                    bi += 1;
                    ai += 1;
                    (b' ', c)
                }
                DiffLineKind::Remove => {
                    let c = self.before.get(bi).copied().unwrap_or(*fallback);
                    bi += 1;
                    (b'-', c)
                }
                DiffLineKind::Add => {
                    let c = self.after.get(ai).copied().unwrap_or(*fallback);
                    ai += 1;
                    (b'+', c)
                }
            };
            self.buf.push(marker);
            self.buf.extend_from_slice(content);
            // Tokens keep their line terminator; a token without one is the last line
            // of a file that lacks a trailing newline.
            if content.last() != Some(&b'\n') {
                self.buf.push(b'\n');
                self.buf.extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}
