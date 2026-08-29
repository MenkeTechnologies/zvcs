//! `git rm [<options>] <pathspec>...` — remove tracked paths from the index and
//! (unless `--cached`) from the working tree.
//!
//! Served natively through the vendored gitoxide crates so tools on PATH observe
//! the same staged index. The full stock `git rm` flag surface is reproduced:
//!
//!   * `--cached`                     — remove from the index only, keep the file
//!   * `-f`, `--force`                — skip the up-to-date safety check
//!   * `-r`                           — allow recursive removal of a directory pathspec
//!   * `-n`, `--dry-run`              — report what would be removed, change nothing
//!   * `--ignore-unmatch`             — exit 0 even if a pathspec matched nothing
//!   * `-q`, `--quiet`                — suppress the `rm '<path>'` lines
//!   * `--sparse`                     — also remove entries outside the sparse cone
//!   * `--pathspec-from-file=<file>`  — read pathspecs from `<file>` (or stdin with `-`)
//!   * `--pathspec-file-nul`          — NUL-separated pathspec file entries
//!   * `--`, `--end-of-options`       — end option parsing
//!
//! Long options accept unambiguous abbreviations (`--dry`, `--cach`) and `--no-`
//! negations (`--no-cached`), matching git's parse-options; the last spelling of a
//! toggle wins. Unknown options/switches exit 129 with git's usage block on
//! stderr; an ambiguous abbreviation (`--p`, which names both
//! `--pathspec-from-file` and `--pathspec-file-nul`) puts its message on stderr
//! and the block on **stdout**, also 129; `` error: option `no-cached' takes no
//! value `` and `` error: option `pathspec-from-file' requires a value `` are
//! `PARSE_OPT_ERROR` and print no block at all. An empty or missing pathspec
//! exits 128 ("No pathspec was given").
//!
//! The name is looked up in the option table *before* any `=<value>` is split
//! off, which is what `parse_long_opt()` does and what keeps a value-carrying
//! spelling from reaching an arm its bare form cannot:
//! `git rm --no-pathspec-from-file=x <file>` used to clear the option and go on
//! to delete `<file>`, where stock refuses at 129 without touching anything.
//!
//! Faithfully reproduced: literal, glob, and full magic-signature pathspecs
//! (`:(glob)`, `:(literal)`, `:(icase)`, `:(top)`, `:(exclude)`/`:!`, `:(attr:…)`)
//! via the shared `repo.pathspec()` engine; the per-spec matched/recursion rules
//! (`did not match any files`, `not removing '<x>' recursively without -r`); the
//! index-vs-HEAD and worktree-vs-index safety check (raw blob hashing; conservative
//! — a filtered worktree that differs at the byte level is reported as modified, so
//! `-f` is required, never silently discarded); submodule (gitlink) removal
//! including recursive worktree pruning and `.gitmodules` section removal + staging;
//! and the `rm '<path>'` output in index order.
//!
//! Unmerged (conflicted) paths are removable without `-f` (all stages dropped),
//! exactly as stock git does.
//!
//! Sparse-checkout, as in `builtin/rm.c`: without `--sparse` an index entry that
//! lives outside the sparse-checkout definition is left out of the removal list
//! entirely, and a pathspec that matched *only* such entries is not the fatal
//! "did not match any files" — it is collected and reported through
//! [`crate::advice::on_updating_sparse_paths`], which makes the command exit 1
//! while still removing whatever the other pathspecs matched. `--sparse` puts
//! those entries back in scope and the report never fires.
//!
//! Deviations kept honest: an entry counts as outside the definition when it
//! carries the index `SKIP_WORKTREE` bit; git additionally re-checks the path
//! against the sparse patterns (`path_in_sparse_checkout`), which only differs
//! for an index left out of sync with an edited pattern file. Pathspec files are
//! read through the one shared `parse_pathspec_file` port in `commit`.

use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::Mode;
use gix::pathspec::search::MatchKind;

