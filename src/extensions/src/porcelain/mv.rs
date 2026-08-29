//! `git mv` — rename/move a tracked path in the index and worktree.
//!
//! Served natively on the vendored gitoxide index so tools on PATH observe the
//! same staged state. Supports the invocation forms stock `git mv` uses in
//! practice:
//!
//!   * `git mv <src> <dst>`                 — rename a tracked file or directory
//!   * `git mv <src>... <existing-dir>`     — move one or more paths into a dir
//!   * flags `-f`/`--force`, `-k`, `-n`/`--dry-run`, `-v`/`--verbose`,
//!     `--sparse`, `-h`, `--`
//!
//! A directory source remaps every tracked entry beneath it; a source that is
//! itself a gitlink is moved as a submodule (`.gitmodules` is rewritten and
//! restaged, and the submodule's `.git` file and `core.worktree` are repointed).
//! Overwriting a tracked/worktree destination requires `-f`. Exit codes match
//! stock git: usage errors return 129, fatal errors return 128, `-k`-skipped
//! failures still return 0.
//!
//! Sparse-checkout: without `--sparse`, a source or destination outside the
//! sparse-checkout definition is not moved. Every such path is collected and
//! reported by `advise_on_updating_sparse_paths()`, and the whole command exits
//! 1 having touched neither the index nor the worktree, exactly as `cmd_mv`
//! does. Not ported: `SKIP_WORKTREE_DIR`, git's handling of a *directory* that
//! exists only as sparse index entries (`empty_dir_has_sparse_contents()`);
//! such a source is still reported as `bad source`.

use anyhow::{anyhow, bail, Result};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stage, Stat};

use super::{Arg, LongOpt};

/// `cmd_mv()`'s `struct option builtin_mv_options[]` (builtin/mv.c), in table
/// order, as [`super::resolve_long`] reads it. `-k` is short-only and so has no
/// entry; no entry carries `PARSE_OPT_NONEG`.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "verbose", neg: true, arg: Arg::None },
    LongOpt { name: "dry-run", neg: true, arg: Arg::None },
    LongOpt { name: "force", neg: true, arg: Arg::None },
    LongOpt { name: "sparse", neg: true, arg: Arg::None },
];

/// `git mv -h` help, printed verbatim to stdout (git exits 129 after it).
const HELP: &str = "\
usage: git mv [-v] [-f] [-n] [-k] <source> <destination>
   or: git mv [-v] [-f] [-n] [-k] <source>... <destination-directory>

    -v, --[no-]verbose    be verbose
    -n, --[no-]dry-run    dry run
    -f, --[no-]force      force move/rename even if target exists
    -k                    skip move/rename errors
    --[no-]sparse         allow updating entries outside of the sparse-checkout cone

";

/// Print a fatal message to stderr and return git's fatal exit code (128).
/// stderr prose is not a compatibility surface (git's own is terse and varies);
/// the exit code is, so it is pinned exactly.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// Print the usage line to stderr and return git's usage exit code (129).
fn usage_err() -> Result<ExitCode> {
    // `usage_with_options()` writes the whole block — both `or:` lines and the
    // option list — not just the first line.
    eprint!("{HELP}");
    Ok(ExitCode::from(129))
}

/// A fully validated move: the on-disk rename plus the index path remaps it
/// implies. For a file the remap list has one pair; for a directory it has one
/// pair per tracked entry beneath the source.
struct Plan {
    src_abs: PathBuf,
    dst_abs: PathBuf,
    src_rel: String,
    dst_rel: String,
    /// (old repo-relative path, new repo-relative path) for each index entry.
    remaps: Vec<(String, String)>,
    /// `submodule_gitfiles[i]`: set when the source is a gitlink, carrying the
    /// git directory its `.git` *file* points at. `None` for a non-submodule;
    /// `Some(None)` is git's `SUBMODULE_WITH_GITDIR` — an embedded `.git`
    /// directory, which needs no repointing.
    submodule: Option<Option<PathBuf>>,
    /// A directory source, whose entries are remapped without a sparse check
    /// (git reaches `act_on_entry` before the sparse gate for those).
    is_dir: bool,
}

