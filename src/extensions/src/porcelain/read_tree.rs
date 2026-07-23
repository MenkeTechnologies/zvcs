//! `git read-tree` — read tree information into the index.
//!
//! Served natively through the vendored gitoxide crates, so tools on `PATH` observe
//! the same index. Every index/worktree mutation is serialized through
//! [`crate::lock::RepoLock`].
//!
//! `read-tree` prints nothing on success, so fidelity here is entirely about the
//! resulting index (entry set, stage, object id, mode, and the stat cache) and the
//! exit code / stderr text on the failure paths.
//!
//! ## Supported forms
//!
//! * `read-tree <tree-ish>...` — plain read: the index is replaced by the union of
//!   the named trees, later trees winning per path. Entries land at stage 0 with a
//!   zeroed stat cache, matching stock git.
//! * `read-tree` (no trees) — emits git's deprecation warning and empties the index.
//! * `read-tree --empty` — empty the index.
//! * `read-tree -m <tree-ish>` — one-way merge: like the plain read, except an index
//!   entry whose id and mode already match the tree keeps its stat cache. Refuses to
//!   run on an unmerged index, and refuses to clobber a modified tracked file.
//! * `read-tree --reset <tree-ish>` — as `-m`, but unmerged entries are discarded and
//!   the safety checks are skipped.
//! * `read-tree --prefix=<p>/ <tree-ish>` — keep the index and bind the tree under
//!   `<p>/`, refusing paths that already exist.
//! * `-u` (with `-m`/`--reset`/`--prefix`) — update the worktree for the paths whose
//!   index entry actually changed, and delete files the read drops. `--reset -u`
//!   additionally restores tracked files that are dirty or missing.
//! * `-i`, `-n`/`--dry-run`, `-q`/`--quiet`, `-v`/`--verbose`,
//!   `--index-output=<file>`, `--exclude-per-directory=<file>`,
//!   `--[no-]sparse-checkout`, `--[no-]recurse-submodules`, `--[no-]debug-unpack`.
//!
//! Options are parsed the way `parse-options` does: short switches cluster (`-mu`),
//! value-taking options accept both `--name=<v>` and `--name <v>`, every boolean has
//! its `--no-` form, `--` ends the option scan, and a usage error prints git's usage
//! block and exits 129.
//!
//! ## Not ported
//!
//! The two- and three-tree merges (`-m $H $M`, `-m $O $A $B`) are the bulk of
//! `read-tree`'s merge machinery — the "carry forward" table in `git-read-tree(1)` —
//! and are not implemented; supplying more than one tree with `-m`/`--reset`/
//! `--prefix` bails rather than writing a wrong index. `--trivial` and `--aggressive`
//! are accepted and ignored because they only tune that three-way merge, which is
//! unreachable here.
//!
//! ## Known deviations
//!
//! * Up-to-dateness is decided by content (via gitoxide's index↔worktree status)
//!   rather than by git's `stat` comparison. The two agree except that git can also
//!   reject a file whose `stat` moved while its content did not.
//! * The `-u` untracked-collision check rejects any existing file at a path the read
//!   adds; git additionally permits it when the file is `.gitignore`d.
//! * The cache-tree (`TREE`) extension is dropped on write, as everywhere else in
//!   zvcs, because gitoxide cannot recompute it (`gix_index::File::write`).
//! * `--sparse-checkout` is accepted but never applies a sparse filter, and
//!   `--recurse-submodules` never descends into submodules — both need substrate
//!   this port does not have, so they behave as their `--no-` counterparts.
//! * `--exclude-per-directory` reproduces git's "meaningless unless -u" gate but the
//!   ignore file it names is not consulted, since `-u` here never overwrites an
//!   existing untracked file in the first place.
//! * `--debug-unpack` is accepted and silent; there is no `unpack-trees` to trace.
//! * `read-tree --help` renders a man page under stock git and is not reproduced.

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};

/// Parsed command line for a single `read-tree` invocation.
#[derive(Default)]
struct Opts {
    merge: bool,               // -m
    reset: bool,               // --reset
    update: bool,              // -u
    index_only: bool,          // -i
    dry_run: bool,             // -n/--dry-run
    read_empty: bool,          // --empty
    prefix: Option<String>,    // --prefix=<p>
    index_output: Option<PathBuf>, // --index-output=<file>
    trees: Vec<String>,
}