/// `usage_with_options()` over `builtin/rm.c`'s option table: the synopsis, a
/// blank line, the options, and the blank line the renderer always ends on.
const USAGE: &str = r"usage: git rm [-f | --force] [-n] [-r] [--cached] [--ignore-unmatch]
              [--quiet] [--pathspec-from-file=<file> [--pathspec-file-nul]]
              [--] [<pathspec>...]

    -n, --[no-]dry-run    dry run
    -q, --[no-]quiet      do not list removed files
    --[no-]cached         only remove from the index
    -f, --[no-]force      override the up-to-date check
    -r                    allow recursive removal
    --[no-]ignore-unmatch exit with a zero status even if nothing matched
    --[no-]sparse         allow updating entries outside of the sparse-checkout cone
    --[no-]pathspec-from-file <file>
                          read pathspec from file
    --[no-]pathspec-file-nul
                          with --pathspec-from-file, pathspec elements are separated with NUL character

";

/// A tracked path selected for removal, captured before the index is mutated.
struct Target {
    path: BString,
    id: ObjectId,
    mode: Mode,
    stage: u32,
    /// `ce_skip_worktree(ce)`: the entry sits outside the sparse-checkout
    /// definition, so `git rm` ignores it unless `--sparse` was given.
    sparse: bool,
}

/// Parsed option state.
#[derive(Default)]
struct Opts {
    cached: bool,
    force: bool,
    recursive: bool,
    dry_run: bool,
    ignore_unmatch: bool,
    quiet: bool,
    /// `--sparse` (git's `include_sparse`): operate on entries outside the
    /// sparse-checkout definition too.
    sparse: bool,
    pathspec_from_file: Option<String>,
    pathspec_file_nul: bool,
}

/// `cmd_rm`'s `struct option builtin_rm_options[]` (builtin/rm.c:288-303), in
/// table order, as [`super::resolve_long`] reads it. Only the entries carrying a
/// `long_name` appear; `OPT_BOOL('r', NULL, …)` has none.
///
/// Every entry is negatable: `OPT__DRY_RUN` and the `OPT_BOOL`s are plain,
/// `OPT__QUIET`/`OPT__FORCE` are `OPT_COUNTUP_F` whose only flags are
/// `PARSE_OPT_NOARG` (plus `PARSE_OPT_NOCOMPLETE` on `--force`, which governs
/// completion rather than negation), and `OPT_PATHSPEC_FROM_FILE` is an
/// `OPT_FILENAME` with no flags at all.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "dry-run",            neg: true, arg: super::Arg::None },
    super::LongOpt { name: "quiet",              neg: true, arg: super::Arg::None },
    super::LongOpt { name: "cached",             neg: true, arg: super::Arg::None },
    super::LongOpt { name: "force",              neg: true, arg: super::Arg::None },
    super::LongOpt { name: "ignore-unmatch",     neg: true, arg: super::Arg::None },
    super::LongOpt { name: "sparse",             neg: true, arg: super::Arg::None },
    super::LongOpt { name: "pathspec-from-file", neg: true, arg: super::Arg::Required },
    super::LongOpt { name: "pathspec-file-nul",  neg: true, arg: super::Arg::None },
];

