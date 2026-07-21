use anyhow::Result;
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::prelude::ObjectIdExt;

/// The exact usage block stock `git ls-files` prints on a usage error (exit 129).
const USAGE: &str = "usage: git ls-files [<options>] [<file>...]

    -z                    separate paths with the NUL character
    -t                    identify the file status with tags
    -v                    use lowercase letters for 'assume unchanged' files
    -f                    use lowercase letters for 'fsmonitor clean' files
    -c, --[no-]cached     show cached files in the output (default)
    -d, --[no-]deleted    show deleted files in the output
    -m, --[no-]modified   show modified files in the output
    -o, --[no-]others     show other files in the output
    -i, --[no-]ignored    show ignored files in the output
    -s, --[no-]stage      show staged contents' object name in the output
    -k, --[no-]killed     show files on the filesystem that need to be removed
    --[no-]directory      show 'other' directories' names only
    --[no-]eol            show line endings of files
    --[no-]empty-directory
                          don't show empty directories
    -u, --[no-]unmerged   show unmerged files in the output
    --[no-]resolve-undo   show resolve-undo information
    -x, --exclude <pattern>
                          skip files matching pattern
    -X, --exclude-from <file>
                          read exclude patterns from <file>
    --[no-]exclude-per-directory <file>
                          read additional per-directory exclude patterns in <file>
    --exclude-standard    add the standard git exclusions
    --full-name           make the output relative to the project top directory
    --[no-]recurse-submodules
                          recurse through submodules
    --[no-]error-unmatch  if any <file> is not in the index, treat this as an error
    --[no-]with-tree <tree-ish>
                          pretend that paths removed since <tree-ish> are still present
    --[no-]abbrev[=<n>]   use <n> digits to display object names
    --[no-]debug          show debugging data
    --[no-]deduplicate    suppress duplicate entries
    --[no-]sparse         show sparse directories in the presence of a sparse index
    --format <format>     format to use for the output

";

/// git's `MINIMUM_ABBREV`: an explicit `--abbrev=<n>` is clamped up to this.
const MINIMUM_ABBREV: usize = 4;

/// Parsed command line for a single `ls-files` invocation.
#[derive(Default)]
struct Opts {
    cached: bool,        // -c / --cached
    stage: bool,         // -s / --stage
    unmerged: bool,      // -u / --unmerged
    deleted: bool,       // -d / --deleted
    modified: bool,      // -m / --modified
    others: bool,        // -o / --others
    directory: bool,     // --directory (collapse wholly-untracked directories)
    tags: bool,          // -t
    dedup: bool,         // --deduplicate
    error_unmatch: bool, // --error-unmatch
    zero: bool,          // -z
    full_name: bool,     // --full-name
    /// `None` = full object name, `Some(None)` = `core.abbrev`/auto, `Some(Some(n))` = `n` digits.
    abbrev: Option<Option<usize>>,
}

impl Opts {
    /// git prints the `<mode> <object> <stage>` columns whenever `-s` was asked
    /// for, and `-u` implies `-s` ("there's no point in showing unmerged unless
    /// you show the stage").
    fn stage_format(&self) -> bool {
        self.stage
    }

    /// The index pass that emits `--cached`/`--stage` lines runs for either flag.
    fn shows_index_entries(&self) -> bool {
        self.cached || self.stage
    }
}

/// Print `msg` followed by git's usage block and return git's usage exit code.
fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Reproduce git's rejection of any pathspec that resolves outside the
/// repository. git normalizes each pathspec against the current prefix and, on
/// the first one that escapes the worktree root (a leading `..`) or names an
/// absolute path outside it, dies with
/// `fatal: <arg>: '<arg>' is outside repository at '<worktree-root>'` (exit 128).
///
/// gix performs the same normalization inside `Search::from_specs`, so we run it
/// per pattern here to map the failure back to the specific command-line argument
/// (git reports the original spelling, `raw`) and emit git's exact message before
/// `repo.pathspec()` can turn the condition into a generic exit-1 anyhow error.
///
/// Magic pathspecs (`:(…)`, `:/…`) carry their own semantics and are left for gix.
fn check_pathspecs_inside_repo(
    repo: &gix::Repository,
    patterns: &[BString],
    raw_patterns: &[String],
) -> Result<Option<ExitCode>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let defaults = repo.pathspec_defaults_inherit_ignore_case(false)?;
    // The CWD relative to the worktree root; git's `prefix`. Empty at the top level.
    let prefix = repo.prefix()?.map(Path::to_path_buf).unwrap_or_default();
    // The absolute, symlink-resolved worktree root; git's `absolute_path(get_git_work_tree())`.
    let root = gix::path::realpath(repo.workdir().unwrap_or_else(|| repo.git_dir()))?;

    for (pattern, raw) in patterns.iter().zip(raw_patterns.iter()) {
        if pattern.first() == Some(&b':') {
            continue;
        }
        let Ok(mut parsed) = gix::pathspec::parse(pattern.as_slice(), defaults) else {
            continue;
        };
        if parsed.normalize(&prefix, &root).is_err() {
            eprintln!(
                "fatal: {raw}: '{raw}' is outside repository at '{}'",
                root.display()
            );
            return Ok(Some(ExitCode::from(128)));
        }
    }
    Ok(None)
}