impl Opts {
    /// Whether git would set `opts.merge` internally: `--reset` and `--prefix` both
    /// imply it, which is what gates `-u`/`-i` and the "at least one tree" check.
    fn merge_like(&self) -> bool {
        self.merge || self.reset || self.prefix.is_some()
    }

    /// Whether the merge safety checks apply. `--reset` explicitly opts out of them.
    fn checked(&self) -> bool {
        (self.merge || self.prefix.is_some()) && !self.reset && !self.index_only
    }
}

/// git's own `read-tree` usage block, byte for byte (`git read-tree -h`).
///
/// Reproduced verbatim because `parse-options` prints it on every usage error, and
/// parity compares stderr bytes. The trailing blank line is part of git's output.
const USAGE: &str = "\
usage: git read-tree [(-m [--trivial] [--aggressive] | --reset | --prefix=<prefix>)
                     [-u | -i]] [--index-output=<file>] [--no-sparse-checkout]
                     (--empty | <tree-ish1> [<tree-ish2> [<tree-ish3>]])

    --index-output <file> write resulting index to <file>
    --[no-]empty          only empty the index
    -v, --[no-]verbose    be verbose

Merging
    -m                    perform a merge in addition to a read
    --[no-]trivial        3-way merge if no file level merging required
    --[no-]aggressive     3-way merge in presence of adds and removes
    --[no-]reset          same as -m, but discard unmerged entries
    --prefix <subdirectory>/
                          read the tree into the index under <subdirectory>/
    -u                    update working tree with merge result
    --exclude-per-directory <gitignore>
                          allow explicitly ignored files to be overwritten
    -i                    don't check the working tree after merging
    -n, --[no-]dry-run    don't update the index or the work tree
    --no-sparse-checkout  skip applying sparse checkout filter
    --sparse-checkout     opposite of --no-sparse-checkout
    --[no-]debug-unpack   debug unpack-trees
    --[no-]recurse-submodules[=<checkout>]
                          control recursive updating of submodules
    -q, --[no-]quiet      suppress feedback messages

";

/// Report a git-style fatal and return git's exit code for it.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// An unrecognised option: an `error:` line, the usage block, and 129.
fn usage_err(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// A malformed value for a *known* option. Still 129, but `parse-options` prints only
/// the `error:` line here — the usage block is reserved for unrecognised options.
fn opt_err(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(129))
}

/// git's `git_parse_maybe_bool` spelling set, used by `--recurse-submodules=<v>`.
/// The empty string is false, matching `git_parse_maybe_bool_text`.
fn parse_maybe_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" | "1" => Some(true),
        "no" | "off" | "false" | "0" | "" => Some(false),
        _ => None,
    }
}

/// Report a git-style `error:` line and return git's exit code for it.
fn rejected(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(128))
}

