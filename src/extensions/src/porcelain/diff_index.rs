//! `git diff-index` — compare a tree object against the working tree or the index.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). The `--cached` form is a
//! plain tree↔index diff (`Repository::tree_index_status`). The default form overlays
//! gitoxide's index↔worktree status pass on top of that same tree↔index diff, which
//! reproduces git's `oneway_diff`: an index entry whose worktree file is stat-dirty
//! contributes the null object id and the mode derived from `lstat`, a missing
//! worktree file turns the pair into a deletion, and everything else is taken
//! straight from the index.
//!
//! Supported invocations (stdout is byte-identical to stock `git diff-index`):
//!
//!   * `git diff-index <tree-ish>`      — the default raw format:
//!     `:<srcmode> <dstmode> <srcsha> <dstsha> <status>\t<path>`.
//!   * `--cached`                       — compare `<tree-ish>` against the index only.
//!   * `-m`                             — treat files missing from the worktree as up
//!     to date instead of reporting them deleted.
//!   * `--raw`, `--name-only`, `--name-status` — output selection.
//!   * `-z`                             — NUL field/record terminators, paths unquoted.
//!   * `--abbrev[=<n>]`, `--no-abbrev`  — abbreviated / full object ids.
//!   * `--exit-code`, `--quiet`         — exit 1 when differences exist (`--quiet` is silent).
//!   * `-s` / `--no-patch`              — suppress output, exit 0 unless `--exit-code`.
//!   * `[--] <path>...`                 — pathspec limiting, resolved relative to the cwd
//!     while output paths stay repository-root relative, as git does.
//!
//! Status letters produced: `A`, `D`, `T` (the `S_IFMT` bits of the two modes differ,
//! e.g. file ↔ symlink) and `M`.
//!
//! Options that only steer patch, stat or colour rendering (`--color[=<when>]`, `-D`,
//! `--ws-error-highlight=`, `--src-prefix=`/`--dst-prefix=`/`--no-prefix`,
//! `--diff-algorithm=`, `--anchored=`, `--color-moved[=]`, `--word-diff[=]`,
//! `--submodule[=]`, `-a`/`--text`, `-W`, …) are accepted and ignored: stock git's raw,
//! `--name-only` and `--name-status` bytes are identical with and without them. The full
//! list is `render_only_option`. `-U<n>`, `--unified=<n>` and `--binary` are *not* in it
//! — despite looking like rendering knobs they switch the output format to a patch.
//!
//! ### Honest limitations (bailed on with a precise message, never faked)
//!
//! * Patch and stat output (`-p`/`-u`/`--patch`, `-U<n>`/`--unified`, `--binary`,
//!   `--stat`, `--numstat`, `--shortstat`, `--dirstat`, `--summary`, `--compact-summary`,
//!   `--patch-with-raw`) is not produced here.
//! * Rename/copy/rewrite detection (`-M`, `-C`, `-B`) is off, which is git's default for
//!   `diff-index` as well, so `--no-renames` is accepted as a no-op.
//! * `--merge-base`, `--diff-filter`, the pickaxe (`-S`/`-G`), `-R`, `--relative`,
//!   `--line-prefix=` and the combined-merge selectors (`-c`/`--cc`) are unimplemented.
//! * The whitespace-insensitive comparisons (`-w`, `-b`, `--ignore-space-change`,
//!   `--ignore-all-space`, `--ignore-space-at-eol`, `--ignore-cr-at-eol`,
//!   `--ignore-blank-lines`, `-I<regex>`) are unimplemented. They are not cosmetic for
//!   the raw format: git sets `diff_from_contents` for them, which both drops pairs whose
//!   content matches once whitespace is folded and replaces the null worktree object id
//!   with the hash git had to compute to decide that.
//! * An unimplemented option is held until after the tree-ish has been resolved, so a
//!   missing tree-ish still exits 129 with git's usage text and an unresolvable one still
//!   exits 128 with git's `ambiguous argument` text, as stock git does.
//! * Unmerged (conflicted) index entries bail. Stock git emits
//!   `:<mode> 000000 <stage-2-id> <null> U` for them under `--cached`, and against the
//!   worktree it emits an ordinary `M` record whose source is the stage-2 entry; neither
//!   is reproduced here rather than approximated.
//! * With a bare `--abbrev` and no `core.abbrev` set, the length comes from gitoxide's
//!   unique-prefix computation for the first real id (falling back to 7); git derives it
//!   from the packed object count, so the two can differ on large packed repositories.
//!   An explicit `--abbrev=<n>` truncates to `n` without git's disambiguation lengthening.
//! * Magic (`:(...)`) and glob pathspecs bail; literal paths and directory prefixes work.
//!
//! ### Known deviation
//!
//! When a tracked file is replaced by a *directory*, gitoxide reports the entry as
//! removed and this prints `D`, whereas git prints a mode change. Everything else in
//! the worktree overlay maps one-to-one onto git's behaviour.

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::hash::ObjectId;
use gix::index::entry::stat::Options as StatOptions;
use gix::prelude::ObjectIdExt;