pub fn mv(args: &[String]) -> Result<ExitCode> {
    // 1. Parse flags and collect positional operands. `--` ends option parsing.
    let mut force = false;
    let mut skip = false;
    let mut dry_run = false;
    let mut verbose = false;
    let mut ignore_sparse = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut opts_done = false;
    for a in args {
        if opts_done {
            positional.push(a);
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, so it is matched before the abbreviation resolution
        // below rather than added to `LONG_OPTS`. This table has no
        // `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the same block `-h`
        // prints.
        if a == "--help-all" {
            print!("{HELP}");
            return Ok(ExitCode::from(129));
        }
        let resolved = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, HELP))
            }
        };
        match resolved.as_ref() {
            "--" => opts_done = true,
            "-h" => {
                // git prints the full help to stdout and exits 129, before any
                // repository lookup — so `-h` works outside a work tree too.
                // (`--help` is deliberately NOT handled here: stock git execs the
                //  man pager for it, a foreign op this server cannot reproduce.)
                print!("{HELP}");
                return Ok(ExitCode::from(129));
            }
            "-f" | "--force" => force = true,
            // Every flag here is an `OPT_BOOL`, whose unset writes 0.
            "--no-force" => force = false,
            "-k" => skip = true,
            "-n" | "--dry-run" => dry_run = true,
            "--no-dry-run" => dry_run = false,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "--sparse" => ignore_sparse = true,
            "--no-sparse" => ignore_sparse = false,
            // A long name no entry claims is `PARSE_OPT_UNKNOWN`, named without
            // its `--`.
            s if s.starts_with("--") => return Ok(super::unknown_option(s, HELP)),
            // Every remaining `-<chars>` token, walked the way
            // `parse_options_step()` walks a short cluster
            // (parse-options.c:1061-1107). None of `mv`'s short options takes a
            // value, so the whole cluster is flags; what a refusal names is the
            // character parsing stopped at, against the synthetic `-<rest>` the
            // C builds at :1095. Reporting the whole token as one long option is
            // what made `git mv -a` say ``unknown option `a'`` and `git mv -fa`
            // say ``unknown option `fa'`` where stock names `a` both times.
            s if s.starts_with('-') && s.len() > 1 => {
                for (off, c) in s.char_indices().skip(1) {
                    match c {
                        'f' => force = true,
                        'k' => skip = true,
                        'n' => dry_run = true,
                        'v' => verbose = true,
                        'h' => {
                            print!("{HELP}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(super::unknown_option(&format!("-{}", &s[off..]), HELP)),
                    }
                }
            }
            // A non-option argument is handed back unchanged by the resolver.
            _ => positional.push(a),
        }
    }

    if positional.len() < 2 {
        return usage_err();
    }

    // 2. Repository + worktree context. All paths are resolved relative to the
    //    current directory via the repo prefix, then made repo-relative.
    let repo = match gix::discover(".") {
        Ok(r) => r,
        Err(_) => return fatal("not a git repository (or any of the parent directories): .git"),
    };
    let workdir = match repo.workdir() {
        Some(w) => w.to_owned(),
        None => return fatal("this operation must be run in a work tree"),
    };
    let prefix = match repo.prefix() {
        Ok(p) => p.map(Path::to_path_buf).unwrap_or_default(),
        Err(e) => return fatal(format!("cannot resolve worktree prefix: {e}")),
    };

    // 3. Split operands: everything but the last is a source; the last is the
    //    destination. Decide file-mode vs into-directory-mode the way git does:
    //    a trailing slash or an existing directory means "into directory".
    let dest_arg = *positional.last().expect("checked len >= 2");
    let sources = &positional[..positional.len() - 1];

    let dest_rel = match normalize_rel(&workdir, &prefix, dest_arg) {
        Ok(r) => r,
        Err(e) => return fatal(e),
    };
    let dest_abs = workdir.join(&dest_rel);
    let trailing_slash = dest_arg.ends_with('/');
    let dest_is_dir = dest_abs.is_dir();

    if trailing_slash && !dest_is_dir {
        let first = match normalize_rel(&workdir, &prefix, sources[0]) {
            Ok(r) => r,
            Err(e) => return fatal(e),
        };
        return fatal(format!(
            "destination directory does not exist, source={first}, destination={dest_arg}"
        ));
    }
    let dir_mode = dest_is_dir;
    if sources.len() > 1 && !dir_mode {
        return fatal(format!("destination '{dest_arg}' is not a directory"));
    }

    // 4. Serialize the whole index read-modify-write through the repo
    //    coordinator for real moves; a dry run mutates nothing and needs no
    //    lock. The guard is held across validation, the disk renames, and the
    //    single index write below.
    let _lock = (!dry_run).then(|| crate::lock::RepoLock::acquire(repo.git_dir()));
    let mut index = match repo.open_index() {
        Ok(i) => i,
        Err(e) => return fatal(format!("index file corrupt: {e}")),
    };

    // 5. Validation phase — build a plan per source against the pristine index.
    //    Without `-k` the first failure aborts before ANY disk/index mutation,
    //    matching git's all-or-nothing behavior. With `-k` a failing source is
    //    silently skipped and the command still succeeds.
    //
    //    `path_in_sparse_checkout()` is the very last gate git applies, after
    //    every other check has passed, so that it can point at `--sparse`.
    let sparsity = if !ignore_sparse
        && repo
            .config_snapshot()
            .boolean("core.sparseCheckout")
            .unwrap_or(false)
    {
        Some(super::sparse_checkout::load_sparsity(&repo)?)
    } else {
        None
    };
    let mut only_match_skip_worktree: Vec<String> = Vec::new();

    let mut plans: Vec<Plan> = Vec::new();
    for s in sources {
        match plan_source(&index, &workdir, &prefix, s, dir_mode, &dest_rel, force) {
            Ok(plan) => {
                // Both ends are checked, and both are named in the report.
                if let Some(sp) = sparsity.as_ref().filter(|_| !plan.is_dir) {
                    let mut skip_sparse = false;
                    for end in [&plan.src_rel, &plan.dst_rel] {
                        if !sp.includes(end) {
                            only_match_skip_worktree.push(end.clone());
                            skip_sparse = true;
                        }
                    }
                    if skip_sparse {
                        continue;
                    }
                }
                plans.push(plan)
            }
            Err(e) => {
                if skip {
                    continue;
                }
                return fatal(format!("{e:#}"));
            }
        }
    }

    // git reports every sparse-excluded path together and then gives up on the
    // whole command — nothing has been renamed or staged at this point.
    if !only_match_skip_worktree.is_empty() {
        crate::advice::on_updating_sparse_paths(&repo, &only_match_skip_worktree);
        if !skip {
            return Ok(ExitCode::from(1));
        }
    }

    // 6. Apply phase — print the same lines git prints, then (unless dry-run)
    //    rename on disk and remap the index entries.
    let mut modified = false;
    let mut gitmodules_modified = false;
    for plan in &plans {
        if dry_run {
            println!("Checking rename of '{}' to '{}'", plan.src_rel, plan.dst_rel);
        }
        if verbose || dry_run {
            println!("Renaming {} to {}", plan.src_rel, plan.dst_rel);
        }
        if !dry_run {
            if let Err(e) = std::fs::rename(&plan.src_abs, &plan.dst_abs) {
                return fatal(format!("renaming '{}' failed: {e}", plan.src_rel));
            }
            if let Some(gitfile) = &plan.submodule {
                // `update_path_in_gitmodules()` then, for a `.git`-file
                // submodule, `connect_work_tree_and_git_dir()`.
                if update_path_in_gitmodules(&workdir, &plan.src_rel, &plan.dst_rel)? {
                    gitmodules_modified = true;
                }
                if let Some(git_dir) = gitfile {
                    connect_work_tree_and_git_dir(&plan.dst_abs, git_dir)?;
                }
            }
            apply_remaps(&mut index, &plan.remaps);
            modified = true;
        }
    }

    // 7. `stage_updated_gitmodules()`: the rewritten file is restaged, so the
    //    move shows up as one commit's worth of change.
    if gitmodules_modified {
        stage_gitmodules(&repo, &mut index, &workdir)?;
    }

    // 8. Persist once. `dangerously_push_entry` appends out of order, so restore
    //    the sort invariant before writing. The tree-cache was invalidated along
    //    both ends of every rename as the entries moved (see `apply_remaps`), so
    //    what is written back describes only the directories the move left alone.
    if modified {
        index.sort_entries();
        // `write_locked_index()` at the end of `cmd_mv()` (builtin/mv.c:634); the
        // options — the trailer's `skip_hash` and the `IEOT` offset table alike —
        // come from the repository, not from this call site
        // (read-cache.c:2830-2831, :2874-2904).
        super::write_tree::prepare_offset_table(&repo, &mut index);
        crate::index_racy::write(&repo, &mut index)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Validate a single source against the current index and worktree and return
/// the resulting [`Plan`], or `bail!` with a git-compatible reason.
fn plan_source(
    index: &gix::index::File,
    workdir: &Path,
    prefix: &Path,
    src_arg: &str,
    dir_mode: bool,
    dest_rel: &str,
    force: bool,
) -> Result<Plan> {
    let src_rel = normalize_rel(workdir, prefix, src_arg)?;
    let src_abs = workdir.join(&src_rel);

    // When moving into a directory the destination basename is the source's.
    let dst_rel = if dir_mode {
        let base = src_rel.rsplit('/').next().unwrap_or(&src_rel);
        format!("{dest_rel}/{base}")
    } else {
        dest_rel.to_owned()
    };
    let dst_abs = workdir.join(&dst_rel);

    // git reports a same-path move (and a move into a subpath of itself) with
    // this exact phrasing regardless of the item being a file.
    if src_rel == dst_rel || dst_rel.starts_with(&format!("{src_rel}/")) {
        crate::git_fatal!("can not move directory into itself, source={src_rel}, destination={dst_rel}");
    }

    // The source must exist on disk first (git lstat's it before consulting the
    // index): a tracked-but-deleted path reports "bad source", not "not under
    // version control".
    let meta = std::fs::symlink_metadata(&src_abs)
        .map_err(|_| anyhow!("bad source, source={src_rel}, destination={dst_rel}"))?;

    let mut submodule = None;
    let remaps: Vec<(String, String)> = if meta.is_dir() {
        // `dir_check`: an index entry *at* the directory itself is a gitlink, so
        // this is a submodule move rather than a subtree remap. git refuses to
        // touch `.gitmodules` while it has unstaged edits, since it is about to
        // rewrite and restage it.
        if let Some(mode) = tracked_mode(index, &src_rel) {
            if mode != Mode::COMMIT {
                crate::git_fatal!("Directory {src_rel} is in index and no submodule?");
            }
            if gitmodules_has_unstaged_changes(index, workdir)? {
                crate::git_fatal!(
                    "Please stage your changes to .gitmodules or stash them to proceed"
                );
            }
            // `read_gitfile()`: `Some(path)` for a `.git` file (a separate git
            // dir that has to be repointed), `None` for an embedded `.git`
            // directory (git's `SUBMODULE_WITH_GITDIR`).
            submodule = Some(read_gitfile(&src_abs.join(".git")));
            if dst_abs.exists() {
                crate::git_fatal!(
                    "destination already exists, source={src_rel}, destination={dst_rel}"
                );
            }
            return Ok(Plan {
                src_abs,
                dst_abs,
                src_rel: src_rel.clone(),
                dst_rel: dst_rel.clone(),
                remaps: vec![(src_rel, dst_rel)],
                submodule,
                is_dir: true,
            });
        }
        // Directory: remap every stage-0 entry beneath `src_rel/`.
        let sub_prefix = format!("{src_rel}/");
        let mut remaps = Vec::new();
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted {
                continue;
            }
            let p = e.path_in(backing);
            if p.starts_with(sub_prefix.as_bytes()) {
                let old = String::from_utf8_lossy(p).into_owned();
                let new = format!("{dst_rel}{}", &old[src_rel.len()..]);
                remaps.push((old, new));
            }
        }
        if remaps.is_empty() {
            crate::git_fatal!("not under version control, source={src_rel}, destination={dst_rel}");
        }
        // A directory destination that already exists on disk can't be merged
        // here; git refuses it too (only file destinations honor -f).
        if dst_abs.exists() {
            crate::git_fatal!("destination already exists, source={src_rel}, destination={dst_rel}");
        }
        remaps
    } else {
        // Regular file / symlink: it must be tracked at stage 0.
        if !is_tracked(index, &src_rel) {
            crate::git_fatal!("not under version control, source={src_rel}, destination={dst_rel}");
        }
        // Refuse to clobber an existing destination (tracked or on disk) unless
        // forced. `-f` relies on POSIX rename() replacing the destination file.
        if !force && (dst_abs.exists() || is_tracked(index, &dst_rel)) {
            crate::git_fatal!("destination exists, source={src_rel}, destination={dst_rel}");
        }
        vec![(src_rel.clone(), dst_rel.clone())]
    };

    // Fail early (before any mutation) if the destination's parent is missing,
    // so the abort stays atomic instead of surfacing mid-rename.
    if let Some(parent) = dst_abs.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            crate::git_fatal!("renaming '{src_rel}' failed: No such file or directory");
        }
    }

    let is_dir = meta.is_dir();
    Ok(Plan {
        src_abs,
        dst_abs,
        src_rel,
        dst_rel,
        remaps,
        submodule,
        is_dir,
    })
}