/// `error: <msg>` + usage, exit 129 (git's usage-error convention).
fn usage_err(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// `fatal: <msg>`, exit 128 (git's fatal convention).
fn fatal(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

pub fn rm(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts::default();
    let mut pathspecs: Vec<String> = Vec::new();
    let mut opts_done = false;

    // 1. Parse flags. Mirrors git's parse-options: `--` / `--end-of-options`
    //    terminate; long options abbreviate and take `--no-` negations; short
    //    flags cluster. Toggles are last-wins.
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if opts_done {
            pathspecs.push(a.clone());
            i += 1;
            continue;
        }
        if a == "--" || a == "--end-of-options" {
            opts_done = true;
            i += 1;
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`. This table has no `PARSE_OPT_HIDDEN` entry, so
        // `USAGE_FULL` renders the same block `-h` prints.
        if a == "--help-all" {
            return Ok(super::show_usage(USAGE));
        }
        if let Some(body) = a.strip_prefix("--") {
            // The table lookup happens on the whole body, *before* the
            // `=<value>` split, exactly as `parse_long_opt()` does. Gating on
            // table membership first is what stops a value-carrying spelling
            // from escaping a restriction its bare form obeys:
            // `--no-pathspec-from-file=x` used to clear the option and go on to
            // remove the named files, where stock refuses at 129.
            let (opt, unset) = match super::resolve_long(LONG_OPTS, body) {
                super::Resolved::One(opt, unset) => (opt, unset),
                super::Resolved::Ambiguous(first, second) => {
                    return Ok(super::ambiguous_option(a, &first, &second, USAGE))
                }
                // `error(_("unknown option `%s'"), ctx.argv[0] + 2)` quotes the
                // argument exactly as typed, `=<value>` and all.
                super::Resolved::Unknown => {
                    return Ok(usage_err(format!("unknown option `{body}'")))
                }
            };
            let inline_val = body.split_once('=').map(|(_, v)| v.to_string());

            // The set sense of the one value-taking entry is the only spelling
            // that consumes a value; every other spelling, the unset sense of
            // that same entry included, is a pure boolean.
            if opt.arg == super::Arg::Required && !unset {
                let val = match inline_val {
                    Some(v) => v,
                    None => match args.get(i + 1) {
                        Some(v) => {
                            i += 1;
                            v.clone()
                        }
                        None => return Ok(super::missing_option_value("--pathspec-from-file")),
                    },
                };
                opts.pathspec_from_file = Some(val);
                i += 1;
                continue;
            }
            if inline_val.is_some() {
                // `PARSE_OPT_ERROR` out of `get_value()`: one line, no usage
                // block, naming the entry the way `optname()` spells it — the
                // table's own name, `no-`-prefixed for the unset sense,
                // however far it was abbreviated.
                let shown = match unset {
                    true => format!("no-{}", opt.name),
                    false => opt.name.to_string(),
                };
                eprintln!("error: option `{shown}' takes no value");
                return Ok(ExitCode::from(129));
            }

            let on = !unset;
            match opt.name {
                "dry-run" => opts.dry_run = on,
                "quiet" => opts.quiet = on,
                "cached" => opts.cached = on,
                "force" => opts.force = on,
                "ignore-unmatch" => opts.ignore_unmatch = on,
                "sparse" => opts.sparse = on,
                "pathspec-file-nul" => opts.pathspec_file_nul = on,
                // Reached only as `--no-pathspec-from-file`; the set sense
                // returned above.
                "pathspec-from-file" => opts.pathspec_from_file = None,
                _ => unreachable!("resolve_long only returns LONG_OPTS entries"),
            }
            i += 1;
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            for c in a[1..].chars() {
                match c {
                    'f' => opts.force = true,
                    'r' => opts.recursive = true,
                    // parse_options_step() tests `internal_help` inside the
                    // short-option loop: `-h` prints the block on stdout at 129
                    // and stops, with no `error:` line.
                    'h' => return Ok(super::show_usage(USAGE)),
                    'n' => opts.dry_run = true,
                    'q' => opts.quiet = true,
                    _ => return Ok(usage_err(format!("unknown switch `{c}'"))),
                }
            }
            i += 1;
            continue;
        }
        // A bare `-` or any non-option token is a pathspec.
        pathspecs.push(a.clone());
        i += 1;
    }

    // 2. --pathspec-from-file: mutually exclusive with cmdline pathspecs, read
    //    before the empty-pathspec check (both fatal, exit 128).
    if let Some(file) = &opts.pathspec_from_file {
        if !pathspecs.is_empty() {
            return Ok(fatal(
                "'--pathspec-from-file' and pathspec arguments cannot be used together",
            ));
        }
        pathspecs = super::commit::read_pathspec_file(file, opts.pathspec_file_nul)?;
    }

    if pathspecs.is_empty() {
        return Ok(fatal("No pathspec was given. Which files should I remove?"));
    }

    // 3. Open the repository and require a working tree.
    let repo = crate::setup::discover()?;
    let workdir = match repo.workdir() {
        Some(w) => w.to_owned(),
        None => return Ok(fatal("this operation must be run in a work tree")),
    };

    // Serialize the whole read-modify-write of the index through the repo
    // coordinator so concurrent zvcs writers queue FCFS instead of racing
    // `index.lock`. Held for the rest of the function; a no-op with no daemon.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // 4. Validate every pathspec up front: bad magic (`:(bogus)…`) or a spec that
    //    escapes the worktree is fatal (exit 128), exactly like git — before any
    //    matching or mutation. Also records which specs are exclusions, since
    //    git's "did not match" report skips exclude specs.
    let defaults = repo.pathspec_defaults_inherit_ignore_case(false)?;
    let prefix = repo.prefix()?.map(|p| p.to_path_buf()).unwrap_or_default();
    let root = gix::path::realpath(repo.workdir().unwrap_or_else(|| repo.git_dir()))?;
    let mut is_exclude: Vec<bool> = Vec::with_capacity(pathspecs.len());
    let mut patterns: Vec<BString> = Vec::with_capacity(pathspecs.len());
    for raw in &pathspecs {
        let mut parsed = match gix::pathspec::parse(raw.as_bytes(), defaults) {
            Ok(p) => p,
            Err(_) => return Ok(fatal(format!("{raw}: bad pathspec magic"))),
        };
        is_exclude.push(parsed.is_excluded());
        if parsed.normalize(&prefix, &root).is_err() {
            return Ok(fatal(format!(
                "{raw}: '{}' is outside repository at '{}'",
                parsed.path().to_str_lossy(),
                root.display()
            )));
        }
        patterns.push(BString::from(raw.as_str()));
    }

    // 5. Snapshot the index entries (owned) so matching/safety reads don't hold a
    //    borrow across the later mutation.
    let index = repo.open_index()?;
    let targets_all: Vec<Target> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .map(|e| Target {
                path: e.path_in(backing).to_owned(),
                id: e.id,
                mode: e.mode,
                stage: e.stage_raw(),
                sparse: e.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE),
            })
            .collect()
    };

    // 6. Match pathspecs against the index via the shared pathspec engine. Track
    //    per-spec how each matched, mirroring git's `seen[]`: RECURSIVELY (a
    //    directory prefix), FNMATCH (wildcard), or EXACTLY (verbatim). A spec that
    //    only ever matched RECURSIVELY needs `-r`.
    const RECURSIVELY: u8 = 1;
    const FNMATCH: u8 = 3;
    const EXACTLY: u8 = 4;

    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;

    let mut seen: Vec<u8> = vec![0; pathspecs.len()];
    // `find_pathspecs_matching_skip_worktree()` (pathspec.c): the parallel `seen[]`
    // built from the entries the sparse-checkout definition excludes, consulted
    // only when a pathspec matched nothing else.
    let mut sparse_seen: Vec<bool> = vec![false; pathspecs.len()];
    // The synthetic "matched because nothing excludes it" case (all specs are
    // exclusions) reports sequence_number == pathspecs.len(); track it apart.
    let mut synthetic_seen: u8 = 0;
    let mut selected: Vec<Target> = Vec::new();
    let mut selected_paths: HashSet<BString> = HashSet::new();
    // Which pathspec items have already matched a path letter for letter — git's
    // `seen[i] == MATCHED_EXACTLY`.
    let mut matched_verbatim: HashSet<usize> = HashSet::new();

    for t in &targets_all {
        let Some(m) = ps.pattern_matching_relative_path(t.path.as_bstr(), Some(false)) else {
            continue;
        };
        if m.is_excluded() {
            continue;
        }
        // An entry outside the sparse-checkout definition is invisible to the
        // removal (and to `seen[]`) unless `--sparse` was given; it only feeds
        // the separate skip-worktree tally that turns "did not match any files"
        // into the sparse-path report.
        if t.sparse && !opts.sparse {
            if m.sequence_number < sparse_seen.len() {
                sparse_seen[m.sequence_number] = true;
            }
            continue;
        }
        let rank = match m.kind {
            MatchKind::Prefix => RECURSIVELY,
            MatchKind::WildcardMatch => FNMATCH,
            MatchKind::Verbatim => EXACTLY,
            // A whole-tree match (empty/synthetic pattern) is a recursive match.
            MatchKind::Always => RECURSIVELY,
        };
        if m.sequence_number < seen.len() {
            if seen[m.sequence_number] < rank {
                seen[m.sequence_number] = rank;
            }
        } else if synthetic_seen < rank {
            synthetic_seen = rank;
        }
        // ```c
        // for (unsigned int i = 0; i < the_repository->index->cache_nr; i++) {
        //         const struct cache_entry *ce = the_repository->index->cache[i];
        //         …
        //         list.entry[list.nr].name = xstrdup(ce->name);
        // ```
        //
        // (builtin/rm.c:315-326.) The list is built per *cache entry*, not per path, so an
        // unmerged path with three stages is listed — and reported — three times.
        //
        // Except when a *literal* pathspec named it. `do_match_pathspec()` skips an item
        // that has already matched exactly:
        //
        // ```c
        // if ((!(ps->items[i].magic & PATHSPEC_EXCLUDE) && seen && seen[i] == MATCHED_EXACTLY))
        //         continue;
        // ```
        //
        // so `git rm conflict.txt` lists the path once while `git rm -r .` — which matches
        // recursively, never exactly — lists it once per stage. gix's `MatchKind::Verbatim`
        // is that `MATCHED_EXACTLY`. (git would then let a *different* item pick the entry
        // up; selecting per item is not something the vendored matcher exposes, and no
        // spec set that would distinguish the two is reachable from `rm`'s own arguments.)
        if m.kind == gix::pathspec::search::MatchKind::Verbatim
            && !matched_verbatim.insert(m.sequence_number)
        {
            continue;
        }
        selected_paths.insert(t.path.clone());
        selected.push(Target {
            path: t.path.clone(),
            id: t.id,
            mode: t.mode,
            stage: t.stage,
            sparse: t.sparse,
        });
    }

    // 7. Per-spec validation loop, in argument order, exactly like git: excludes
    //    are skipped; an unmatched positive spec is fatal unless --ignore-unmatch;
    //    a spec that matched only recursively is fatal without -r.
    let mut seen_any = false;
    let mut only_match_skip_worktree: Vec<String> = Vec::new();
    for (idx, raw) in pathspecs.iter().enumerate() {
        if is_exclude[idx] {
            continue;
        }
        let how = seen[idx];
        if how != 0 {
            seen_any = true;
        } else if opts.ignore_unmatch {
            continue;
        } else if sparse_seen[idx] {
            // Matched only entries the sparse-checkout definition excludes: git
            // defers to `advise_on_updating_sparse_paths()` instead of dying.
            only_match_skip_worktree.push(raw.clone());
        } else {
            return Ok(fatal(format!("pathspec '{raw}' did not match any files")));
        }
        if !opts.recursive && how == RECURSIVELY {
            return Ok(fatal(format!("not removing '{raw}' recursively without -r")));
        }
    }
    // The all-exclusions case: git treats the implicit whole-tree match as `.`.
    if is_exclude.iter().all(|&e| e) && synthetic_seen == RECURSIVELY && !opts.recursive {
        return Ok(fatal("not removing '.' recursively without -r"));
    }
    // That implicit whole-tree match is still a match, so it counts toward
    // git's `seen_any` even though no numbered pathspec recorded it.
    seen_any |= synthetic_seen != 0;

    // `ret` in `cmd_rm()`: the sparse-path report is the command's only non-fatal
    // failure, and it survives to the final `return ret` — so a run that removed
    // other paths still exits 1. `--dry-run` returns before that (git's
    // `if (show_only) return 0;`), which is why `-n` always exits 0 here.
    let sparse_report = !only_match_skip_worktree.is_empty();
    if sparse_report {
        crate::advice::on_updating_sparse_paths(&repo, &only_match_skip_worktree);
    }
    let ret = if sparse_report {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    };

    if !seen_any || selected.is_empty() {
        // git: `if (!seen_any) exit(ret);` — nothing to remove, but the sparse
        // report (if any) still decides the status.
        return Ok(ret);
    }

    // 8. Submodule removals need `.gitmodules` in a clean staging state, and the
    //    section for each removed submodule stripped and restaged. Resolve the
    //    path→name map (from the current config) before any mutation.
    let has_submodule = selected.iter().any(|t| t.mode == Mode::COMMIT);
    let mut submodule_name_by_path: HashMap<BString, BString> = HashMap::new();
    if has_submodule {
        if let Some(modules) = repo.submodules()? {
            for sm in modules {
                if let Ok(p) = sm.path() {
                    submodule_name_by_path.insert(p, sm.name().to_owned());
                }
            }
        }
        // git refuses to proceed with unstaged `.gitmodules` edits.
        if gitmodules_has_unstaged_changes(&repo, &index)? {
            return Ok(fatal(
                "please stage your changes to .gitmodules or stash them to proceed",
            ));
        }
    }

    // 9. Up-to-date safety check (skipped with -f). Unmerged (stage != 0) paths are
    //    always removable and bypass it. Per stage-0 path:
    //      staged = index blob differs from HEAD blob
    //      local  = worktree content differs from index blob (missing == no change);
    //               for a submodule, "local" means the submodule worktree is dirty.
    //    Full removal refuses on staged OR local; --cached refuses only when the
    //    staged content matches neither HEAD nor the worktree (staged AND local).
    if !opts.force {
        let hash_kind = repo.object_hash();
        let head_tree = repo.head_tree().ok();

        let mut both: Vec<String> = Vec::new();
        let mut staged_only: Vec<String> = Vec::new();
        let mut local_only: Vec<String> = Vec::new();

        for t in &selected {
            if t.stage != 0 {
                continue; // unmerged: always removable
            }
            let path_str = t.path.to_str_lossy().into_owned();

            let head_id: Option<ObjectId> = match &head_tree {
                Some(tree) => tree
                    .lookup_entry_by_path(std::path::Path::new(&path_str))?
                    .map(|e| e.id().detach()),
                None => None,
            };
            let staged = head_id.map(|h| h != t.id).unwrap_or(true);

            let local = if t.mode == Mode::COMMIT {
                submodule_is_dirty(&repo, &t.path)
            } else {
                match worktree_blob(&repo, &t.path, t.mode, hash_kind)? {
                    Some(wt_id) => wt_id != t.id,
                    None => false, // already gone from the worktree
                }
            };

            match (staged, local) {
                (true, true) => both.push(path_str),
                (true, false) => staged_only.push(path_str),
                (false, true) => local_only.push(path_str),
                (false, false) => {}
            }
        }

        // ```c
        // print_error_files(&files_staged, …, _("\n(use -f to force removal)"), &errs);
        // print_error_files(&files_cached, …,
        //                   _("\n(use --cached to keep the file, or -f to force removal)"), &errs);
        // print_error_files(&files_local,  …,
        //                   _("\n(use --cached to keep the file, or -f to force removal)"), &errs);
        // ```
        //
        // (builtin/rm.c:215-241.) Three separate `error()` calls, each carrying its own
        // hint under `advice.rmHints` — not one block with a single trailing hint. The
        // first category's hint is `-f` alone, because `--cached` is what put those files
        // in it.
        let advice = crate::advice::Advice::RmHints.enabled();
        let plural = |v: &[String]| if v.len() == 1 { ("file", "has") } else { ("files", "have") };
        let mut errs = false;
        let mut report = |files: &[String], main: String, hint: &str| {
            if files.is_empty() {
                return;
            }
            errs = true;
            let body = format!("{main}\n    {}", files.join("\n    "));
            match advice {
                true => eprintln!("error: {body}\n{hint}"),
                false => eprintln!("error: {body}"),
            }
        };
        {
            let (f, h) = plural(&both);
            report(
                &both,
                format!("the following {f} {h} staged content different from both the\nfile and the HEAD:"),
                "(use -f to force removal)",
            );
        }
        if !opts.cached {
            let (f, h) = plural(&staged_only);
            report(
                &staged_only,
                format!("the following {f} {h} changes staged in the index:"),
                "(use --cached to keep the file, or -f to force removal)",
            );
            let (f, h) = plural(&local_only);
            report(
                &local_only,
                format!("the following {f} {h} local modifications:"),
                "(use --cached to keep the file, or -f to force removal)",
            );
        }
        if errs {
            return Ok(ExitCode::from(1));
        }
    }

    // 10. Print the removals (index order) unless quiet. Done before mutating so
    //     dry-run and real runs report identically. Paths are emitted as raw bytes
    //     in single quotes (git applies no quoting to `rm '%s'`).
    if !opts.quiet {
        let mut out = Vec::new();
        for t in &selected {
            out.extend_from_slice(b"rm '");
            out.extend_from_slice(t.path.as_bytes());
            out.extend_from_slice(b"'\n");
        }
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        lock.write_all(&out)?;
        lock.flush()?;
    }

    if opts.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // 11. Remove the selected worktree files first (unless --cached), pruning any
    //     leading directories left empty. Submodule (gitlink) paths are directories
    //     and are removed recursively (their gitdir under .git/modules survives).
    if !opts.cached {
        for t in &selected {
            let Some(abs) = repo.workdir_path(t.path.as_bstr()) else {
                continue;
            };
            let res = if t.mode == Mode::COMMIT {
                std::fs::remove_dir_all(&abs)
            } else {
                std::fs::remove_file(&abs)
            };
            match res {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => crate::git_fatal!("failed to remove {}: {e}", t.path.to_str_lossy()),
            }
            // `remove_path()` (dir.c:3520-3540) walks the parents up, `rmdir`ing each
            // until one is not empty — and stopping at `startup_info->original_cwd`, so a
            // `git rm -r .` run from inside a directory does not delete the directory the
            // caller is standing in. That guard is [`crate::worktree::prune_empty_dirs`]'s.
            crate::worktree::prune_empty_dirs(&workdir, &abs);
        }
    }

    // 12. For removed submodules, strip their `.gitmodules` sections and restage
    //     the edited file. Only for full removal — `--cached` leaves `.gitmodules`
    //     untouched (the worktree submodule survives, now untracked), matching git.
    let mut index = index;
    let gitmodules_update = if has_submodule && !opts.cached {
        let removed_paths: Vec<&BString> = selected
            .iter()
            .filter(|t| t.mode == Mode::COMMIT)
            .map(|t| &t.path)
            .collect();
        update_gitmodules(&repo, &workdir, &removed_paths, &submodule_name_by_path)?
    } else {
        None
    };

    // 13. Drop every selected path (all stages) from the owned index, apply any
    //     `.gitmodules` restage, and persist.
    //
    //     `cmd_rm()` drops each path with `remove_file_from_index()`
    //     (builtin/rm.c:398), and that function's *first* act is
    //     `cache_tree_invalidate_path(istate, path)` (read-cache.c:627-637) — the
    //     only cache-tree work `rm` does. Invalidating per path rather than
    //     dropping the whole extension is what keeps every directory the removal
    //     never touched at its cached tree id, and what lets a cache-tree stock
    //     git wrote survive a `zvcs rm` instead of being thrown away wholesale.
    for path in &selected_paths {
        index.invalidate_path_in_tree(path.as_bstr());
    }
    index.remove_entries(|_, path, _| selected_paths.contains(&path.to_owned()));
    if let Some((id, stat)) = gitmodules_update {
        // `stage_updated_gitmodules()` restages the rewritten file through
        // `add_file_to_index()`, whose `add_index_entry_with_check()` invalidates
        // the path it adds (read-cache.c:1273-1274).
        index.invalidate_path_in_tree(b".gitmodules".as_bstr());
        index.remove_entries(|_, path, _| path == b".gitmodules".as_bstr());
        index.dangerously_push_entry(
            stat,
            id,
            gix::index::entry::Flags::empty(),
            Mode::FILE,
            b".gitmodules".as_bstr(),
        );
        index.sort_entries();
    }
    // git's `rm` finishes with `write_locked_index()` (builtin/rm.c:442), whose
    // `do_write_index()` reads `skip_hash` out of the settings block
    // (read-cache.c:2830-2831) and decides the `IEOT` offset table from
    // `index.threads` / `index.recordOffsetTable` (read-cache.c:2874-2904) — so
    // this index gets the same trailer and the same extensions any other verb
    // would have written in this repository.
    super::write_tree::prepare_offset_table(&repo, &mut index);
    crate::index_racy::write(&repo, &mut index)?;

    Ok(ret)
}