/// How the change list should be rendered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    /// `:<srcmode> <dstmode> <srcsha> <dstsha> <status>\t<path>` (git's default).
    Raw,
    /// `<path>`
    NameOnly,
    /// `<status>\t<path>`
    NameStatus,
    /// Nothing at all (`-s`, `--no-patch`, `--quiet`).
    Silent,
}

/// Parsed command-line options for a single `diff-index` invocation.
struct Opts {
    cached: bool,                  // --cached: compare against the index, ignore the worktree
    match_missing: bool,           // -m: files missing from the worktree count as up to date
    format: Format,
    nul: bool,                     // -z: NUL field/record terminators, no path quoting
    abbrev: Option<Option<usize>>, // --abbrev[=N]: None=full, Some(None)=auto, Some(Some(n))=N
    exit_code: bool,               // --exit-code/--quiet: exit 1 when anything differs
}

/// One file-level change, already reduced to the columns git's raw format prints.
/// A mode of `0` means the side does not exist.
struct Delta {
    src_mode: u32,
    dst_mode: u32,
    src_id: ObjectId,
    dst_id: ObjectId,
    status: u8,
    /// Repository-root relative path.
    path: BString,
}

/// A tree↔index difference for one path.
enum TreeChange {
    Added { mode: u32, id: ObjectId },
    Deleted { mode: u32, id: ObjectId },
    Modified { old_mode: u32, old_id: ObjectId, new_mode: u32, new_id: ObjectId },
}

/// How the worktree deviates from the index entry for one path.
enum Wt {
    /// The worktree file is gone.
    Removed,
    /// The worktree file exists but differs from the index; git records this mode and
    /// the null object id because the content was never written to the odb.
    NewMode(u32),
    /// A checked-out submodule with local changes; git still emits the index id.
    SubmoduleDirty,
}

/// The flag list quoted back at the user when an unimplemented option shows up.
const PORTED: &str = "--cached, -m, --raw, --name-only, --name-status, -z, --abbrev[=<n>], \
                      --no-abbrev, --full-index, --exit-code, --quiet, -s/--no-patch, --no-renames";

/// Stock `git diff-index`'s usage text, reproduced byte for byte (including the
/// trailing blank line) because it is written to stderr on every usage error.
const USAGE: &str = r"usage: git diff-index [-m] [--cached] [--merge-base] [<common-diff-options>] <tree-ish> [<path>...]

common diff options:
  -z            output diff-raw with lines terminated with NUL.
  -p            output patch format.
  -u            synonym for -p.
  --patch-with-raw
                output both a patch and the diff-raw format.
  --stat        show diffstat instead of patch.
  --numstat     show numeric diffstat instead of patch.
  --patch-with-stat
                output a patch and prepend its diffstat.
  --name-only   show only names of changed files.
  --name-status show names and status of changed files.
  --full-index  show full object name on index lines.
  --abbrev=<n>  abbreviate object names in diff-tree header and diff-raw.
  -R            swap input file pairs.
  -B            detect complete rewrites.
  -M            detect renames.
  -C            detect copies.
  --find-copies-harder
                try unchanged files as candidate for copy detection.
  -l<n>         limit rename attempts up to <n> paths.
  -O<file>      reorder diffs according to the <file>.
  -S<string>    find filepair whose only one side contains the string.
  --pickaxe-all
                show all files diff when -S is used and hit is found.
  -a  --text    treat all files as text.