/// `git ls-files` — list index entries, and optionally worktree-derived sets.
///
/// Supported invocations:
///   * `-c/--cached` (the default), `-s/--stage`, `-u/--unmerged`
///   * `-d/--deleted`, `-m/--modified`, `-o/--others`, `--directory`
///   * `-t` (status tags), `--deduplicate`, `--error-unmatch`
///   * `--full-name`, `-z`, `--abbrev[=<n>]`
///   * trailing pathspecs, optionally after `--`
///
/// Output ordering mirrors git exactly: the directory walk (`--others`) is
/// emitted first, then a single pass over the index emits, per entry, the
/// cached line, the deleted line, and the modified line in that order.
///
/// Exclude handling (`-x`, `-X`, `-i`, `--exclude-standard`), `-k/--killed`,
/// `--eol`, `--with-tree`, `--resolve-undo`, `--format`, `-v`, `-f` and
/// `--debug` are not ported and are rejected rather than silently ignored.
pub fn ls_files(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts::default();
    let mut no_more_flags = false;
    // Original pathspec spelling, kept for `--error-unmatch` diagnostics.
    let mut raw_patterns: Vec<String> = Vec::new();
    let mut patterns: Vec<BString> = Vec::new();

    for a in args {
        let s = a.as_str();
        if no_more_flags {
            raw_patterns.push(a.clone());
            patterns.push(normalize_pattern(s));
            continue;
        }
        match s {
            "--" => no_more_flags = true,
            "--cached" => opts.cached = true,
            "--no-cached" => opts.cached = false,
            "--stage" => opts.stage = true,
            "--no-stage" => opts.stage = false,
            "--unmerged" => opts.unmerged = true,
            "--no-unmerged" => opts.unmerged = false,
            "--deleted" => opts.deleted = true,
            "--no-deleted" => opts.deleted = false,
            "--modified" => opts.modified = true,
            "--no-modified" => opts.modified = false,
            "--others" => opts.others = true,
            "--no-others" => opts.others = false,
            "--directory" => opts.directory = true,
            "--no-directory" => opts.directory = false,
            "--deduplicate" => opts.dedup = true,
            "--no-deduplicate" => opts.dedup = false,
            "--error-unmatch" => opts.error_unmatch = true,
            "--no-error-unmatch" => opts.error_unmatch = false,
            "--full-name" => opts.full_name = true,
            "--abbrev" => opts.abbrev = Some(None),
            "--no-abbrev" => opts.abbrev = None,
            _ if s.starts_with("--abbrev=") => {
                let raw = &s["--abbrev=".len()..];
                let Ok(n) = raw.parse::<usize>() else {
                    return Ok(usage_error("option `abbrev' expects a numerical value"));
                };
                // git maps `--abbrev=0` to "print the full object name".
                opts.abbrev = if n == 0 {
                    None
                } else {
                    Some(Some(n.max(MINIMUM_ABBREV)))
                };
            }
            _ if s.starts_with("--") => {
                return Ok(usage_error(&format!(
                    "unknown option `{}'",
                    s.trim_start_matches('-')
                )));
            }
            // A lone `-` is a pathspec, everything else starting with `-` is a
            // (possibly clustered) short-option run such as `-czs`.
            _ if s.len() > 1 && s.starts_with('-') => {
                for c in s[1..].chars() {
                    match c {
                        'c' => opts.cached = true,
                        's' => opts.stage = true,
                        'u' => opts.unmerged = true,
                        'd' => opts.deleted = true,
                        'm' => opts.modified = true,
                        'o' => opts.others = true,
                        't' => opts.tags = true,
                        'z' => opts.zero = true,
                        _ => return Ok(usage_error(&format!("unknown switch `{c}'"))),
                    }
                }
            }
            _ => {
                raw_patterns.push(a.clone());
                patterns.push(normalize_pattern(s));
            }
        }
    }

    // "There's no point in showing unmerged unless you show the stage."
    if opts.unmerged {
        opts.stage = true;
    }
    // With no selector at all, git lists the cache.
    if !opts.cached
        && !opts.stage
        && !opts.deleted
        && !opts.modified
        && !opts.others
        && !opts.unmerged
    {
        opts.cached = true;
    }

    let repo = gix::discover(".")?;
    let index = repo.open_index()?;

    // Index paths are repository-root relative; unless `--full-name` was asked
    // for, git prints them relative to the current directory.
    let prefix: Option<BString> = if opts.full_name {
        None
    } else {
        match repo.prefix()? {
            Some(p) if !p.as_os_str().is_empty() => {
                let mut b = gix::path::into_bstr(p).into_owned();
                b.push(b'/');
                Some(b)
            }
            _ => None,
        }
    };

    // A pathspec that resolves outside the repository is a fatal error in git:
    //   `fatal: <arg>: '<arg>' is outside repository at '<worktree-root>'` (exit 128).
    // gix surfaces the same condition as a normalize error inside `repo.pathspec()`,
    // which would otherwise collapse to exit 1 via `?`. Detect it up front, reporting
    // the first offending pathspec in argument order exactly as git does.
    if let Some(code) = check_pathspecs_inside_repo(&repo, &patterns, &raw_patterns)? {
        return Ok(code);
    }

    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;

    let worktree = if opts.others || opts.modified || opts.deleted {
        Some(collect_worktree(&repo, opts.others, opts.directory)?)
    } else {
        None
    };

    // git quotes non-plain paths per `core.quotePath` (default on), but never
    // when `-z` was asked for: NUL-terminated output is meant to be unambiguous.
    let quote = !opts.zero
        && repo
            .config_snapshot()
            .boolean("core.quotePath")
            .unwrap_or(true);

    let mut lines: Vec<Vec<u8>> = Vec::new();

    // Phase 1: the directory walk, exactly as git emits it before touching the index.
    if let Some(state) = &worktree {
        // The pathspec is matched against the bare path; the trailing slash that
        // `--directory` prints is presentation only.
        let mut others: Vec<(&BString, bool)> = state
            .others
            .iter()
            .filter(|(path, is_dir)| ps.is_included(path.as_bstr(), Some(*is_dir)))
            .map(|(path, is_dir)| (path, *is_dir))
            .collect();
        others.sort();
        for (path, is_dir) in others {
            let mut display = strip_prefix(path.as_bstr(), prefix.as_ref()).to_vec();
            if is_dir {
                display.push(b'/');
            }
            lines.push(render(&opts, "? ", None, &repo, &display, quote));
        }
    }

    // Phase 2: one pass over the index; each entry can contribute a cached line,
    // a deleted line, and a modified line, in that order.
    let mut matched: HashSet<usize> = HashSet::new();
    for entry in index.entries() {
        let path = entry.path(&index);
        let Some(m) = ps.pattern_matching_relative_path(path, Some(false)) else {
            continue;
        };
        if m.is_excluded() {
            continue;
        }
        matched.insert(m.sequence_number);

        let stage = entry.stage_raw();
        let display = strip_prefix(path, prefix.as_ref());

        if opts.shows_index_entries() && !(opts.unmerged && stage == 0) {
            let tag = if entry
                .flags
                .contains(gix::index::entry::Flags::SKIP_WORKTREE)
            {
                "S "
            } else if stage != 0 {
                "M "
            } else {
                "H "
            };
            lines.push(render(&opts, tag, Some(entry), &repo, display, quote));
        }

        if opts.deleted || opts.modified {
            let state = worktree.as_ref().expect("collected when -d/-m is set");
            let (is_deleted, is_modified) = entry_worktree_change(&repo, state, entry, path);
            if opts.deleted && is_deleted {
                lines.push(render(&opts, "R ", Some(entry), &repo, display, quote));
            }
            if opts.modified && is_modified {
                lines.push(render(&opts, "C ", Some(entry), &repo, display, quote));
            }
        }
    }

    if opts.error_unmatch {
        if let Some(raw) = raw_patterns
            .iter()
            .enumerate()
            .find(|(i, _)| !matched.contains(i))
            .map(|(_, raw)| raw)
        {
            eprintln!("error: pathspec '{raw}' did not match any file(s) known to git");
            eprintln!("Did you forget to 'git add'?");
            return Ok(ExitCode::from(1));
        }
    }

    let terminator = if opts.zero { b'\0' } else { b'\n' };
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut previous: Option<&Vec<u8>> = None;
    for line in &lines {
        // `--deduplicate` suppresses repeats, which the per-entry emission order
        // always places next to each other.
        if opts.dedup && previous == Some(line) {
            continue;
        }
        out.write_all(line)?;
        out.write_all(&[terminator])?;
        previous = Some(line);
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Worktree-derived facts needed by `-o`, `-m` and `-d`.
struct Worktree {
    /// Tracked paths whose worktree file is gone.
    removed: HashSet<BString>,
    /// Tracked paths whose worktree content differs from the index.
    modified: HashSet<BString>,
    /// Paths carrying higher-stage (conflicted) entries; gitoxide folds their
    /// up-to-three stages into one status, so they are re-checked per entry.
    conflicted: HashSet<BString>,
    /// Untracked, non-ignored paths from the directory walk, each flagged as a
    /// directory or not (`--directory` prints collapsed directories with a `/`).
    others: Vec<(BString, bool)>,
}

/// Run one index↔worktree status pass and bucket the result.
fn collect_worktree(repo: &gix::Repository, others: bool, directory: bool) -> Result<Worktree> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let mut out = Worktree {
        removed: HashSet::new(),
        modified: HashSet::new(),
        conflicted: HashSet::new(),
        others: Vec::new(),
    };

    let untracked = match (others, directory) {
        (false, _) => gix::status::UntrackedFiles::None,
        // `--directory` is git's "show 'other' directories' names only".
        (true, true) => gix::status::UntrackedFiles::Collapsed,
        (true, false) => gix::status::UntrackedFiles::Files,
    };

    // Pathspec filtering is applied by the caller against every candidate, so the
    // walk itself stays unrestricted and cannot narrow the set incorrectly.
    let platform = repo
        .status(gix::progress::Discard)?
        .untracked_files(untracked);
    for item in platform.into_index_worktree_iter(Vec::<BString>::new())? {
        match item? {
            Item::Modification {
                rela_path, status, ..
            } => match status {
                EntryStatus::Conflict { .. } => {
                    out.conflicted.insert(rela_path);
                }
                // `git add -N` records a null blob, so the file always differs.
                EntryStatus::IntentToAdd => {
                    out.modified.insert(rela_path);
                }
                EntryStatus::Change(Change::Removed) => {
                    out.removed.insert(rela_path.clone());
                    out.modified.insert(rela_path);
                }
                EntryStatus::Change(_) => {
                    out.modified.insert(rela_path);
                }
                // A racy entry that only needs its stat data refreshed is unchanged.
                EntryStatus::NeedsUpdate(_) => {}
            },
            Item::DirectoryContents { entry, .. } => {
                if matches!(entry.status, gix::dir::entry::Status::Untracked) {
                    let is_dir = matches!(
                        entry.disk_kind,
                        Some(gix::dir::entry::Kind::Directory)
                            | Some(gix::dir::entry::Kind::Repository)
                    );
                    out.others.push((entry.rela_path, is_dir));
                }
            }
            Item::Rewrite { .. } => {}
        }
    }
    Ok(out)
}

/// Decide `(deleted, modified)` for one index entry, the way git's per-entry
/// `lstat` + `ie_modified` pair does.
///
/// Conflicted paths are re-checked here because gitoxide reports a single folded
/// status for all stages of a path, while git compares the worktree file against
/// *each* stage separately — which is why `git ls-files -m` prints a conflicted
/// path once per surviving stage.
fn entry_worktree_change(
    repo: &gix::Repository,
    state: &Worktree,
    entry: &gix::index::Entry,
    path: &BStr,
) -> (bool, bool) {
    if !state.conflicted.contains(path) {
        let deleted = state.removed.contains(path);
        return (deleted, deleted || state.modified.contains(path));
    }

    let Some(workdir) = repo.workdir() else {
        return (false, false);
    };
    let rela = gix::path::from_bstr(path);
    let full = workdir.join(&rela);
    let Ok(meta) = std::fs::symlink_metadata(&full) else {
        return (true, true);
    };
    // A symlink's "content" in git terms is its target, not the linked file.
    let content: Vec<u8> = if meta.is_symlink() {
        match std::fs::read_link(&full) {
            Ok(target) => gix::path::into_bstr(target).into_owned().into(),
            Err(_) => return (true, true),
        }
    } else {
        match std::fs::read(&full) {
            Ok(bytes) => bytes,
            Err(_) => return (true, true),
        }
    };
    let modified =
        match gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Blob, &content) {
            Ok(id) => id != entry.id,
            Err(_) => true,
        };
    (false, modified)
}