/// Hash the working-tree content at `path` into its git blob id, or `None` if the
/// file is absent. Symlinks hash their target string (as git stores them); an
/// unreadable file is treated as changed (conservative — forces `-f`).
fn worktree_blob(
    repo: &gix::Repository,
    path: &BString,
    mode: Mode,
    hash_kind: gix::hash::Kind,
) -> Result<Option<ObjectId>> {
    let Some(abs) = repo.workdir_path(path.as_bstr()) else {
        return Ok(None);
    };
    let meta = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => crate::git_fatal!("failed to stat {}: {e}", path.to_str_lossy()),
    };

    let content: Vec<u8> = if mode == Mode::SYMLINK || meta.is_symlink() {
        use std::os::unix::ffi::OsStrExt;
        std::fs::read_link(&abs)
            .map_err(|e| anyhow::anyhow!("failed to read symlink {}: {e}", path.to_str_lossy()))?
            .as_os_str()
            .as_bytes()
            .to_vec()
    } else {
        std::fs::read(&abs)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.to_str_lossy()))?
    };

    let id = gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &content)?;
    Ok(Some(id))
}

/// Whether the submodule rooted at `path` has changes that make it "modified" for
/// `git rm`'s safety check (worktree modifications, untracked files, or a checked
/// out HEAD that differs from the recorded gitlink). Missing/unopenable submodules
/// are treated as clean (nothing to lose).
fn submodule_is_dirty(repo: &gix::Repository, path: &BString) -> bool {
    let Ok(Some(modules)) = repo.submodules() else {
        return false;
    };
    for sm in modules {
        if sm.path().map(|p| &p == path).unwrap_or(false) {
            return match sm.status(gix::submodule::config::Ignore::None, true) {
                Ok(status) => status.is_dirty().unwrap_or(false),
                Err(_) => false,
            };
        }
    }
    false
}