pub fn read_tree(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 (dispatch strips it today).
    let argv: &[String] = match args.first() {
        Some(a) if a == "read-tree" => &args[1..],
        _ => args,
    };

    let mut o = Opts::default();
    let mut only_positionals = false;
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;

        if only_positionals || a == "-" || !a.starts_with('-') {
            o.trees.push(a.to_string());
            continue;
        }
        if a == "--" {
            only_positionals = true;
            continue;
        }

        // ---- Short option clusters (`-mu` is accepted by parse-options). ----
        if !a.starts_with("--") {
            for c in a[1..].chars() {
                match c {
                    'm' => o.merge = true,
                    'u' => o.update = true,
                    'i' => o.index_only = true,
                    'n' => o.dry_run = true,
                    // Progress and feedback: this port emits neither, so both no-op.
                    'v' | 'q' => {}
                    'h' => {
                        print!("{USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                    _ => return usage_err(format!("unknown switch `{c}'")),
                }
            }
            continue;
        }

        // ---- Long options: `--name` or `--name=<value>`. ----
        let body = &a[2..];
        let (name, inline) = match body.find('=') {
            Some(eq) => (&body[..eq], Some(&body[eq + 1..])),
            None => (body, None),
        };

        // Every arm below that takes no argument rejects an inline `=value`, which is
        // what parse-options does for OPTION_SET_INT / OPTION_BOOL.
        macro_rules! no_value {
            () => {
                if inline.is_some() {
                    return opt_err(format!("option `{name}' takes no value"));
                }
            };
        }
        // A value-taking option accepts `--name=<v>` or `--name <v>`.
        macro_rules! value {
            () => {
                match inline {
                    Some(v) => v.to_string(),
                    None => match argv.get(i) {
                        Some(next) => {
                            i += 1;
                            next.clone()
                        }
                        None => return opt_err(format!("option `{name}' requires a value")),
                    },
                }
            };
        }

        match name {
            "reset" => {
                no_value!();
                o.reset = true;
            }
            "no-reset" => {
                no_value!();
                o.reset = false;
            }
            "dry-run" => {
                no_value!();
                o.dry_run = true;
            }
            "no-dry-run" => {
                no_value!();
                o.dry_run = false;
            }
            "empty" => {
                no_value!();
                o.read_empty = true;
            }
            "no-empty" => {
                no_value!();
                o.read_empty = false;
            }
            // Feedback-only switches: this port is silent either way.
            "verbose" | "no-verbose" | "quiet" | "no-quiet" => no_value!(),
            // `--trivial`/`--aggressive` only tune the two- and three-tree merges,
            // which are not reachable here, so they carry no behaviour of their own.
            "trivial" | "no-trivial" | "aggressive" | "no-aggressive" => no_value!(),
            // Sparse checkout is never applied by this port, so both directions no-op.
            "sparse-checkout" | "no-sparse-checkout" => no_value!(),
            // `unpack-trees` tracing has no analogue here.
            "debug-unpack" | "no-debug-unpack" => no_value!(),
            "no-recurse-submodules" => no_value!(),
            "recurse-submodules" => {
                // The optional value is a boolean; anything else is fatal in git.
                if let Some(v) = inline {
                    if parse_maybe_bool(v).is_none() {
                        return fatal(format!("bad recurse-submodules argument: {v}"));
                    }
                }
            }
            "prefix" => o.prefix = Some(value!()),
            "index-output" => o.index_output = Some(PathBuf::from(value!())),
            "exclude-per-directory" => {
                let _ignore_file = value!();
                // git checks this inside the option callback, so it fires during the
                // scan — before the tree-ishes are resolved, and it only sees the
                // `-u` seen so far.
                if !o.update {
                    return fatal("--exclude-per-directory is meaningless unless -u");
                }
            }
            _ => return usage_err(format!("unknown option `{name}'")),
        }
    }

    // ---- Argument validation, in git's own order (builtin/read-tree.c). ----
    if 1 < usize::from(o.merge) + usize::from(o.reset) + usize::from(o.prefix.is_some()) {
        return fatal("Which one? -m, --reset, or --prefix?");
    }

    let repo = gix::discover(".")?;

    // Resolve every tree-ish before any other check, exactly like git's read loop.
    let mut tree_ids: Vec<ObjectId> = Vec::with_capacity(o.trees.len());
    for spec in &o.trees {
        let Ok(obj) = repo.rev_parse_single(spec.as_str()) else {
            return fatal(format!("Not a valid object name {spec}"));
        };
        // `object()` and `peel_to_tree()` have distinct error types, so the two
        // fallible steps are joined through anyhow rather than `and_then`.
        let peeled = obj
            .object()
            .map_err(anyhow::Error::from)
            .and_then(|obj| obj.peel_to_tree().map_err(anyhow::Error::from));
        let Ok(tree) = peeled else {
            return fatal(format!("failed to unpack tree object {spec}"));
        };
        tree_ids.push(tree.id);
    }

    if tree_ids.is_empty() && !o.read_empty && !o.merge_like() {
        eprintln!("warning: read-tree: emptying the index with no arguments is deprecated; use --empty");
    } else if !tree_ids.is_empty() && o.read_empty {
        return fatal("passing trees as arguments contradicts --empty");
    }

    if o.index_only && o.update {
        return fatal("-u and -i at the same time makes no sense");
    }
    if (o.update || o.index_only) && !o.merge_like() {
        let flag = if o.update { "-u" } else { "-i" };
        return fatal(format!("{flag} is meaningless without -m, --reset, or --prefix"));
    }
    if o.merge_like() && tree_ids.is_empty() {
        return fatal("you must specify at least one tree to merge");
    }
    if o.merge_like() && tree_ids.len() > 1 {
        bail!(
            "unsupported: {} tree-ishes with -m/--reset/--prefix (the two- and three-way \
             read-tree merges are not ported; only the one-tree form is)",
            tree_ids.len()
        );
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Bound first: `index_or_empty` yields an Arc<FileSnapshot<File>>, and the
    // deref chain to &File only resolves once it is a named binding.
    let index = repo.index_or_empty()?;
    let old = gix::index::File::clone(&index);

    if o.merge_like() && !o.reset && old.entries().iter().any(|e| e.stage_raw() != 0) {
        return fatal("You need to resolve your current index first");
    }

    // ---- Build the index this invocation wants to end up with. ----
    let mut new_index = if o.read_empty || tree_ids.is_empty() {
        gix::index::File::from_state(
            gix::index::State::new(repo.object_hash()),
            repo.index_path(),
        )
    } else if let Some(prefix) = &o.prefix {
        match bind_prefix(&repo, &old, tree_ids[0], prefix)? {
            Ok(index) => index,
            Err(code) => return Ok(code),
        }
    } else {
        union_of_trees(&repo, &tree_ids)?
    };

    // `-m`/`--reset` carry the stat cache of entries the tree leaves untouched, so a
    // later `git status` does not see the whole tree as freshly modified.
    let old_map = stage0_map(&old);
    if o.merge || o.reset {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            if let Some((id, mode, stat)) = old_map.get(&e.path_in(&backing).to_owned()) {
                if *id == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }

    // ---- Classify the paths this read touches. ----
    let new_map = stage0_map(&new_index);
    let new_paths: BTreeSet<BString> = new_map.keys().cloned().collect();
    let old_paths: BTreeSet<BString> = old_map.keys().cloned().collect();

    // Changed = added by this read, or present before with a different id/mode.
    let changed: BTreeSet<BString> = new_paths
        .iter()
        .filter(|p| match (old_map.get(*p), new_map.get(*p)) {
            (Some((oid, omode, _)), Some((nid, nmode, _))) => oid != nid || omode != nmode,
            _ => true,
        })
        .cloned()
        .collect();
    // `--prefix` never drops anything; every other form replaces the index wholesale.
    let removed: BTreeSet<BString> = if o.prefix.is_some() {
        BTreeSet::new()
    } else {
        old_paths.difference(&new_paths).cloned().collect()
    };

    // ---- Merge safety checks, before touching anything. ----
    // Needed for the checks, and for `--reset -u` to know which files to restore.
    let dirty = if o.checked() || (o.reset && o.update) {
        worktree_dirty(&repo)?
    } else {
        HashSet::new()
    };

    if o.checked() {
        for path in old_paths.union(&new_paths) {
            let loses_content = removed.contains(path)
                || (changed.contains(path) && old_map.contains_key(path));
            if loses_content && dirty.contains(path) {
                return rejected(format!(
                    "Entry '{}' not uptodate. Cannot merge.",
                    path.to_str_lossy()
                ));
            }
            // A file we are about to create must not already exist untracked, but
            // only when we would actually write it (`-u`).
            if o.update && changed.contains(path) && !old_map.contains_key(path) {
                let exists = repo
                    .workdir_path(path.as_bstr())
                    .is_some_and(|p| p.symlink_metadata().is_ok());
                if exists {
                    return rejected(format!(
                        "Untracked working tree file '{}' would be overwritten by merge.",
                        path.to_str_lossy()
                    ));
                }
            }
        }
    }

    if o.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // ---- Worktree update. ----
    if o.update {
        // `--reset` also restores tracked files that drifted from the index.
        let mut wanted = changed.clone();
        if o.reset {
            wanted.extend(new_paths.iter().filter(|p| dirty.contains(*p)).cloned());
        }
        checkout_subset(&repo, &mut new_index, &wanted)?;
        for path in &removed {
            if let Some(full) = repo.workdir_path(path.as_bstr()) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    // ---- Persist. ----
    if let Some(out) = &o.index_output {
        new_index.set_path(out.clone());
    }
    new_index.remove_tree();
    new_index.write(Default::default())?;

    Ok(ExitCode::SUCCESS)
}

/// The index built by reading `tree_ids` in order: the union of their entries, with
/// a later tree replacing an earlier one at the same path. All entries are stage 0
/// with a zeroed stat cache, which is what stock git produces for a plain read.
fn union_of_trees(repo: &gix::Repository, tree_ids: &[ObjectId]) -> Result<gix::index::File> {
    let mut index = repo.index_from_tree(&tree_ids[0])?;
    for id in &tree_ids[1..] {
        let extra = repo.index_from_tree(id)?;
        let extra_paths: HashSet<BString> = {
            let backing = extra.path_backing();
            extra.entries().iter().map(|e| e.path_in(backing).to_owned()).collect()
        };
        index.remove_entries(|_, path, _| extra_paths.contains(&path.to_owned()));
        let backing = extra.path_backing().to_owned();
        for e in extra.entries() {
            index.dangerously_push_entry(e.stat, e.id, e.flags, e.mode, e.path_in(&backing));
        }
        index.sort_entries();
    }
    Ok(index)
}

/// `--prefix=<p>`: keep `old` and add every entry of `tree` under `<p>/`.
///
/// Returns `Err(ExitCode)` for git's bind-overlap rejection so the caller can exit
/// with git's code and message instead of an `anyhow` error.
fn bind_prefix(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    prefix: &str,
) -> Result<std::result::Result<gix::index::File, ExitCode>> {
    let prefix = if prefix.is_empty() || prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    };

    let existing: HashSet<BString> = {
        let backing = old.path_backing();
        old.entries().iter().map(|e| e.path_in(backing).to_owned()).collect()
    };

    let mut index = gix::index::File::clone(old);
    let from_tree = repo.index_from_tree(&tree)?;
    let backing = from_tree.path_backing().to_owned();
    for e in from_tree.entries() {
        let mut path = BString::from(prefix.as_bytes());
        path.extend_from_slice(e.path_in(&backing).as_ref());
        if existing.contains(&path) {
            let shown = path.to_str_lossy();
            eprintln!("error: Entry '{shown}' overlaps with '{shown}'.  Cannot bind.");
            return Ok(Err(ExitCode::from(128)));
        }
        index.dangerously_push_entry(e.stat, e.id, e.flags, e.mode, path.as_bstr());
    }
    index.sort_entries();
    Ok(Ok(index))
}

/// Path → (id, mode, stat) for the stage-0 entries of `index`.
fn stage0_map(index: &gix::index::File) -> HashMap<BString, (ObjectId, Mode, Stat)> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .filter(|e| e.stage_raw() == 0)
        .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode, e.stat)))
        .collect()
}

