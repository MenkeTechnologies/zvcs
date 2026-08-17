use anyhow::{Result, bail};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};

/// The exact usage block `git clean` prints for `-h` and for every usage error,
/// reproduced byte for byte including the trailing blank line.
const USAGE: &str = concat!(
    "usage: git clean [-d] [-f] [-i] [-n] [-q] [-e <pattern>] [-x | -X] [--] [<pathspec>...]\n",
    "\n",
    "    -q, --[no-]quiet      do not print names of files removed\n",
    "    -n, --[no-]dry-run    dry run\n",
    "    -f, --[no-]force      force\n",
    "    -i, --[no-]interactive\n",
    "                          interactive cleaning\n",
    "    -d                    remove whole directories\n",
    "    -e, --exclude <pattern>\n",
    "                          add <pattern> to ignore rules\n",
    "    -x                    remove ignored files, too\n",
    "    -X                    remove only ignored files\n",
    "\n",
);

/// git's `parse-options` table for `clean`: canonical name, whether `--no-<name>`
/// is accepted, and whether the option takes a value.
const LONG_OPTS: &[(&str, bool, bool)] = &[
    ("quiet", true, false),
    ("dry-run", true, false),
    ("force", true, false),
    ("interactive", true, false),
    ("exclude", false, true),
];

/// Resolve a long option name the way `parse-options` does: an exact match wins,
/// otherwise a unique prefix is accepted (`--dry` for `--dry-run`).
fn resolve_long(name: &str) -> Option<&'static (&'static str, bool, bool)> {
    if let Some(exact) = LONG_OPTS.iter().find(|(n, _, _)| *n == name) {
        return Some(exact);
    }
    let mut hits = LONG_OPTS.iter().filter(|(n, _, _)| n.starts_with(name));
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// Everything the command line can express, after `parse-options` has run.
#[derive(Default)]
struct Parsed {
    dry_run: bool,
    force: usize,
    remove_directories: bool,
    quiet: bool,
    interactive: bool,
    ignored_too: bool,  // -x
    ignored_only: bool, // -X
    excludes: Vec<String>,
    pathspecs: Vec<String>,
}

/// Emulate git's `parse-options` for `clean`, including option/pathspec
/// permutation, `--no-` negation, unique-prefix abbreviation and the exact
/// diagnostics. On failure the message is written where git writes it and the
/// process exit code is returned.
fn parse(args: &[String]) -> std::result::Result<Parsed, u8> {
    let mut p = Parsed::default();
    let mut no_more_opts = false;
    let mut i = 0usize;

    while i < args.len() {
        let a = args[i].clone();
        i += 1;

        if no_more_opts || a == "-" || !a.starts_with('-') {
            p.pathspecs.push(a);
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }
        // `parse_options_step()` tests `--help-all` with a `strcmp()` of its own,
        // ahead of `parse_long_opt()`: the name never abbreviates and never takes
        // an `=<value>`. This table has no `PARSE_OPT_HIDDEN` entry, so
        // `USAGE_FULL` renders the same block `-h` prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Err(129);
        }

        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_owned())),
                None => (long, None),
            };

            let mut negated = false;
            let mut opt = resolve_long(name);
            if opt.is_none() {
                if let Some(stripped) = name.strip_prefix("no-") {
                    if let Some(cand) = resolve_long(stripped).filter(|(_, neg, _)| *neg) {
                        negated = true;
                        opt = Some(cand);
                    }
                }
            }
            let Some(&(canonical, _, takes_value)) = opt else {
                eprint!("error: unknown option `{long}'\n{USAGE}");
                return Err(129);
            };

            let value = if takes_value {
                match inline.or_else(|| {
                    let v = args.get(i).cloned();
                    if v.is_some() {
                        i += 1;
                    }
                    v
                }) {
                    Some(v) => v,
                    None => {
                        eprintln!("error: option `{canonical}' requires a value");
                        return Err(129);
                    }
                }
            } else {
                if inline.is_some() {
                    eprintln!("error: option `{canonical}' takes no value");
                    return Err(129);
                }
                String::new()
            };

            match canonical {
                "quiet" => p.quiet = !negated,
                "dry-run" => p.dry_run = !negated,
                // `-f` is a counter, and `--no-force` resets it to zero.
                "force" => {
                    if negated {
                        p.force = 0;
                    } else {
                        p.force += 1;
                    }
                }
                "interactive" => p.interactive = !negated,
                "exclude" => p.excludes.push(value),
                _ => unreachable!("every entry of LONG_OPTS is handled"),
            }
            continue;
        }

        let cluster: Vec<char> = a[1..].chars().collect();
        let mut j = 0usize;
        while j < cluster.len() {
            let c = cluster[j];
            j += 1;
            match c {
                'q' => p.quiet = true,
                'n' => p.dry_run = true,
                'f' => p.force += 1,
                'i' => p.interactive = true,
                'd' => p.remove_directories = true,
                'x' => p.ignored_too = true,
                'X' => p.ignored_only = true,
                'e' => {
                    // The rest of the cluster is the value, else the next argument.
                    let rest: String = cluster[j..].iter().collect();
                    j = cluster.len();
                    let value = if rest.is_empty() {
                        match args.get(i).cloned() {
                            Some(v) => {
                                i += 1;
                                v
                            }
                            None => {
                                eprintln!("error: switch `e' requires a value");
                                return Err(129);
                            }
                        }
                    } else {
                        rest
                    };
                    p.excludes.push(value);
                }
                'h' => {
                    print!("{USAGE}");
                    return Err(129);
                }
                _ => {
                    eprint!("error: unknown switch `{c}'\n{USAGE}");
                    return Err(129);
                }
            }
        }
    }

    Ok(p)
}

