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
///   * `-n`/`--dry-run`   — list as `Would remove <path>` without touching disk.
///   * `-f`/`--force`     — actually delete; `-ff` also deletes nested
///                          repositories, which a single `-f` refuses to touch.
///   * `-d`               — recurse into untracked directories and remove them
///                          (including empty ones) as single entries.
///   * `-q`/`--quiet`     — suppress the per-path lines, keep warnings.
///   * `-x`               — also remove ignored files.
///   * `-X`               — remove *only* ignored files.
///   * `-e`/`--exclude=<pattern>` — extra ignore patterns, layered above every
///                          `.gitignore` exactly like git's `EXC_CMDL` group, so
///                          with `-X` they become removal targets and otherwise
///                          they shield paths from removal.
///   * `--no-quiet`, `--no-dry-run`, `--no-force`, `--no-interactive`, and
///     unique-prefix abbreviations such as `--dry` or `--no-dr`.
///   * `--` and `<pathspec>...` — as with git, any pathspec implies `-d`, and
///     options may be given after pathspecs.
///   * grouped short flags (`-ndx`, `-ffd`, …).
///
/// Diagnostics follow git: an unknown option or a missing option value exits
/// 129, a pathspec that leaves the worktree and the `clean.requireForce` refusal
/// exit 128, in the same order git checks them (force refusal first, then
/// `-x`/`-X`, then pathspec validation).
///
/// Paths are sorted by their repository-relative form (directories carrying a
/// trailing `/`) and then rendered relative to the current working directory,
/// C-quoted exactly as git's `quote_path` does.
///
/// Faithfully unsupported — this `bail!`s rather than emit wrong results:
/// `-i`/`--interactive` (git's own prompt loop), and running from inside a
/// directory that is itself a deletion candidate, where git prints an unsorted,
/// readdir-ordered `./`-prefixed listing after `Refusing to remove current
/// working directory`.
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

    if interactive {
        bail!("unsupported flag \"-i\": git's interactive prompt loop is not ported");
    }

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
    for spec in &pathspecs {
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

    let entries = walk(&repo, index, &pathspecs, &excludes, options)?;

    // (sort key = repo-relative path with a trailing '/' for directories, repo-relative path, is_dir)
    let mut targets: Vec<(BString, BString, bool)> = Vec::new();
    for entry in entries {
        match entry.status {
            gix::dir::entry::Status::Pruned | gix::dir::entry::Status::Tracked => continue,
            gix::dir::entry::Status::Untracked if ignored_only => continue,
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
    let parse = gix::ignore::search::Ignore {
        support_precious: repo
            .config_snapshot()
            .boolean("gitoxide.parsePrecious")
            .unwrap_or(false),
    };
    let overrides = gix::ignore::Search::from_overrides(excludes.iter().cloned(), parse);
    let mut exclude_stack = repo
        .excludes(
            state,
            Some(overrides),
            gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
        )?
        .detach();
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
