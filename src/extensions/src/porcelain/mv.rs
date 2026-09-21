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

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
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
    /// git's `SPARSE` mode bit: the source is in the index with `skip-worktree`
    /// and nothing of it is on disk, so `--sparse` moves the *index entry* and
    /// there is no `rename()` to make (builtin/mv.c:335-344).
    index_only: bool,
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
        if let Some(code) = super::long_takes_no_value(a, LONG_OPTS) {
            return Ok(code);
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
    let repo = match crate::setup::discover() {
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
    // ```c
    // if (repo_read_index(the_repository) < 0)
    //         die(_("index file corrupt"));
    // ```
    //
    // (builtin/mv.c:251-252.) `repo_read_index()` reaches `do_read_index()` with
    // `must_exist == 0`, which treats an absent file as an *empty* index and
    // returns success (read-cache.c) — only a file that exists and fails to parse
    // is `index file corrupt`. Opening unconditionally made every `git mv` in a
    // repository that has never staged anything die with `index file corrupt: An
    // IO error occurred while opening the index`, where git reports the real
    // problem with the arguments (`bad source`, `not under version control`).
    let mut index = if repo.index_path().exists() {
        match repo.open_index() {
            Ok(i) => i,
            Err(e) => return fatal(format!("index file corrupt: {e}")),
        }
    } else {
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path())
    };

    // 5. Validation phase — build a plan per source against the pristine index.
    //    Without `-k` the first failure aborts before ANY disk/index mutation,
    //    matching git's all-or-nothing behavior. With `-k` a failing source is
    //    silently skipped and the command still succeeds.
    //
    //    `path_in_sparse_checkout()` is the very last gate git applies, after
    //    every other check has passed, so that it can point at `--sparse`.
    let sparsity = if repo
        .config_snapshot()
        .boolean("core.sparseCheckout")
        .unwrap_or(false)
    {
        Some(super::sparse_checkout::load_sparsity(&repo)?)
    } else {
        None
    };
    // git's `ignore_case`, which `core.ignorecase` sets and `git init` records
    // from what the filesystem turned out to be.
    let ignore_case = repo.config_snapshot().boolean("core.ignorecase").unwrap_or(false);
    let mut only_match_skip_worktree: Vec<String> = Vec::new();

    let mut plans: Vec<Plan> = Vec::new();
    // git's `src_for_dst` (builtin/mv.c:479): the destination of every file move
    // already accepted. A directory source or a sparse-skipped entry leaves the
    // checking loop before the insert, so neither is registered here either.
    let mut src_for_dst: std::collections::BTreeSet<String> = Default::default();
    for s in sources {
        match plan_source(
            &index,
            &workdir,
            &prefix,
            s,
            dir_mode,
            &dest_rel,
            force,
            ignore_sparse,
            ignore_case,
            dry_run,
            verbose,
            &src_for_dst,
        ) {
            Ok(Planned::SparseSkip(src)) => only_match_skip_worktree.push(src),
            Ok(Planned::Move(plan)) => {
                // Both ends are checked, and both are named in the report.
                if let Some(sp) = sparsity.as_ref().filter(|_| !plan.is_dir && !ignore_sparse) {
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
                if !plan.is_dir && !plan.index_only {
                    src_for_dst.insert(plan.dst_rel.clone());
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

    // ```c
    // strvec_push(&sources, path);
    // strvec_push(&destinations, prefixed_path);
    //
    // modes[argc + j] = MOVE_VIA_PARENT_DIR | (ce_skip_worktree(ce) ? SPARSE : INDEX);
    // …
    // argc += last - first;
    // ```
    //
    // (builtin/mv.c:394-410.) A directory source appends every index entry under
    // it to the *end* of `sources`, so the checking loop reaches them only once
    // it has walked every path the command line named. Under `--dry-run` that
    // ordering is visible: `git mv -n d d/n dest` announces `d` and `d/n` first
    // and their expansions afterwards, where printing each expansion from inside
    // the directory's own check put `d`'s contents ahead of `d/n`.
    //
    // A submodule source never reaches the expansion (`goto act_on_entry` at
    // `:370`), so its self-remap is not announced.
    if dry_run {
        for plan in plans.iter().filter(|p| p.is_dir && p.submodule.is_none()) {
            for (old, new) in &plan.remaps {
                println!("Checking rename of '{old}' to '{new}'");
            }
        }
    }

    // ```c
    // for (i = 0; i < argc; i++) {
    //         const char *slash_pos;
    //
    //         if (modes[i] & MOVE_VIA_PARENT_DIR)
    //                 continue;
    //
    //         strbuf_reset(&pathbuf);
    //         strbuf_addstr(&pathbuf, sources.v[i]);
    //
    //         slash_pos = strrchr(pathbuf.buf, '/');
    //         while (slash_pos > pathbuf.buf) {
    //                 struct pathmap_entry needle;
    //
    //                 strbuf_setlen(&pathbuf, slash_pos - pathbuf.buf);
    //                 …
    //                 if (hashmap_get_entry(&moved_dirs, &needle, ent, NULL))
    //                         die(_("cannot move both '%s' and its parent directory '%s'"),
    //                             sources.v[i], pathbuf.buf);
    //
    //                 slash_pos = strrchr(pathbuf.buf, '/');
    //         }
    // }
    // ```
    //
    // (builtin/mv.c:499-523.) `moved_dirs` holds every directory source that was
    // expanded into its tracked entries (`hashmap_add()` at `:380`) — a submodule
    // source returns at `:370` before that, and a directory that failed a check
    // never gets there at all. Moving a path *and* an ancestor of it in one
    // command would rename the ancestor first and then chase a source that no
    // longer exists, so git refuses the pair outright.
    //
    // This is a `die()`, not a `bad`: `-k` does not suppress it, and it runs
    // before the sparse report and before the first `rename()`, so the refusal
    // costs nothing. Without it `git mv d d/a dest` moved `d` and then failed
    // with `renaming 'd/a' failed: No such file or directory` — having already
    // moved the directory.
    let moved_dirs: std::collections::BTreeSet<&str> = plans
        .iter()
        .filter(|p| p.is_dir && p.submodule.is_none())
        .map(|p| p.src_rel.as_str())
        .collect();
    if !moved_dirs.is_empty() {
        for plan in &plans {
            let src = plan.src_rel.as_str();
            let mut cut = src.len();
            while let Some(at) = src[..cut].rfind('/') {
                if at == 0 {
                    break;
                }
                cut = at;
                if moved_dirs.contains(&src[..cut]) {
                    return fatal(format!(
                        "cannot move both '{src}' and its parent directory '{}'",
                        &src[..cut]
                    ));
                }
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
    // Destinations a `--sparse` move brought back into the cone, checked out once
    // the index that describes them has been written.
    let mut materialize: Vec<String> = Vec::new();
    for plan in &plans {
        if verbose || dry_run {
            // ```c
            // if (show_only || verbose)
            //         printf(_("Renaming %s to %s\n"), src, dst);
            // ```
            //
            // (builtin/mv.c:543-544.) A directory source stays in the list *and* every
            // index entry under it is appended as an entry of its own
            // (`MOVE_VIA_PARENT_DIR`, builtin/mv.c:394-407), so the report names the
            // directory and then each file that moved with it.
            println!("Renaming {} to {}", plan.src_rel, plan.dst_rel);
            // A submodule source returns at builtin/mv.c:370, before the
            // expansion that appends the directory's index entries, so it has no
            // second line — its `remaps` list holds only the gitlink entry, which
            // *is* the move already announced above. Printing it again gave
            // `git mv -v mod mod2` two identical `Renaming mod to mod2` lines.
            if plan.is_dir && plan.submodule.is_none() {
                for (old, new) in &plan.remaps {
                    println!("Renaming {old} to {new}");
                }
            }
        }
        if !dry_run {
            // A `SPARSE` move has nothing on disk to rename; `act_on_entry` goes
            // straight to the index remap for it (builtin/mv.c:507-515).
            if !plan.index_only {
                if let Err(e) = std::fs::rename(&plan.src_abs, &plan.dst_abs) {
                    return fatal(format!(
                        "renaming '{}' failed: {}",
                        plan.src_rel,
                        super::config::errno_text(&e)
                    ));
                }
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
            // ```c
            // if ((mode & SPARSE) &&
            //     path_in_sparse_checkout(dst, the_repository->index)) {
            //         /* from out-of-cone to in-cone */
            //         dst_ce->ce_flags &= ~CE_SKIP_WORKTREE;
            //         if (checkout_entry(dst_ce, &state, NULL, NULL))
            //                 die(_("cannot checkout %s"), dst_ce->name);
            // }
            // ```
            //
            // (`builtin/mv.c:585-595`, under `ignore_sparse && cone`.) A path moved
            // out of the excluded cone belongs in the worktree again, so the entry
            // loses `skip-worktree` and the file is written out.
            if plan.index_only
                && sparsity.as_ref().is_some_and(|sp| sp.is_cone() && sp.includes(&plan.dst_rel))
            {
                clear_skip_worktree(&mut index, &plan.dst_rel);
                materialize.push(plan.dst_rel.clone());
            }
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

    if !materialize.is_empty() {
        // Re-open so the writer sees the cleared skip bits: an entry that still
        // carries `SKIP_WORKTREE` is one it declines to write.
        let mut subset = repo.open_index()?;
        subset.remove_entries(|_, path, _| !materialize.iter().any(|p| p.as_bytes() == path));
        super::sparse_checkout::checkout_subset(&repo, &mut subset)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// `dst_ce->ce_flags &= ~CE_SKIP_WORKTREE` for the entry at `rel`.
fn clear_skip_worktree(index: &mut gix::index::File, rel: &str) {
    let backing = index.path_backing();
    let at = index.entries().iter().position(|e| {
        e.stage() == Stage::Unconflicted
            && AsRef::<[u8]>::as_ref(e.path_in(&backing)) == rel.as_bytes()
    });
    if let Some(at) = at {
        index.entries_mut()[at].flags.remove(gix::index::entry::Flags::SKIP_WORKTREE);
    }
}

/// Validate a single source against the current index and worktree and return
/// the resulting [`Plan`], or `bail!` with a git-compatible reason.
/// What one `<source>` turned into: a move to make, or a path outside the
/// sparse-checkout definition that `advise_on_updating_sparse_paths()` will name.
enum Planned {
    Move(Plan),
    SparseSkip(String),
}

fn plan_source(
    index: &gix::index::File,
    workdir: &Path,
    prefix: &Path,
    src_arg: &str,
    dir_mode: bool,
    dest_rel: &str,
    force: bool,
    ignore_sparse: bool,
    ignore_case: bool,
    show_only: bool,
    verbose: bool,
    // git's `src_for_dst`: the destinations of the file moves already accepted
    // by this run, which is what makes a second source for one target an error.
    src_for_dst: &std::collections::BTreeSet<String>,
) -> Result<Planned> {
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

    // ```c
    // /* Checking */
    // for (i = 0; i < argc; i++) {
    //         const char *src = sources.v[i], *dst = destinations.v[i];
    //         …
    //         if (show_only)
    //                 printf(_("Checking rename of '%s' to '%s'\n"), src, dst);
    // ```
    //
    // (builtin/mv.c:295-303.) The dry run announces the pair it is about to check
    // *before* checking it, so every `bad` this loop can reach — `bad source`
    // (`:322`), `can not move directory into itself` (`:348`), `destination
    // exists` (`:365`) — has the line above it on stdout. Printing it from the
    // apply phase instead, as this port did, meant a `git mv -n` that dies in the
    // checking loop printed nothing at all.
    if show_only {
        println!("Checking rename of '{src_rel}' to '{dst_rel}'");
    }

    // git reports a same-path move (and a move into a subpath of itself) with
    // this exact phrasing regardless of the item being a file.
    if src_rel == dst_rel || dst_rel.starts_with(&format!("{src_rel}/")) {
        crate::git_fatal!("can not move directory into itself, source={src_rel}, destination={dst_rel}");
    }

    // ```c
    // if (lstat(src, &st) < 0) {
    //         pos = index_name_pos(the_repository->index, src, length);
    //         if (pos < 0) { … bad = _("bad source"); goto act_on_entry; }
    //         ce = the_repository->index->cache[pos];
    //         if (!ce_skip_worktree(ce)) { bad = _("bad source"); goto act_on_entry; }
    //         if (!ignore_sparse) {
    //                 string_list_append(&only_match_skip_worktree, src);
    //                 goto act_on_entry;
    //         }
    //         /* Check if dst exists in index */
    //         if (index_name_pos(the_repository->index, dst, strlen(dst)) < 0) {
    //                 modes[i] |= SPARSE;
    //                 goto act_on_entry;
    //         }
    //         if (!force) { bad = _("destination exists"); goto act_on_entry; }
    //         modes[i] |= SPARSE;
    //         goto act_on_entry;
    // }
    // ```
    //
    // (`builtin/mv.c:306-345`.) A source that is not on disk is only `bad source`
    // when the index does not explain its absence. An entry carrying
    // `skip-worktree` explains it: the path is outside the sparse-checkout
    // definition, so without `--sparse` it is collected for the advice block, and
    // with it the move happens in the index alone.
    // git `lstat()`s `src` as it stands, and `prefix_path()` normalises both `''`
    // and `.` to the empty string — `lstat("")` is ENOENT, so those land in the
    // not-on-disk arm and come back as `bad source`. Joining the empty name to
    // the worktree root instead pointed the stat at the root directory, which
    // exists, so both reported `source directory is empty` from the branch below.
    let meta = match (src_rel.is_empty(), std::fs::symlink_metadata(&src_abs)) {
        (false, Ok(meta)) => meta,
        _ => {
            if !skip_worktree_entry(index, &src_rel) {
                return Err(anyhow!("bad source, source={src_rel}, destination={dst_rel}"));
            }
            if !ignore_sparse {
                return Ok(Planned::SparseSkip(src_rel));
            }
            if is_tracked(index, &dst_rel) && !force {
                crate::git_fatal!("destination exists, source={src_rel}, destination={dst_rel}");
            }
            return Ok(Planned::Move(Plan {
                src_abs,
                dst_abs,
                src_rel: src_rel.clone(),
                dst_rel: dst_rel.clone(),
                remaps: vec![(src_rel, dst_rel)],
                submodule: None,
                is_dir: false,
                index_only: true,
            }));
        }
    };

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
            return Ok(Planned::Move(Plan {
                src_abs,
                dst_abs,
                src_rel: src_rel.clone(),
                dst_rel: dst_rel.clone(),
                remaps: vec![(src_rel, dst_rel)],
                submodule,
                is_dir: true,
                index_only: false,
            }));
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
        // ```c
        // } else if (index_range_of_same_dir(src, length,
        //                                    &first, &last) < 1) {
        //         bad = _("source directory is empty");
        //         goto act_on_entry;
        // }
        // ```
        //
        // (builtin/mv.c:371-375.) A directory that exists on disk but holds no
        // index entry never reaches the `not under version control` test at
        // `:413` — that one only sees non-directories. git names the actual
        // problem: the directory has nothing tracked under it to move.
        if remaps.is_empty() {
            crate::git_fatal!("source directory is empty, source={src_rel}, destination={dst_rel}");
        }
        // A directory destination that already exists on disk can't be merged
        // here; git refuses it too (only file destinations honor -f).
        if dst_abs.exists() {
            crate::git_fatal!("destination already exists, source={src_rel}, destination={dst_rel}");
        }
        remaps
    } else {
        // ```c
        // if (!(ce = index_file_exists(the_repository->index, src, length, 0))) {
        //         bad = _("not under version control");
        //         goto act_on_entry;
        // }
        // if (ce_stage(ce)) {
        //         bad = _("conflicted");
        //         goto act_on_entry;
        // }
        // ```
        //
        // (`cmd_mv()`, builtin/mv.c:413-420.) The lookup finds the entry whatever stage it
        // sits at, so an unmerged path is `conflicted` and not `not under version control`
        // — the difference between "resolve this first" and "this is not a tracked file".
        if !is_tracked(index, &src_rel) {
            let message = match is_unmerged(index, &src_rel) {
                true => "conflicted",
                false => "not under version control",
            };
            crate::git_fatal!("{message}, source={src_rel}, destination={dst_rel}");
        }
        // ```c
        // if (lstat(dst, &st) == 0 &&
        //     (!ignore_case || strcasecmp(src, dst))) {
        //         bad = _("destination exists");
        // ```
        //
        // (`builtin/mv.c:421-423`.) On a case-insensitive filesystem the
        // destination of a case-only rename is the source itself, which is why the
        // check stands down for it — without that, `git mv README.md readme.md`
        // reports the file it is about to move as being in its own way.
        let clobbers = dst_abs.exists() && !(ignore_case && src_rel.eq_ignore_ascii_case(&dst_rel));
        if !force && (clobbers || is_tracked(index, &dst_rel)) {
            crate::git_fatal!("destination exists, source={src_rel}, destination={dst_rel}");
        }
        // ```c
        // if (force) {
        //         /*
        //          * only files can overwrite each other:
        //          * check both source and destination
        //          */
        //         if (S_ISREG(st.st_mode) || S_ISLNK(st.st_mode)) {
        //                 if (verbose)
        //                         warning(_("overwriting '%s'"), dst);
        //                 bad = NULL;
        //         } else
        //                 bad = _("Cannot overwrite");
        // }
        // ```
        //
        // (builtin/mv.c:424-435.) `-f` only *silences* the refusal; with `-v` git
        // still says on stderr which file it is about to destroy. The warning is
        // tied to the destination existing on disk, not to `-f` alone, and to the
        // same `clobbers` test the refusal uses — a case-only rename on a
        // case-insensitive filesystem is its own destination and overwrites
        // nothing.
        if force && verbose && clobbers {
            eprintln!("warning: overwriting '{dst_rel}'");
        }
        // ```c
        // if (string_list_has_string(&src_for_dst, dst)) {
        //         bad = _("multiple sources for the same target");
        //         goto act_on_entry;
        // }
        // ```
        //
        // (builtin/mv.c:438-441.) `src_for_dst` collects the destination of every
        // *file* move this run has already accepted — a directory source and a
        // sparse-only entry both reach `act_on_entry` before the
        // `string_list_insert()` at `:479`, so neither registers. Without this
        // test the second `git mv d/a d/a dest` renamed the file, then tried to
        // rename it again from a path that no longer existed and died with
        // `renaming 'd/a' failed: No such file or directory` — after the first
        // rename had already landed.
        if src_for_dst.contains(&dst_rel) {
            crate::git_fatal!(
                "multiple sources for the same target, source={src_rel}, destination={dst_rel}"
            );
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
    Ok(Planned::Move(Plan {
        src_abs,
        dst_abs,
        src_rel,
        dst_rel,
        remaps,
        submodule,
        is_dir,
        index_only: false,
    }))
}

/// Whether `rel` is in the index carrying `skip-worktree`, which is git's
/// explanation for a tracked path that is not on disk.
fn skip_worktree_entry(index: &gix::index::File, rel: &str) -> bool {
    let backing = index.path_backing();
    index.entries().iter().any(|e| {
        e.stage() == Stage::Unconflicted
            && e.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE)
            && AsRef::<[u8]>::as_ref(e.path_in(backing)) == rel.as_bytes()
    })
}

/// Whether a stage-0 index entry exists at exactly `rel`.
fn is_tracked(index: &gix::index::File, rel: &str) -> bool {
    tracked_mode(index, rel).is_some()
}

/// Whether `rel` is in the index at a conflicted stage.
fn is_unmerged(index: &gix::index::File, rel: &str) -> bool {
    let backing = index.path_backing();
    index.entries().iter().any(|e| {
        e.stage() != Stage::Unconflicted
            && AsRef::<[u8]>::as_ref(e.path_in(backing)) == rel.as_bytes()
    })
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

/// `prefix_path()` (setup.c:149-160) — what `builtin/mv.c` runs every operand
/// through before it looks at the index.
///
/// Relative operands are joined to the worktree `prefix` (the repo-relative
/// CWD); absolute ones are cut down to the part inside the worktree
/// (`abspath_part_inside_repo()`, setup.c:50-106). `.` and `..` are folded by
/// `normalize_path_copy_len()` and a `..` that climbs past the top is the
/// `'%s' is outside repository at '%s'` die.
///
/// Two things were private here and wrong in the same way three times over: the
/// die named `workdir` as gix hands it back, which at the top of a worktree is
/// the relative `.` rather than git's absolute, symlink-resolved path; and an
/// operand that normalised to nothing (`''`, `.`) was refused with an invented
/// `invalid path: <arg>` where git returns the empty string and lets `mv`'s own
/// `bad source, source=, destination=<dst>` report it (builtin/mv.c:306-345).
fn normalize_rel(workdir: &Path, prefix: &Path, arg: &str) -> Result<String> {
    let prefix = prefix.to_string_lossy().replace('\\', "/");
    let joined = if Path::new(arg).is_absolute() {
        BString::from(arg)
    } else if prefix.is_empty() {
        BString::from(arg)
    } else {
        let mut joined = BString::from(prefix.trim_end_matches('/').as_bytes().to_vec());
        joined.push(b'/');
        joined.extend_from_slice(arg.as_bytes());
        joined
    };
    // `absolute_path(repo_get_work_tree())`: the worktree as `setup_git_directory()`
    // left it, which is `xgetcwd()`'s already-symlink-resolved spelling.
    let real_wd = workdir.canonicalize().unwrap_or_else(|_| workdir.to_path_buf());
    let outside =
        || crate::fatal::die(format!("'{arg}' is outside repository at '{}'", real_wd.display()));
    let normalized = match crate::pathspec::normalize_path(joined.as_bstr()) {
        Some(normalized) => normalized,
        None => crate::git_fatal!("'{arg}' is outside repository at '{}'", real_wd.display()),
    };
    if !Path::new(arg).is_absolute() {
        return Ok(normalized.to_string());
    }
    // An absolute operand is measured against the worktree's realpath, so a
    // worktree reached through a symlink (macOS `/tmp` -> `/private/tmp`) is not
    // called an outside one.
    let real = canonicalize_lenient(Path::new(&normalized.to_string()));
    match real.strip_prefix(&real_wd) {
        Ok(rel) => Ok(rel.to_string_lossy().replace('\\', "/")),
        Err(_) => Err(outside().into()),
    }
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