";

/// Options that steer only patch, stat or colour rendering — never the raw,
/// `--name-only` or `--name-status` listings this module emits.
///
/// Each entry was checked against stock git by diffing `git diff-index HEAD` with and
/// without the option in a repository holding a worktree modification; all of them
/// leave those bytes and the exit status untouched. Deliberately absent: `-U<n>`,
/// `--unified=<n>` and `--binary`, which look like rendering knobs but switch the
/// output format to a patch, and `--line-prefix=`, which prefixes every raw record.
fn render_only_option(a: &str) -> bool {
    const EXACT: &[&str] = &[
        "-a",
        "-D",
        "-W",
        "--color",
        "--color-moved",
        "--color-words",
        "--default-prefix",
        "--ext-diff",
        "--full-index",
        "--function-context",
        "--histogram",
        "--indent-heuristic",
        "--irreversible-delete",
        "--ita-visible-in-index",
        "--minimal",
        "--no-color",
        "--no-color-moved",
        "--no-color-moved-ws",
        "--no-diff-merges",
        "--no-ext-diff",
        "--no-function-context",
        "--no-indent-heuristic",
        "--no-prefix",
        "--no-relative",
        "--no-rename-empty",
        "--no-renames",
        "--no-textconv",
        "--patience",
        "--pickaxe-all",
        "--pickaxe-regex",
        "--rename-empty",
        "--submodule",
        "--text",
        "--textconv",
        "--word-diff",
    ];
    const WITH_VALUE: &[&str] = &[
        "--anchored=",
        "--color=",
        "--color-moved=",
        "--color-moved-ws=",
        "--diff-algorithm=",
        "--diff-merges=",
        "--dst-prefix=",
        "--inter-hunk-context=",
        "--output-indicator-context=",
        "--output-indicator-new=",
        "--output-indicator-old=",
        "--src-prefix=",
        "--submodule=",
        "--word-diff=",
        "--word-diff-regex=",
        "--ws-error-highlight=",
    ];
    EXACT.contains(&a) || WITH_VALUE.iter().any(|p| a.starts_with(*p))
}

/// Short options whose value may be written as a separate argument (`-S fn` as well as
/// `-Sfn`). All of them are unimplemented here, but the value still has to be consumed
/// so it is not mistaken for the tree-ish.
fn short_option_takes_value(a: &str) -> bool {
    matches!(a, "-S" | "-G" | "-I" | "-O" | "-U" | "-l")
}