/// Whether a stage-0 index entry exists at exactly `rel`.
fn is_tracked(index: &gix::index::File, rel: &str) -> bool {
    tracked_mode(index, rel).is_some()
}

/// The mode of the stage-0 index entry at exactly `rel`, if there is one.
fn tracked_mode(index: &gix::index::File, rel: &str) -> Option<Mode> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .find(|e| {
            e.stage() == Stage::Unconflicted
                && AsRef::<[u8]>::as_ref(e.path_in(backing)) == rel.as_bytes()
        })
        .map(|e| e.mode)
}

/// `read_gitfile()`: the git directory a `gitdir: <path>` file points at, made
/// absolute against the file's own directory. `None` when `path` is not a
/// gitfile — an embedded `.git` directory, or nothing at all.
fn read_gitfile(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let target = text.strip_prefix("gitdir: ")?.trim_end_matches(['\n', '\r']);
    let target = Path::new(target);
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        path.parent()?.join(target)
    };
    Some(joined.canonicalize().unwrap_or(joined))
}

/// `is_staging_gitmodules_ok()`: true when the worktree `.gitmodules` differs
/// from the blob the index records, which is when git refuses to rewrite it.
fn gitmodules_has_unstaged_changes(index: &gix::index::File, workdir: &Path) -> Result<bool> {
    let Some(entry) = index.entry_by_path(BStr::new(b".gitmodules")) else {
        return Ok(false);
    };
    let content = match std::fs::read(workdir.join(".gitmodules")) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    let id = gix::objs::compute_hash(entry.id.kind(), gix::objs::Kind::Blob, &content)?;
    Ok(id != entry.id)
}

