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
    cached: bool,          // -c / --cached
    stage: bool,           // -s / --stage
    unmerged: bool,        // -u / --unmerged
    resolve_undo: bool,    // --resolve-undo (show recorded conflict resolutions)
    deleted: bool,         // -d / --deleted
    modified: bool,        // -m / --modified
    others: bool,          // -o / --others
    ignored: bool,         // -i / --ignored (show only excluded paths)
    directory: bool,       // --directory (collapse wholly-untracked directories)
    tags: bool,            // -t
    valid_bit: bool,       // -v (lowercase tag for 'assume unchanged' entries)
    fsmonitor_bit: bool,   // -f (lowercase tag for 'fsmonitor clean' entries)
    dedup: bool,           // --deduplicate
    error_unmatch: bool,   // --error-unmatch
    debug: bool,           // --debug (dump the cache entry's stat data)
    zero: bool,            // -z
    full_name: bool,       // --full-name
    exclude_standard: bool, // --exclude-standard (add the standard git exclusions)
    /// `--format` template; when set, replaces the default per-entry rendering.
    format: Option<String>,
    /// `-x/--exclude <pattern>` command-line exclude patterns, highest priority.
    exclude: Vec<String>,
    /// `-X/--exclude-from <file>` files to read additional exclude patterns from.
    exclude_from: Vec<String>,
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

/// Reproduce git's exit-128 pathspec taxonomy. git parses every pathspec up
/// front (its `parse_pathspec` / `init_pathspec_item`) and dies with `fatal:`
/// (exit 128) on the first spec that either
///   (a) carries **invalid magic** — `:(bogusmagic)…`, an unimplemented short
///       magic like `:"…`, a missing `)`, incompatible `literal`/`glob`, or an
///       empty `attr:` — reported before any path handling, or
///   (b) **escapes the worktree** — a leading `..` or an absolute path outside
///       the root — reported as
///       `fatal: <raw>: '<path>' is outside repository at '<worktree-root>'`,
///       where the quoted portion is the path with its magic prefix stripped
///       (`:!../x` → `'../x'`), and `<raw>` is the original spelling.
///
/// gitoxide surfaces both conditions later inside `repo.pathspec()`, where the
/// `?` operator would collapse them into a generic exit-1 anyhow error. We walk
/// the specs in argument order and, per spec, parse-then-normalize exactly as
/// git does, emitting git's message and returning 128 on the first failure.
///
/// Parse failures git does *not* treat as fatal magic (attribute-value corner
/// cases where gitoxide is stricter than git) are left for `repo.pathspec()`,
/// so a spec git accepts is never forced to 128 here.
fn check_pathspecs(
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
        // (a) Magic parsing — git rejects bad magic before touching the path.
        let mut parsed = match gix::pathspec::parse(pattern.as_slice(), defaults) {
            Ok(p) => p,
            Err(err) => match pathspec_parse_fatal(&err, raw) {
                Some(msg) => {
                    eprintln!("fatal: {msg}");
                    return Ok(Some(ExitCode::from(128)));
                }
                None => continue,
            },
        };
        // (b) Path normalization — a spec escaping the worktree is fatal. git
        // quotes the path portion (magic stripped), captured before normalize
        // consumes it, and prefixes the whole line with the raw spelling.
        let path = parsed.path().to_str_lossy().into_owned();
        if parsed.normalize(&prefix, &root).is_err() {
            eprintln!(
                "fatal: {raw}: '{path}' is outside repository at '{}'",
                root.display()
            );
            return Ok(Some(ExitCode::from(128)));
        }
    }
    Ok(None)
}