pub fn diff_index(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand at index 0; tolerate its absence so the entry
    // point behaves the same either way.
    let args = match args.first() {
        Some(first) if first == "diff-index" => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        cached: false,
        match_missing: false,
        format: Format::Raw,
        nul: false,
        abbrev: None,
        exit_code: false,
    };
    let mut quiet = false;
    let mut treeish: Option<&str> = None;
    let mut paths: Vec<BString> = Vec::new();
    let mut after_dashdash = false;
    // The first option git understands but this module does not. Held back rather than
    // raised immediately: git parses the whole command line before it looks at the
    // tree-ish, so a missing or unresolvable revision still has to win, exactly as it
    // does in stock git, and only a run that would otherwise have produced output is
    // refused.
    let mut unsupported: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        if after_dashdash {
            paths.push(a.into());
            continue;
        }
        match a {
            "--" => after_dashdash = true,
            "--cached" => opts.cached = true,
            "-m" => opts.match_missing = true,
            "--raw" => opts.format = Format::Raw,
            "--name-only" => opts.format = Format::NameOnly,
            "--name-status" => opts.format = Format::NameStatus,
            "-s" | "--no-patch" => opts.format = Format::Silent,
            "-z" => opts.nul = true,
            "--abbrev" => opts.abbrev = Some(None),
            "--no-abbrev" => opts.abbrev = None,
            "--exit-code" => opts.exit_code = true,
            "--quiet" => {
                opts.exit_code = true;
                quiet = true;
            }
            s if s.starts_with("--abbrev=") => {
                let n: usize = s["--abbrev=".len()..]
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid --abbrev value in {s:?}"))?;
                opts.abbrev = Some(Some(n));
            }
            s if render_only_option(s) => {}
            s if s.starts_with('-') && s.len() > 1 => {
                if short_option_takes_value(s) {
                    i += 1;
                }
                unsupported.get_or_insert_with(|| s.to_owned());
            }
            s if treeish.is_none() => treeish = Some(s),
            s => paths.push(s.into()),
        }
    }
    if quiet {
        opts.format = Format::Silent;
    }

    let Some(spec) = treeish else {
        eprint!("{}", USAGE);
        return Ok(ExitCode::from(129));
    };

    let repo = gix::discover(".")?;

    let tree_id = match repo
        .rev_parse_single(spec)
        .map_err(anyhow::Error::from)
        .and_then(|id| Ok(id.object()?.peel_to_tree()?.id))
    {
        Ok(id) => id,
        Err(_) => {
            eprintln!(
                "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
                 Use '--' to separate paths from revisions, like this:\n\
                 'git <command> [<revision>...] -- [<file>...]'"
            );
            return Ok(ExitCode::from(128));
        }
    };

    if let Some(flag) = unsupported {
        bail!("unsupported flag {flag:?} (ported: {PORTED})");
    }

    // Match the house line on pathspecs: literal paths and directory prefixes are
    // honoured, magic and glob prefixes are refused rather than silently matching
    // differently than git would.
    for p in &paths {
        if p.first() == Some(&b':') {
            bail!("pathspec magic is not supported: {p:?}");
        }
        if p.iter().any(|&b| matches!(b, b'*' | b'?' | b'[')) {
            bail!("glob pathspecs are not supported: {p:?}");
        }
    }

    // Pathspecs are cwd-relative in git while output paths are root-relative, so lift
    // every pattern into repository-root space before matching.
    let prefix = repo_prefix(&repo)?;
    let paths: Vec<BString> = paths
        .into_iter()
        .map(|p| {
            let mut full = prefix.clone();
            full.extend_from_slice(&p);
            full
        })
        .collect();

    let mut deltas = collect(&repo, &tree_id, &opts)?;
    if !paths.is_empty() {
        deltas.retain(|d| paths.iter().any(|p| path_matches(&d.path, p)));
    }
    // git emits index order, which is a plain byte-wise sort of the paths.
    deltas.sort_by(|a, b| a.path.cmp(&b.path));

    if opts.format != Format::Silent {
        let text = render(&repo, &deltas, &opts)?;
        let stdout = std::io::stdout();
        stdout.lock().write_all(&text)?;
    }

    Ok(if opts.exit_code && !deltas.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Diff `tree_id` against the index, then (unless `--cached`) fold in how the worktree
/// deviates from that index, exactly as git's `oneway_diff` does.
fn collect(repo: &gix::Repository, tree_id: &ObjectId, opts: &Opts) -> Result<Vec<Delta>> {
    let null = ObjectId::null(repo.object_hash());
    let index = repo.index_or_empty()?;
    let index_state: &gix::index::State = &index;

    if index_state.entries().iter().any(|e| e.stage_raw() != 0) {
        bail!("unmerged (conflicted) index entries are not supported");
    }

    let tree_changes = tree_index_changes(repo, tree_id, index_state)?;

    if opts.cached {
        let mut deltas = Vec::with_capacity(tree_changes.len());
        for (path, change) in tree_changes {
            let (src_mode, src_id, dst_mode, dst_id) = match change {
                TreeChange::Added { mode, id } => (0, null, mode, id),
                TreeChange::Deleted { mode, id } => (mode, id, 0, null),
                TreeChange::Modified {
                    old_mode,
                    old_id,
                    new_mode,
                    new_id,
                } => (old_mode, old_id, new_mode, new_id),
            };
            deltas.push(make_delta(src_mode, src_id, dst_mode, dst_id, path));
        }
        return Ok(deltas);
    }

    if repo.workdir().is_none() {
        bail!("this operation must be run in a work tree");
    }
    let wt = worktree_deviations(repo)?;

    let mut deltas = Vec::new();
    for (path, change) in &tree_changes {
        // `force` covers the one case where both sides are identical yet git still
        // reports a change: a submodule that is checked out and locally dirty.
        let (src, dst, force) = match change {
            // The path is absent from the index, so the worktree never enters into it.
            TreeChange::Deleted { mode, id } => ((*mode, *id), (0, null), false),
            TreeChange::Added { mode, id } => match wt.get(path) {
                None => ((0, null), (*mode, *id), false),
                Some((Wt::Removed, ..)) => {
                    if opts.match_missing {
                        ((0, null), (*mode, *id), false)
                    } else {
                        // git's `get_stat_data` fails here and `show_new_file` prints
                        // nothing at all for a staged addition that is gone on disk.
                        continue;
                    }
                }
                Some((Wt::NewMode(m), ..)) => ((0, null), (*m, null), false),
                Some((Wt::SubmoduleDirty, ..)) => ((0, null), (*mode, *id), false),
            },
            TreeChange::Modified {
                old_mode,
                old_id,
                new_mode,
                new_id,
            } => {
                let src = (*old_mode, *old_id);
                match wt.get(path) {
                    None => (src, (*new_mode, *new_id), false),
                    Some((Wt::Removed, ..)) => {
                        if opts.match_missing {
                            (src, (*new_mode, *new_id), false)
                        } else {
                            (src, (0, null), false)
                        }
                    }
                    Some((Wt::NewMode(m), ..)) => (src, (*m, null), false),
                    Some((Wt::SubmoduleDirty, ..)) => (src, (*new_mode, *new_id), true),
                }
            }
        };
        if !force && src == dst {
            continue;
        }
        deltas.push(make_delta(src.0, src.1, dst.0, dst.1, path.clone()));
    }

    // Paths where tree and index agree: the index entry is also the tree entry, so it
    // supplies the source side of a worktree-only difference.
    for (path, (w, entry_mode, entry_id)) in &wt {
        if tree_changes.contains_key(path) {
            continue;
        }
        let src = (*entry_mode, *entry_id);
        let (dst, force) = match w {
            Wt::Removed => {
                if opts.match_missing {
                    continue;
                }
                ((0, null), false)
            }
            Wt::NewMode(m) => ((*m, null), false),
            Wt::SubmoduleDirty => (src, true),
        };
        if !force && src == dst {
            continue;
        }
        deltas.push(make_delta(src.0, src.1, dst.0, dst.1, path.clone()));
    }

    Ok(deltas)
}

/// Every tree↔index difference, keyed by repository-relative path.
fn tree_index_changes(
    repo: &gix::Repository,
    tree_id: &ObjectId,
    index_state: &gix::index::State,
) -> Result<HashMap<BString, TreeChange>> {
    use gix::diff::index::ChangeRef;
    use gix::status::tree_index::TrackRenames;

    let mut changes: HashMap<BString, TreeChange> = HashMap::new();
    repo.tree_index_status(
        tree_id,
        index_state,
        None,
        TrackRenames::Disabled,
        |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
            match change {
                ChangeRef::Addition {
                    location,
                    entry_mode,
                    id,
                    ..
                } => {
                    changes.insert(
                        location.into_owned(),
                        TreeChange::Added {
                            mode: entry_mode.bits(),
                            id: id.into_owned(),
                        },
                    );
                }
                ChangeRef::Deletion {
                    location,
                    entry_mode,
                    id,
                    ..
                } => {
                    changes.insert(
                        location.into_owned(),
                        TreeChange::Deleted {
                            mode: entry_mode.bits(),
                            id: id.into_owned(),
                        },
                    );
                }
                ChangeRef::Modification {
                    location,
                    previous_entry_mode,
                    previous_id,
                    entry_mode,
                    id,
                    ..
                } => {
                    changes.insert(
                        location.into_owned(),
                        TreeChange::Modified {
                            old_mode: previous_entry_mode.bits(),
                            old_id: previous_id.into_owned(),
                            new_mode: entry_mode.bits(),
                            new_id: id.into_owned(),
                        },
                    );
                }
                // Rename tracking is disabled above, so this never fires.
                ChangeRef::Rewrite { .. } => {}
            }
            Ok(gix::diff::index::Action::Continue(()))
        },
    )?;
    Ok(changes)
}