/// `update_path_in_gitmodules()`: point the `submodule.<name>.path` of whichever
/// section currently maps to `old` at `new`. The *name* never changes — only the
/// path does. Returns whether the file was rewritten; git only warns (and stages
/// nothing) when no section matches.
fn update_path_in_gitmodules(workdir: &Path, old: &str, new: &str) -> Result<bool> {
    let file = workdir.join(".gitmodules");
    if !file.exists() {
        return Ok(false);
    }
    let mut config = gix::config::File::from_path_no_includes(
        file.clone(),
        gix::config::Source::Worktree,
    )?;
    let name = config
        .sections_by_name("submodule")
        .into_iter()
        .flatten()
        .find(|s| s.value("path").is_some_and(|v| v.as_slice() == old.as_bytes()))
        .and_then(|s| s.header().subsection_name().map(ToOwned::to_owned));
    let Some(name) = name else {
        eprintln!("warning: Could not find section in .gitmodules where path={old}");
        return Ok(false);
    };
    config.set_raw_value_by("submodule", Some(name.as_ref()), "path", new)?;
    std::fs::write(&file, config.to_string())?;
    Ok(true)
}

/// `connect_work_tree_and_git_dir()`: rewrite `<work_tree>/.git` to point at
/// `git_dir` and `git_dir`'s `core.worktree` back at `work_tree`, both as paths
/// relative to each other, which is what makes a moved submodule keep working.
fn connect_work_tree_and_git_dir(work_tree: &Path, git_dir: &Path) -> Result<()> {
    let work_tree = work_tree.canonicalize().unwrap_or_else(|_| work_tree.to_path_buf());
    let git_dir = git_dir.canonicalize().unwrap_or_else(|_| git_dir.to_path_buf());

    std::fs::write(
        work_tree.join(".git"),
        format!("gitdir: {}\n", relative_path(&git_dir, &work_tree).display()),
    )?;

    let config_path = git_dir.join("config");
    let mut config = gix::config::File::from_path_no_includes(
        config_path.clone(),
        gix::config::Source::Local,
    )?;
    config.set_raw_value_by(
        "core",
        None::<&BStr>,
        "worktree",
        relative_path(&work_tree, &git_dir).to_string_lossy().as_ref(),
    )?;
    std::fs::write(&config_path, config.to_string())?;
    Ok(())
}