/// True when the worktree `.gitmodules` differs from its staged (index) blob — the
/// condition under which git refuses to touch `.gitmodules` during `rm`.
fn gitmodules_has_unstaged_changes(
    repo: &gix::Repository,
    index: &gix::index::State,
) -> Result<bool> {
    let name = b".gitmodules".as_bstr();
    let Some(entry) = index.entry_by_path(name) else {
        return Ok(false); // not tracked → nothing to conflict with
    };
    let Some(abs) = repo.workdir_path(name) else {
        return Ok(false);
    };
    let content = match std::fs::read(&abs) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => crate::git_fatal!("failed to read .gitmodules: {e}"),
    };
    let id = gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &content)?;
    Ok(id != entry.id)
}

/// Strip the `[submodule "<name>"]` sections of every removed submodule from the
/// worktree `.gitmodules`, write it back, and return the staged blob id + stat.
/// Returns `None` when `.gitmodules` is absent or unchanged.
fn update_gitmodules(
    repo: &gix::Repository,
    workdir: &std::path::Path,
    removed_paths: &[&BString],
    name_by_path: &HashMap<BString, BString>,
) -> Result<Option<(ObjectId, gix::index::entry::Stat)>> {
    let gm_path = workdir.join(".gitmodules");
    let mut content = match std::fs::read(&gm_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => crate::git_fatal!("failed to read .gitmodules: {e}"),
    };

    let mut changed = false;
    for path in removed_paths {
        if let Some(name) = name_by_path.get(*path) {
            if remove_gitmodules_section(&mut content, name.as_bytes()) {
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(None);
    }

    std::fs::write(&gm_path, &content)?;
    let id = repo.write_blob(&content)?.detach();
    let md = gix::index::fs::Metadata::from_path_no_follow(&gm_path)?;
    let stat = gix::index::entry::Stat::from_fs(&md).unwrap_or_default();
    Ok(Some((id, stat)))
}

/// Delete the `[submodule "<name>"]` section from git-config bytes, spanning from
/// the header line to the next section header (a line beginning with `[`) or EOF —
/// matching git's byte-range section removal for a git-generated `.gitmodules`.
/// Returns whether a section was removed.
fn remove_gitmodules_section(content: &mut Vec<u8>, name: &[u8]) -> bool {
    let mut header = Vec::with_capacity(name.len() + 16);
    header.extend_from_slice(b"[submodule \"");
    header.extend_from_slice(name);
    header.extend_from_slice(b"\"]");

    // Find the header line (its content, ignoring leading/trailing ASCII space).
    let mut line_start = 0usize;
    let mut section_start = None;
    while line_start <= content.len() {
        let line_end = content[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| line_start + p)
            .unwrap_or(content.len());
        let line = &content[line_start..line_end];
        let trimmed = trim_ascii(line);
        if trimmed == header.as_slice() {
            section_start = Some(line_start);
            break;
        }
        if line_end == content.len() {
            break;
        }
        line_start = line_end + 1;
    }
    let Some(start) = section_start else {
        return false;
    };

    // Find the next section header line at or after the following line.
    let mut cursor = content[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|p| start + p + 1)
        .unwrap_or(content.len());
    let mut end = content.len();
    while cursor < content.len() {
        let line_end = content[cursor..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| cursor + p)
            .unwrap_or(content.len());
        let trimmed = trim_ascii(&content[cursor..line_end]);
        if trimmed.first() == Some(&b'[') {
            end = cursor;
            break;
        }
        if line_end == content.len() {
            break;
        }
        cursor = line_end + 1;
    }

    content.drain(start..end);
    true
}

/// Trim leading/trailing ASCII whitespace from a byte slice.
fn trim_ascii(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if first.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = b {
        if last.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}