/// How the worktree deviates from each index entry, along with that entry's mode and id.
fn worktree_deviations(
    repo: &gix::Repository,
) -> Result<HashMap<BString, (Wt, u32, ObjectId)>> {
    use gix::status::UntrackedFiles;
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let mut out: HashMap<BString, (Wt, u32, ObjectId)> = HashMap::new();

    let iter = repo
        .status(gix::progress::Discard)?
        // Untracked paths are invisible to `diff-index`, so skip the directory walk.
        .untracked_files(UntrackedFiles::None)
        .into_index_worktree_iter(Vec::new())?;

    for item in iter {
        let Item::Modification {
            entry,
            rela_path,
            status,
            ..
        } = item?
        else {
            // Rewrites need rename tracking (off by default) and directory contents
            // need the dirwalk (disabled above); neither can occur here.
            continue;
        };
        let entry_mode = entry.mode.bits();

        let w = match status {
            EntryStatus::Conflict { .. } => {
                bail!("unmerged (conflicted) paths are not supported")
            }
            EntryStatus::NeedsUpdate(new_stat) => {
                // gitoxide reports this both for a stat-dirty entry whose content turned
                // out to match and for a racily-clean one. git only ever re-reads content
                // in the racy case, so a genuine stat mismatch is still a difference to
                // it — that is why `git diff-index HEAD` flags merely touched files.
                if new_stat.matches(&entry.stat, StatOptions::default()) {
                    continue;
                }
                Wt::NewMode(entry_mode)
            }
            // An `--intent-to-add` entry has no content in the odb; against the worktree
            // git shows the null id on both sides.
            EntryStatus::IntentToAdd => Wt::NewMode(entry_mode),
            EntryStatus::Change(Change::Removed) => Wt::Removed,
            EntryStatus::Change(Change::Type { worktree_mode }) => {
                Wt::NewMode(worktree_mode.bits())
            }
            EntryStatus::Change(Change::Modification {
                executable_bit_changed,
                ..
            }) => Wt::NewMode(if executable_bit_changed {
                toggle_exec(entry_mode)
            } else {
                entry_mode
            }),
            EntryStatus::Change(Change::SubmoduleModification(_)) => Wt::SubmoduleDirty,
        };
        out.insert(rela_path, (w, entry_mode, entry.id));
    }
    Ok(out)
}