/// Build one output line: optional status tag, optional stage columns, path.
fn render(
    opts: &Opts,
    tag: &str,
    entry: Option<&gix::index::Entry>,
    repo: &gix::Repository,
    display: &[u8],
    quote: bool,
) -> Vec<u8> {
    let mut line = Vec::with_capacity(display.len() + 64);
    if opts.tags {
        line.extend_from_slice(tag.as_bytes());
    }
    // Directory-walk results never carry stage columns, even under `-s`.
    if let (true, Some(entry)) = (opts.stage_format(), entry) {
        let object = match opts.abbrev {
            None => entry.id.to_hex().to_string(),
            Some(None) => entry.id.attach(repo).shorten_or_id().to_string(),
            Some(Some(n)) => entry.id.to_hex_with_len(n).to_string(),
        };
        line.extend_from_slice(
            format!(
                "{:06o} {} {}\t",
                entry.mode.bits(),
                object,
                entry.stage_raw()
            )
            .as_bytes(),
        );
    }
    if quote {
        line.extend_from_slice(quote_path(display).as_bytes());
    } else {
        line.extend_from_slice(display);
    }
    line
}

/// Drop the repository-to-cwd prefix so paths print relative to the caller.
fn strip_prefix<'a>(path: &'a BStr, prefix: Option<&BString>) -> &'a [u8] {
    match prefix {
        Some(pref) => path
            .as_bytes()
            .strip_prefix(pref.as_bytes())
            .unwrap_or_else(|| path.as_bytes()),
        None => path.as_bytes(),
    }
}

