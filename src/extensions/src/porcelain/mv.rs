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
//! does. A directory that exists only as skip-worktree entries
//! (`empty_dir_has_sparse_contents()`) is `SKIP_WORKTREE_DIR` as a source or a
//! destination; nothing is renamed on disk when either side is outside the
//! definition, and under `--sparse` an entry leaving a cone-mode definition
//! becomes skip-worktree, its file unlinked or, when dirty, kept and named.

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

/// `git mv -h` help, printed verbatim to stdout (git exits 0 after it).
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
    /// git's `SKIP_WORKTREE_DIR` mode bit: a directory source that is not on
    /// disk and holds only skip-worktree entries, whose entries move in the
    /// index alone (builtin/mv.c:311-318).
    skip_worktree_dir: bool,
    /// git's `SPARSE` bit per entry of a directory source's expansion,
    /// parallel to `remaps`: set for an entry `--sparse` moves in the index
    /// alone because it is skip-worktree and not on disk (builtin/mv.c:340-351).
    sparse_remaps: Vec<bool>,
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
            return Ok(super::show_usage(HELP));
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
                // git prints the full help to stdout and exits 0, before any
                // repository lookup — so `-h` works outside a work tree too.
                // (`--help` is deliberately NOT handled here: stock git execs the
                //  man pager for it, a foreign op this server cannot reproduce.)
                return Ok(super::show_usage(HELP));
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
                            return Ok(super::show_usage(HELP));
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
    // `cmd_mv()` runs `git_default_config` first (the dispatcher's gate) and
    // `prepare_repo_settings()` only at `repo_read_index()`, after its option parse and
    // operand count, so a bad settings value never pre-empts a usage error. The
    // dispatcher leaves `mv` to this order (`parse_before_settings`).
    crate::repo_settings::RepoSettings::load(&repo).map_err(crate::fatal::die)?;
    let workdir = match repo.workdir() {
        Some(w) => w.to_owned(),
        None => return fatal("this operation must be run in a work tree"),
    };
    let prefix = match repo.prefix() {
        Ok(p) => p.map(Path::to_path_buf).unwrap_or_default(),
        Err(e) => return fatal(format!("cannot resolve worktree prefix: {e}")),
    };

    // 3. Split operands: everything but the last is a source; the last is the
    //    destination.
    let dest_arg = *positional.last().expect("checked len >= 2");
    let sources = &positional[..positional.len() - 1];

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

    // ```c
    // internal_prefix_pathspec(&sources, prefix, argv, argc, 0);
    // …
    // flags = KEEP_TRAILING_SLASH;
    // if (argc == 1 && is_directory(argv[0]) && !is_directory(argv[1]))
    //         flags = 0;
    // internal_prefix_pathspec(&dest_paths, prefix, argv + argc, 1, flags);
    // dst_w_slash = add_slash(dest_paths.v[0]);
    // …
    // if (dest_paths.v[0][0] == '\0')
    //         /* special case: "." was normalized to "" */
    //         internal_prefix_pathspec(&destinations, dest_paths.v[0], argv, argc, DUP_BASENAME);
    // else if (!lstat(dest_paths.v[0], &st) && S_ISDIR(st.st_mode)) {
    //         internal_prefix_pathspec(&destinations, dst_w_slash, argv, argc, DUP_BASENAME);
    // } else if (…SKIP_WORKTREE_DIR…) {
    // } else if (argc != 1) {
    //         die(_("destination '%s' is not a directory"), dest_paths.v[0]);
    // } else {
    //         strvec_pushv(&destinations, dest_paths.v);
    // ```
    //
    // (builtin/mv.c:254-281.) A source loses its trailing slashes; the
    // destination keeps its own so that `git mv file no-such-dir/` can be
    // refused per source (`:442`), except for `git mv dir no-such-dir/`, which
    // is a plain rename. An existing directory (`lstat()`, so a symlink to one
    // is a file) receives each source's basename under `add_slash()`, which is
    // what keeps `git mv a dir/` from recording `dir//a`. `is_directory()`
    // `stat()`s the raw operands from the top of the worktree, where
    // `setup_git_directory()` left the process.
    let src_rels: Vec<String> = match sources
        .iter()
        .map(|s| normalize_rel(&workdir, &prefix, trim_dir_seps(s)))
        .collect()
    {
        Ok(v) => v,
        Err(e) => return fatal(e),
    };
    let keep_trailing_slash = !(sources.len() == 1
        && workdir.join(sources[0]).is_dir()
        && !workdir.join(dest_arg).is_dir());
    let dest_rel = match normalize_rel(
        &workdir,
        &prefix,
        if keep_trailing_slash { dest_arg } else { trim_dir_seps(dest_arg) },
    ) {
        Ok(r) => r,
        Err(e) => return fatal(e),
    };
    let dst_w_slash = add_slash(&dest_rel);
    let into_dir = |under: &str| -> Result<Vec<String>> {
        sources
            .iter()
            .map(|s| normalize_rel(&workdir, Path::new(under), dup_basename(s)))
            .collect()
    };
    // `path_in_sparse_checkout()` is the very last gate git applies, after
    // every other check has passed, so that it can point at `--sparse`.
    let sparsity = if repo
        .config_snapshot()
        .boolean("core.sparseCheckout")
        .unwrap_or(false)
    {
        super::sparse_checkout::load_sparsity_for_path_checks(&repo)?
    } else {
        None
    };
    // git's `dst_mode & (SKIP_WORKTREE_DIR | SPARSE)` (builtin/mv.c:276-299), the
    // only way the checking and moving loops read `dst_mode`:
    // `SKIP_WORKTREE_DIR` for a destination that is not on disk, lies outside
    // the sparse-checkout definition and holds skip-worktree entries in the
    // index (`empty_dir_has_sparse_contents()`), which then receives each
    // source's basename as an existing directory would; `SPARSE` for a single
    // destination that is a file outside a *cone-mode* definition
    // (`path_in_cone_mode_sparse_checkout()` answers 1 for a non-cone or
    // non-sparse worktree). `needs_worktree_rename()` is false under either.
    let mut dst_sparse = false;
    // The `struct stat st` of `cmd_mv()`: the checking loop's `lstat(src, &st)`
    // leaves it alone when it fails, so a source that is a directory existing
    // only as skip-worktree entries reaches `dir_check` with whatever the last
    // successful `lstat()` into it saw (builtin/mv.c:276, :318-327, :362).
    // Only its `S_ISDIR()` is ever read there.
    let mut st_is_dir = false;
    let dest_st = (!dest_rel.is_empty())
        .then(|| std::fs::symlink_metadata(workdir.join(&dest_rel)).ok())
        .flatten();
    if let Some(m) = &dest_st {
        st_is_dir = m.is_dir();
    }
    let dst_rels = if dest_rel.is_empty() {
        into_dir(&dest_rel)
    } else if dest_st.as_ref().is_some_and(|m| m.is_dir()) {
        into_dir(&dst_w_slash)
    } else if !crate::sparse_index::path_in_sparse_checkout(&repo, dst_w_slash.as_bytes())
        && empty_dir_has_sparse_contents(&index, &dst_w_slash)
    {
        dst_sparse = true;
        into_dir(&dst_w_slash)
    } else if sources.len() != 1 {
        return fatal(format!("destination '{dest_rel}' is not a directory"));
    } else {
        dst_sparse = !crate::sparse_index::path_in_cone_mode_sparse_checkout(&repo, dest_rel.as_bytes());
        Ok(vec![dest_rel.clone()])
    };
    let dst_rels = match dst_rels {
        Ok(v) => v,
        Err(e) => return fatal(e),
    };

    // 5. Validation phase — build a plan per source against the pristine index.
    //    Without `-k` the first failure aborts before ANY disk/index mutation,
    //    matching git's all-or-nothing behavior. With `-k` a failing source is
    //    silently skipped and the command still succeeds.
    // git's `ignore_case`, which `core.ignorecase` sets and `git init` records
    // from what the filesystem turned out to be.
    let ignore_case = repo.config_snapshot().boolean("core.ignorecase").unwrap_or(false);
    let mut only_match_skip_worktree: Vec<String> = Vec::new();

    let mut plans: Vec<Plan> = Vec::new();
    // git's `src_for_dst` (builtin/mv.c:479): the destination of every file move
    // already accepted. A directory source or a sparse-skipped entry leaves the
    // checking loop before the insert, so neither is registered here either.
    let mut src_for_dst: std::collections::BTreeSet<String> = Default::default();
    for (src_rel, dst_rel) in src_rels.into_iter().zip(dst_rels) {
        match plan_source(
            &repo,
            &index,
            &workdir,
            src_rel,
            dst_rel,
            force,
            ignore_sparse,
            ignore_case,
            dry_run,
            verbose,
            &src_for_dst,
            dst_sparse,
            &mut st_is_dir,
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
    //
    // Each expansion is then checked like any other source: `lstat()` failing on a tracked file
    // the work tree lacks is `bad source` (`:322`) unless `skip-worktree` explains it, and `-k`
    // drops just that entry.
    //
    // An expanded entry that is not on disk is checked by the `lstat()`-failed
    // arm (builtin/mv.c:332-351): without `--sparse` a skip-worktree entry is
    // named in the advice block and stays in the list, with it an entry already
    // at the destination is `destination exists` unless `-f`. One on disk meets
    // the sparse gate at the end of the loop (builtin/mv.c:507-520), which
    // names a source or destination outside the definition and drops the entry.
    // The entry's mode is `SPARSE` when it is skip-worktree, `INDEX` otherwise
    // (:401).
    for plan in plans.iter_mut().filter(|p| p.is_dir && p.submodule.is_none()) {
        let mut kept = Vec::with_capacity(plan.remaps.len());
        let mut sparse_remaps = Vec::with_capacity(plan.remaps.len());
        for (old, new) in std::mem::take(&mut plan.remaps) {
            if dry_run {
                println!("Checking rename of '{old}' to '{new}'");
            }
            let sparse = skip_worktree_entry(&index, &old);
            if std::fs::symlink_metadata(workdir.join(&old)).is_err() {
                if !sparse {
                    if skip {
                        continue;
                    }
                    return fatal(format!("bad source, source={old}, destination={new}"));
                }
                if !ignore_sparse {
                    only_match_skip_worktree.push(old.clone());
                } else if is_tracked(&index, &new) && !force {
                    if skip {
                        continue;
                    }
                    return fatal(format!("destination exists, source={old}, destination={new}"));
                }
            } else if !ignore_sparse {
                let mut skip_sparse = false;
                for end in [&old, &new] {
                    if !crate::sparse_index::path_in_sparse_checkout(&repo, end.as_bytes()) {
                        only_match_skip_worktree.push(end.clone());
                        skip_sparse = true;
                    }
                }
                if skip_sparse {
                    continue;
                }
            }
            kept.push((old, new));
            sparse_remaps.push(sparse);
        }
        plan.remaps = kept;
        plan.sparse_remaps = sparse_remaps;
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
    //
    //    git walks `sources` in order, and a directory source's expansion was
    //    appended to its *end* (`MOVE_VIA_PARENT_DIR`, builtin/mv.c:394-410), so
    //    every named source is announced and renamed first and the entries that
    //    moved with a directory come after all of them, each remapped in the
    //    index alone (its mode is `INDEX`, so `needs_worktree_rename()` is false).
    let mut modified = false;
    // `rename_index_entry_at()` refreshes each moved entry against the file now at
    // its new name; see [`Refresh`].
    let refresh = Refresh {
        workdir: workdir.clone(),
        stat_options: repo.stat_options().unwrap_or_default(),
        trust_executable_bit: repo.config_snapshot().boolean("core.fileMode").unwrap_or(true),
        big_file_refusal: crate::config::big_file_threshold_refusal(&repo),
    };
    let mut gitmodules_modified = false;
    let mut sparse_moves = SparseMoves {
        repo: &repo,
        workdir: &workdir,
        refresh: &refresh,
        // `ignore_sparse && cfg->apply_sparse_checkout && cfg->core_sparse_checkout_cone`
        // (builtin/mv.c:617-619).
        cone: ignore_sparse && sparsity.as_ref().is_some_and(|sp| sp.is_cone()),
        dst_sparse,
        filter: None,
        materialize: Vec::new(),
        dirty_paths: Vec::new(),
    };
    for plan in &plans {
        // ```c
        // if (show_only || verbose)
        //         printf(_("Renaming %s to %s\n"), src, dst);
        // if (show_only)
        //         continue;
        // if (needs_worktree_rename(mode, dst_mode) &&
        //     rename(src, dst) < 0) {
        //         if (ignore_errors)
        //                 continue;
        //         die_errno(_("renaming '%s' to '%s' failed"), src, dst);
        // }
        // ```
        //
        // (builtin/mv.c:585-594.) 2.56 names the destination as well as the
        // source. Under `-k` the failed entry is dropped — but only that entry: a
        // directory's own entry has no index work anyway, and the entries
        // expanded from it are separate entries that still reach
        // `rename_index_entry_at()` below.
        if verbose || dry_run {
            println!("Renaming {} to {}", plan.src_rel, plan.dst_rel);
        }
        if dry_run {
            continue;
        }
        // `needs_worktree_rename(mode, dst_mode)` (builtin/mv.c:41-45): a `SPARSE`
        // or `SKIP_WORKTREE_DIR` source has nothing on disk to rename, and a
        // destination outside the sparse-checkout definition gets nothing on disk.
        if !plan.index_only && !plan.skip_worktree_dir && !dst_sparse {
            if let Err(e) = std::fs::rename(&plan.src_abs, &plan.dst_abs) {
                if skip {
                    continue;
                }
                return fatal(format!(
                    "renaming '{}' to '{}' failed: {}",
                    plan.src_rel,
                    plan.dst_rel,
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
        // `if (mode & (WORKING_DIRECTORY | SKIP_WORKTREE_DIR)) continue;`
        // (builtin/mv.c:604-605): a directory's entries are remapped in the
        // second pass. A submodule source returns at builtin/mv.c:370, before
        // the expansion, so its `remaps` is the gitlink entry itself.
        if plan.is_dir && plan.submodule.is_none() {
            continue;
        }
        if let Err(message) = sparse_moves.move_entry(&mut index, &plan.src_rel, &plan.dst_rel, plan.index_only) {
            return fatal(message);
        }
        modified = true;
    }
    for plan in plans.iter().filter(|p| p.is_dir && p.submodule.is_none()) {
        for (at, (old, new)) in plan.remaps.iter().enumerate() {
            if verbose || dry_run {
                println!("Renaming {old} to {new}");
            }
            if !dry_run {
                let sparse = plan.sparse_remaps.get(at).copied().unwrap_or(false);
                if let Err(message) = sparse_moves.move_entry(&mut index, old, new, sparse) {
                    return fatal(message);
                }
                modified = true;
            }
        }
    }

    // `remove_empty_src_dirs(src_dir.v, src_dir.nr)` (builtin/mv.c:663, :170-188):
    // a directory source that has no index entry left under it is removed with
    // whatever it still holds. A directory renamed on disk is gone already; one
    // whose entries went out of the sparse-checkout definition without a
    // `rename()` is what this clears. A dry run moved no entry.
    if !dry_run {
        let backing = index.path_backing();
        for plan in plans.iter().filter(|p| p.is_dir && p.submodule.is_none()) {
            let under = add_slash(&plan.src_rel);
            if !index.entries().iter().any(|e| e.path_in(backing).starts_with(under.as_bytes())) {
                let _ = std::fs::remove_dir_all(workdir.join(&plan.src_rel));
            }
        }
    }

    // `advise_on_moving_dirty_path(&dirty_paths)` (advice.c:298-315).
    if !sparse_moves.dirty_paths.is_empty() {
        eprintln!(
            "The following paths have been moved outside the\n\
             sparse-checkout definition but are not sparse due to local\n\
             modifications."
        );
        for path in &sparse_moves.dirty_paths {
            eprintln!("{path}");
        }
        crate::advice::Advice::UpdateSparsePath.advise_in(
            &repo,
            "To correct the sparsity of these paths, do the following:\n\
             * Use \"git add --sparse <paths>\" to update the index\n\
             * Use \"git sparse-checkout reapply\" to apply the sparsity rules",
        );
    }
    let materialize = std::mem::take(&mut sparse_moves.materialize);

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

/// The index half of `cmd_mv()`'s moving loop for one entry (builtin/mv.c:607-660),
/// with what it carries across entries.
struct SparseMoves<'a> {
    repo: &'a gix::Repository,
    workdir: &'a Path,
    refresh: &'a Refresh,
    /// `ignore_sparse && cfg->apply_sparse_checkout && cfg->core_sparse_checkout_cone`.
    cone: bool,
    /// `dst_mode & (SKIP_WORKTREE_DIR | SPARSE)`.
    dst_sparse: bool,
    /// The `convert_to_git()` pipeline `ie_modified()` hashes through, built on first use.
    filter: Option<super::convert_to_git::WorktreeFilter>,
    /// Destinations a `--sparse` move brought back into the cone, checked out once
    /// the index that describes them has been written.
    materialize: Vec<String>,
    /// `dirty_paths`: destinations left on disk because their source was dirty.
    dirty_paths: Vec<String>,
}

impl SparseMoves<'_> {
    /// Move the entry at `src` to `dst`; `sparse` is the entry's `SPARSE` mode bit.
    ///
    /// ```c
    /// if (!(mode & SPARSE) && !lstat(src, &st))
    ///         sparse_and_dirty = ie_modified(the_repository->index,
    ///                                        the_repository->index->cache[pos],
    ///                                        &st, 0);
    /// rename_index_entry_at(the_repository->index, pos, dst);
    /// ```
    ///
    /// then, under `--sparse` in a cone-mode sparse checkout, an entry coming into
    /// the definition loses skip-worktree and is checked out, and one leaving it
    /// (`dst_mode` out of the cone, a source that was on disk, a destination
    /// outside) becomes skip-worktree with its source unlinked when clean, or keeps
    /// its file, renamed into place with its leading directories, when dirty
    /// (builtin/mv.c:607-660). The "out-to-out" move is the C's NEEDSWORK and does
    /// nothing more.
    fn move_entry(&mut self, index: &mut gix::index::File, src: &str, dst: &str, sparse: bool) -> Result<(), String> {
        let src_abs = self.workdir.join(src);
        let mut sparse_and_dirty = false;
        if !sparse {
            if let Ok(meta) = gix::index::fs::Metadata::from_path_no_follow(&src_abs) {
                // A linear lookup: the moves before this one appended entries out of order.
                if let Some(entry) = stage0_position(index, src).map(|at| &index.entries()[at]) {
                    let st = super::rm::Lstat { abs: src_abs.clone(), meta };
                    sparse_and_dirty =
                        super::rm::ie_modified(self.repo, index, entry, &BString::from(src), &st, &mut self.filter)
                            .map_err(|e| format!("{e:#}"))?;
                }
            }
        }
        apply_remaps(index, &[(src.to_owned(), dst.to_owned())], self.refresh)?;
        if !self.cone {
            return Ok(());
        }
        if sparse && crate::sparse_index::path_in_sparse_checkout(self.repo, dst.as_bytes()) {
            // from out-of-cone to in-cone
            clear_skip_worktree(index, dst);
            self.materialize.push(dst.to_owned());
        } else if self.dst_sparse && !sparse && !crate::sparse_index::path_in_sparse_checkout(self.repo, dst.as_bytes()) {
            // from in-cone to out-of-cone
            if !sparse_and_dirty {
                set_skip_worktree(index, dst);
                // `unlink_or_warn(src)` (wrapper.c:735-746): a missing file is no failure.
                if let Err(e) = std::fs::remove_file(&src_abs) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        eprintln!("warning: unable to unlink '{src}': {}", crate::external::strerror(&e));
                    }
                }
            } else {
                self.dirty_paths.push(dst.to_owned());
                let dst_abs = self.workdir.join(dst);
                if let Some(parent) = dst_abs.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::rename(&src_abs, &dst_abs);
            }
        }
        Ok(())
    }
}

/// `dst_ce->ce_flags |= CE_SKIP_WORKTREE` for the entry at `rel`.
fn set_skip_worktree(index: &mut gix::index::File, rel: &str) {
    if let Some(at) = stage0_position(index, rel) {
        index.entries_mut()[at].flags.insert(Flags::SKIP_WORKTREE | Flags::EXTENDED);
    }
}

/// `dst_ce->ce_flags &= ~CE_SKIP_WORKTREE` for the entry at `rel`.
fn clear_skip_worktree(index: &mut gix::index::File, rel: &str) {
    if let Some(at) = stage0_position(index, rel) {
        index.entries_mut()[at].flags.remove(gix::index::entry::Flags::SKIP_WORKTREE);
    }
}

/// The position of the stage-0 entry at `rel`, found by a linear scan because the
/// moves already made appended their entries out of order.
fn stage0_position(index: &gix::index::File, rel: &str) -> Option<usize> {
    let backing = index.path_backing();
    index.entries().iter().position(|e| {
        e.stage() == Stage::Unconflicted && AsRef::<[u8]>::as_ref(e.path_in(backing)) == rel.as_bytes()
    })
}

/// `empty_dir_has_sparse_contents()` (builtin/mv.c:128-153): `name` with a slash
/// added is not itself in the index, and the entry it would sort before lies
/// under it and is skip-worktree. Only that first entry is asked.
fn empty_dir_has_sparse_contents(index: &gix::index::File, name: &str) -> bool {
    let with_slash = add_slash(name);
    let backing = index.path_backing();
    let entries = index.entries();
    let key = with_slash.as_bytes();
    // `index_name_pos()`: the entries are ordered by path, then stage.
    let pos = entries.partition_point(|e| AsRef::<[u8]>::as_ref(e.path_in(backing)) < key);
    match entries.get(pos) {
        Some(e) if AsRef::<[u8]>::as_ref(e.path_in(backing)) == key => false,
        Some(e) => e.path_in(backing).starts_with(key) && e.flags.contains(Flags::SKIP_WORKTREE),
        None => false,
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
    repo: &gix::Repository,
    index: &gix::index::File,
    workdir: &Path,
    src_rel: String,
    dst_rel: String,
    force: bool,
    ignore_sparse: bool,
    ignore_case: bool,
    show_only: bool,
    verbose: bool,
    // git's `src_for_dst`: the destinations of the file moves already accepted
    // by this run, which is what makes a second source for one target an error.
    src_for_dst: &std::collections::BTreeSet<String>,
    // git's `dst_mode & (SKIP_WORKTREE_DIR | SPARSE)`, see the call site.
    dst_sparse: bool,
    // `S_ISDIR(st.st_mode)` of `cmd_mv()`'s shared `struct stat`, see the call site.
    st_is_dir: &mut bool,
) -> Result<Planned> {
    let src_abs = workdir.join(&src_rel);
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
    //
    // A source the index does not hold either may still be a directory whose
    // every entry is skip-worktree, which is why it is absent:
    //
    // ```c
    // if (!path_in_sparse_checkout(src_w_slash, the_repository->index) &&
    //     empty_dir_has_sparse_contents(src)) {
    //         modes[i] |= SKIP_WORKTREE_DIR;
    //         goto dir_check;
    // }
    // ```
    //
    // (builtin/mv.c:311-318.) `dir_check` then tests the `st` no `lstat()` of this
    // source filled in, see [`plan_source`]'s caller.
    let meta = match (src_rel.is_empty(), std::fs::symlink_metadata(&src_abs)) {
        (false, Ok(meta)) => {
            *st_is_dir = meta.is_dir();
            Some(meta)
        }
        _ if !is_tracked(index, &src_rel) => {
            if crate::sparse_index::path_in_sparse_checkout(repo, add_slash(&src_rel).as_bytes())
                || !empty_dir_has_sparse_contents(index, &src_rel)
            {
                return Err(anyhow!("bad source, source={src_rel}, destination={dst_rel}"));
            }
            None
        }
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
                skip_worktree_dir: false,
                sparse_remaps: Vec::new(),
            }));
        }
    };

    // `dir_check:` reads `S_ISDIR(st.st_mode)`; a `SKIP_WORKTREE_DIR` source got
    // there with no `lstat()` of its own.
    let skip_worktree_dir = meta.is_none();
    let is_dir = meta.as_ref().map_or(*st_is_dir, |m| m.is_dir());
    let mut submodule = None;
    let remaps: Vec<(String, String)> = if is_dir {
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
                skip_worktree_dir: false,
                sparse_remaps: Vec::new(),
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
                let new = format!("{}{}", add_slash(&dst_rel), &old[src_rel.len() + 1..]);
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
        // here; git refuses it too (only file destinations honor -f). A
        // `SKIP_WORKTREE_DIR` source jumped past this test to `dir_check`
        // (builtin/mv.c:355-359).
        if !skip_worktree_dir && dst_abs.exists() {
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
        //
        // `index_file_exists()` (name-hash.c:735-750) walks the name-hash bucket,
        // where `hashmap_add()` put each entry at the head (hashmap.c:232-242),
        // so of several entries under one name it finds the one added last, the
        // highest stage: a stage-0 entry that sits beside an unmerged one is still
        // `conflicted`. (A `rehash()` of an index past the table's growth point
        // reverses each chain; that order is not modelled.)
        if is_unmerged(index, &src_rel) {
            crate::git_fatal!("conflicted, source={src_rel}, destination={dst_rel}");
        }
        if !is_tracked(index, &src_rel) {
            crate::git_fatal!("not under version control, source={src_rel}, destination={dst_rel}");
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
        let dst_st = std::fs::symlink_metadata(&dst_abs).ok();
        if let Some(m) = &dst_st {
            *st_is_dir = m.is_dir();
        }
        let clobbers = dst_st.is_some() && !(ignore_case && src_rel.eq_ignore_ascii_case(&dst_rel));
        // Only the file system decides: a destination that is in the index but not on disk (deleted,
        // or outside the sparse-checkout definition) is no obstacle here.
        if !force && clobbers {
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
        // ```c
        // if (is_dir_sep(dst[strlen(dst) - 1])) {
        //         bad = _("destination directory does not exist");
        //         goto act_on_entry;
        // }
        // ```
        //
        // (builtin/mv.c:442-445.) Only a destination that `KEEP_TRAILING_SLASH`
        // preserved and that is not an existing directory gets here; like any
        // `bad`, `-k` skips the source.
        if dst_rel.ends_with('/') {
            return Err(anyhow!(
                "destination directory does not exist, source={src_rel}, destination={dst_rel}"
            ));
        }
        // ```c
        // if (has_symlink_leading_path(dst, strlen(dst))) {
        //         bad = _("destination is beyond a symbolic link");
        //         goto act_on_entry;
        // }
        // ```
        //
        // (builtin/mv.c:453-456, new in 2.56.) A destination reached through a
        // symlinked directory — `git mv README.md lnk/` with `lnk` a symlink to
        // `src` — would put the file in the directory the link names while the
        // index recorded `lnk/README.md`.
        if super::check_ignore::has_symlink_leading_path(workdir, BStr::new(dst_rel.as_bytes())) {
            return Err(anyhow!(
                "destination is beyond a symbolic link, source={src_rel}, destination={dst_rel}"
            ));
        }
        // ```c
        // if (needs_worktree_rename(modes[i], dst_mode)) {
        //         const char *slash_ = strrchr(dst, '/');
        //
        //         if (slash_) {
        //                 …
        //                 *slash = '\0';
        //                 if (lstat(dst_dir, &dir_st) < 0) {
        //                         if (errno == ENOENT || errno == ENOTDIR)
        //                                 bad = _("destination directory does not exist");
        //                 } else if (!S_ISDIR(dir_st.st_mode)) {
        //                         bad = _("destination is not a directory");
        //                 }
        //                 …
        //         }
        //
        //         if (bad)
        //                 goto act_on_entry;
        // }
        // ```
        //
        // (builtin/mv.c:458-487, new in 2.56.) A source that reaches this point
        // carries no mode bit of its own, so only `dst_mode`'s `SPARSE` stands
        // the check down. Being a `bad`, a missing leading directory is now
        // reported by `-n` and skipped by `-k`, where 2.55 only found out at
        // `rename()` time. Any other `lstat()` failure falls through to
        // `rename()`, which reports it.
        if !dst_sparse {
            if let Some(slash) = dst_rel.rfind('/') {
                let bad = match std::fs::symlink_metadata(workdir.join(&dst_rel[..slash])) {
                    Err(e)
                        if e.kind() == std::io::ErrorKind::NotFound
                            || e.raw_os_error() == Some(libc::ENOTDIR) =>
                    {
                        Some("destination directory does not exist")
                    }
                    Err(_) => None,
                    Ok(m) if !m.is_dir() => Some("destination is not a directory"),
                    Ok(_) => None,
                };
                if let Some(bad) = bad {
                    return Err(anyhow!("{bad}, source={src_rel}, destination={dst_rel}"));
                }
            }
        }
        // ```c
        // if (ignore_sparse &&
        //     (dst_mode & (SKIP_WORKTREE_DIR | SPARSE)) &&
        //     index_entry_exists(the_repository->index, dst, strlen(dst))) {
        //         bad = _("destination exists in the index");
        //         if (force) {
        //                 if (verbose)
        //                         warning(_("overwriting '%s'"), dst);
        //                 bad = NULL;
        //         } else {
        //                 goto act_on_entry;
        //         }
        // }
        // ```
        //
        // (builtin/mv.c:489-501.) Only a destination outside the sparse-checkout
        // definition is asked, and any stage-0 entry there counts.
        if ignore_sparse && dst_sparse && is_tracked(index, &dst_rel) {
            if !force {
                return Err(anyhow!(
                    "destination exists in the index, source={src_rel}, destination={dst_rel}"
                ));
            }
            if verbose {
                eprintln!("warning: overwriting '{dst_rel}'");
            }
        }
        vec![(src_rel.clone(), dst_rel.clone())]
    };

    Ok(Planned::Move(Plan {
        src_abs,
        dst_abs,
        src_rel,
        dst_rel,
        remaps,
        submodule,
        is_dir,
        index_only: false,
        skip_worktree_dir,
        sparse_remaps: Vec::new(),
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

/// `refresh_cache_entry(istate, new_entry, CE_MATCH_REFRESH)` in
/// `rename_index_entry_at()` (read-cache.c:151-163): the entry is re-stated
/// against the file now at its new name, so the index carries the rename's
/// `ctime` — but only when the file still holds the entry's content and mode, so
/// unstaged changes are not marked clean.
struct Refresh {
    workdir: PathBuf,
    stat_options: gix::index::entry::stat::Options,
    trust_executable_bit: bool,
    /// [`crate::config::big_file_threshold_refusal`]: the die the first blob hashing meets.
    big_file_refusal: Option<String>,
}

impl Refresh {
    /// The stat data the moved entry at `path` is added with.
    ///
    /// `refresh_cache_ent()` returns the entry untouched for `skip-worktree`, when
    /// `lstat()` fails, when `ie_match_stat()` finds nothing changed, and when
    /// `ie_modified()` finds the content or mode changed; only a stat-only
    /// difference over unchanged content is refreshed (`fill_stat_cache_info()`).
    /// A gitlink is left as it was.
    fn stat_for(&self, path: &BString, stat: Stat, id: ObjectId, flags: Flags, mode: Mode) -> Result<Stat, String> {
        if flags.contains(Flags::SKIP_WORKTREE) || !matches!(mode, Mode::FILE | Mode::FILE_EXECUTABLE | Mode::SYMLINK) {
            return Ok(stat);
        }
        let Ok(rela) = gix::path::try_from_bstr(path.as_bstr()) else {
            return Ok(stat);
        };
        let full = self.workdir.join(rela);
        let Ok(md) = std::fs::symlink_metadata(&full) else {
            return Ok(stat);
        };
        let Ok(fs_md) = gix::index::fs::Metadata::from_path_no_follow(&full) else {
            return Ok(stat);
        };
        let Ok(current) = Stat::from_fs(&fs_md) else {
            return Ok(stat);
        };
        if stat.matches(&current, self.stat_options) {
            return Ok(stat);
        }
        let same_type = match mode {
            Mode::SYMLINK => md.file_type().is_symlink(),
            _ => md.is_file() && (!self.trust_executable_bit || (mode == Mode::FILE_EXECUTABLE) == fs_md.is_executable()),
        };
        if !same_type {
            return Ok(stat);
        }
        // `ie_modified()` only reads the content when the size is unchanged (or was never
        // recorded); a regular file read goes through `index_fd()`, whose
        // `core.bigFileThreshold` lookup dies on a value `git_config_ulong()` refuses.
        // A symlink target is compared without it.
        let reads_content = stat.size == 0 || stat.size == current.size;
        if reads_content && mode != Mode::SYMLINK {
            if let Some(message) = &self.big_file_refusal {
                return Err(message.clone());
            }
        }
        if crate::index_racy::modified_check_fs(id.kind(), &full, &md, &id) {
            return Ok(stat);
        }
        Ok(current)
    }
}

/// Apply the (old → new) path remaps to the in-memory index: capture the moved
/// entries' fields, drop the old entries and any entry occupying a new path
/// (the force-overwrite case), then re-append the entries at their new paths.
/// A `sort_entries()` by the caller restores lookup invariants afterward.
fn apply_remaps(index: &mut gix::index::File, remaps: &[(String, String)], refresh: &Refresh) -> Result<(), String> {
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
    // `rename_index_entry_at()` adds it with `ADD_CACHE_OK_TO_REPLACE`
    // (read-cache.c:182-185), so an entry in its way as a file or directory is
    // dropped first: `git mv README.md lnk/` through a tracked symlink `lnk` to a
    // directory removes the `lnk` entry. Renames apply one at a time, so an
    // earlier one's result can be in the way of a later one.
    for (stat, id, flags, mode, new) in pushes {
        let new_bytes = BString::from(new);
        let in_way =
            super::update_index::file_directory_conflicts(index, new_bytes.as_bstr(), Stage::Unconflicted);
        if !in_way.is_empty() {
            index.remove_entries(|_, path, e| {
                e.stage() == Stage::Unconflicted && in_way.iter().any(|c| c.as_bstr() == path)
            });
        }
        let stat = refresh.stat_for(&new_bytes, stat, id, flags, mode)?;
        index.dangerously_push_entry(stat, id, flags, mode, BStr::new(&new_bytes));
    }
    Ok(())
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

/// `internal_prefix_pathspec()`'s trim when `KEEP_TRAILING_SLASH` is not set
/// (builtin/mv.c:68-70): every trailing directory separator goes.
fn trim_dir_seps(arg: &str) -> &str {
    arg.trim_end_matches('/')
}

/// `DUP_BASENAME` (builtin/mv.c:72-73): POSIX `basename()` of the operand with
/// its trailing separators trimmed, which is `.` when nothing is left.
fn dup_basename(arg: &str) -> &str {
    match trim_dir_seps(arg) {
        "" => ".",
        trimmed => trimmed.rsplit('/').next().unwrap_or(trimmed),
    }
}

/// `add_slash()` (builtin/mv.c:82-93): `path` with one `/` appended, unless it
/// is empty or already ends in one.
fn add_slash(path: &str) -> String {
    match path {
        "" => String::new(),
        p if p.ends_with('/') => p.to_owned(),
        p => format!("{p}/"),
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