fn make_delta(
    src_mode: u32,
    src_id: ObjectId,
    dst_mode: u32,
    dst_id: ObjectId,
    path: BString,
) -> Delta {
    Delta {
        src_mode,
        dst_mode,
        src_id,
        dst_id,
        status: status_letter(src_mode, dst_mode),
        path,
    }
}

/// git's `diff_resolve_rename_copy` letter: absent source is an addition, absent
/// destination a deletion, differing `S_IFMT` bits a type change, otherwise a
/// modification.
fn status_letter(src_mode: u32, dst_mode: u32) -> u8 {
    const S_IFMT: u32 = 0o170000;
    if src_mode == 0 {
        b'A'
    } else if dst_mode == 0 {
        b'D'
    } else if (src_mode & S_IFMT) != (dst_mode & S_IFMT) {
        b'T'
    } else {
        b'M'
    }
}

/// Flip the executable bit of a regular-file mode, leaving anything else alone.
fn toggle_exec(mode: u32) -> u32 {
    match mode {
        0o100644 => 0o100755,
        0o100755 => 0o100644,
        other => other,
    }
}

/// The repository-relative directory the command was invoked from, with a trailing
/// slash, or empty when it was run at the root.
fn repo_prefix(repo: &gix::Repository) -> Result<BString> {
    let Some(prefix) = repo.prefix()? else {
        return Ok(BString::default());
    };
    if prefix.as_os_str().is_empty() {
        return Ok(BString::default());
    }
    let mut out: BString = gix::path::into_bstr(prefix).into_owned();
    out.push(b'/');
    Ok(out)
}