/// `relative_path(target, base)`: how to reach `target` starting from directory
/// `base`, using `..` for each level `base` sits below their common ancestor.
/// Both must already be absolute and normalized.
fn relative_path(target: &Path, base: &Path) -> PathBuf {
    let mut t = target.components().peekable();
    let mut b = base.components().peekable();
    while t.peek().is_some() && t.peek() == b.peek() {
        t.next();
        b.next();
    }
    let mut out = PathBuf::new();
    for _ in b {
        out.push("..");
    }
    out.extend(t);
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `stage_updated_gitmodules()`: hash the rewritten worktree file back into the
/// index so the move is one staged change, not a staged rename plus a dirty file.
fn stage_gitmodules(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    workdir: &Path,
) -> Result<()> {
    let path = workdir.join(".gitmodules");
    let content = std::fs::read(&path)?;
    let id = repo.write_blob(&content)?.detach();
    // Restaging goes through `add_file_to_index()` in git, so the path is
    // invalidated like any other staged one (read-cache.c:1273-1274).
    index.invalidate_path_in_tree(BStr::new(b".gitmodules"));
    let stat = gix::index::fs::Metadata::from_path_no_follow(&path)
        .ok()
        .and_then(|md| Stat::from_fs(&md).ok())
        .unwrap_or_default();
    match index.entry_index_by_path(BStr::new(b".gitmodules")) {
        Ok(at) => {
            let entry = &mut index.entries_mut()[at];
            entry.id = id;
            entry.stat = stat;
        }
        Err(_) => {
            let name = BString::from(".gitmodules");
            index.dangerously_push_entry(
                stat,
                id,
                Flags::empty(),
                Mode::FILE,
                BStr::new(&name),
            );
        }
    }
    Ok(())
}

/// Apply the (old → new) path remaps to the in-memory index: capture the moved
/// entries' fields, drop the old entries and any entry occupying a new path
/// (the force-overwrite case), then re-append the entries at their new paths.
/// A `sort_entries()` by the caller restores lookup invariants afterward.
fn apply_remaps(index: &mut gix::index::File, remaps: &[(String, String)]) {
    // `cmd_mv()` moves each entry with `rename_index_entry_at()`
    // (builtin/mv.c:615), which invalidates the cache-tree along the *old* name
    // (read-cache.c:169) and then re-adds the entry under the new one, where
    // `add_index_entry_with_check()` invalidates along the *new* name
    // (read-cache.c:1273-1274). Both ends, per rename — a directory neither the
    // source nor the destination passes through keeps its cached tree id.
    for (old, new) in remaps {
        index.invalidate_path_in_tree(BStr::new(old.as_bytes()));
        index.invalidate_path_in_tree(BStr::new(new.as_bytes()));
    }

    // Capture (new_path, fields) for each source entry before mutating.
    let mut pushes: Vec<(Stat, ObjectId, Flags, Mode, String)> = Vec::with_capacity(remaps.len());
    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted {
                continue;
            }
            let p = e.path_in(backing);
            if let Some((_, new)) = remaps
                .iter()
                .find(|(old, _)| old.as_bytes() == AsRef::<[u8]>::as_ref(p))
            {
                pushes.push((e.stat, e.id, e.flags, e.mode, new.clone()));
            }
        }
    }

    // Remove the old source paths and any destination they overwrite.
    let doomed: Vec<&[u8]> = remaps
        .iter()
        .flat_map(|(old, new)| [old.as_bytes(), new.as_bytes()])
        .collect();
    index.remove_entries(|_, path, _| doomed.iter().any(|d| *d == AsRef::<[u8]>::as_ref(path)));

    // Re-append each entry at its new path with the original blob and mode.
    for (stat, id, flags, mode, new) in pushes {
        let new_bytes = BString::from(new);
        index.dangerously_push_entry(stat, id, flags, mode, BStr::new(&new_bytes));
    }
}