/// Tracked paths whose worktree content differs from the current index.
///
/// A path missing from the worktree is deliberately *not* reported: git's
/// `verify_uptodate` treats `ENOENT` as up to date.
fn worktree_dirty(repo: &gix::Repository) -> Result<HashSet<BString>> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    if repo.workdir().is_none() {
        return Ok(HashSet::new());
    }

    let patterns: Vec<BString> = Vec::new();
    let iter = repo
        .status(gix::progress::Discard)?
        .untracked_files(gix::status::UntrackedFiles::None)
        .into_index_worktree_iter(patterns)?;

    let mut dirty = HashSet::new();
    for item in iter {
        if let Item::Modification { rela_path, status, .. } = item? {
            if let EntryStatus::Change(
                Change::Modification { .. } | Change::Type { .. } | Change::SubmoduleModification(_),
            ) = status
            {
                dirty.insert(rela_path);
            }
        }
    }
    Ok(dirty)
}

/// Check out exactly the entries of `index` named by `wanted`, then carry the stat
/// information gained back onto `index` so the written index is clean for them.
fn checkout_subset(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    wanted: &BTreeSet<BString>,
) -> Result<()> {
    if wanted.is_empty() {
        return Ok(());
    }
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("this operation must be run in a work tree"))?
        .to_owned();

    // Checking out a subset index writes only those paths, leaving the worktree
    // copies of carried-forward entries (which may be modified) untouched.
    let mut subset = gix::index::File::clone(index);
    subset.remove_entries(|_, path, _| !wanted.contains(&path.to_owned()));
    subset.remove_tree();

    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let should_interrupt = AtomicBool::new(false);
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

    let fresh: HashMap<BString, Stat> = {
        let backing = subset.path_backing();
        subset
            .entries()
            .iter()
            .map(|e| (e.path_in(backing).to_owned(), e.stat))
            .collect()
    };
    let backing = index.path_backing().to_owned();
    for e in index.entries_mut() {
        if let Some(stat) = fresh.get(&e.path_in(&backing).to_owned()) {
            e.stat = *stat;
        }
    }
    Ok(())
}