/// `true` if `path` equals `pat` or lives under the directory `pat`.
fn path_matches(path: &BString, pat: &BString) -> bool {
    let pat: &[u8] = {
        let raw = pat.as_slice();
        match raw.strip_suffix(b"/") {
            Some(trimmed) => trimmed,
            None => raw,
        }
    };
    let path = path.as_slice();
    path == pat || (path.len() > pat.len() && path.starts_with(pat) && path[pat.len()] == b'/')
}

/// Render the whole listing into the exact bytes git would write.
fn render(repo: &gix::Repository, deltas: &[Delta], opts: &Opts) -> Result<Vec<u8>> {
    let hexsz = repo.object_hash().len_in_hex();
    let len = abbrev_len(repo, deltas, opts, hexsz);

    // Field separator (between status and path) and record terminator.
    let (sep, term): (u8, u8) = if opts.nul { (0, 0) } else { (b'\t', b'\n') };

    let mut out = Vec::new();
    for d in deltas {
        match opts.format {
            Format::Silent => unreachable!("silent output is short-circuited by the caller"),
            Format::NameOnly => {}
            Format::NameStatus => {
                out.push(d.status);
                out.push(sep);
            }
            Format::Raw => {
                out.extend_from_slice(
                    format!(
                        ":{:06o} {:06o} {} {} ",
                        d.src_mode,
                        d.dst_mode,
                        hex(&d.src_id, len),
                        hex(&d.dst_id, len),
                    )
                    .as_bytes(),
                );
                out.push(d.status);
                out.push(sep);
            }
        }
        if opts.nul {
            out.extend_from_slice(d.path.as_ref());
        } else {
            out.extend_from_slice(quote_path(&d.path).as_bytes());
        }
        out.push(term);
    }
    Ok(out)
}

/// The object id column, full or truncated to `len` hex characters.
fn hex(id: &ObjectId, len: Option<usize>) -> String {
    match len {
        None => id.to_hex().to_string(),
        Some(n) => id.to_hex_with_len(n).to_string(),
    }
}

/// Resolve `--abbrev` into a concrete hex length, or `None` for full ids.
///
/// An explicit `--abbrev=<n>` is clamped to git's `[4, hash-length]` range. A bare
/// `--abbrev` follows `core.abbrev`; when that is unset (or the non-numeric `auto`)
/// the length is taken from gitoxide's unique-prefix computation for the first real
/// id in the listing, falling back to git's minimum default of 7 when there is none.
fn abbrev_len(
    repo: &gix::Repository,
    deltas: &[Delta],
    opts: &Opts,
    hexsz: usize,
) -> Option<usize> {
    let n = match opts.abbrev? {
        Some(n) => n,
        None => repo
            .config_snapshot()
            .integer("core.abbrev")
            .and_then(|v| usize::try_from(v).ok())
            .or_else(|| {
                deltas
                    .iter()
                    .flat_map(|d| [&d.src_id, &d.dst_id])
                    .find(|id| !id.is_null())
                    .map(|id| id.attach(repo).shorten_or_id().hex_len())
            })
            .unwrap_or(7),
    };
    Some(n.clamp(4, hexsz))
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.push_str(&format!("\\{b:03o}"));
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}