/// Turn an operand into a clean, repo-relative, slash-separated path.
///
/// Relative operands are resolved against the worktree `prefix` (the repo-
/// relative CWD). Absolute operands are resolved against the worktree root
/// `workdir` and stripped back to repo-relative — stock git accepts an absolute
/// path that lands inside the worktree (verified: `git mv /abs/inside/a b`
/// exits 0). `.`/`..` are folded lexically. Any path that escapes the worktree
/// is a fatal "outside repository", matching git's exit 128.
fn normalize_rel(workdir: &Path, prefix: &Path, arg: &str) -> Result<String> {
    let arg_path = Path::new(arg);
    let joined = if arg_path.is_absolute() {
        // Resolve symlinks on the longest existing ancestor (macOS /tmp ->
        // /private/tmp), keep any not-yet-created tail, then strip the worktree
        // root. Anything not under it is outside the repository.
        let canon_wd = workdir
            .canonicalize()
            .unwrap_or_else(|_| workdir.to_path_buf());
        let real = canonicalize_lenient(arg_path);
        match real.strip_prefix(&canon_wd) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
            _ => crate::git_fatal!("'{arg}' is outside repository at '{}'", canon_wd.display()),
        }
    } else {
        prefix.join(arg)
    };
    let mut parts: Vec<String> = Vec::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    crate::git_fatal!("'{arg}' is outside repository at '{}'", workdir.display());
                }
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => {
                // Absolute inputs are stripped to worktree-relative above, so a
                // residual root component here means the path escaped.
                crate::git_fatal!("'{arg}' is outside repository at '{}'", workdir.display())
            }
        }
    }
    if parts.is_empty() {
        crate::git_fatal!("invalid path: {arg}");
    }
    Ok(parts.join("/"))
}

/// Canonicalize the longest existing prefix of `p`, re-appending the trailing
/// components that don't exist yet (a not-yet-created move destination). Falls
/// back to the path as given when nothing along it can be canonicalized.
fn canonicalize_lenient(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p;
    while let Some(parent) = cur.parent() {
        if let Some(name) = cur.file_name() {
            tail.push(name.to_os_string());
        }
        if let Ok(c) = parent.canonicalize() {
            let mut out = c;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return out;
        }
        cur = parent;
    }
    p.to_path_buf()
}
