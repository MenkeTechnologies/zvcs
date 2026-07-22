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
///   * `-i`/`--interactive` — git's prompt loop (`clean`, `filter by pattern`,
///                          `select by numbers`, `ask each`, `quit`, `help`),
///                          reading selections from stdin; the column layout,
///                          menu wording, `Huh (…)?` diagnostics and per-command
///                          semantics are ported from `builtin/clean.c`.
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
            eprintln!("fatal: {}", git_pathspec_error(spec, &err));
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

    // `-i` drives git's prompt loop over the sorted candidate set, narrowing it
    // to the survivors that the removal pass below then deletes (or, with `-n`,
    // reports). With nothing to clean the loop is a no-op, matching git.
    if interactive {
        targets = interactive_main_loop(&repo, targets, &prefix_parts);
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

/// Translate a gitoxide pathspec parse error into git's exact `fatal:` message
/// text (the `fatal: ` prefix is added by the caller). Every one of these is an
/// exit-128 fatal in git; gitoxide's own wording differs, so map each variant to
/// the string git's `pathspec.c` / `attr.c` print for the same input.
fn git_pathspec_error(spec: &str, err: &gix::pathspec::parse::Error) -> String {
    use gix::pathspec::parse::Error;
    match err {
        Error::EmptyString => {
            "empty string is not a valid pathspec. please use . instead if you meant to match all paths"
                .to_string()
        }
        Error::InvalidKeyword { keyword } => {
            format!("Invalid pathspec magic '{keyword}' in '{spec}'")
        }
        Error::Unimplemented { short_keyword } => {
            format!("Unimplemented pathspec magic '{short_keyword}' in '{spec}'")
        }
        Error::MissingClosingParenthesis => {
            format!("Missing ')' at the end of pathspec magic in '{spec}'")
        }
        Error::InvalidAttribute { attribute } => format!("invalid attribute name {attribute}"),
        Error::InvalidAttributeValue { character } => {
            format!("cannot use '{character}' for value matching")
        }
        Error::TrailingEscapeCharacter => "cannot use '\\' for value matching".to_string(),
        Error::EmptyAttribute => "attr spec must not be empty".to_string(),
        Error::MultipleAttributeSpecifications => "Only one 'attr:' specification is allowed.".to_string(),
        Error::IncompatibleSearchModes => format!("{spec}: 'literal' and 'glob' are incompatible"),
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

// ---------------------------------------------------------------------------
// `-i` / `--interactive`: a faithful port of `builtin/clean.c`'s prompt loop.
//
// A candidate is the same `(sort-key, repo-relative path, is_dir)` triple the
// non-interactive path builds. `interactive_main_loop` mutates the live list in
// place and returns the survivors, which `clean` then deletes (or, under `-n`,
// reports) exactly as before. Colour is deliberately omitted: git's colour is
// `auto`, so it is off whenever stdout is not a TTY — which is the byte-for-byte
// case that matters for pipes and CI.
// ---------------------------------------------------------------------------

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

/// git's `term_columns()` in the non-TTY case: honour `COLUMNS` if it parses to a
/// positive integer, otherwise fall back to 80.
fn term_columns() -> usize {
    if let Ok(v) = std::env::var("COLUMNS") {
        let n = atoi(&v);
        if n > 0 {
            return n as usize;
        }
    }
    80
}

/// Render `items` with git's `print_columns(COL_ENABLED | COL_ROW)` layout used
/// by `pretty_print_dels`/`print_highlight_menu_stuff`: indent `"  "`, padding 2,
/// row-major fill into `term_columns() - 1` columns. Every string is ASCII once
/// C-quoted, so byte length equals display width.
fn print_columns_row(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    const INDENT: &str = "  ";
    const PADDING: usize = 2;
    let width = term_columns().saturating_sub(1);
    let lens: Vec<usize> = items.iter().map(String::len).collect();
    // `initial_width` in git: the widest cell plus the padding column.
    let colwidth = lens.iter().max().copied().unwrap_or(0) + PADDING;
    let mut cols = width.saturating_sub(INDENT.len()) / colwidth;
    if cols == 0 {
        cols = 1;
    }
    let rows = (items.len() + cols - 1) / cols;
    let mut out = String::new();
    for y in 0..rows {
        for x in 0..cols {
            let i = x + y * cols;
            if i >= items.len() {
                break;
            }
            let newline = x == cols - 1 || i == items.len() - 1;
            if x == 0 {
                out.push_str(INDENT);
            }
            out.push_str(&items[i]);
            if newline {
                out.push('\n');
            } else {
                out.extend(std::iter::repeat(' ').take(colwidth - lens[i]));
            }
        }
    }
    out
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
            print!("Huh ({s})?\n");
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
    const MENU: [(u8, &str); 6] = [
        (b'c', "clean"),
        (b'f', "filter by pattern"),
        (b's', "select by numbers"),
        (b'a', "ask each"),
        (b'q', "quit"),
        (b'h', "help"),
    ];
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
/// `printf_ln` adds.
fn help_cmd() {
    print!(concat!(
        "clean               - start cleaning\n",
        "filter by pattern   - exclude items from deletion\n",
        "select by numbers   - select items to be deleted by numbers\n",
        "ask each            - confirm each deletion (like \"rm -i\")\n",
        "quit                - stop cleaning\n",
        "help                - this screen\n",
        "?                   - help for prompt selection\n",
    ));
}

/// git's singleton `list_and_choose` over the command menu. Reprints the header,
/// the highlighted menu and the `What now> ` prompt until a command resolves;
/// `?` prints prompt help, an empty line re-prompts, and EOF returns `None`.
fn list_and_choose_menu(stdin: &mut impl std::io::BufRead) -> Option<usize> {
    const MENU: [&str; 6] = [
        "clean",
        "filter by pattern",
        "select by numbers",
        "ask each",
        "quit",
        "help",
    ];
    loop {
        print!("*** Commands ***\n");
        let disp: Vec<String> = MENU
            .iter()
            .enumerate()
            .map(|(i, title)| format!(" {:2}: {}", i + 1, title))
            .collect();
        print!("{}", print_columns_row(&disp));
        print!("What now> ");
        let line = read_line_interactively(stdin)?;
        if line == "?" {
            print!("{PROMPT_HELP_SINGLETON}");
            continue;
        }
        let mut chosen = [false; 6];
        parse_choice(MENU.len(), true, &line, &mut chosen, find_unique_menu);
        if let Some(idx) = chosen.iter().position(|&c| c) {
            return Some(idx);
        }
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
) -> Vec<usize> {
    let nr = shown.len();
    let mut chosen = vec![false; nr];
    loop {
        let disp: Vec<String> = shown
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}{:2}: {}", if chosen[i] { "*" } else { " " }, i + 1, s))
            .collect();
        print!("{}", print_columns_row(&disp));
        print!("{prompt}>> ");
        let line = match read_line_interactively(stdin) {
            Some(l) => l,
            None => {
                // EOF: git returns no selection, discarding anything chosen so far.
                chosen.iter_mut().for_each(|c| *c = false);
                break;
            }
        };
        if line == "?" {
            print!("{PROMPT_HELP_MULTI}");
            continue;
        }
        if line.is_empty() {
            break;
        }
        parse_choice(nr, false, &line, &mut chosen, |s| {
            find_unique_strings(shown, s)
        });
    }
    (0..nr).filter(|&i| chosen[i]).collect()
}

/// git's `select_by_numbers_cmd`: keep only the chosen candidates, drop the rest.
fn select_by_numbers_cmd(
    del: &mut Vec<(BString, BString, bool)>,
    prefix_parts: &[&[u8]],
    stdin: &mut impl std::io::BufRead,
) {
    let shown = shown_paths(del, prefix_parts);
    let keep: std::collections::HashSet<usize> =
        list_and_choose_strings(&shown, "Select items to delete", stdin)
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
                    print!("\n");
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
            print!("{}", print_columns_row(&shown));
        }
        print!("Input ignore patterns>> ");
        let line = match read_line_interactively(stdin) {
            Some(l) => l,
            None => {
                print!("\n");
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
            print!("WARNING: Cannot find items matched by: {line}\n");
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
) -> Vec<(BString, BString, bool)> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();

    while !del.is_empty() {
        if del.len() == 1 {
            print!("Would remove the following item:\n");
        } else {
            print!("Would remove the following items:\n");
        }
        let shown = shown_paths(&del, prefix_parts);
        print!("{}", print_columns_row(&shown));

        match list_and_choose_menu(&mut stdin) {
            // EOF at the command prompt behaves exactly like `quit`.
            None | Some(4) => {
                del.clear();
                print!("Bye.\n");
                break;
            }
            // clean: remove everything still in the list.
            Some(0) => break,
            // filter by pattern.
            Some(1) => {
                filter_by_patterns_cmd(repo, &mut del, prefix_parts, &mut stdin);
                if del.is_empty() {
                    print!("No more files to clean, exiting.\n");
                    break;
                }
            }
            // select by numbers.
            Some(2) => {
                select_by_numbers_cmd(&mut del, prefix_parts, &mut stdin);
                if del.is_empty() {
                    print!("No more files to clean, exiting.\n");
                    break;
                }
            }
            // ask each, then remove the confirmed survivors.
            Some(3) => {
                ask_each_cmd(&mut del, prefix_parts, &mut stdin);
                break;
            }
            // help, then re-display and loop.
            Some(5) => help_cmd(),
            Some(_) => unreachable!("the command menu has exactly six entries"),
        }
    }
    del
}