/// `git clean` — remove untracked files from the working tree.
///
/// Backed by gitoxide's directory walk (`gix::dir::walk`) configured for
/// deletion, which reproduces git's own `dir.c` collapsing rules: a wholly
/// untracked directory folds into a single entry, while a directory that also
/// holds files we are *not* about to delete (e.g. ignored files without `-x`)
/// stays expanded so only the deletable leaves are reported.
///
/// Supported invocations (stdout, exit code and resulting worktree state match
/// stock `git clean`):
/// ```text
///   * `-n`/`--dry-run`   — list as `Would remove <path>` without touching disk.
///   * `-f`/`--force`     — actually delete; `-ff` also deletes nested
///                          repositories, which a single `-f` refuses to touch.
///   * `-d`               — recurse into untracked directories and remove them
///                          (including empty ones) as single entries.
///   * `-q`/`--quiet`     — suppress the per-path lines, keep warnings.
///   * `-x`               — also remove ignored files.
///   * `-X`               — remove *only* ignored files.
///   * `-i`/`--interactive` — git's prompt loop (`clean`, `filter by pattern`,
///                          `select by numbers`, `ask each`, `quit`, `help`),
///                          reading selections from stdin; the column layout,
///                          menu wording, `Huh (…)?` diagnostics and per-command
///                          semantics are ported from `builtin/clean.c`.
///   * `-e`/`--exclude=<pattern>` — extra ignore patterns, layered above every
///                          `.gitignore` exactly like git's `EXC_CMDL` group, so
///                          with `-X` they become removal targets and otherwise
///                          they shield paths from removal. `-x` drops the
///                          repository's own ignore rules but keeps these, which
///                          then also keep the directory holding a shielded file
///                          expanded — removing the directory would take that
///                          file with it. A `!`-negated pattern gives the
///                          protection back up.
///   * `--no-quiet`, `--no-dry-run`, `--no-force`, `--no-interactive`, and
///     unique-prefix abbreviations such as `--dry` or `--no-dr`.
///   * `--` and `<pathspec>...` — as with git, any pathspec implies `-d`, and
///     options may be given after pathspecs.
///   * grouped short flags (`-ndx`, `-ffd`, …).
/// ```
///
/// Diagnostics follow git: an unknown option or a missing option value exits
/// 129, while a pathspec with invalid magic (`:(bogusmagic)…`), a pathspec that
/// leaves the worktree, and the `clean.requireForce` refusal exit 128, in the
/// same order git checks them (force refusal first, then `-x`/`-X`, then per
/// pathspec left-to-right: magic parse, then worktree-escape).
///
/// Paths are sorted by their repository-relative form (directories carrying a
/// trailing `/`) and then rendered relative to the current working directory,
/// C-quoted exactly as git's `quote_path` does.
///
/// Faithfully unsupported — this `bail!`s rather than emit wrong results:
/// running from inside a directory that is itself a deletion candidate, where
/// git prints an unsorted, readdir-ordered `./`-prefixed listing after `Refusing
/// to remove current working directory`.
pub fn clean(args: &[String]) -> Result<ExitCode> {
    let p = match parse(args) {
        Ok(p) => p,
        Err(code) => return Ok(ExitCode::from(code)),
    };

    let Parsed {
        dry_run,
        force,
        mut remove_directories,
        quiet,
        interactive,
        ignored_too,
        ignored_only,
        excludes,
        pathspecs,
    } = p;

    let repo = gix::discover(".")?;

    // git checks the force refusal before anything else it could diagnose, so
    // `git clean ../outside-repo` reports the refusal rather than the pathspec.
    if !interactive
        && !dry_run
        && force == 0
        && repo.config_snapshot().boolean("clean.requireForce") != Some(false)
    {
        eprintln!("fatal: clean.requireForce is true and -f not given: refusing to clean");
        return Ok(ExitCode::from(128));
    }

    if ignored_too && ignored_only {
        eprintln!("fatal: options '-x' and '-X' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    // `git_clean_config()` runs while the configuration is being read, before the
    // command does anything, so an unparseable `column.ui`/`column.clean` is fatal
    // for every `git clean` — `-n` and `-i` alike — not just the interactive path
    // that goes on to use the value.
    let mut colopts = 0u32;
    if let Err(msg) = super::column::config_colopts_key(&mut colopts, "column.clean") {
        eprint!("{msg}");
        return Ok(ExitCode::from(128));
    }

    // Same story for the `color.interactive.<slot>` table: `git_clean_config()`
    // runs `color_parse()` on every slot it recognizes — including `plain`, which
    // the UI never prints — so a spec git's parser rejects aborts even a plain
    // `git clean -n`.
    if let Some((key, spec, meta)) =
        super::color::first_invalid_slot(&repo, "color.interactive", &CleanColors::SLOTS)
    {
        return Ok(super::color::invalid_color_fatal(&key, &spec, &meta));
    }
    let colors = CleanColors::resolve(&repo);

    // The prefix is the repo-relative current directory; it scopes the walk when
    // no pathspec is given, and every reported path is rendered relative to it.
    let prefix: BString = repo
        .prefix()?
        .map(|p| gix::path::to_unix_separators_on_windows(gix::path::into_bstr(p)).into_owned())
        .unwrap_or_default();
    let prefix_parts: Vec<&[u8]> = prefix
        .split(|b| *b == b'/')
        .filter(|c| !c.is_empty())
        .collect();

    let workdir_real = repo
        .workdir()
        .map(gix::path::realpath)
        .transpose()?
        .unwrap_or_default();
    // git validates every pathspec left-to-right: for each element it first
    // parses the magic prefix (`:(…)`), then checks it does not escape the
    // worktree. A magic-parse failure is `fatal:` / exit 128 — not the exit 1
    // that `anyhow` would collapse a walk-time parse error to. Parse here with
    // the same defaults the walk uses so acceptance never diverges from it.
    let pathspec_defaults = repo.pathspec_defaults_inherit_ignore_case(true)?;
    for spec in &pathspecs {
        if let Err(err) = gix::pathspec::parse(spec.as_bytes(), pathspec_defaults) {
            eprintln!(
                "fatal: {}",
                crate::pathspec::parse_error_message(spec.as_str().into(), &err)
            );
            return Ok(ExitCode::from(128));
        }
        if pathspec_leaves_worktree(spec, prefix_parts.len(), &workdir_real) {
            eprintln!(
                "fatal: {spec}: '{spec}' is outside repository at '{}'",
                workdir_real.display()
            );
            return Ok(ExitCode::from(128));
        }
    }

    // With a pathspec, git removes everything it matches, directories included.
    if !pathspecs.is_empty() {
        remove_directories = true;
    }

    let index = repo.index_or_load_from_head_or_empty()?;

    // A directory only exists in the worktree because it holds tracked files or
    // because it is untracked/ignored; if nothing tracked lives under the prefix
    // the current directory is itself a deletion candidate, which git reports in
    // a shape we do not reproduce.
    if !prefix_parts.is_empty() {
        let mut under_prefix = prefix.clone();
        if under_prefix.last() != Some(&b'/') {
            under_prefix.push(b'/');
        }
        let backing = index.path_backing();
        let any_tracked = index
            .entries()
            .iter()
            .any(|e| e.path_in(backing).starts_with_str(&under_prefix));
        if !any_tracked {
            bail!(
                "cleaning from inside a directory that is itself a deletion candidate is not supported"
            );
        }
    }

    // Emission modes, chosen to mirror git's `dir.c` flags for each combination:
    //   * `-X` keeps untracked entries un-collapsed so an untracked directory
    //     never swallows the ignored files inside it (which are the targets).
    //   * `for_deletion` is only set with `-d`; it is what stops a directory
    //     from collapsing when it also holds files we would not delete, so that
    //     `git clean -nd` reports `dir/file` instead of `dir/`.
    //   * with `-x` and `-e` the only ignore patterns left are the command-line ones,
    //     and everything they match is a file to *keep*. The walk is told so by making
    //     those patterns *precious*, which is what stops a directory holding one of
    //     them from collapsing over it — git reports `dir/deletable` there, never
    //     `dir/`, because removing the directory would take the excluded file with it.
    //
    // `-X` keeps git's standard excludes (they are what it deletes); only `-x` drops
    // them and leaves the `-e` patterns as the sole protection.
    let cmdl_excludes_only = ignored_too && !ignored_only && !excludes.is_empty();
    let mut options = repo
        .dirwalk_options()?
        .empty_patterns_match_prefix(true)
        .emit_untracked(if ignored_only {
            gix::dir::walk::EmissionMode::Matching
        } else {
            gix::dir::walk::EmissionMode::CollapseDirectory
        })
        .emit_ignored(
            (ignored_too || ignored_only)
                .then_some(gix::dir::walk::EmissionMode::CollapseDirectory),
        )
        .emit_empty_directories(remove_directories);
    options = options.for_deletion(
        remove_directories
            .then_some(gix::dir::walk::ForDeletionMode::IgnoredDirectoriesCanHideNestedRepositories),
    );
    let entries = walk(&repo, index, &pathspecs, &excludes, cmdl_excludes_only, options)?;

    // (sort key = repo-relative path with a trailing '/' for directories, repo-relative path, is_dir)
    let mut targets: Vec<(BString, BString, bool)> = Vec::new();
    for entry in entries {
        match entry.status {
            gix::dir::entry::Status::Pruned | gix::dir::entry::Status::Tracked => continue,
            gix::dir::entry::Status::Untracked if ignored_only => continue,
            // With `-x` and `-e` the stack holds nothing but the command-line patterns,
            // so an ignored entry is one the caller asked to keep, not one to delete.
            gix::dir::entry::Status::Ignored(_) if cmdl_excludes_only => continue,
            gix::dir::entry::Status::Ignored(_) if !(ignored_too || ignored_only) => continue,
            _ => {}
        }
        if entry.property == Some(gix::dir::entry::Property::EmptyDirectoryAndCWD) {
            bail!(
                "cleaning from inside a directory that is itself a deletion candidate is not supported"
            );
        }

        let is_repo = entry.disk_kind == Some(gix::dir::entry::Kind::Repository);
        let is_dir = is_repo || entry.disk_kind == Some(gix::dir::entry::Kind::Directory);
        if is_dir && !remove_directories {
            continue;
        }
        // A nested repository is only removed with a second -f, as in git.
        if is_repo && force < 2 {
            continue;
        }

        let mut key = entry.rela_path.clone();
        if is_dir {
            key.push(b'/');
        }
        targets.push((key, entry.rela_path, is_dir));
    }

    targets.sort_by(|a, b| a.0.cmp(&b.0));

    // `-i` drives git's prompt loop over the sorted candidate set, narrowing it
    // to the survivors that the removal pass below then deletes (or, with `-n`,
    // reports). With nothing to clean the loop is a no-op, matching git.
    if interactive {
        targets = interactive_main_loop(&repo, targets, &prefix_parts, colopts, &colors);
    }

    let mut out = String::new();
    let mut failed = false;
    for (key, rela_path, is_dir) in targets {
        let shown = quote_path(relative_to_prefix(key.as_bstr(), &prefix_parts));

        if dry_run {
            if !quiet {
                out.push_str(&format!("Would remove {shown}\n"));
            }
            continue;
        }

        let Some(abs) = repo.workdir_path(&rela_path) else {
            continue;
        };
        let res = if is_dir {
            std::fs::remove_dir_all(&abs)
        } else {
            std::fs::remove_file(&abs)
        };
        match res {
            Ok(()) => {
                if !quiet {
                    out.push_str(&format!("Removing {shown}\n"));
                }
            }
            Err(err) => {
                print!("{out}");
                out.clear();
                eprintln!("warning: failed to remove {shown}: {}", errno_text(&err));
                failed = true;
            }
        }
    }
    print!("{out}");

    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Run the directory walk and collect every emitted entry.
///
/// Without `-e` patterns this is `Repository::dirwalk_iter`. With them the walk
/// has to be driven directly, because the convenience entry points hard-code an
/// empty override group for the exclude stack; the overrides are consulted ahead
/// of every `.gitignore`, which is where git puts its `EXC_CMDL` patterns.
fn walk(
    repo: &gix::Repository,
    index: gix::worktree::IndexPersistedOrInMemory,
    pathspecs: &[String],
    excludes: &[String],
    // `-x`: `cmd_clean()` skips `setup_standard_excludes()`, so no `.gitignore`, no
    // `info/exclude` and no `core.excludesFile` is read — but the `EXC_CMDL` group the
    // `-e` patterns live in is added either way, and it is the only thing left that can
    // hold a file back from deletion.
    cmdl_excludes_only: bool,
    options: gix::dirwalk::Options,
) -> Result<Vec<gix::dir::Entry>> {
    let patterns: Vec<BString> = pathspecs
        .iter()
        .map(|s| BString::from(s.clone().into_bytes()))
        .collect();

    if excludes.is_empty() {
        let mut iter = repo.dirwalk_iter(index, patterns, Default::default(), options)?;
        let mut entries = Vec::new();
        for item in iter.by_ref() {
            entries.push(item?.entry);
        }
        return Ok(entries);
    }

    let state: &gix::index::State = &index;
    let mut parse = gix::ignore::search::Ignore {
        support_precious: repo
            .config_snapshot()
            .boolean("gitoxide.parsePrecious")
            .unwrap_or(false),
    };
    // Under `-x` a `-e` match is the one thing that keeps a file, so the patterns enter
    // the walk as *precious* (gitoxide's `$` marker): a directory holding a precious
    // entry never collapses while walking for deletion, and one that holds nothing else
    // is reported as ignored rather than as a deletion target.
    let overrides = if cmdl_excludes_only {
        parse.support_precious = true;
        // A negated pattern takes protection *away* again, so it stays a plain negation:
        // `$` and `!` cannot be combined, and a `!`-line is only ever consulted to undo
        // an earlier match.
        gix::ignore::Search::from_overrides(
            excludes.iter().map(|p| {
                if p.starts_with('!') {
                    p.clone()
                } else {
                    format!("${p}")
                }
            }),
            parse,
        )
    } else {
        gix::ignore::Search::from_overrides(excludes.iter().cloned(), parse)
    };
    let source = gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped;
    let mut exclude_stack = if cmdl_excludes_only {
        // Command-line patterns only: empty globals stand in for the `info/exclude` and
        // `core.excludesFile` git does not read under `-x`, and an empty per-directory
        // name stops the stack from looking for `.gitignore` (the same lever
        // `--no-exclude-per-directory` pulls in `ls-files`).
        //
        // The case sensitivity is the repository's own, as `Repository::excludes()`
        // resolves it — `core.ignoreCase` as probed for this filesystem.
        let case = if repo.filesystem_options()?.ignore_case {
            gix::glob::pattern::Case::Fold
        } else {
            gix::glob::pattern::Case::Sensitive
        };
        let ignore = gix::worktree::stack::state::Ignore::new(
            overrides,
            Default::default(),
            Some("".into()),
            source,
            parse,
        );
        let state_stack = gix::worktree::stack::State::IgnoreStack(ignore);
        let id_mappings = state_stack.id_mappings_from_index(state, state.path_backing(), case);
        gix::worktree::Stack::new(
            repo.workdir().unwrap_or_else(|| repo.git_dir()),
            state_stack,
            case,
            Vec::with_capacity(512),
            id_mappings,
        )
    } else {
        repo.excludes(state, Some(overrides), source)?.detach()
    };
    let gix::PathspecDetached {
        mut search,
        mut stack,
        odb,
    } = repo
        .pathspec(
            true,
            patterns.iter(),
            true,
            state,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
        )?
        .detach()?;

    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("this operation requires a work tree"))?;
    let git_dir_realpath = gix::path::realpath(repo.git_dir())?;
    let fs_caps = repo.filesystem_options()?;
    let accelerate = fs_caps.ignore_case.then(|| state.prepare_icase_backing());

    let mut opts: gix::dir::walk::Options<'_> = options.into();
    // Linked worktrees inside our own worktree are marked tracked so a deletion
    // walk cannot wander into them, exactly as `Repository::dirwalk` does.
    let worktree_dirs: std::collections::BTreeSet<BString> = if opts.for_deletion.is_some() {
        let real_workdir = gix::path::realpath(workdir)?;
        repo.worktrees()?
            .into_iter()
            .filter_map(|proxy| proxy.base().ok())
            .filter_map(|base| base.strip_prefix(&real_workdir).map(ToOwned::to_owned).ok())
            .map(|rela| {
                gix::path::to_unix_separators_on_windows(gix::path::into_bstr(rela)).into_owned()
            })
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        std::collections::BTreeSet::new()
    };
    if !worktree_dirs.is_empty() {
        opts.worktree_relative_worktree_dirs = Some(&worktree_dirs);
    }

    let mut pathspec_attributes = |relative_path: &BStr,
                                   case: gix::pathspec::attributes::glob::pattern::Case,
                                   is_dir: bool,
                                   out: &mut gix::pathspec::attributes::search::Outcome|
     -> bool {
        let stack = stack
            .as_mut()
            .expect("only called when pathspecs use attributes");
        let mode = if is_dir {
            gix::index::entry::Mode::DIR
        } else {
            gix::index::entry::Mode::FILE
        };
        stack
            .set_case(case)
            .at_entry(relative_path, Some(mode), &odb)
            .is_ok_and(|platform| platform.matching_attributes(out))
    };

    let mut collect = Collect(Vec::new());
    gix::dir::walk(
        workdir,
        gix::dir::walk::Context {
            should_interrupt: None,
            git_dir_realpath: git_dir_realpath.as_ref(),
            current_dir: repo.current_dir(),
            index: state,
            ignore_case_index_lookup: accelerate.as_ref(),
            pathspec: &mut search,
            pathspec_attributes: &mut pathspec_attributes,
            excludes: Some(&mut exclude_stack),
            objects: &repo.objects,
            explicit_traversal_root: None,
        },
        opts,
        &mut collect,
    )?;
    Ok(collect.0)
}

/// Accumulates every entry the walk emits, in walk order.
struct Collect(Vec<gix::dir::Entry>);

impl gix::dir::walk::Delegate for Collect {
    fn emit(
        &mut self,
        entry: gix::dir::EntryRef<'_>,
        _collapsed_directory_status: Option<gix::dir::entry::Status>,
    ) -> gix::dir::walk::Action {
        self.0.push(entry.to_owned());
        std::ops::ControlFlow::Continue(())
    }
}

/// Whether a pathspec resolves outside the worktree, which git rejects with
/// `'<spec>' is outside repository at '<worktree>'`.
///
/// Relative specs are resolved against the repository prefix by counting
/// components, so `..` from the top level escapes while `./src/../src` does not.
/// Absolute specs must live under the worktree. Specs carrying magic (`:/…`,
/// `:(top)…`) are resolved by the pathspec parser instead and are not checked.
fn pathspec_leaves_worktree(spec: &str, prefix_depth: usize, workdir_real: &std::path::Path) -> bool {
    if spec.starts_with(':') {
        return false;
    }
    if spec.starts_with('/') {
        let mut normalized = std::path::PathBuf::new();
        for comp in std::path::Path::new(spec).components() {
            match comp {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => {}
                other => normalized.push(other),
            }
        }
        return !normalized.starts_with(workdir_real);
    }

    let mut depth = prefix_depth as i64;
    for comp in spec.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

/// Render a repository-relative path as git does for display: relative to the
/// current working directory, walking up with `../` for each prefix component
/// the path does not share.
fn relative_to_prefix(path: &BStr, prefix_parts: &[&[u8]]) -> BString {
    let comps: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
    let mut shared = 0;
    while shared < prefix_parts.len() && shared < comps.len() && prefix_parts[shared] == comps[shared]
    {
        shared += 1;
    }

    let mut outp = BString::default();
    for _ in shared..prefix_parts.len() {
        outp.extend_from_slice(b"../");
    }
    for (i, c) in comps[shared..].iter().enumerate() {
        if i > 0 {
            outp.push(b'/');
        }
        outp.extend_from_slice(c);
    }
    outp
}

/// The message text of an I/O error without Rust's ` (os error N)` suffix, so
/// the warning reads like git's `strerror` output.
fn errno_text(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    crate::quote::quoted_name_string(path.as_ref())
}

// ---------------------------------------------------------------------------
// `-i` / `--interactive`: a faithful port of `builtin/clean.c`'s prompt loop.
//
// A candidate is the same `(sort-key, repo-relative path, is_dir)` triple the
// non-interactive path builds. `interactive_main_loop` mutates the live list in
// place and returns the survivors, which `clean` then deletes (or, under `-n`,
// reports) exactly as before.
// ---------------------------------------------------------------------------

/// `builtin/clean.c`'s `clean_colors[]`, resolved from `color.interactive` and
/// the `color.interactive.<slot>` overrides. Every field is the empty string
/// when coloring is off, matching `clean_get_color()`, so the call sites can
/// interpolate unconditionally.
///
/// `git clean -i` is the only command that reads the full six-slot table:
/// `add -i`/`add -p` share `header`/`help`/`prompt`/`error` but hard-code their
/// reset, and neither reads `plain`.
pub(crate) struct CleanColors {
    /// `color.interactive.error`, default bold red — `Huh (…)?`, the
    /// "cannot find items matched by" warning, "No more files to clean".
    error: String,
    /// `color.interactive.header`, default bold — `Would remove …` and
    /// `*** Commands ***`.
    header: String,
    /// `color.interactive.help`, default bold red — both help screens.
    help: String,
    /// `color.interactive.prompt`, default bold blue — every prompt, and the
    /// hotkey letter highlighted inside each menu entry.
    prompt: String,
    /// `color.interactive.reset`, default `\e[m` — the sequence that closes
    /// every one of the above. git makes it a configurable slot of its own, so
    /// setting it replaces the reset everywhere in the interactive UI.
    reset: String,
}

impl CleanColors {
    /// `color_interactive_slots[]` — every name `git_clean_config()` accepts
    /// under `color.interactive.`. `plain` is in the table because the callback
    /// parses (and so validates) it, even though `builtin/clean.c` never prints
    /// the slot.
    pub(crate) const SLOTS: [&'static str; 6] =
        ["error", "header", "help", "plain", "prompt", "reset"];

    /// Resolve the table to SGR sequences. The validation half of
    /// `git_clean_config()` lives in `color::first_invalid_slot`, which the
    /// caller runs first.
    fn resolve(repo: &gix::Repository) -> Self {
        let on = super::color::want_color_stdout(repo, "interactive");
        let snap = repo.config_snapshot();
        let slot = |name: &str, default: &str| -> String {
            if !on {
                return String::new();
            }
            let spec = snap
                .string(&format!("color.interactive.{name}"))
                .map(|v| v.to_string());
            match spec {
                // git's `reset` slot defaults to the literal `\e[m`, which the
                // spec parser would render from the `reset` attribute as `\e[0m`.
                None if name == "reset" => "\x1b[m".to_string(),
                None => super::color::parse_color_spec(default).unwrap_or_default(),
                Some(s) => super::color::parse_color_spec(&s).unwrap_or_default(),
            }
        };
        CleanColors {
            error: slot("error", "bold red"),
            header: slot("header", "bold"),
            help: slot("help", "bold red"),
            prompt: slot("prompt", "bold blue"),
            reset: slot("reset", "reset"),
        }
    }
}

/// `interactive_main_loop`'s `struct menu_item menus[]`: hotkey letter and title,
/// in the order the menu numbers them.
const MENU: [(u8, &str); 6] = [
    (b'c', "clean"),
    (b'f', "filter by pattern"),
    (b's', "select by numbers"),
    (b'a', "ask each"),
    (b'q', "quit"),
    (b'h', "help"),
];

const PROMPT_HELP_SINGLETON: &str = concat!(
    "Prompt help:\n",
    "1          - select a numbered item\n",
    "foo        - select item based on unique prefix\n",
    "           - (empty) select nothing\n",
);

const PROMPT_HELP_MULTI: &str = concat!(
    "Prompt help:\n",
    "1          - select a single item\n",
    "3-5        - select a range of items\n",
    "2-3,6-9    - select multiple ranges\n",
    "foo        - select item based on unique prefix\n",
    "-...       - unselect specified items\n",
    "*          - choose all items\n",
    "           - (empty) finish selecting\n",
);

/// Read one line from `stdin` the way git's `git_read_line_interactively` does:
/// flush stdout first, strip a trailing `\n` (and a preceding `\r`), and report
/// end-of-input as `None` (git's `EOF`).
fn read_line_interactively(stdin: &mut impl std::io::BufRead) -> Option<String> {
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut buf = Vec::new();
    if stdin.read_until(b'\n', &mut buf).ok()? == 0 {
        return None;
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The `struct column_options` both of `clean.c`'s printers `memset` to zero and
/// then fill in identically: `indent = "  "`, `padding = 2`, terminal width.
fn clean_column_options() -> super::column::ColumnOptions {
    super::column::ColumnOptions {
        width: 0,
        padding: 2,
        indent: Some("  ".to_owned()),
        nl: None,
    }
}

/// Render `items` the way `pretty_print_dels()` does: the layout bits come from
/// `column.ui` / `column.clean` (git's `git_column_config(var, value, "clean",
/// &colopts)`), but the *enable* bit is forced on, so `column.clean=never` still
/// prints a table while `column.clean=plain` prints one item per line and
/// `column.clean=column` fills down columns instead of across rows.
fn print_dels_columns(colopts: u32, items: &[String]) -> String {
    render(super::column::force_enabled(colopts), items)
}

/// Render `items` the way `pretty_print_menus()` does: a `local_colopts` of
/// `COL_ENABLED | COL_ROW` that ignores `column.*` entirely.
fn print_menu_columns(items: &[String]) -> String {
    render(super::column::ENABLED_ROW, items)
}

/// Hand `items` to the shared `print_columns()` port. Every string is ASCII once
/// C-quoted, so the byte-length display-width assumption inside it holds.
fn render(colopts: u32, items: &[String]) -> String {
    let cells: Vec<Vec<u8>> = items.iter().map(|s| s.as_bytes().to_vec()).collect();
    let bytes = super::column::layout(&cells, colopts, &clean_column_options());
    String::from_utf8_lossy(&bytes).into_owned()
}


/// The display strings for the current del-list: each candidate rendered exactly
/// as the removal pass would (`relative_to_prefix` then `quote_path`).
fn shown_paths(del: &[(BString, BString, bool)], prefix_parts: &[&[u8]]) -> Vec<String> {
    del.iter()
        .map(|(key, _, _)| quote_path(relative_to_prefix(key.as_bstr(), prefix_parts)))
        .collect()
}

/// C `atoi`: skip leading blanks, an optional sign, then consume leading digits.
fn atoi(s: &str) -> i64 {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let mut n: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        n = n * 10 + i64::from(b[i] - b'0');
        i += 1;
    }
    if neg {
        -n
    } else {
        n
    }
}

/// git's `parse_choice` classification: is the token a bare number, or a range
/// (`a-b` / `a-`)? A second `-`, or any non-digit, makes it neither.
fn classify(s: &str) -> (bool, bool) {
    let mut is_range = false;
    let mut is_number = true;
    for &c in s.as_bytes() {
        if c == b'-' {
            if !is_range {
                is_range = true;
                is_number = false;
            } else {
                is_number = false;
                is_range = false;
                break;
            }
        } else if !c.is_ascii_digit() {
            is_number = false;
            is_range = false;
            break;
        }
    }
    (is_number, is_range)
}

/// git's `parse_choice`: split `input` (on `\n` for singleton menus, on `, `/
/// space otherwise), resolve each token to a 1-based index or range, and toggle
/// the corresponding `chosen` slots. Unresolvable tokens print `Huh (<tok>)?`.
fn parse_choice(
    nr: usize,
    is_single: bool,
    input: &str,
    chosen: &mut [bool],
    find: impl Fn(&str) -> i64,
    c: &CleanColors,
) {
    let is_sep = |c: char| {
        if is_single {
            c == '\n'
        } else {
            c == ',' || c == ' '
        }
    };
    for raw in input.split(is_sep) {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }
        // A leading '-' unchooses the token's items.
        let (choose, s) = match s.strip_prefix('-') {
            Some(rest) => (false, rest),
            None => (true, s),
        };
        let (is_number, is_range) = classify(s);
        let (bottom, top): (i64, i64) = if is_number {
            let b = atoi(s);
            (b, b)
        } else if is_range {
            let b = atoi(s);
            let after = &s[s.find('-').unwrap() + 1..];
            let t = if after.is_empty() {
                nr as i64
            } else {
                atoi(after)
            };
            (b, t)
        } else if s == "*" {
            (1, nr as i64)
        } else {
            let b = find(s);
            (b, b)
        };
        if top <= 0
            || bottom <= 0
            || top > nr as i64
            || bottom > top
            || (is_single && bottom != top)
        {
            println!("{}Huh ({s})?", c.error);
            print!("{}", c.reset);
            continue;
        }
        for i in bottom..=top {
            chosen[(i - 1) as usize] = choose;
        }
    }
}

/// git's `find_unique` for the command menu: a length-1 token matches a hotkey,
/// otherwise a case-insensitive unique title prefix. Returns a 1-based index, 0
/// for none/ambiguous, or -1 for an ambiguous hotkey (both rejected downstream).
fn find_unique_menu(choice: &str) -> i64 {
    let len = choice.len();
    let cb = choice.as_bytes();
    let mut found: i64 = 0;
    for (i, (hotkey, title)) in MENU.iter().enumerate() {
        if len == 1 && cb[0] == *hotkey {
            found = (i + 1) as i64;
            break;
        }
        if title.len() >= len && title.as_bytes()[..len].eq_ignore_ascii_case(cb) {
            if found != 0 {
                if len == 1 {
                    found = -1;
                } else {
                    found = 0;
                    break;
                }
            } else {
                found = (i + 1) as i64;
            }
        }
    }
    found
}

/// git's `find_unique` for a string list: a case-insensitive unique prefix of a
/// displayed item. Returns a 1-based index, or 0 for none/ambiguous.
fn find_unique_strings(items: &[String], choice: &str) -> i64 {
    let len = choice.len();
    let cb = choice.as_bytes();
    let mut found: i64 = 0;
    for (i, s) in items.iter().enumerate() {
        if s.len() >= len && s.as_bytes()[..len].eq_ignore_ascii_case(cb) {
            if found != 0 {
                found = 0;
                break;
            }
            found = (i + 1) as i64;
        }
    }
    found
}

/// git's `help_cmd`: the command reference, closed by the trailing newline
/// `printf_ln` adds and then by the reset slot.
fn help_cmd(c: &CleanColors) {
    print!(
        concat!(
            "{}",
            "clean               - start cleaning\n",
            "filter by pattern   - exclude items from deletion\n",
            "select by numbers   - select items to be deleted by numbers\n",
            "ask each            - confirm each deletion (like \"rm -i\")\n",
            "quit                - stop cleaning\n",
            "help                - this screen\n",
            "?                   - help for prompt selection\n",
            "{}",
        ),
        c.help, c.reset
    );
}

/// git's singleton `list_and_choose` over the command menu. Reprints the header,
/// the highlighted menu and the `What now> ` prompt until a command resolves;
/// `?` prints prompt help, an empty line re-prompts, and EOF returns `None`.
fn list_and_choose_menu(stdin: &mut impl std::io::BufRead, c: &CleanColors) -> Option<usize> {
    loop {
        // `printf_ln("%s%s%s", HEADER, header, RESET)` — the reset precedes the
        // newline here, unlike the `Would remove …` banner.
        println!("{}*** Commands ***{}", c.header, c.reset);
        let disp: Vec<String> = MENU
            .iter()
            .enumerate()
            .map(|(i, (hotkey, title))| {
                format!(" {:2}: {}", i + 1, highlight_hotkey(title, *hotkey as char, c))
            })
            .collect();
        print!("{}", print_menu_columns(&disp));
        print!("{}What now> {}", c.prompt, c.reset);
        let line = read_line_interactively(stdin)?;
        if line == "?" {
            print!("{}{PROMPT_HELP_SINGLETON}{}", c.help, c.reset);
            continue;
        }
        let mut chosen = [false; 6];
        parse_choice(MENU.len(), true, &line, &mut chosen, find_unique_menu, c);
        if let Some(idx) = chosen.iter().position(|&c| c) {
            return Some(idx);
        }
    }
}

/// git's `print_highlight_menu_stuff`: paint the first occurrence of the entry's
/// hotkey letter with the prompt slot. With coloring off this is the title
/// unchanged, so the plain layout (and its column widths) is untouched.
fn highlight_hotkey(title: &str, hotkey: char, c: &CleanColors) -> String {
    if c.prompt.is_empty() && c.reset.is_empty() {
        return title.to_string();
    }
    match title.find(hotkey) {
        Some(at) => format!(
            "{}{}{}{}{}",
            &title[..at],
            c.prompt,
            hotkey,
            c.reset,
            &title[at + hotkey.len_utf8()..]
        ),
        None => title.to_string(),
    }
}

/// git's multi-choice `list_and_choose` over a string list. Returns the selected
/// 1-based-minus-one indices in ascending order; an empty line finishes with the
/// current selection, `?` prints prompt help, and EOF discards all selections
/// (git returns a bare `EOF`).
fn list_and_choose_strings(
    shown: &[String],
    prompt: &str,
    stdin: &mut impl std::io::BufRead,
    c: &CleanColors,
) -> Vec<usize> {
    let nr = shown.len();
    let mut chosen = vec![false; nr];
    loop {
        let disp: Vec<String> = shown
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}{:2}: {}", if chosen[i] { "*" } else { " " }, i + 1, s))
            .collect();
        print!("{}", print_menu_columns(&disp));
        print!("{}{prompt}>> {}", c.prompt, c.reset);
        let line = match read_line_interactively(stdin) {
            Some(l) => l,
            None => {
                // EOF: git returns no selection, discarding anything chosen so far.
                chosen.iter_mut().for_each(|c| *c = false);
                break;
            }
        };
        if line == "?" {
            print!("{}{PROMPT_HELP_MULTI}{}", c.help, c.reset);
            continue;
        }
        if line.is_empty() {
            break;
        }
        parse_choice(
            nr,
            false,
            &line,
            &mut chosen,
            |s| find_unique_strings(shown, s),
            c,
        );
    }
    (0..nr).filter(|&i| chosen[i]).collect()
}

/// git's `select_by_numbers_cmd`: keep only the chosen candidates, drop the rest.
fn select_by_numbers_cmd(
    del: &mut Vec<(BString, BString, bool)>,
    prefix_parts: &[&[u8]],
    stdin: &mut impl std::io::BufRead,
    c: &CleanColors,
) {
    let shown = shown_paths(del, prefix_parts);
    let keep: std::collections::HashSet<usize> =
        list_and_choose_strings(&shown, "Select items to delete", stdin, c)
            .into_iter()
            .collect();
    let mut i = 0usize;
    del.retain(|_| {
        let k = keep.contains(&i);
        i += 1;
        k
    });
}

/// git's `ask_each_cmd`: confirm each candidate `Remove <path> [y/N]?`. Only a
/// case-insensitive prefix of "yes" keeps it (so it is deleted); EOF spares the
/// rest.
fn ask_each_cmd(
    del: &mut Vec<(BString, BString, bool)>,
    prefix_parts: &[&[u8]],
    stdin: &mut impl std::io::BufRead,
) {
    let mut eof = false;
    let mut confirm = String::new();
    del.retain(|(key, _, _)| {
        if !eof {
            let qname = quote_path(relative_to_prefix(key.as_bstr(), prefix_parts));
            print!("Remove {qname} [y/N]? ");
            match read_line_interactively(stdin) {
                Some(l) => confirm = l,
                None => {
                    println!();
                    eof = true;
                    confirm.clear();
                }
            }
        }
        let a = confirm.as_bytes();
        !a.is_empty() && a.len() <= 3 && b"yes"[..a.len()].eq_ignore_ascii_case(a)
    });
}

/// git's `filter_by_patterns_cmd`: read space-separated gitignore patterns and
/// drop every candidate they match, looping until an empty line (or EOF). When a
/// round matches nothing it warns; each non-empty round reprints the survivors.
fn filter_by_patterns_cmd(
    repo: &gix::Repository,
    del: &mut Vec<(BString, BString, bool)>,
    prefix_parts: &[&[u8]],
    stdin: &mut impl std::io::BufRead,
    colopts: u32,
    c: &CleanColors,
) {
    let parse = gix::ignore::search::Ignore {
        support_precious: repo
            .config_snapshot()
            .boolean("gitoxide.parsePrecious")
            .unwrap_or(false),
    };
    // git's `changed` starts truthy so the first round always prints the list.
    let mut changed: i64 = -1;
    loop {
        if del.is_empty() {
            break;
        }
        if changed != 0 {
            let shown = shown_paths(del, prefix_parts);
            print!("{}", print_dels_columns(colopts, &shown));
        }
        print!("{}Input ignore patterns>> {}", c.prompt, c.reset);
        let line = match read_line_interactively(stdin) {
            Some(l) => l,
            None => {
                println!();
                String::new()
            }
        };
        if line.is_empty() {
            break;
        }
        let patterns: Vec<String> = line
            .split(' ')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        let search = gix::ignore::Search::from_overrides(patterns.iter().cloned(), parse);
        changed = 0;
        del.retain(|(_, rela, is_dir)| {
            let excluded = search
                .pattern_matching_relative_path(
                    rela.as_bstr(),
                    Some(*is_dir),
                    gix::glob::pattern::Case::Sensitive,
                )
                .is_some_and(|m| !m.pattern.is_negative());
            if excluded {
                changed += 1;
            }
            !excluded
        });
        if changed == 0 {
            println!("{}WARNING: Cannot find items matched by: {line}", c.error);
            print!("{}", c.reset);
        }
    }
}

/// git's `interactive_main_loop`: show the del-list, run the command menu, and
/// dispatch until a command finishes cleaning (`clean`/`ask each`), the list is
/// emptied, or the user quits. Returns the survivors for the caller to remove.
fn interactive_main_loop(
    repo: &gix::Repository,
    mut del: Vec<(BString, BString, bool)>,
    prefix_parts: &[&[u8]],
    colopts: u32,
    c: &CleanColors,
) -> Vec<(BString, BString, bool)> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();

    while !del.is_empty() {
        // `clean_print_color(HEADER)`, `printf_ln(…)`, `clean_print_color(RESET)`
        // — so the reset lands *after* the newline, not before it.
        if del.len() == 1 {
            println!("{}Would remove the following item:", c.header);
        } else {
            println!("{}Would remove the following items:", c.header);
        }
        print!("{}", c.reset);
        let shown = shown_paths(&del, prefix_parts);
        print!("{}", print_dels_columns(colopts, &shown));

        match list_and_choose_menu(&mut stdin, c) {
            // EOF at the command prompt behaves exactly like `quit`.
            None | Some(4) => {
                del.clear();
                // `quit_cmd()` is one of the few notices git leaves unpainted.
                println!("Bye.");
                break;
            }
            // clean: remove everything still in the list.
            Some(0) => break,
            // filter by pattern.
            Some(1) => {
                filter_by_patterns_cmd(repo, &mut del, prefix_parts, &mut stdin, colopts, c);
                if del.is_empty() {
                    println!("{}No more files to clean, exiting.", c.error);
                    print!("{}", c.reset);
                    break;
                }
            }
            // select by numbers.
            Some(2) => {
                select_by_numbers_cmd(&mut del, prefix_parts, &mut stdin, c);
                if del.is_empty() {
                    println!("{}No more files to clean, exiting.", c.error);
                    print!("{}", c.reset);
                    break;
                }
            }
            // ask each, then remove the confirmed survivors.
            Some(3) => {
                ask_each_cmd(&mut del, prefix_parts, &mut stdin);
                break;
            }
            // help, then re-display and loop.
            Some(5) => help_cmd(c),
            Some(_) => unreachable!("the command menu has exactly six entries"),
        }
    }
    del
}