/// Map a gitoxide pathspec parse error to git's exact `fatal:` message body
/// (everything after `fatal: `), or `None` for the attribute corner cases where
/// gitoxide is stricter than git and forcing a 128 would reject a spec git
/// accepts (e.g. `:(attr:-unset)`), which must instead flow through to gix.
fn pathspec_parse_fatal(err: &gix::pathspec::parse::Error, raw: &str) -> Option<String> {
    use gix::pathspec::parse::Error as E;
    Some(match err {
        E::InvalidKeyword { keyword } => {
            format!("Invalid pathspec magic '{}' in '{raw}'", keyword.to_str_lossy())
        }
        E::Unimplemented { short_keyword } => {
            format!("Unimplemented pathspec magic '{short_keyword}' in '{raw}'")
        }
        E::MissingClosingParenthesis => {
            format!("Missing ')' at the end of pathspec magic in '{raw}'")
        }
        E::IncompatibleSearchModes => format!("{raw}: 'literal' and 'glob' are incompatible"),
        E::EmptyAttribute => "attr spec must not be empty".to_string(),
        _ => return None,
    })
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
/// Exclude handling (`-x`, `-X`, `-i`, `--exclude-standard`), `-v`, `-f`,
/// `--debug`, `--format` and `--resolve-undo` are ported. `-k/--killed`,
/// `--eol` and `--with-tree` are not ported and are rejected rather than
/// silently ignored.
pub fn ls_files(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts::default();
    let mut no_more_flags = false;
    // Original pathspec spelling, kept for `--error-unmatch` diagnostics.
    let mut raw_patterns: Vec<String> = Vec::new();
    let mut patterns: Vec<BString> = Vec::new();

    // Index-based so option-argument forms (`-x <pat>`, `--format <fmt>`) can
    // consume the following argument, matching git's parse-options behaviour.
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let s = a.as_str();
        if no_more_flags {
            raw_patterns.push(a.clone());
            patterns.push(normalize_pattern(s));
            i += 1;
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
            "--resolve-undo" => opts.resolve_undo = true,
            "--no-resolve-undo" => opts.resolve_undo = false,
            "--deleted" => opts.deleted = true,
            "--no-deleted" => opts.deleted = false,
            "--modified" => opts.modified = true,
            "--no-modified" => opts.modified = false,
            "--others" => opts.others = true,
            "--no-others" => opts.others = false,
            "--ignored" => opts.ignored = true,
            "--no-ignored" => opts.ignored = false,
            "--directory" => opts.directory = true,
            "--no-directory" => opts.directory = false,
            "--deduplicate" => opts.dedup = true,
            "--no-deduplicate" => opts.dedup = false,
            "--error-unmatch" => opts.error_unmatch = true,
            "--no-error-unmatch" => opts.error_unmatch = false,
            "--debug" => opts.debug = true,
            "--no-debug" => opts.debug = false,
            "--exclude-standard" => opts.exclude_standard = true,
            "--full-name" => opts.full_name = true,
            "--abbrev" => opts.abbrev = Some(None),
            "--no-abbrev" => opts.abbrev = None,
            "--exclude" => match args.get(i + 1) {
                Some(v) => {
                    opts.exclude.push(v.clone());
                    i += 1;
                }
                None => return Ok(usage_error("option `exclude' requires a value")),
            },
            _ if s.starts_with("--exclude=") => {
                opts.exclude.push(s["--exclude=".len()..].to_string());
            }
            "--exclude-from" => match args.get(i + 1) {
                Some(v) => {
                    opts.exclude_from.push(v.clone());
                    i += 1;
                }
                None => return Ok(usage_error("option `exclude-from' requires a value")),
            },
            _ if s.starts_with("--exclude-from=") => {
                opts.exclude_from.push(s["--exclude-from=".len()..].to_string());
            }
            "--format" => match args.get(i + 1) {
                Some(v) => {
                    opts.format = Some(v.clone());
                    i += 1;
                }
                None => return Ok(usage_error("option `format' requires a value")),
            },
            _ if s.starts_with("--format=") => {
                opts.format = Some(s["--format=".len()..].to_string());
            }
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
            // (possibly clustered) short-option run such as `-czs`. The value
            // options `-x`/`-X` consume the rest of the cluster, or the next
            // argument when they end it, exactly like git's parse-options.
            _ if s.len() > 1 && s.starts_with('-') => {
                let bytes = s.as_bytes();
                let mut j = 1;
                while j < bytes.len() {
                    let c = bytes[j] as char;
                    match c {
                        'c' => opts.cached = true,
                        's' => opts.stage = true,
                        'u' => opts.unmerged = true,
                        'd' => opts.deleted = true,
                        'm' => opts.modified = true,
                        'o' => opts.others = true,
                        'i' => opts.ignored = true,
                        't' => opts.tags = true,
                        'v' => opts.valid_bit = true,
                        'f' => opts.fsmonitor_bit = true,
                        'z' => opts.zero = true,
                        'x' | 'X' => {
                            let rest = &s[j + 1..];
                            let val = if !rest.is_empty() {
                                rest.to_string()
                            } else {
                                match args.get(i + 1) {
                                    Some(v) => {
                                        i += 1;
                                        v.clone()
                                    }
                                    None => {
                                        return Ok(usage_error(&format!(
                                            "switch `{c}' requires a value"
                                        )));
                                    }
                                }
                            };
                            if c == 'x' {
                                opts.exclude.push(val);
                            } else {
                                opts.exclude_from.push(val);
                            }
                            break;
                        }
                        _ => return Ok(usage_error(&format!("unknown switch `{c}'"))),
                    }
                    j += 1;
                }
            }
            _ => {
                raw_patterns.push(a.clone());
                patterns.push(normalize_pattern(s));
            }
        }
        i += 1;
    }

    // `--format` shares git's exact incompatibility set (exit 129). `-k` and
    // `--eol` aren't parsed here, so `-s`/`-o`/`-t`/`--resolve-undo`/
    // `--deduplicate` are the selectors that can actually co-occur.
    if opts.format.is_some()
        && (opts.stage || opts.others || opts.tags || opts.dedup || opts.resolve_undo)
    {
        return Ok(usage_error(
            "--format cannot be used with -s, -o, -k, -t, --resolve-undo, --deduplicate, --eol",
        ));
    }

    // "There's no point in showing unmerged unless you show the stage."
    if opts.unmerged {
        opts.stage = true;
    }
    // With no selector at all, git lists the cache. `-i` is not a selector, so it
    // leaves the default in place — which is what lets `git ls-files -i` reach the
    // "needs some exclude pattern" diagnostic below.
    if !opts.cached
        && !opts.stage
        && !opts.deleted
        && !opts.modified
        && !opts.others
        && !opts.unmerged
        && !opts.resolve_undo
    {
        opts.cached = true;
    }

    // git's two `-i` guards, in order, each fatal (exit 128).
    if opts.ignored && !opts.others && !opts.cached {
        eprintln!("fatal: ls-files -i must be used with either -o or -c");
        return Ok(ExitCode::from(128));
    }
    let exc_given = !opts.exclude.is_empty() || !opts.exclude_from.is_empty() || opts.exclude_standard;
    if opts.ignored && !exc_given {
        eprintln!("fatal: ls-files --ignored needs some exclude pattern");
        return Ok(ExitCode::from(128));
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

    // git validates every pathspec up front and dies with exit 128 on the first
    // one that carries invalid magic (`:(bogusmagic)…`) or escapes the worktree
    // (a leading `..`, an absolute path outside the root). gix surfaces both as
    // errors inside `repo.pathspec()`, which would otherwise collapse to exit 1
    // via `?`. Detect them here, reporting the first offender in argument order
    // with git's exact message and code.
    if let Some(code) = check_pathspecs(&repo, &patterns, &raw_patterns)? {
        return Ok(code);
    }

    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;

    // The exclude stack git assembles from `-x`, `-X` and `--exclude-standard`.
    // `-x`/`-X` become the highest-priority override group (git's `EXC_CMDL`);
    // `--exclude-standard` adds `info/exclude`, `core.excludesFile` and the
    // per-directory `.gitignore` files. Without `--exclude-standard` no on-disk
    // ignore files are consulted, exactly like git.
    let mut matcher = Excludes::build(&repo, &index, &opts)?;

    // `-i` needs the walk to surface ignored paths too, which gix normally drops.
    let worktree = if opts.others || opts.modified || opts.deleted {
        Some(collect_worktree(
            &repo,
            opts.others,
            opts.directory,
            opts.ignored,
        )?)
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
    // Each rendered line carries its own terminator (and, under `--debug`, the
    // trailing stat block) so the emit loop can stay a verbatim byte copy.
    let terminator = if opts.zero { b'\0' } else { b'\n' };

    let mut lines: Vec<Vec<u8>> = Vec::new();

    // Phase 1: the directory walk, exactly as git emits it before touching the
    // index. Only `--others` prints these `? ` lines.
    if opts.others {
        if let Some(state) = &worktree {
            // The pathspec is matched against the bare path; the trailing slash
            // that `--directory` prints is presentation only. Under `-i` the
            // candidate set is untracked ∪ ignored so our own exclude stack — not
            // gix's `.gitignore` classification — decides which to keep.
            let extra: &[(BString, bool)] = if opts.ignored { &state.ignored } else { &[] };
            let mut others: Vec<(&BString, bool)> = state
                .others
                .iter()
                .chain(extra.iter())
                .filter(|(path, is_dir)| ps.is_included(path.as_bstr(), Some(*is_dir)))
                .map(|(path, is_dir)| (path, *is_dir))
                .collect();
            others.sort();
            others.dedup();
            for (path, is_dir) in others {
                // `-i` keeps only excluded paths; the default keeps only the rest.
                if matcher.is_excluded(path.as_bstr(), is_dir) != opts.ignored {
                    continue;
                }
                let mut display = strip_prefix(path.as_bstr(), prefix.as_ref()).to_vec();
                if is_dir {
                    display.push(b'/');
                }
                lines.push(render(&opts, "? ", None, &repo, &display, quote, terminator));
            }
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

        // Under `-i`, every index-derived line (cached, deleted, modified) is
        // restricted to entries the exclude stack matches, exactly as git's
        // `ce_excluded` gate does in both of its index loops.
        if opts.ignored && !matcher.is_excluded(path, false) {
            continue;
        }

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
            lines.push(render(&opts, tag, Some(entry), &repo, display, quote, terminator));
        }

        if opts.deleted || opts.modified {
            let state = worktree.as_ref().expect("collected when -d/-m is set");
            let (is_deleted, is_modified) = entry_worktree_change(&repo, state, entry, path);
            if opts.deleted && is_deleted {
                lines.push(render(&opts, "R ", Some(entry), &repo, display, quote, terminator));
            }
            if opts.modified && is_modified {
                lines.push(render(&opts, "C ", Some(entry), &repo, display, quote, terminator));
            }
        }
    }

    // Phase 3: resolve-undo records (git's `show_ru_info`), emitted after every
    // index line. Each recorded conflict contributes one line per surviving
    // stage — `<tag><mode> <object> <stage>\t<name>` — with the `U ` tag present
    // only under `-t`/`-v`/`-f`, exactly as git assigns `tag_resolve_undo`. The
    // path is pathspec-matched like an index entry, so a spec that matches only a
    // resolve-undo path still satisfies `--error-unmatch`.
    if opts.resolve_undo {
        if let Some(records) = index.resolve_undo() {
            let ru_tag = if opts.tags || opts.valid_bit || opts.fsmonitor_bit {
                "U "
            } else {
                ""
            };
            for rec in records {
                let name = rec.name();
                let Some(m) = ps.pattern_matching_relative_path(name, Some(false)) else {
                    continue;
                };
                if m.is_excluded() {
                    continue;
                }
                matched.insert(m.sequence_number);
                let display = strip_prefix(name, prefix.as_ref());
                let path_bytes = if quote {
                    quote_path(display).into_bytes()
                } else {
                    display.to_vec()
                };
                for (i, stage) in rec.stages().iter().enumerate() {
                    let Some(st) = stage else { continue };
                    lines.push(resolve_undo_line(
                        ru_tag,
                        st.mode(),
                        &abbrev_oid(st.id(), &repo, opts.abbrev),
                        i + 1,
                        &path_bytes,
                        terminator,
                    ));
                }
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

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut previous: Option<&Vec<u8>> = None;
    for line in &lines {
        // `--deduplicate` suppresses repeats, which the per-entry emission order
        // always places next to each other. Each line already carries its own
        // terminator, so the compare is over the fully-rendered bytes.
        if opts.dedup && previous == Some(line) {
            continue;
        }
        out.write_all(line)?;
        previous = Some(line);
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The exclude machinery git configures from `-x`, `-X` and `--exclude-standard`.
///
/// Two shapes, mirroring what git consults:
///   * [`Excludes::Standard`] — a full worktree exclude stack (`info/exclude`,
///     `core.excludesFile`, per-directory `.gitignore`) with the `-x`/`-X`
///     patterns layered on top as the highest-priority override group. Built
///     only when `--exclude-standard` is given.
///   * [`Excludes::Overrides`] — just the `-x`/`-X` patterns, matched directly,
///     with no on-disk ignore files consulted (git's behaviour without
///     `--exclude-standard`).
///   * [`Excludes::None`] — nothing configured; nothing is ever excluded.
enum Excludes<'repo> {
    None,
    Overrides {
        search: gix::ignore::Search,
        case: gix::glob::pattern::Case,
    },
    Standard {
        stack: gix::AttributeStack<'repo>,
    },
}

impl<'repo> Excludes<'repo> {
    fn build(repo: &'repo gix::Repository, index: &gix::index::State, opts: &Opts) -> Result<Self> {
        let has_overrides = !opts.exclude.is_empty() || !opts.exclude_from.is_empty();
        if !opts.exclude_standard && !has_overrides {
            return Ok(Excludes::None);
        }

        let parse = gix::ignore::search::Ignore {
            support_precious: false,
        };
        // `-x` patterns first (git's `EXC_CMDL`), then each `-X` file appended.
        let mut search = gix::ignore::Search::from_overrides(opts.exclude.iter().cloned(), parse);
        for file in &opts.exclude_from {
            if let Ok(bytes) = std::fs::read(file) {
                search.add_patterns_buffer(&bytes, file.clone(), None, parse);
            }
        }

        if opts.exclude_standard {
            let stack = repo.excludes(
                index,
                Some(search),
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
            )?;
            Ok(Excludes::Standard { stack })
        } else {
            let case = if repo
                .config_snapshot()
                .boolean("core.ignoreCase")
                .unwrap_or(false)
            {
                gix::glob::pattern::Case::Fold
            } else {
                gix::glob::pattern::Case::Sensitive
            };
            Ok(Excludes::Overrides { search, case })
        }
    }

    /// Whether `path` is excluded, i.e. matched by a non-negated pattern.
    fn is_excluded(&mut self, path: &BStr, is_dir: bool) -> bool {
        match self {
            Excludes::None => false,
            Excludes::Overrides { search, case } => search
                .pattern_matching_relative_path(path, Some(is_dir), *case)
                .is_some_and(|m| !m.pattern.is_negative()),
            Excludes::Standard { stack } => {
                let mode = is_dir.then_some(gix::index::entry::Mode::DIR);
                stack
                    .at_entry(path, mode)
                    .map(|p| p.is_excluded())
                    .unwrap_or(false)
            }
        }
    }
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
    /// Untracked paths from the directory walk, each flagged as a directory or
    /// not (`--directory` prints collapsed directories with a `/`).
    others: Vec<(BString, bool)>,
    /// Ignored paths, collected only under `-i` so the caller's own exclude stack
    /// can classify the untracked ∪ ignored candidate set (gix's `.gitignore`
    /// verdict is discarded — git's `-x`/`-X` patterns can differ from it).
    ignored: Vec<(BString, bool)>,
}

/// Run one index↔worktree status pass and bucket the result.
fn collect_worktree(
    repo: &gix::Repository,
    others: bool,
    directory: bool,
    want_ignored: bool,
) -> Result<Worktree> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let mut out = Worktree {
        removed: HashSet::new(),
        modified: HashSet::new(),
        conflicted: HashSet::new(),
        others: Vec::new(),
        ignored: Vec::new(),
    };

    let untracked = match (others, directory) {
        (false, _) => gix::status::UntrackedFiles::None,
        // `--directory` is git's "show 'other' directories' names only".
        (true, true) => gix::status::UntrackedFiles::Collapsed,
        (true, false) => gix::status::UntrackedFiles::Files,
    };

    // Pathspec filtering is applied by the caller against every candidate, so the
    // walk itself stays unrestricted and cannot narrow the set incorrectly.
    let mut platform = repo
        .status(gix::progress::Discard)?
        .untracked_files(untracked);
    // `-i` needs ignored entries emitted individually (git lists ignored files,
    // not collapsed directories, by default).
    if want_ignored {
        platform = platform.dirwalk_options(|o| {
            o.emit_ignored(Some(gix::dir::walk::EmissionMode::Matching))
        });
    }
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
                let is_dir = matches!(
                    entry.disk_kind,
                    Some(gix::dir::entry::Kind::Directory)
                        | Some(gix::dir::entry::Kind::Repository)
                );
                match entry.status {
                    gix::dir::entry::Status::Untracked => {
                        out.others.push((entry.rela_path, is_dir));
                    }
                    gix::dir::entry::Status::Ignored(_) if want_ignored => {
                        out.ignored.push((entry.rela_path, is_dir));
                    }
                    _ => {}
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

/// Render an object id the way git's `find_unique_abbrev` does for these
/// columns: the full hex name when `--abbrev` was absent, the `core.abbrev`/auto
/// length when `--abbrev` carried no value, or exactly `n` hex digits for
/// `--abbrev=<n>` (already clamped to `MINIMUM_ABBREV` during parsing).
fn abbrev_oid(id: gix::ObjectId, repo: &gix::Repository, abbrev: Option<Option<usize>>) -> String {
    match abbrev {
        None => id.to_hex().to_string(),
        Some(None) => id.attach(repo).shorten_or_id().to_string(),
        Some(Some(n)) => id.to_hex_with_len(n).to_string(),
    }
}

/// Build one resolve-undo line, matching git's `show_ru_info`
/// `printf("%s%06o %s %d\t", tag, mode, object, stage)` followed by the
/// prefix-stripped, quoted name and the line terminator.
fn resolve_undo_line(
    tag: &str,
    mode: u32,
    object: &str,
    stage: usize,
    path_bytes: &[u8],
    terminator: u8,
) -> Vec<u8> {
    let mut line = Vec::with_capacity(path_bytes.len() + 64);
    line.extend_from_slice(tag.as_bytes());
    line.extend_from_slice(format!("{mode:06o} {object} {stage}\t").as_bytes());
    line.extend_from_slice(path_bytes);
    line.push(terminator);
    line
}

/// Build one output line: optional status tag, optional stage columns, path,
/// the line terminator, and (under `--debug`) the trailing stat block.
fn render(
    opts: &Opts,
    tag: &str,
    entry: Option<&gix::index::Entry>,
    repo: &gix::Repository,
    display: &[u8],
    quote: bool,
    terminator: u8,
) -> Vec<u8> {
    let mut line = Vec::with_capacity(display.len() + 64);
    let path_bytes = if quote {
        quote_path(display).into_bytes()
    } else {
        display.to_vec()
    };

    // `--format` replaces the whole per-entry layout with the interpolated
    // template; it is validated to never co-occur with `-o`/`-s`/`-t`/dedup.
    if let Some(fmt) = &opts.format {
        expand_format(&mut line, fmt, entry, repo, &path_bytes, opts.abbrev);
        line.push(terminator);
        return line;
    }

    if opts.tags {
        // `-v`/`-f` lowercase the tag for 'assume unchanged' / 'fsmonitor clean'
        // index entries (git's `get_tag`); directory-walk results have no entry.
        let tag = alt_tag(opts, tag, entry);
        line.extend_from_slice(tag.as_bytes());
    }
    // Directory-walk results never carry stage columns, even under `-s`.
    if let (true, Some(entry)) = (opts.stage_format(), entry) {
        let object = abbrev_oid(entry.id, repo, opts.abbrev);
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
    line.extend_from_slice(&path_bytes);
    line.push(terminator);

    // git prints the `--debug` stat block after the (terminated) name, and only
    // for real index entries.
    if opts.debug {
        if let Some(entry) = entry {
            append_debug(&mut line, entry);
        }
    }
    line
}

/// git's `get_tag`: for `-v`/`-f`, an index entry marked 'assume unchanged'
/// (`ASSUME_VALID`) or 'fsmonitor clean' (`FSMONITOR_VALID`) gets its status tag
/// lowercased (`H `→`h `, `M `→`m `, …); a non-alpha `?` becomes `!`.
fn alt_tag<'a>(opts: &Opts, tag: &'a str, entry: Option<&gix::index::Entry>) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    let Some(entry) = entry else {
        return Cow::Borrowed(tag);
    };
    let hit = (opts.valid_bit
        && entry
            .flags
            .contains(gix::index::entry::Flags::ASSUME_VALID))
        || (opts.fsmonitor_bit
            && entry
                .flags
                .contains(gix::index::entry::Flags::FSMONITOR_VALID));
    let Some(first) = tag.chars().next().filter(|_| hit) else {
        return Cow::Borrowed(tag);
    };
    if first.is_ascii_alphabetic() {
        Cow::Owned(format!("{}{}", first.to_ascii_lowercase(), &tag[first.len_utf8()..]))
    } else if first == '?' {
        Cow::Borrowed("! ")
    } else {
        Cow::Owned(format!("v{tag}"))
    }
}

/// Append git's `print_debug` block: the cache entry's raw stat data. git labels
/// this output as intended for manual inspection and free to change, so the
/// per-field layout is matched but exact byte parity is not a goal.
fn append_debug(line: &mut Vec<u8>, entry: &gix::index::Entry) {
    let s = &entry.stat;
    line.extend_from_slice(
        format!(
            "  ctime: {}:{}\n  mtime: {}:{}\n  dev: {}\tino: {}\n  uid: {}\tgid: {}\n  size: {}\tflags: {:x}\n",
            s.ctime.secs,
            s.ctime.nsecs,
            s.mtime.secs,
            s.mtime.nsecs,
            s.dev,
            s.ino,
            s.uid,
            s.gid,
            s.size,
            entry.flags.bits(),
        )
        .as_bytes(),
    );
}

/// Expand one `--format` template for a single index entry, supporting the atoms
/// stock `git ls-files --format` documents: `%(objectmode)`, `%(objectname)`,
/// `%(objecttype)`, `%(objectsize)`, `%(objectsize:padded)`, `%(stage)` and
/// `%(path)`, plus `%%` and `%x<hh>` byte escapes.
///
/// The `%(eolinfo:index)`, `%(eolinfo:worktree)` and `%(eolattr)` atoms are
/// recognised but expand to the empty string, as line-ending convert-stats are
/// not ported. An unrecognised `%(...)` atom is copied through verbatim.
fn expand_format(
    out: &mut Vec<u8>,
    fmt: &str,
    entry: Option<&gix::index::Entry>,
    repo: &gix::Repository,
    path_bytes: &[u8],
    abbrev: Option<Option<usize>>,
) {
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(chars[i].encode_utf8(&mut buf).as_bytes());
            i += 1;
            continue;
        }
        let Some(&next) = chars.get(i + 1) else {
            out.push(b'%');
            break;
        };
        if next == '%' {
            out.push(b'%');
            i += 2;
            continue;
        }
        if next == 'x' && i + 3 < chars.len() {
            let hex: String = chars[i + 2..i + 4].iter().collect();
            if let Ok(b) = u8::from_str_radix(&hex, 16) {
                out.push(b);
                i += 4;
                continue;
            }
        }
        if next == '(' {
            if let Some(close) = chars[i + 2..].iter().position(|&c| c == ')') {
                let atom: String = chars[i + 2..i + 2 + close].iter().collect();
                match atom.as_str() {
                    "objectmode" => {
                        if let Some(e) = entry {
                            out.extend_from_slice(format!("{:06o}", e.mode.bits()).as_bytes());
                        }
                    }
                    "objecttype" => {
                        if let Some(e) = entry {
                            let ty = if e.mode.bits() == 0o160000 { "commit" } else { "blob" };
                            out.extend_from_slice(ty.as_bytes());
                        }
                    }
                    "objectname" => {
                        if let Some(e) = entry {
                            out.extend_from_slice(abbrev_oid(e.id, repo, abbrev).as_bytes());
                        }
                    }
                    "objectsize" => {
                        out.extend_from_slice(format_objectsize(entry, repo).as_bytes());
                    }
                    "objectsize:padded" => {
                        out.extend_from_slice(
                            format!("{:>7}", format_objectsize(entry, repo)).as_bytes(),
                        );
                    }
                    "stage" => {
                        if let Some(e) = entry {
                            out.extend_from_slice(e.stage_raw().to_string().as_bytes());
                        }
                    }
                    "path" => out.extend_from_slice(path_bytes),
                    // Line-ending convert-stats are not ported; git yields empty
                    // for these on binary/non-regular content and unset attrs.
                    "eolinfo:index" | "eolinfo:worktree" | "eolattr" => {}
                    other => {
                        out.extend_from_slice(b"%(");
                        out.extend_from_slice(other.as_bytes());
                        out.push(b')');
                    }
                }
                i += 2 + close + 1;
                continue;
            }
        }
        out.push(b'%');
        i += 1;
    }
}

/// The `%(objectsize)` value: a blob reports its byte count, a gitlink `commit`
/// (or a missing object) reports `-`, matching git's `expand_objectsize`.
fn format_objectsize(entry: Option<&gix::index::Entry>, repo: &gix::Repository) -> String {
    let Some(e) = entry else {
        return "-".to_string();
    };
    if e.mode.bits() == 0o160000 {
        return "-".to_string();
    }
    match repo.find_header(e.id) {
        Ok(h) => h.size().to_string(),
        Err(_) => "-".to_string(),
    }
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
    fn resolve_undo_line_matches_git_format() {
        // git's show_ru_info: printf("%s%06o %s %d\t", tag, mode, oid, i+1)
        // followed by write_name (name + line terminator). `git ls-files -t
        // --resolve-undo` on a resolved conflict prints e.g.
        //   U 100644 <oid> 1\tpath/file
        let line = resolve_undo_line("U ", 0o100644, "0123abc", 1, b"path/file", b'\n');
        assert_eq!(line, b"U 100644 0123abc 1\tpath/file\n");
        // Untagged (`git ls-files --resolve-undo`), the leading column is empty;
        // under `-z` the NUL terminates and no quoting is applied by the caller.
        let z = resolve_undo_line("", 0o100755, "def4567", 3, b"x", b'\0');
        assert_eq!(z, b"100755 def4567 3\tx\0");
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