/// Resolve `.` and interior `./` components in a pathspec the way git's
/// `prefix_path()` does.
///
/// gitoxide keeps a literal `.` as the pattern text, which then becomes the
/// search's common prefix and matches nothing. git instead resolves `.` to "the
/// current prefix", i.e. everything the caller can see. A pattern that reduces
/// to nothing is handed over as the nil pathspec `:`, which gitoxide normalizes
/// against the prefix for exactly that meaning.
fn normalize_pattern(pattern: &str) -> BString {
    // Magic pathspecs (`:(exclude)…`, `:/…`) carry their own syntax; leave them be.
    if pattern.starts_with(':') || !pattern.split('/').any(|c| c == ".") {
        return BString::from(pattern);
    }
    let parts: Vec<&str> = pattern
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.is_empty() {
        BString::from(":")
    } else {
        BString::from(parts.join("/"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_like_git() {
        assert_eq!(quote_path("src/lib.rs"), "src/lib.rs");
        assert_eq!(quote_path("with space.txt"), "with space.txt");
        assert_eq!(quote_path("quote\"name.txt"), "\"quote\\\"name.txt\"");
        assert_eq!(
            quote_path("üñïçødé.txt".as_bytes()),
            "\"\\303\\274\\303\\261\\303\\257\\303\\247\\303\\270d\\303\\251.txt\""
        );
        assert_eq!(quote_path("a\tb"), "\"a\\tb\"");
    }

    #[test]
    fn dot_pathspec_becomes_the_nil_pathspec() {
        // The literal `.` is what makes gitoxide's search compute a common
        // prefix of "." and match nothing at all.
        assert_eq!(normalize_pattern("."), ":");
        assert_eq!(normalize_pattern("./"), ":");
        assert_eq!(normalize_pattern("./src/lib.rs"), "src/lib.rs");
        assert_eq!(normalize_pattern("src/./lib.rs"), "src/lib.rs");
    }

    #[test]
    fn leaves_ordinary_and_magic_pathspecs_alone() {
        assert_eq!(normalize_pattern("src"), "src");
        assert_eq!(normalize_pattern("src/"), "src/");
        assert_eq!(normalize_pattern("*.md"), "*.md");
        assert_eq!(normalize_pattern("no/such/path"), "no/such/path");
        assert_eq!(normalize_pattern(":(exclude)./x"), ":(exclude)./x");
        assert_eq!(normalize_pattern("../sibling"), "../sibling");
    }
}
