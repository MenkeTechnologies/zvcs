use anyhow::Result;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::prelude::ObjectIdExt;

use super::{Arg, LongOpt};

/// `builtin_ls_files_options[]` (builtin/ls-files.c:601-666), in table order, as
/// [`super::resolve_long`] reads it. Only the long names appear: the short-only
/// entries (`-z`, `-t`, `-v`, `-f`) are never reached by name.
///
/// `-x`/`--exclude`, `-X`/`--exclude-from`, `--exclude-standard`, `--full-name`
/// and `--format` carry `PARSE_OPT_NONEG`, so they have no `--no-` spelling;
/// every other entry does. Nothing here is `PARSE_OPT_LASTARG_DEFAULT`.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "cached", neg: true, arg: Arg::None },
    LongOpt { name: "deleted", neg: true, arg: Arg::None },
    LongOpt { name: "modified", neg: true, arg: Arg::None },
    LongOpt { name: "others", neg: true, arg: Arg::None },
    LongOpt { name: "ignored", neg: true, arg: Arg::None },
    LongOpt { name: "stage", neg: true, arg: Arg::None },
    LongOpt { name: "killed", neg: true, arg: Arg::None },
    LongOpt { name: "directory", neg: true, arg: Arg::None },
    LongOpt { name: "eol", neg: true, arg: Arg::None },
    LongOpt { name: "empty-directory", neg: true, arg: Arg::None },
    LongOpt { name: "unmerged", neg: true, arg: Arg::None },
    LongOpt { name: "resolve-undo", neg: true, arg: Arg::None },
    LongOpt { name: "exclude", neg: false, arg: Arg::Required },
    LongOpt { name: "exclude-from", neg: false, arg: Arg::Required },
    LongOpt { name: "exclude-per-directory", neg: true, arg: Arg::Required },
    LongOpt { name: "exclude-standard", neg: false, arg: Arg::None },
    LongOpt { name: "full-name", neg: false, arg: Arg::None },
    LongOpt { name: "recurse-submodules", neg: true, arg: Arg::None },
    LongOpt { name: "error-unmatch", neg: true, arg: Arg::None },
    LongOpt { name: "with-tree", neg: true, arg: Arg::Required },
    LongOpt { name: "abbrev", neg: true, arg: Arg::Optional },
    LongOpt { name: "debug", neg: true, arg: Arg::None },
    LongOpt { name: "deduplicate", neg: true, arg: Arg::None },
    LongOpt { name: "sparse", neg: true, arg: Arg::None },
    LongOpt { name: "format", neg: false, arg: Arg::Required },
];

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
    killed: bool,          // -k / --killed (untracked paths blocking a checkout)
    ignored: bool,         // -i / --ignored (show only excluded paths)
    directory: bool,       // --directory (collapse wholly-untracked directories)
    /// git's `DIR_HIDE_EMPTY_DIRECTORIES` (wired as `OPT_NEGBIT`): default off,
    /// so an empty untracked directory is shown as `dir/` under `--directory`.
    /// `--no-empty-directory` sets the bit (hide them); `--empty-directory` (and
    /// the default) clears it (show them).
    hide_empty_dir: bool,  // --no-empty-directory
    tags: bool,            // -t
    valid_bit: bool,       // -v (lowercase tag for 'assume unchanged' entries)
    fsmonitor_bit: bool,   // -f (lowercase tag for 'fsmonitor clean' entries)
    dedup: bool,           // --deduplicate
    error_unmatch: bool,   // --error-unmatch
    debug: bool,           // --debug (dump the cache entry's stat data)
    zero: bool,            // -z
    eol: bool,             // --eol (show index/worktree line endings and the eol attribute)
    recurse_submodules: bool, // --recurse-submodules
    /// `--sparse`: leave sparse directory entries collapsed. Without it, git
    /// expands a sparse index to a full one before listing (`ensure_full_index`).
    sparse: bool,
    full_name: bool,       // --full-name
    exclude_standard: bool, // --exclude-standard (add the standard git exclusions)
    /// git's `exc_given`, latched by `-x`, `-X` and `--exclude-standard` as they
    /// are parsed; it gates the `--ignored needs some exclude pattern` guard.
    exc_given: bool,
    /// `--with-tree <tree-ish>`: overlay the named tree onto the index so paths
    /// removed since that tree still appear. Holds the raw tree-ish spelling.
    with_tree: Option<String>,
    /// `--format` template; when set, replaces the default per-entry rendering.
    format: Option<String>,
    /// `-x/--exclude <pattern>` command-line exclude patterns, highest priority.
    exclude: Vec<String>,
    /// `-X/--exclude-from <file>` files to read additional exclude patterns from.
    exclude_from: Vec<String>,
    /// git's `dir.exclude_per_dir`: the name of the per-directory ignore file.
    ///
    /// `None` until something sets it, exactly like git's initially-`NULL` field:
    /// `--exclude-standard` sets `.gitignore` (as part of `setup_standard_excludes`),
    /// `--exclude-per-directory=<file>` sets `<file>`, and `--no-exclude-per-directory`
    /// clears it again. Whichever comes last on the command line wins. An empty name is
    /// git's non-`NULL`-but-unreadable case: no file is consulted, yet it still satisfies
    /// the `--ignored` guard.
    exclude_per_directory: Option<String>,
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

    /// git assigns the non-empty status tags under any of `-t`, `-v` and `-f`
    /// (`if (show_tag || show_valid_bit || show_fsmonitor_bit)`), so `-v` on its
    /// own prints them too — with `get_tag` lowercasing the marked entries.
    fn shows_tags(&self) -> bool {
        self.tags || self.valid_bit || self.fsmonitor_bit
    }
}

/// Print `msg` followed by git's usage block and return git's usage exit code.
/// This is parse-options' own diagnostic for a malformed command line.
/// `add_patterns_from_file()`'s refusal (`dir.c`), raised by `-X`/`--exclude-from`
/// from inside its option callback — so it fires during argument parsing, before
/// the command has listed anything:
///
/// ```c
/// void add_patterns_from_file(struct dir_struct *dir, const char *fname)
/// {
///         if (add_patterns(fname, "", 0, &dir->exclude_list_group[EXC_FILE].pl[0],
///                          NULL, 0, NULL) < 0)
///                 die(_("cannot use %s as an exclude file"), fname);
/// }
/// ```
///
/// `add_patterns()` (dir.c:1054-1099) tests nothing about the file's *type*: it
/// `open()`s the path, `fstat()`s it, returns 0 outright for a zero-sized file
/// (:1083-1091) and otherwise has to `read_in_full()` it (:1093-1097). So a
/// missing path fails at the open and a directory fails at the read, while
/// `/dev/null` — zero-sized — is a perfectly good exclude file that contributes
/// no patterns.
fn reject_exclude_file(path: &str) -> Option<ExitCode> {
    let readable = std::fs::File::open(path).and_then(|mut f| {
        use std::io::Read as _;
        if f.metadata()?.len() == 0 {
            return Ok(());
        }
        let mut sink = Vec::new();
        f.read_to_end(&mut sink).map(|_| ())
    });
    if readable.is_ok() {
        return None;
    }
    eprintln!("fatal: cannot use {path} as an exclude file");
    Some(ExitCode::from(128))
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// The other parse-options failure shape: the `error:` line alone, exit 129, no
/// usage block.
///
/// `get_arg()` and a rejecting option callback `return error(...)`, which is
/// `PARSE_OPT_ERROR` (`parse-options.h:62`: "must be the same as error()"), and
/// `parse_options()` answers that with a bare `exit(129)` — only the
/// `PARSE_OPT_UNKNOWN` arm below it calls `usage_with_options()`. Verified
/// against stock 2.55.0, stderr only: `ls-files --exclude` 41 bytes,
/// `--format` 40, `--with-tree` 43, `--abbrev=x` 49,
/// `--exclude-per-directory` 55, `-x` 35 — against 2117 for `--zzbogus`.
fn option_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(129)
}

/// git's `usage_msg_opt`, used for the incompatible-option combinations the
/// command rejects after parsing: a `fatal:` line, a blank line, then the usage
/// block, still at exit code 129.
fn usage_fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}\n");
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
/// The (a) wording comes from [`crate::pathspec`], which every verb that takes a
/// pathspec shares; see its notes on why nothing gitoxide rejects there is
/// something git accepts, so translating the failure is right and gating it is
/// not.
fn check_pathspecs(
    repo: &gix::Repository,
    patterns: &[BString],
    raw_patterns: &[String],
) -> Result<Option<ExitCode>> {
    // `init_pathspec_magic()` runs first and looks only at the four global
    // settings, so a contradictory pair is fatal before any element is read.
    if let Some(msg) = crate::pathspec::global_magic_fatal() {
        eprintln!("fatal: {msg}");
        return Ok(Some(ExitCode::from(128)));
    }
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
            Err(err) => {
                eprintln!(
                    "fatal: {}",
                    crate::pathspec::parse_error_message(raw.as_str().into(), &err)
                );
                return Ok(Some(ExitCode::from(128)));
            }
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

/// `git ls-files` — list index entries, and optionally worktree-derived sets.
///
/// Supported invocations:
///   * `-c/--cached` (the default), `-s/--stage`, `-u/--unmerged`
///   * `-d/--deleted`, `-m/--modified`, `-o/--others`, `-k/--killed`, `--directory`
///   * `--empty-directory`/`--no-empty-directory` (show/hide empty untracked dirs)
///   * `-t` (status tags), `--deduplicate`, `--error-unmatch`
///   * `--full-name`, `-z`, `--abbrev[=<n>]`, `--with-tree <tree-ish>`
///   * `--eol`, `--sparse`, `--recurse-submodules`
///   * trailing pathspecs, optionally after `--`
///
/// Output ordering mirrors git exactly: the directory walk emits its `--others`
/// lines and then its `--killed` lines (git's `show_other_files` followed by
/// `show_killed_files` over the same collected entries), then a single pass over
/// the index emits, per entry, the cached line, the deleted line, and the
/// modified line in that order.
///
/// Exclude handling is git's: `-x`/`-X` form the highest-priority override group
/// and `--exclude-standard` adds `info/exclude`, `core.excludesFile` and the
/// per-directory `.gitignore` files. Nothing on disk is consulted when neither was
/// given, so the directory walk hands over every untracked *and* ignored path and
/// this exclude stack alone classifies them.
///
/// `--exclude-per-directory=<file>` renames the per-directory ignore file, git's
/// `dir.exclude_per_dir`, which it *replaces* rather than adds to: with it, no
/// `.gitignore` is read anywhere. It reads no `info/exclude` or `core.excludesFile`
/// on its own, so those arrive only with `--exclude-standard`, which itself sets the
/// per-directory name back to `.gitignore` — whichever of the two comes last on the
/// command line wins. `--no-exclude-per-directory` clears the name, leaving no
/// per-directory file at all. A set name also satisfies the `--ignored` guard.
///
/// `--eol` reproduces git's `write_eolinfo`: `i/<eolinfo> w/<eolinfo>
/// attr/<eolattr>` columns derived from `convert.c`'s text statistics over the
/// indexed blob and the worktree file, plus the `text`/`crlf`/`eol` attributes.
/// The same statistics back the `%(eolinfo:index)`, `%(eolinfo:worktree)` and
/// `%(eolattr)` `--format` atoms.
///
/// `--recurse-submodules` opens every active submodule gitlink and lists its
/// index with the submodule path prefixed, recursing for nested submodules; git's
/// `-d/-o/-u/-k/-m/--resolve-undo/--with-tree` and `--error-unmatch` rejections
/// are reproduced. `--sparse` keeps sparse directory entries collapsed; without
/// it a sparse index is expanded to a full one first, exactly like git's
/// `ensure_full_index`, including its `advice.sparseIndexExpanded` hint.
///
/// `-v`, `-f`, `--debug`, `--format`, `--resolve-undo` and `--with-tree` are
/// ported as well.
pub fn ls_files(args: &[String]) -> Result<ExitCode> {
    // `show_usage_with_options_if_asked()` (builtin/ls-files.c:670): a lone `-h`
    // answers on stdout at 129, before the index is read.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }

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
        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): an exact match tested ahead of
        // parse_long_opt(), so it neither abbreviates nor takes an `=<value>`.
        // This table has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders
        // the same block `-h` prints.
        if s == "--help-all" {
            return Ok(super::show_usage(USAGE));
        }
        // Resolve a long option's name the way `parse_long_opt()` does before
        // dispatching on it, so a unique abbreviation (`--stag`) reaches the arm
        // its full spelling reaches and an ambiguous one is refused by name.
        let resolved = match super::canonical_long(s, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(s, &first, &second, USAGE))
            }
        };
        let s = resolved.as_ref();
        // `do_get_value()` (parse-options.c:138-143): an `=<value>` written on an
        // option the table declares `PARSE_OPT_NOARG` is refused by *name*, with
        // the `error:` line alone and no usage block — not as an unknown option.
        if let Some((head, _)) = s.split_once('=') {
            let bare = head.trim_start_matches('-');
            let (stem, negated) = match bare.strip_prefix("no-") {
                Some(stem) => (stem, true),
                None => (bare, false),
            };
            if LONG_OPTS
                .iter()
                .any(|o| o.name == stem && matches!(o.arg, super::Arg::None))
            {
                let name = if negated {
                    crate::parseopt::OptName::Unset(stem)
                } else {
                    crate::parseopt::OptName::Long(stem)
                };
                return Ok(crate::parseopt::takes_no_value(name));
            }
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
            "--killed" => opts.killed = true,
            "--no-killed" => opts.killed = false,
            "--eol" => opts.eol = true,
            "--no-eol" => opts.eol = false,
            "--recurse-submodules" => opts.recurse_submodules = true,
            "--no-recurse-submodules" => opts.recurse_submodules = false,
            "--sparse" => opts.sparse = true,
            "--no-sparse" => opts.sparse = false,
            "--ignored" => opts.ignored = true,
            "--no-ignored" => opts.ignored = false,
            "--directory" => opts.directory = true,
            "--no-directory" => opts.directory = false,
            "--empty-directory" => opts.hide_empty_dir = false,
            "--no-empty-directory" => opts.hide_empty_dir = true,
            "--deduplicate" => opts.dedup = true,
            "--no-deduplicate" => opts.dedup = false,
            "--error-unmatch" => opts.error_unmatch = true,
            "--no-error-unmatch" => opts.error_unmatch = false,
            "--debug" => opts.debug = true,
            "--no-debug" => opts.debug = false,
            // git's `option_parse_exclude_standard` latches `exc_given` and calls
            // `setup_standard_excludes`.
            "--exclude-standard" => {
                opts.exclude_standard = true;
                opts.exc_given = true;
                // `setup_standard_excludes` also (re)sets `dir->exclude_per_dir`, so a
                // `--exclude-per-directory` that came earlier is overridden by it.
                opts.exclude_per_directory = Some(".gitignore".to_string());
            }
            // git's `dir.exclude_per_dir`, a plain string option: it replaces `.gitignore`
            // rather than adding to it, and takes no part in `exc_given` — the `--ignored`
            // guard accepts it simply by the field being non-`NULL`.
            "--exclude-per-directory" => match args.get(i + 1) {
                Some(v) => {
                    opts.exclude_per_directory = Some(v.clone());
                    i += 1;
                }
                None => {
                    return Ok(option_error("option `exclude-per-directory' requires a value"));
                }
            },
            _ if s.starts_with("--exclude-per-directory=") => {
                opts.exclude_per_directory = Some(s["--exclude-per-directory=".len()..].to_string());
            }
            "--no-exclude-per-directory" => opts.exclude_per_directory = None,
            "--full-name" => opts.full_name = true,
            "--abbrev" => opts.abbrev = Some(None),
            "--no-abbrev" => opts.abbrev = None,
            "--exclude" => match args.get(i + 1) {
                Some(v) => {
                    opts.exclude.push(v.clone());
                    opts.exc_given = true;
                    i += 1;
                }
                None => return Ok(option_error("option `exclude' requires a value")),
            },
            _ if s.starts_with("--exclude=") => {
                opts.exclude.push(s["--exclude=".len()..].to_string());
                opts.exc_given = true;
            }
            "--exclude-from" => match args.get(i + 1) {
                Some(v) => {
                    if let Some(code) = reject_exclude_file(v) {
                        return Ok(code);
                    }
                    opts.exclude_from.push(v.clone());
                    opts.exc_given = true;
                    i += 1;
                }
                None => return Ok(option_error("option `exclude-from' requires a value")),
            },
            _ if s.starts_with("--exclude-from=") => {
                let file = s["--exclude-from=".len()..].to_string();
                if let Some(code) = reject_exclude_file(&file) {
                    return Ok(code);
                }
                opts.exclude_from.push(file);
                opts.exc_given = true;
            }
            "--format" => match args.get(i + 1) {
                Some(v) => {
                    opts.format = Some(v.clone());
                    i += 1;
                }
                None => return Ok(option_error("option `format' requires a value")),
            },
            _ if s.starts_with("--format=") => {
                opts.format = Some(s["--format=".len()..].to_string());
            }
            "--with-tree" => match args.get(i + 1) {
                Some(v) => {
                    opts.with_tree = Some(v.clone());
                    i += 1;
                }
                None => return Ok(option_error("option `with-tree' requires a value")),
            },
            _ if s.starts_with("--with-tree=") => {
                opts.with_tree = Some(s["--with-tree=".len()..].to_string());
            }
            "--no-with-tree" => opts.with_tree = None,
            _ if s.starts_with("--abbrev=") => {
                let raw = &s["--abbrev=".len()..];
                let Ok(n) = raw.parse::<usize>() else {
                    return Ok(option_error("option `abbrev' expects a numerical value"));
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
                        'k' => opts.killed = true,
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
                                        return Ok(option_error(&format!(
                                            "switch `{c}' requires a value"
                                        )));
                                    }
                                }
                            };
                            opts.exc_given = true;
                            if c == 'x' {
                                opts.exclude.push(val);
                            } else {
                                if let Some(code) = reject_exclude_file(&val) {
                                    return Ok(code);
                                }
                                opts.exclude_from.push(val);
                            }
                            break;
                        }
                        // parse_options' `internal_help` inside the
                        // short-option loop, which the entry-point check only
                        // covers for a lone `-h`.
                        'h' => return Ok(super::show_usage(USAGE)),
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

    // `--format` shares git's exact incompatibility set (exit 129).
    if opts.format.is_some()
        && (opts.stage
            || opts.others
            || opts.killed
            || opts.tags
            || opts.dedup
            || opts.resolve_undo
            || opts.eol)
    {
        return Ok(usage_fatal(
            "--format cannot be used with -s, -o, -k, -t, --resolve-undo, --deduplicate, --eol",
        ));
    }

    // "There's no point in showing unmerged unless you show the stage."
    if opts.unmerged {
        opts.stage = true;
    }
    // git's `if (show_tag || show_stage) skipping_duplicates = 0;`: the tag and
    // stage columns make every line distinguishable, so dedup is turned off.
    if opts.tags || opts.stage {
        opts.dedup = false;
    }

    // git's setup runs before `cmd_ls_files()` is entered at all, so a missing
    // repository outranks every check above; here it can only outrank the ones
    // below. Opening it this early is also what the work-tree gate needs.
    let repo = crate::setup::discover()?;

    // git's `require_work_tree` (builtin/ls-files.c:707-708, 720-721): the five
    // selectors that read the filesystem need one, and `setup_work_tree()` dies
    // for them when setup found none.
    //
    //     if (show_modified || show_others || show_deleted ||
    //         (dir.flags & DIR_SHOW_IGNORED) || show_killed)
    //             require_work_tree = 1;
    //
    // Nothing else is in the set. `--directory` and `--exclude-standard` only
    // shape a walk one of those five has to have asked for, and `-c`, `-s`, `-u`,
    // `-t` and `--resolve-undo` read the index alone — all of them work in a bare
    // repository. The gate sits *before* the guards below, which is why
    // `ls-files -i` in one reports the work tree rather than the missing
    // `-o`/`-c`, and after the `--format` rejection above, which still wins.
    //
    // A work tree named by `GIT_WORK_TREE` satisfies it: git installs that in
    // `setup_explicit_git_dir()` before `core.bare` is read, so `ls-files -o` in a
    // bare repository lists the given tree instead of dying.
    if (opts.modified || opts.others || opts.deleted || opts.ignored || opts.killed)
        && !repo.workdir().is_some_and(|wt| wt.is_dir())
    {
        // `setup_work_tree()` dies with the same line whether no work tree was
        // configured or `chdir()` into the configured one failed (setup.c:503-505).
        return Err(crate::fatal::need_work_tree());
    }

    // git's `--recurse-submodules` guards, both fatal (exit 128) and both checked
    // before the default `-c` is applied, so `--recurse-submodules` on its own is
    // fine while any of the worktree-facing selectors is not.
    if opts.recurse_submodules
        && (opts.deleted
            || opts.others
            || opts.unmerged
            || opts.killed
            || opts.modified
            || opts.resolve_undo
            || opts.with_tree.is_some())
    {
        eprintln!("fatal: ls-files --recurse-submodules unsupported mode");
        return Ok(ExitCode::from(128));
    }
    if opts.recurse_submodules && opts.error_unmatch {
        eprintln!("fatal: ls-files --recurse-submodules does not support --error-unmatch");
        return Ok(ExitCode::from(128));
    }

    // git's two `-i` guards, in order, each fatal (exit 128). Both run *before*
    // the implicit `-c` below, so a bare `ls-files -i` reports the first one.
    if opts.ignored && !opts.others && !opts.cached {
        eprintln!("fatal: ls-files -i must be used with either -o or -c");
        return Ok(ExitCode::from(128));
    }
    // A set `dir.exclude_per_dir` counts as an exclude source here, so
    // `-i --exclude-per-directory=<file>` is accepted while a following
    // `--no-exclude-per-directory` makes it fatal again.
    if opts.ignored && !opts.exc_given && opts.exclude_per_directory.is_none() {
        eprintln!("fatal: ls-files --ignored needs some exclude pattern");
        return Ok(ExitCode::from(128));
    }

    // With no selector at all, git lists the cache.
    if !opts.cached
        && !opts.stage
        && !opts.deleted
        && !opts.modified
        && !opts.others
        && !opts.killed
        && !opts.unmerged
        && !opts.resolve_undo
    {
        opts.cached = true;
    }

    // git's `do_read_index` (read-cache.c:2216-2224) only dies on an open
    // failure when the caller demanded the file; `ls-files` reaches it through
    // `read_index_from`'s `must_exist == 0` path, so a missing index is an
    // initialized-but-empty one. A repository that has never staged anything
    // therefore prints nothing and exits 0 instead of erroring.
    let mut index = crate::index_open::or_empty(&repo)?;

    // git's lazy `ensure_full_index`: unless `--sparse` was asked for, a sparse
    // directory entry is replaced by the blobs of the tree it stands for, each
    // marked skip-worktree, before any index-derived line is produced.
    if !opts.sparse && (opts.shows_index_entries() || opts.deleted || opts.modified) {
        expand_sparse_index(&repo, &mut index)?;
    }

    // `--with-tree <tree-ish>` overlays the named tree onto the index so that
    // paths removed since that tree are still listed (git's
    // `overlay_tree_on_index`). git rejects it alongside `-s`/`-u` ("show-stages
    // and show-unmerged would not make any sense"); the message and exit 128 are
    // git's. Tree paths that already have a stage-0 index entry stay hidden (git
    // marks them `CE_UPDATE`); the rest are appended as stage-1 entries, so they
    // render with git's `tag_unmerged` (`M `) and, under `-d`/`-m`, are compared
    // against the worktree directly. The overlaid paths are tracked so those
    // per-entry worktree checks bypass the index-status pass, which never saw
    // them (it runs over the on-disk index, not this in-memory overlay).
    let mut overlaid: HashSet<BString> = HashSet::new();
    if let Some(spec) = opts.with_tree.clone() {
        if opts.stage {
            eprintln!(
                "fatal: options 'ls-files --with-tree' and '-s/-u' cannot be used together"
            );
            return Ok(ExitCode::from(128));
        }
        // `overlay_tree_on_index` (read-cache.c) splits the two failures: only a
        // name that does not resolve at all is "tree-ish %s not found.", while
        // `repo_parse_tree_indirect()` returning NULL — a missing object as much
        // as a non-tree — is "bad tree-ish %s". `repo_get_oid()` accepts a
        // full-length hex string without consulting the odb (see
        // [`crate::objname::full_hex`]), so an absent id lands in the second.
        let Some(id) = crate::objname::resolve(&repo, spec.as_str()) else {
            eprintln!("fatal: tree-ish {spec} not found.");
            return Ok(ExitCode::from(128));
        };
        let peeled = repo
            .find_object(id)
            .ok()
            .and_then(|o| o.peel_to_tree().ok());
        let Some(tree) = peeled else {
            eprintln!("fatal: bad tree-ish {spec}");
            return Ok(ExitCode::from(128));
        };

        // Paths already carried at stage 0 keep their index entry (git's
        // `last_stage0` / `CE_UPDATE` dedup); collect them before mutating so the
        // overlay never double-lists a still-tracked path.
        let stage0: HashSet<BString> = index
            .entries()
            .iter()
            .filter(|e| e.stage_raw() == 0)
            .map(|e| e.path(&index).to_owned())
            .collect();

        // The recorder reports the trees it descends through as well; only the
        // leaves are paths git's `overlay_tree_on_index` puts into the index.
        let tree_entries = tree.traverse().breadthfirst.files()?;
        for te in tree_entries {
            if te.mode.is_tree() || stage0.contains(te.filepath.as_bstr()) {
                continue;
            }
            index.dangerously_push_entry(
                gix::index::entry::Stat::default(),
                te.oid,
                gix::index::entry::Flags::from_stage(gix::index::entry::Stage::Base),
                gix::index::entry::Mode::from(te.mode),
                te.filepath.as_bstr(),
            );
            overlaid.insert(te.filepath);
        }
        // Restore the sort invariant the raw pushes broke, so the emit pass and
        // any path lookups iterate in git's name order.
        index.sort_entries();
    }

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

    // git runs `fill_directory` for `-o` *or* `-k`; both read the same collected
    // entry list. For `-k` without `-o` git also sets `DIR_COLLECT_KILLED_ONLY`,
    // which prunes directories the index has nothing at or below — nothing under
    // such a directory can ever be killed, so skipping that pruning costs walk
    // time but cannot change which entries the killed test keeps.
    let walk = opts.others || opts.killed;
    let worktree = if walk || opts.modified || opts.deleted {
        // git shows empty untracked directories (`dir/`) under `--directory` by
        // default; `--no-empty-directory` suppresses them. gix's walk hides them
        // unless `emit_empty_directories` is set, so enable it to match git's
        // default whenever a collapsed `--directory` walk is in play.
        let emit_empty = walk && opts.directory && !opts.hide_empty_dir;
        Some(collect_worktree(&repo, walk, emit_empty)?)
    } else {
        None
    };

    // `-z` suppresses quoting outright (`write_name_quoted`'s `line_terminator`
    // check): NUL-terminated output is meant to be unambiguous. Otherwise the
    // name goes through `quote_c_style()`, which is where `core.quotePath` is
    // read — the key decides whether bytes >= 0x80 are escaped, never whether a
    // control byte, a quote or a backslash is.
    crate::quote::init(&repo);
    let quote = !opts.zero;
    // Each rendered line carries its own terminator (and, under `--debug`, the
    // trailing stat block) so the emit loop can stay a verbatim byte copy.
    let terminator = if opts.zero { b'\0' } else { b'\n' };

    // The convert-stat machinery behind `--eol` and the three `%(eol…)` format
    // atoms. It caches an attribute stack, so build it once and only when a
    // caller can actually reach it.
    let mut eol = if opts.eol || opts.format.is_some() {
        Some(Eol::new(&repo, &index)?)
    } else {
        None
    };

    let mut lines: Vec<Vec<u8>> = Vec::new();

    // Phase 1: the directory walk, exactly as git emits it before touching the
    // index — every `? ` line first (`show_other_files`), then every `K ` line
    // (`show_killed_files`), both drawn from the same collected entry list.
    if walk {
        if let Some(state) = &worktree {
            // The pathspec is matched against the bare path; the trailing slash
            // that `--directory` prints is presentation only. The candidate set is
            // untracked ∪ ignored so our own exclude stack — not gix's `.gitignore`
            // classification — decides which to keep.
            let walked: Vec<(BString, bool)> = state
                .others
                .iter()
                .filter(|(path, is_dir)| ps.is_included(path.as_bstr(), Some(*is_dir)))
                .cloned()
                .collect();
            let mut candidates: Vec<(BString, bool)> = walked
                .iter()
                .map(|(path, is_dir)| {
                    if opts.directory {
                        collapse_other_directory(&index, path.as_bstr(), *is_dir)
                    } else {
                        (path.clone(), *is_dir)
                    }
                })
                .collect();
            candidates.sort();
            candidates.dedup();

            // `DIR_HIDE_EMPTY_DIRECTORIES` (`--no-empty-directory`). A collapsed
            // directory is only reported if `read_directory_recursive()` found
            // something under it to report; otherwise `treat_directory()` leaves
            // the state at `path_none` and nothing is emitted:
            //
            // ```c
            // if (state == path_none && !(dir->flags & DIR_HIDE_EMPTY_DIRECTORIES))
            //         state = excluded ? path_excluded : path_untracked;
            // ```
            // (dir.c:2091-2092)
            //
            // "Something to report" is a path below it that survives the same
            // exclude verdict this listing keeps, so a directory holding nothing
            // but ignored files is as empty as one holding no files at all. A
            // directory the walk emitted in its own right — a nested repository,
            // or an empty directory under the default `--empty-directory` — is
            // never subject to this, exactly as `treat_directory()` returns those
            // before it ever recurses.
            let mut nonempty: HashSet<BString> = HashSet::new();
            if opts.directory && opts.hide_empty_dir {
                for (path, is_dir) in &walked {
                    if *is_dir {
                        nonempty.insert(path.clone());
                        continue;
                    }
                    if matcher.is_excluded(path.as_bstr(), false) != opts.ignored {
                        continue;
                    }
                    for (at, _) in path.iter().enumerate().filter(|(_, b)| **b == b'/') {
                        nonempty.insert(BString::from(&path[..at]));
                    }
                }
            }
            // `-i` keeps only excluded paths; the default keeps only the rest.
            // This is the whole of git's `dir->entries`, so `-o` and `-k` share it.
            // A directory entry carries git's trailing `/` in its very name, which
            // both the killed test and the printed line depend on.
            //
            // Not covered: `-i --directory`'s roll-up of a directory whose whole
            // subtree is ignored. `treat_directory()` turns that into a
            // `path_excluded` for the directory itself and then pops the ignored
            // paths it collected below — all but the first, because the loop starts
            // at `old_ignored_nr + 1` (dir.c:2070-2074) — so stock reports the
            // directory *and* one nested level. This listing reports the individual
            // files instead. Reproducing it needs git's recursive state machine,
            // not a post-filter over a flat walk.
            let entries: Vec<BString> = candidates
                .into_iter()
                .filter(|(path, is_dir)| matcher.is_excluded(path.as_bstr(), *is_dir) == opts.ignored)
                .filter(|(path, is_dir)| {
                    !(*is_dir && opts.directory && opts.hide_empty_dir) || nonempty.contains(path)
                })
                .map(|(mut name, is_dir)| {
                    if is_dir {
                        name.push(b'/');
                    }
                    name
                })
                .collect();

            if opts.others {
                // `show_other_files` drops anything the index already knows under
                // that name; `show_killed_files` below deliberately does not.
                for name in entries.iter().filter(|n| index_name_is_other(&index, n.as_bstr())) {
                    let display = strip_prefix(name.as_bstr(), prefix.as_ref()).to_vec();
                    lines.push(render(
                        &opts,
                        "? ",
                        None,
                        &repo,
                        name.as_bstr(),
                        &display,
                        quote,
                        terminator,
                        eol.as_mut(),
                    ));
                }
            }
            if opts.killed {
                for name in entries.iter().filter(|n| is_killed(&index, n.as_bstr())) {
                    let display = strip_prefix(name.as_bstr(), prefix.as_ref()).to_vec();
                    lines.push(render(
                        &opts,
                        "K ",
                        None,
                        &repo,
                        name.as_bstr(),
                        &display,
                        quote,
                        terminator,
                        eol.as_mut(),
                    ));
                }
            }
        }
    }

    // Phase 2: one pass over the index; each entry can contribute a cached line,
    // a deleted line, and a modified line, in that order.
    let mut matched: HashSet<usize> = HashSet::new();
    // git's `is_submodule_active` gate on the gitlink entries `--recurse-submodules`
    // descends into. Resolved once per repository, as `.gitmodules` cannot change
    // mid-listing.
    let active = active_submodules(&repo, &opts);
    for entry in index.entries() {
        let path = entry.path(&index);
        // git's `show_ce` swaps an active submodule's own line for the listing of
        // that submodule's index *before* it consults the pathspec, which is what
        // lets a spec name something inside the submodule.
        if opts.recurse_submodules
            && opts.shows_index_entries()
            && entry.mode == gix::index::entry::Mode::COMMIT
            && active.contains(path)
        {
            if !opts.ignored || matcher.is_excluded(path, true) {
                emit_submodule(
                    &repo,
                    path,
                    path,
                    &opts,
                    &mut ps,
                    &mut matcher,
                    prefix.as_ref(),
                    quote,
                    terminator,
                    eol.as_mut(),
                    &mut lines,
                )?;
            }
            continue;
        }
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
        let display = display.as_slice();

        if opts.shows_index_entries() && !(opts.unmerged && stage == 0) {
            // git's `show_ce` replaces an active submodule's own line with the
            // listing of that submodule's index; the gitlink itself is not shown.
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
            lines.push(render(
                &opts,
                tag,
                Some(entry),
                &repo,
                path,
                display,
                quote,
                terminator,
                eol.as_mut(),
            ));
        }

        if opts.deleted || opts.modified {
            let state = worktree.as_ref().expect("collected when -d/-m is set");
            // Overlaid (`--with-tree`) entries never appeared in the index-status
            // pass, so force the direct worktree comparison for them, exactly as
            // git's `show_files` lstats every cache entry uniformly.
            let direct = overlaid.contains(path);
            let (is_deleted, is_modified) =
                entry_worktree_change(&repo, state, entry, path, direct);
            if opts.deleted && is_deleted {
                lines.push(render(
                    &opts,
                    "R ",
                    Some(entry),
                    &repo,
                    path,
                    display,
                    quote,
                    terminator,
                    eol.as_mut(),
                ));
            }
            if opts.modified && is_modified {
                lines.push(render(
                    &opts,
                    "C ",
                    Some(entry),
                    &repo,
                    path,
                    display,
                    quote,
                    terminator,
                    eol.as_mut(),
                ));
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
                let display = display.as_slice();
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

    // `expand_show_index()` (`builtin/ls-files.c`) dies on an atom it does not
    // know, while expanding the *first* entry — so the refusal lands before any
    // output, and a run with nothing to show never reaches it.
    if !lines.is_empty() {
        if let Some(atom) = opts.format.as_deref().and_then(unknown_format_atom) {
            eprintln!("fatal: bad ls-files format: {atom}");
            return Ok(ExitCode::from(128));
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

    // `cmd_ls_files` calls `show_files()` first and only then hands `ps_matched`
    // to `report_path_error()`, so the entries that *did* match are already on
    // stdout when the diagnostic is written. It reports every unmatched element,
    // one `error:` line each, and prints the hint once afterwards.
    //
    // An `:(exclude)` element is never among them: `parse_pathspec()` appends an
    // implicit empty positive item when every element is an exclude, and
    // `do_match_pathspec()` marks an exclude item seen as soon as it excludes
    // something — verified against git 2.55.0, where `ls-files --error-unmatch --
    // :!nosuch.txt` exits 0 and says nothing.
    if opts.error_unmatch {
        let mut bad = 0usize;
        for (i, raw) in raw_patterns.iter().enumerate() {
            if matched.contains(&i) || is_exclude_pathspec(raw) {
                continue;
            }
            eprintln!("error: pathspec '{raw}' did not match any file(s) known to git");
            bad += 1;
        }
        if bad > 0 {
            eprintln!("Did you forget to 'git add'?");
            return Ok(ExitCode::from(1));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The first thing in `fmt` that `expand_show_index()` (builtin/ls-files.c:244-287)
/// refuses, rendered as the text its `die()` puts after `bad ls-files format: `.
///
/// It has three refusals, and `strbuf_expand()` reaches them in this order for
/// every `%` that `strbuf_expand_literal_cb()` did not consume itself (`%n` and a
/// well-formed `%xNN`; `%%` is `strbuf_expand()`'s own, strbuf.c:428-431):
///
/// * not followed by `(` — `element '<rest of the format>' does not start with '('`,
///   which a trailing bare `%` reaches with an empty element;
/// * `(` with no `)` after it — `element '<rest>' does not end in ')'`;
/// * a well-formed element naming an atom it does not know — `%(<atom>)`.
///
/// The atom list is [`expand_format`]'s; the two must stay in step, since an atom
/// this accepts and that one ignores would print empty instead of dying.
fn unknown_format_atom(fmt: &str) -> Option<String> {
    const ATOMS: [&str; 10] = [
        "objectmode",
        "objecttype",
        "objectname",
        "objectsize",
        "objectsize:padded",
        "stage",
        "path",
        "eolinfo:index",
        "eolinfo:worktree",
        "eolattr",
    ];
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            i += 1;
            continue;
        }
        match chars.get(i + 1) {
            Some('%') => {
                i += 2;
                continue;
            }
            // `strbuf_expand_literal_cb` consumes `%n` and `%xNN` before the
            // `(` check ever runs (strbuf.c:405-421).
            Some('n') => {
                i += 2;
                continue;
            }
            Some('x') if i + 3 < chars.len() => {
                let hex: String = chars[i + 2..i + 4].iter().collect();
                if u8::from_str_radix(&hex, 16).is_ok() {
                    i += 4;
                    continue;
                }
            }
            None | Some(_) => {}
        }
        // Everything from here is `expand_show_index()`'s `start`, i.e. the rest
        // of the format after the `%`. A missing `(` and a missing `)` both name
        // that whole remainder.
        let rest: String = chars[i + 1..].iter().collect();
        if chars.get(i + 1) != Some(&'(') {
            return Some(format!("element '{rest}' does not start with '('"));
        }
        let Some(close) = chars[i + 2..].iter().position(|&c| c == ')') else {
            return Some(format!("element '{rest}' does not end in ')'"));
        };
        let atom: String = chars[i + 2..i + 2 + close].iter().collect();
        if !ATOMS.contains(&atom.as_str()) {
            return Some(format!("%({atom})"));
        }
        i += 2 + close + 1;
    }
    None
}

/// `pattern` with its trailing spaces escaped, so a parser that strips
/// unescaped ones leaves it alone. Patterns with no trailing space come back
/// unchanged.
fn keep_trailing_spaces(pattern: &str) -> String {
    let trimmed = pattern.trim_end_matches(' ');
    if trimmed.len() == pattern.len() {
        return pattern.to_string();
    }
    let mut out = trimmed.to_string();
    for _ in 0..pattern.len() - trimmed.len() {
        out.push_str("\\ ");
    }
    out
}

/// Whether a pathspec element carries `exclude` magic, in either spelling —
/// the `:!`/`:^` short form or the long `:(exclude)` / `:(…,exclude,…)` one.
fn is_exclude_pathspec(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix(':') else {
        return false;
    };
    if rest.starts_with('!') || rest.starts_with('^') {
        return true;
    }
    let Some(long) = rest.strip_prefix('(') else {
        return false;
    };
    let Some((keywords, _)) = long.split_once(')') else {
        return false;
    };
    keywords.split(',').any(|k| k.trim() == "exclude")
}

/// git's `ADVICE_MSG` from `sparse-index.c`, printed once through the `hint:`
/// channel when a sparse index has to be expanded to a full one.
const SPARSE_EXPANDED_ADVICE: &str = "\
The sparse index is expanding to a full index, a slow operation.
Your working directory likely has contents that are outside of
your sparse-checkout patterns. Use 'git sparse-checkout list' to
see your sparse-checkout definition and compare it to your working
directory contents. Cleaning up any merge conflicts or staged
changes before running 'git sparse-checkout clean' or 'git
sparse-checkout reapply' may assist in this cleanup.";

/// Port of git's `ensure_full_index`/`expand_index(istate, NULL)`.
///
/// A sparse index stores a whole out-of-cone directory as a single entry whose
/// mode is `040000` and whose name carries a trailing `/`, pointing at the tree
/// that directory would expand to. Replace each of those with the blobs of that
/// tree, prefixed by the directory name and flagged `SKIP_WORKTREE`, which is
/// what git's `add_path_to_index` callback produces.
///
/// Returns `true` when at least one entry was expanded, which is also the
/// condition under which git emits its `advice.sparseIndexExpanded` hint.
fn expand_sparse_index(repo: &gix::Repository, index: &mut gix::index::File) -> Result<bool> {
    let sparse: Vec<(BString, gix::ObjectId)> = {
        let state: &gix::index::State = index;
        state
            .entries()
            .iter()
            .filter(|e| e.mode == gix::index::entry::Mode::DIR)
            .map(|e| (e.path(state).to_owned(), e.id))
            .collect()
    };
    if sparse.is_empty() {
        return Ok(false);
    }

    index.remove_entries(|_, _, e| e.mode == gix::index::entry::Mode::DIR);
    for (dir, tree_id) in &sparse {
        let Some(tree) = repo
            .find_object(*tree_id)
            .ok()
            .and_then(|o| o.peel_to_tree().ok())
        else {
            continue;
        };
        // The traversal records the trees it descends through as well; only the
        // leaves become index entries, which is what git's `add_path_to_index`
        // callback does with its `READ_TREE_RECURSIVE` return.
        for te in tree.traverse().breadthfirst.files()?.into_iter().filter(|te| !te.mode.is_tree()) {
            let mut path = dir.clone();
            path.extend_from_slice(&te.filepath);
            index.dangerously_push_entry(
                gix::index::entry::Stat::default(),
                te.oid,
                gix::index::entry::Flags::SKIP_WORKTREE,
                gix::index::entry::Mode::from(te.mode),
                path.as_bstr(),
            );
        }
    }
    index.sort_entries();

    if repo
        .config_snapshot()
        .boolean("advice.sparseIndexExpanded")
        .unwrap_or(true)
    {
        for line in SPARSE_EXPANDED_ADVICE.lines() {
            eprintln!("hint: {line}");
        }
        eprintln!(
            "hint: Disable this message with \"git config set advice.sparseIndexExpanded false\""
        );
    }
    Ok(true)
}

/// The exclude machinery git configures from `-x`, `-X`, `--exclude-standard` and
/// `--exclude-per-directory`.
///
/// Three shapes, mirroring what git consults:
///   * [`Excludes::Stack`] — a worktree exclude stack with the `-x`/`-X` patterns
///     layered on top as the highest-priority override group. Built whenever an
///     on-disk ignore file is in play, i.e. for `--exclude-standard` (which adds the
///     `info/exclude` and `core.excludesFile` globals and reads `.gitignore` per
///     directory) and for `--exclude-per-directory=<file>` (which reads `<file>` per
///     directory and *no* globals). Given both, the globals come from the former and
///     the per-directory name from whichever was parsed last.
///   * [`Excludes::Overrides`] — just the `-x`/`-X` patterns, matched directly,
///     with no on-disk ignore files consulted at all (git's behaviour without
///     `--exclude-standard` and without a per-directory file).
///   * [`Excludes::None`] — nothing configured; nothing is ever excluded.
#[allow(clippy::large_enum_variant)] // boxing would churn every construct/match site
enum Excludes<'repo> {
    None,
    Overrides {
        /// git's `EXC_CMDL`: the `-x` patterns, consulted first.
        cmdl: gix::ignore::Search,
        /// git's `EXC_FILE`: the `-X`/`--exclude-from` files, consulted last, in
        /// the reverse of the order they were named (dir.c:1514-1523).
        files: gix::ignore::Search,
        case: gix::glob::pattern::Case,
    },
    Stack {
        stack: gix::AttributeStack<'repo>,
    },
}

impl<'repo> Excludes<'repo> {
    fn build(repo: &'repo gix::Repository, index: &gix::index::State, opts: &Opts) -> Result<Self> {
        let has_overrides = !opts.exclude.is_empty() || !opts.exclude_from.is_empty();
        // An unset or empty `dir.exclude_per_dir` reads no per-directory file at all.
        let per_directory = opts
            .exclude_per_directory
            .as_deref()
            .filter(|name| !name.is_empty());
        if !opts.exclude_standard && !has_overrides && per_directory.is_none() {
            return Ok(Excludes::None);
        }

        let parse = gix::ignore::search::Ignore {
            support_precious: false,
        };
        // `-x` patterns first (git's `EXC_CMDL`), then each `-X` file appended.
        //
        // A command-line pattern keeps its trailing spaces: `trim_trailing_spaces()`
        // runs in `add_patterns_from_buffer()`, over the bytes of a *file*, and
        // `add_pattern()` — which is what `-x` reaches — does not call it. So
        // `-x 'name.txt '` matches a file whose name ends in a space and nothing
        // else. gitoxide parses an override through the same line parser as a
        // file and would trim it, so the trailing run is escaped here, which is
        // the spelling that parser preserves.
        let cmdline = opts.exclude.iter().map(|p| keep_trailing_spaces(p));
        let search = gix::ignore::Search::from_overrides(cmdline, parse);
        // The `-X` files, read once here and handed to whichever shape is built
        // below — the *globals* group of a stack, or the low-priority half of
        // [`Excludes::Overrides`]. Never the override group: that is `-x` alone.
        let mut exclude_from: Vec<(Vec<u8>, String)> = Vec::new();
        for file in &opts.exclude_from {
            // `add_patterns()` returns before it reads a zero-sized file
            // (dir.c:1083-1091), so a character device like `/dev/null` is never
            // read at all — only stat'd.
            if std::fs::metadata(file).is_ok_and(|m| m.len() == 0) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(file) {
                exclude_from.push((bytes, file.clone()));
            }
        }

        let case = if repo
            .config_snapshot()
            .boolean("core.ignoreCase")
            .unwrap_or(false)
        {
            gix::glob::pattern::Case::Fold
        } else {
            gix::glob::pattern::Case::Sensitive
        };

        let source = gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped;

        // Without `--exclude-standard` no *global* ignore file is consulted, so unless a
        // per-directory file was named there is nothing on disk left to read and only the
        // command-line patterns can match.
        if !opts.exclude_standard {
            let Some(name) = per_directory else {
                let mut files = gix::ignore::Search::default();
                for (bytes, name) in &exclude_from {
                    files.add_patterns_buffer(bytes, std::path::PathBuf::from(name), None, parse);
                }
                return Ok(Excludes::Overrides {
                    cmdl: search,
                    files,
                    case,
                });
            };
            // `--exclude-per-directory` on its own reads neither `info/exclude` nor
            // `core.excludesFile`, so the stack is assembled with empty globals instead of
            // going through `Repository::excludes()`, which would load both.
            let state = gix::worktree::stack::State::IgnoreStack(
                {
                    let mut ignore = gix::worktree::stack::state::Ignore::new(
                        search,
                        Default::default(),
                        Some(name.into()),
                        source,
                        parse,
                    );
                    for (bytes, name) in &exclude_from {
                        ignore.add_global_patterns_buffer(bytes, std::path::PathBuf::from(name), None);
                    }
                    ignore
                },
            );
            let id_mappings = state.id_mappings_from_index(index, index.path_backing(), case);
            let stack = gix::worktree::Stack::new(
                repo.workdir().unwrap_or_else(|| repo.git_dir()),
                state,
                case,
                Vec::with_capacity(512),
                id_mappings,
            );
            return Ok(Excludes::Stack {
                stack: gix::AttributeStack::new(stack, repo),
            });
        }

        let mut stack = repo.excludes(index, Some(search), source)?;
        // `-X` belongs to `EXC_FILE`, beside `info/exclude` and
        // `core.excludesFile` and *below* the per-directory `.gitignore` files —
        // see [`gix::worktree::stack::state::Ignore::add_global_patterns_buffer`].
        // Appending puts it first within that group, which is where git puts a
        // `-X` that followed `--exclude-standard` on the command line.
        if !exclude_from.is_empty() {
            if let gix::worktree::stack::State::IgnoreStack(ignore) = stack.state_mut() {
                for (bytes, name) in &exclude_from {
                    ignore.add_global_patterns_buffer(bytes, std::path::PathBuf::from(name), None);
                }
            }
        }
        // `--exclude-standard` assembled the stack around `.gitignore`; a later
        // `--exclude-per-directory` renames the file it looks for, and a later
        // `--no-exclude-per-directory` (an empty name) stops it looking for one.
        if per_directory != Some(".gitignore") {
            if let gix::worktree::stack::State::IgnoreStack(ignore) = stack.state_mut() {
                ignore.set_exclude_file_name_for_directories(
                    opts.exclude_per_directory.as_deref().unwrap_or("").into(),
                );
            }
        }
        Ok(Excludes::Stack { stack })
    }

    /// Whether `path` is excluded, i.e. matched by a non-negated pattern.
    fn is_excluded(&mut self, path: &BStr, is_dir: bool) -> bool {
        match self {
            Excludes::None => false,
            // With no on-disk ignore file in play there is no directory stack to
            // carry a directory's verdict down, so it is applied here: git's walk
            // stops at an excluded directory (`treat_directory()` returns
            // `path_excluded` and `read_directory_recursive()` does not descend
            // for the listing modes that hide it), which makes every path below
            // one excluded too. The leading components are therefore tested as
            // directories, longest last, before the path itself.
            Excludes::Overrides { cmdl, files, case } => {
                let bytes = path.as_bytes();
                for (at, _) in bytes.iter().enumerate().filter(|(_, b)| **b == b'/') {
                    if Self::overrides_verdict(cmdl, files, bytes[..at].as_bstr(), true, *case) {
                        return true;
                    }
                }
                Self::overrides_verdict(cmdl, files, path, is_dir, *case)
            }
            Excludes::Stack { stack } => {
                let mode = is_dir.then_some(gix::index::entry::Mode::DIR);
                stack
                    .at_entry(path, mode)
                    .map(|p| p.is_excluded())
                    .unwrap_or(false)
            }
        }
    }

    /// `last_exclude_matching_from_lists()` (dir.c:1514-1527) reduced to the two
    /// groups this shape holds: the first group with a match decides, and a
    /// negated pattern decides "not excluded" rather than falling through to the
    /// next group.
    fn overrides_verdict(
        cmdl: &gix::ignore::Search,
        files: &gix::ignore::Search,
        path: &BStr,
        is_dir: bool,
        case: gix::glob::pattern::Case,
    ) -> bool {
        [cmdl, files]
            .into_iter()
            .find_map(|group| group.pattern_matching_relative_path(path, Some(is_dir), case))
            .is_some_and(|m| !m.pattern.is_negative())
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
    /// Every path the directory walk turned up — untracked *and* gix-ignored —
    /// each flagged as a directory or not (`--directory` prints collapsed
    /// directories with a `/`). This is git's `dir->entries` before its own
    /// exclude verdict is applied: gix's `.gitignore` classification is discarded
    /// because git consults no on-disk ignore file unless asked to, and the
    /// `-x`/`-X`/`--exclude-per-directory` patterns it is asked to use can differ
    /// from `.gitignore` entirely.
    others: Vec<(BString, bool)>,
}

/// Run one index↔worktree status pass and bucket the result.
fn collect_worktree(
    repo: &gix::Repository,
    others: bool,
    emit_empty: bool,
) -> Result<Worktree> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let mut out = Worktree {
        removed: HashSet::new(),
        modified: HashSet::new(),
        conflicted: HashSet::new(),
        others: Vec::new(),
    };

    // Always ask for individual files. `--directory` collapsing is git's own
    // index-driven rule, applied by the caller over the full path list; gix's
    // `Collapsed` mode decides it from what is tracked *and present*, which
    // differs whenever a tracked file below the directory has been deleted.
    let untracked = if others {
        gix::status::UntrackedFiles::Files
    } else {
        gix::status::UntrackedFiles::None
    };

    // Pathspec filtering is applied by the caller against every candidate, so the
    // walk itself stays unrestricted and cannot narrow the set incorrectly.
    let mut platform = repo
        .status(gix::progress::Discard)?
        .untracked_files(untracked);
    // gix hides `.gitignore`-matched paths from the walk, but git's walk only
    // hides what the *caller's* exclude configuration matches, so ask for the
    // ignored entries too and let [`Excludes`] deliver the single verdict.
    // `emit_empty` surfaces empty untracked directories so `--directory` can show
    // them like git's default.
    if others {
        platform = platform.dirwalk_options(move |mut o| {
            o = o.emit_ignored(Some(gix::dir::walk::EmissionMode::Matching));
            if emit_empty {
                o = o.emit_empty_directories(true);
            }
            o
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
                    // gix stops at a directory its `.gitignore` rules exclude and
                    // reports just that directory. git's walk has no such rules
                    // unless the caller supplied them, so recover the contents it
                    // would have collected.
                    gix::dir::entry::Status::Ignored(_) => {
                        let plain_dir = matches!(
                            entry.disk_kind,
                            Some(gix::dir::entry::Kind::Directory)
                        );
                        match repo.workdir().filter(|_| plain_dir) {
                            Some(root) => {
                                expand_ignored_dir(root, entry.rela_path.as_bstr(), &mut out.others);
                            }
                            None => out.others.push((entry.rela_path, is_dir)),
                        }
                    }
                    _ => {}
                }
            }
            Item::Rewrite { .. } => {}
        }
    }
    Ok(out)
}

/// Collect the paths git's `read_directory_recursive` would have gathered under
/// `rela`, which gix handed over as one collapsed ignored directory.
///
/// Mirrors what that walk does with a `readdir` result: `.git` is never
/// descended, a nested repository is reported as the directory itself, a symlink
/// counts as a file rather than a directory to recurse into, and an empty
/// directory contributes nothing.
fn expand_ignored_dir(root: &Path, rela: &BStr, out: &mut Vec<(BString, bool)>) {
    let dir = root.join(gix::path::from_bstr(rela));
    let Ok(read) = std::fs::read_dir(&dir) else {
        out.push((rela.to_owned(), true));
        return;
    };
    for entry in read.flatten() {
        let name = gix::path::into_bstr(PathBuf::from(entry.file_name())).into_owned();
        if name == ".git" {
            continue;
        }
        let mut child = BString::from(rela.to_vec());
        child.push(b'/');
        child.extend_from_slice(&name);
        // `file_type` does not follow symlinks, so a symlink to a directory is a
        // file here, exactly as `DT_LNK` is for git.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if dir.join(entry.file_name()).join(".git").exists() {
                out.push((child, true));
            } else {
                expand_ignored_dir(root, child.as_bstr(), out);
            }
        } else {
            out.push((child, false));
        }
    }
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
    direct: bool,
) -> (bool, bool) {
    // `direct` (an `--with-tree` overlay entry) forces the per-entry lstat+hash
    // path below; otherwise a non-conflicted entry is resolved from the cached
    // index-status buckets.
    if !direct && !state.conflicted.contains(path) {
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

/// git's `index_name_pos`: binary search for `name` at stage 0 over the index,
/// which is ordered by name and then stage, returning `Err(insertion_point)` when
/// there is no stage-0 entry under that exact name.
fn index_name_pos(index: &gix::index::State, name: &[u8]) -> Result<usize, usize> {
    index.entries().binary_search_by(|e| {
        e.path(index)
            .as_bytes()
            .cmp(name)
            .then_with(|| e.stage_raw().cmp(&0))
    })
}

/// git's `index_name_is_other`: a walked path is reported by `-o` only when the
/// index holds no entry under that name at any stage. A collapsed directory
/// entry's trailing `/` is dropped before the lookup, which is what keeps
/// `git ls-files -o --directory` from printing an untracked directory shadowing a
/// tracked file of the same name — while `-k` still reports it as killed.
fn index_name_is_other(index: &gix::index::State, name: &BStr) -> bool {
    let bytes = name.as_bytes();
    let bytes = bytes.strip_suffix(b"/").unwrap_or(bytes);
    let Err(pos) = index_name_pos(index, bytes) else {
        return false; // exact stage-0 match
    };
    // The entry at the insertion point carries the same name only when it is
    // unmerged, which still counts as known to the index.
    !index
        .entries()
        .get(pos)
        .is_some_and(|e| e.path(index).as_bytes() == bytes)
}

/// git's `directory_exists_in_index` reduced to its `index_directory` verdict:
/// does the index hold anything *below* `dir`? Names sort bytewise and `dir/`
/// sorts before every path under it, so the first entry at or after `dir/` is the
/// only one that can carry that prefix.
fn index_has_directory(index: &gix::index::State, dir: &[u8]) -> bool {
    let mut probe = dir.to_vec();
    probe.push(b'/');
    let pos = match index_name_pos(index, &probe) {
        Ok(pos) | Err(pos) => pos,
    };
    index
        .entries()
        .get(pos)
        .is_some_and(|e| e.path(index).as_bytes().starts_with(&probe))
}

/// git's `treat_directory` under `DIR_SHOW_OTHER_DIRECTORIES` (`--directory`): a
/// walked path is reported as the outermost of its parent directories the index
/// knows nothing below. A directory the index does have entries under is recursed
/// into instead, which is why a path such as `a/b/deep` survives uncollapsed while
/// `a/b/c.txt` is still tracked — even when that tracked file is gone from disk.
fn collapse_other_directory(
    index: &gix::index::State,
    path: &BStr,
    is_dir: bool,
) -> (BString, bool) {
    let bytes = path.as_bytes();
    let mut at = 0;
    while let Some(off) = bytes[at..].iter().position(|&b| b == b'/') {
        let cut = at + off;
        if index_has_directory(index, &bytes[..cut]) {
            at = cut + 1;
            continue;
        }
        return (BString::from(&bytes[..cut]), true);
    }
    (path.to_owned(), is_dir)
}

/// Port of the predicate inside git's `show_killed_files()`, applied to one
/// collected directory entry (a directory entry carries git's trailing `/`).
///
/// A walked path is *killed* when checking the index out would have to remove it:
/// either one of its leading directories is registered in the index as a file, so
/// that file cannot be written while the directory is in the way, or the index
/// registers something beneath the path, forcing it to become a directory.
fn is_killed(index: &gix::index::State, name: &BStr) -> bool {
    let bytes = name.as_bytes();
    let entries = index.entries();
    let mut start = 0;
    while start < bytes.len() {
        let Some(off) = bytes[start..].iter().position(|&b| b == b'/') else {
            // No further slash: "if ent->name is prefix of an entry in the cache,
            // it will be killed".
            let Err(mut pos) = index_name_pos(index, bytes) else {
                // git `BUG()`s here — the walk never collects an indexed path.
                return false;
            };
            while entries.get(pos).is_some_and(|e| e.stage_raw() != 0) {
                pos += 1; // skip unmerged
            }
            let Some(next) = entries.get(pos) else {
                return false;
            };
            // `next` is the name immediately after `bytes`; does it expect
            // `bytes` to be a directory?
            let cand = next.path(index).as_bytes();
            return cand.len() > bytes.len()
                && cand.starts_with(bytes)
                && cand[bytes.len()] == b'/';
        };
        // "If any of the leading directories in ent->name is registered in the
        // cache, ent->name will be killed."
        let cut = start + off;
        if index_name_pos(index, &bytes[..cut]).is_ok() {
            return true;
        }
        start = cut + 1;
    }
    false
}

/// The `text`/`crlf` attribute reduced to `convert.c`'s `enum convert_crlf_action`,
/// in the `attr_action` form `get_convert_attr_ascii` renders — i.e. before
/// `core.autocrlf`/`core.eol` get a say, which is why `text` stays `text`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CrlfAction {
    Undefined,
    Binary,
    Text,
    TextInput,
    TextCrlf,
    Auto,
    AutoCrlf,
    AutoInput,
}

impl CrlfAction {
    /// git's `git_path_check_crlf`, shared by the `text` and `crlf` attributes.
    fn from_state(state: gix::attrs::StateRef<'_>) -> Self {
        match state {
            gix::attrs::StateRef::Set => CrlfAction::Text,
            gix::attrs::StateRef::Unset => CrlfAction::Binary,
            gix::attrs::StateRef::Unspecified => CrlfAction::Undefined,
            gix::attrs::StateRef::Value(v) => match v.as_bstr().as_bytes() {
                b"input" => CrlfAction::TextInput,
                b"auto" => CrlfAction::Auto,
                _ => CrlfAction::Undefined,
            },
        }
    }

    /// git's `get_convert_attr_ascii` spelling of each action.
    fn as_str(self) -> &'static str {
        match self {
            CrlfAction::Undefined => "",
            CrlfAction::Binary => "-text",
            CrlfAction::Text => "text",
            CrlfAction::TextInput => "text eol=lf",
            CrlfAction::TextCrlf => "text eol=crlf",
            CrlfAction::Auto => "text=auto",
            CrlfAction::AutoCrlf => "text=auto eol=crlf",
            CrlfAction::AutoInput => "text=auto eol=lf",
        }
    }
}

/// git's `enum eol` as far as the `eol` attribute can express it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EolAttr {
    Unset,
    Lf,
    Crlf,
}

impl EolAttr {
    /// git's `git_path_check_eol`.
    fn from_state(state: gix::attrs::StateRef<'_>) -> Self {
        match state {
            gix::attrs::StateRef::Value(v) => match v.as_bstr().as_bytes() {
                b"lf" => EolAttr::Lf,
                b"crlf" => EolAttr::Crlf,
                _ => EolAttr::Unset,
            },
            _ => EolAttr::Unset,
        }
    }
}

/// The convert-stat machinery behind `--eol` and the three `%(eol…)` format atoms.
struct Eol<'r> {
    /// The object store every indexed blob is read from. git reads it from the
    /// *superproject* even while listing a submodule, which is why a submodule's
    /// entries report `i/none`; keeping one repository here reproduces that.
    repo: &'r gix::Repository,
    /// Absolute worktree root. git's `RUN_SETUP` has already chdir'd there, so its
    /// `lstat(fullname)` is worktree-relative; this port keeps the caller's
    /// directory and joins instead.
    workdir: Option<PathBuf>,
    stack: gix::AttributeStack<'r>,
    outcome: gix::attrs::search::Outcome,
}

impl<'r> Eol<'r> {
    fn new(repo: &'r gix::Repository, index: &gix::index::State) -> Result<Self> {
        Ok(Eol {
            repo,
            workdir: repo.workdir().map(Path::to_path_buf),
            stack: repo.attributes_only(
                index,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
            )?,
            outcome: gix::attrs::search::Outcome::default(),
        })
    }

    /// git's `write_eolinfo`: `printf("i/%-5s w/%-5s attr/%-17s\t", …)`.
    fn columns(&mut self, entry: Option<&gix::index::Entry>, full: &BStr) -> String {
        let index = self.index_stats(entry);
        let worktree = self.worktree_stats(full);
        let attr = self.attr(full);
        format!("i/{index:<5} w/{worktree:<5} attr/{attr:<17}\t")
    }

    /// git's `get_cached_convert_stats_ascii`, gated on the cache entry being a
    /// regular file — a symlink or gitlink contributes an empty column.
    fn index_stats(&mut self, entry: Option<&gix::index::Entry>) -> &'static str {
        let Some(entry) = entry.filter(|e| {
            matches!(
                e.mode,
                gix::index::entry::Mode::FILE | gix::index::entry::Mode::FILE_EXECUTABLE
            )
        }) else {
            return "";
        };
        match self.repo.find_object(entry.id) {
            // A blob git cannot read yields a NULL buffer, which its statistics
            // report as "none" rather than as an error.
            Ok(obj) => convert_stats_ascii(&obj.data),
            Err(_) => convert_stats_ascii(&[]),
        }
    }

    /// git's `lstat` + `get_wt_convert_stats_ascii` pair: only a regular file in
    /// the worktree contributes a column.
    fn worktree_stats(&mut self, full: &BStr) -> &'static str {
        let Some(root) = self.workdir.as_ref() else {
            return "";
        };
        let path = root.join(gix::path::from_bstr(full));
        match std::fs::symlink_metadata(&path) {
            Ok(md) if md.is_file() => match std::fs::read(&path) {
                Ok(bytes) => convert_stats_ascii(&bytes),
                Err(_) => "",
            },
            _ => "",
        }
    }

    /// git's `get_convert_attr_ascii`: the `text`, `crlf` and `eol` attributes
    /// combined the way `convert_attrs` records them in `ca->attr_action`.
    fn attr(&mut self, full: &BStr) -> &'static str {
        let mode = Some(gix::index::entry::Mode::FILE);
        // The first descent loads the `.gitattributes` along the path so the
        // collection knows every attribute name before the outcome is sized.
        if self.stack.at_entry(full, mode).is_err() {
            return "";
        }
        self.outcome
            .initialize_with_selection(self.stack.attributes_collection(), ["text", "crlf", "eol"]);
        let Ok(platform) = self.stack.at_entry(full, mode) else {
            return "";
        };
        platform.matching_attributes(&mut self.outcome);
        // `iter_selected` yields in the order the selection was given.
        let states: Vec<_> = self
            .outcome
            .iter_selected()
            .map(|m| m.assignment.state)
            .collect();
        let [text, crlf, eol] = states[..] else {
            return "";
        };

        let mut action = CrlfAction::from_state(text);
        if action == CrlfAction::Undefined {
            action = CrlfAction::from_state(crlf);
        }
        if action != CrlfAction::Binary {
            action = match (action, EolAttr::from_state(eol)) {
                (CrlfAction::Auto, EolAttr::Lf) => CrlfAction::AutoInput,
                (CrlfAction::Auto, EolAttr::Crlf) => CrlfAction::AutoCrlf,
                (_, EolAttr::Lf) => CrlfAction::TextInput,
                (_, EolAttr::Crlf) => CrlfAction::TextCrlf,
                (action, EolAttr::Unset) => action,
            };
        }
        action.as_str()
    }
}

/// Port of `convert.c`'s `struct text_stat`, whose counts decide both the
/// binary heuristic and the reported line-ending flavour.
#[derive(Default)]
struct TextStat {
    nul: u32,
    lonecr: u32,
    lonelf: u32,
    crlf: u32,
    printable: u32,
    nonprintable: u32,
}

/// Port of `convert.c`'s `gather_stats`.
fn gather_stats(buf: &[u8]) -> TextStat {
    let mut stats = TextStat::default();
    let mut i = 0;
    while i < buf.len() {
        match buf[i] {
            b'\r' => {
                if buf.get(i + 1) == Some(&b'\n') {
                    stats.crlf += 1;
                    i += 1;
                } else {
                    stats.lonecr += 1;
                }
            }
            b'\n' => stats.lonelf += 1,
            // DEL
            127 => stats.nonprintable += 1,
            // BS, HT, ESC and FF read as printable; NUL is counted twice, once as
            // a NUL and once (by fall-through) as non-printable.
            c if c < 32 => match c {
                0x08 | 0x09 | 0x1b | 0x0c => stats.printable += 1,
                0 => {
                    stats.nul += 1;
                    stats.nonprintable += 1;
                }
                _ => stats.nonprintable += 1,
            },
            _ => stats.printable += 1,
        }
        i += 1;
    }
    // "If file ends with EOF then don't count this EOF as non-printable."
    if buf.last() == Some(&0x1a) {
        stats.nonprintable = stats.nonprintable.wrapping_sub(1);
    }
    stats
}

/// Port of `convert.c`'s `convert_is_binary`: the same heuristic `diff` uses,
/// plus treating a bare CR as binary.
fn is_binary(stats: &TextStat) -> bool {
    stats.lonecr != 0 || stats.nul != 0 || (stats.printable >> 7) < stats.nonprintable
}

/// Port of `convert.c`'s `gather_convert_stats_ascii`, the `<eolinfo>` vocabulary
/// documented for `--eol`. Empty (or unreadable) content reports `none`, matching
/// git's `gather_convert_stats` early return for a NULL buffer.
fn convert_stats_ascii(data: &[u8]) -> &'static str {
    if data.is_empty() {
        return "none";
    }
    let stats = gather_stats(data);
    if is_binary(&stats) {
        return "-text";
    }
    match (stats.crlf != 0, stats.lonelf != 0) {
        (false, true) => "lf",
        (true, false) => "crlf",
        (true, true) => "mixed",
        (false, false) => "none",
    }
}

/// The submodule paths git's `is_submodule_active` would accept, empty unless
/// `--recurse-submodules` was given.
fn active_submodules(repo: &gix::Repository, opts: &Opts) -> HashSet<BString> {
    if !opts.recurse_submodules {
        return HashSet::new();
    }
    let Ok(Some(subs)) = repo.submodules() else {
        return HashSet::new();
    };
    subs.filter(|sm| sm.is_active().unwrap_or(false))
        .filter_map(|sm| sm.path().ok())
        .collect()
}

/// Port of git's `show_submodule`: open the submodule checked out at `sub_rela`
/// inside `parent`, read its index, and emit its entries under `name_prefix`,
/// recursing for nested active submodules.
///
/// Only `--cached`/`--stage` can reach here — git rejects every worktree-facing
/// selector alongside `--recurse-submodules` — so this reproduces just the
/// cached-line half of `show_files`. A submodule whose repository or index cannot
/// be opened is skipped, where git's `repo_submodule_init` returns and its
/// `repo_read_index` dies.
#[allow(clippy::too_many_arguments)] // mirrors the state show_files threads through
fn emit_submodule(
    parent: &gix::Repository,
    sub_rela: &BStr,
    name_prefix: &BStr,
    opts: &Opts,
    ps: &mut gix::Pathspec<'_>,
    matcher: &mut Excludes<'_>,
    prefix: Option<&BString>,
    quote: bool,
    terminator: u8,
    mut eol: Option<&mut Eol<'_>>,
    lines: &mut Vec<Vec<u8>>,
) -> Result<()> {
    let Some(workdir) = parent.workdir() else {
        return Ok(());
    };
    let Ok(sub) = gix::open(workdir.join(gix::path::from_bstr(sub_rela))) else {
        return Ok(());
    };
    let Ok(index) = sub.open_index() else {
        return Ok(());
    };
    let active = active_submodules(&sub, opts);

    for entry in index.entries() {
        let path = entry.path(&index);
        let mut full = BString::from(name_prefix.to_vec());
        full.push(b'/');
        full.extend_from_slice(path.as_bytes());

        // A nested active submodule is descended into ahead of the pathspec, just
        // as at the top level.
        if entry.mode == gix::index::entry::Mode::COMMIT && active.contains(path) {
            if !opts.ignored || matcher.is_excluded(full.as_bstr(), true) {
                emit_submodule(
                    &sub,
                    path,
                    full.as_bstr(),
                    opts,
                    ps,
                    matcher,
                    prefix,
                    quote,
                    terminator,
                    eol.as_deref_mut(),
                    lines,
                )?;
            }
            continue;
        }

        let Some(m) = ps.pattern_matching_relative_path(full.as_bstr(), Some(false)) else {
            continue;
        };
        if m.is_excluded() {
            continue;
        }
        if opts.ignored && !matcher.is_excluded(full.as_bstr(), false) {
            continue;
        }

        let stage = entry.stage_raw();
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
        let display = strip_prefix(full.as_bstr(), prefix).to_vec();
        lines.push(render(
            opts,
            tag,
            Some(entry),
            &sub,
            full.as_bstr(),
            &display,
            quote,
            terminator,
            eol.as_deref_mut(),
        ));
    }
    Ok(())
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

/// Build one output line: optional status tag, optional stage columns, the
/// `--eol` columns, the path, the line terminator, and (under `--debug`) the
/// trailing stat block.
///
/// `full` is the repository-root-relative name git passes to `write_eolinfo` —
/// the same string it `lstat`s and looks attributes up for — while `display` is
/// that name already reduced to the caller's current directory.
#[allow(clippy::too_many_arguments)] // git's show_ce carries the same state
fn render(
    opts: &Opts,
    tag: &str,
    entry: Option<&gix::index::Entry>,
    repo: &gix::Repository,
    full: &BStr,
    display: &[u8],
    quote: bool,
    terminator: u8,
    mut eol: Option<&mut Eol<'_>>,
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
        expand_format(
            &mut line,
            fmt,
            entry,
            repo,
            full,
            &path_bytes,
            opts.abbrev,
            eol.as_deref_mut(),
        );
        line.push(terminator);
        // ```c
        // if (format) {
        //         show_ce_fmt(repo, ce, format, fullname);
        //         print_debug(ce);
        //         return;
        // }
        // ```
        // (builtin/ls-files.c:318-322) — `--debug` survives `--format`, and the
        // block lands after the template's own terminator just as it lands after
        // the name in the default layout.
        if opts.debug {
            if let Some(entry) = entry {
                append_debug(&mut line, entry);
            }
        }
        return line;
    }

    if opts.shows_tags() {
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
    // git's `write_eolinfo` sits between the stage columns and the name, and its
    // own trailing tab is what separates it from that name.
    if opts.eol {
        if let Some(eol) = eol.as_deref_mut() {
            line.extend_from_slice(eol.columns(entry, full).as_bytes());
        }
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
/// `%(eolinfo:index)`, `%(eolinfo:worktree)` and `%(eolattr)` expand to the same
/// convert-stat strings `--eol` prints, just without the column padding. An
/// unrecognised `%(...)` atom is copied through verbatim.
#[allow(clippy::too_many_arguments)] // mirrors show_ce_fmt's parameter set
fn expand_format(
    out: &mut Vec<u8>,
    fmt: &str,
    entry: Option<&gix::index::Entry>,
    repo: &gix::Repository,
    full: &BStr,
    path_bytes: &[u8],
    abbrev: Option<Option<usize>>,
    mut eol: Option<&mut Eol<'_>>,
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
        // `strbuf_expand_literal_cb`'s `case 'n'` (strbuf.c:410-412).
        if next == 'n' {
            out.push(b'\n');
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
                    "eolinfo:index" => {
                        if let Some(eol) = eol.as_deref_mut() {
                            out.extend_from_slice(eol.index_stats(entry).as_bytes());
                        }
                    }
                    "eolinfo:worktree" => {
                        if let Some(eol) = eol.as_deref_mut() {
                            out.extend_from_slice(eol.worktree_stats(full).as_bytes());
                        }
                    }
                    "eolattr" => {
                        if let Some(eol) = eol.as_deref_mut() {
                            out.extend_from_slice(eol.attr(full).as_bytes());
                        }
                    }
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

/// Name `path` from the caller's directory, which is `write_name_quoted_relative()`
/// — `relative_path()` and not a prefix strip.
///
/// The two agree for everything inside the current directory, and only a pathspec
/// that reaches outside it (`:(top)`, `:/`) can produce a name that does not start
/// with the prefix. git climbs out of the prefix for those (`../README.md`) rather
/// than printing them from the repository root.
fn strip_prefix(path: &BStr, prefix: Option<&BString>) -> Vec<u8> {
    match prefix {
        Some(pref) => crate::objpath::relative_path(path.as_bytes(), pref.as_bytes()),
        None => path.as_bytes().to_vec(),
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

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    crate::quote::quoted_name_string(path.as_ref())
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
    fn convert_stats_follow_gits_eolinfo_vocabulary() {
        // The five spellings `git ls-files --eol` documents, from convert.c's
        // gather_convert_stats_ascii.
        assert_eq!(convert_stats_ascii(b"a\nb\n"), "lf");
        assert_eq!(convert_stats_ascii(b"a\r\nb\r\n"), "crlf");
        assert_eq!(convert_stats_ascii(b"a\nb\r\n"), "mixed");
        // No line terminator at all, and the NULL/empty buffer git reports the
        // same way, are both "none".
        assert_eq!(convert_stats_ascii(b"abc"), "none");
        assert_eq!(convert_stats_ascii(b""), "none");
        // A NUL byte and a bare CR are each enough to call it binary.
        assert_eq!(convert_stats_ascii(b"a\0b\n"), "-text");
        assert_eq!(convert_stats_ascii(b"a\rb"), "-text");
        // BS, HT, ESC and FF count as printable, so they stay text.
        assert_eq!(convert_stats_ascii(b"\x08\x09\x1b\x0c ok\n"), "lf");
        // DEL is non-printable, and one of them outweighs `printable >> 7`.
        assert_eq!(convert_stats_ascii(b"\x7f\n"), "-text");
    }

    #[test]
    fn trailing_sub_byte_is_not_counted_as_non_printable() {
        // convert.c decrements `nonprintable` when the last byte is 0x1A, which
        // is what keeps a DOS-terminated text file out of the binary bucket.
        assert_eq!(convert_stats_ascii(b"x\ny\n\x1a"), "lf");
        // Without that adjustment the same content one byte earlier is binary,
        // since 0x1A is otherwise a control byte.
        assert_eq!(convert_stats_ascii(b"x\n\x1a\n"), "-text");
    }

    #[test]
    fn eol_columns_match_gits_write_eolinfo_widths() {
        // git: printf("i/%-5s w/%-5s attr/%-17s\t", …). The longest <eolinfo> is
        // exactly 5 wide, so only the attribute column ever pads visibly.
        let line = format!("i/{:<5} w/{:<5} attr/{:<17}\t", "lf", "crlf", "text eol=crlf");
        assert_eq!(line, "i/lf    w/crlf  attr/text eol=crlf    \t");
        let empty = format!("i/{:<5} w/{:<5} attr/{:<17}\t", "", "", "");
        assert_eq!(empty, "i/      w/      attr/                 \t");
        let binary = format!("i/{:<5} w/{:<5} attr/{:<17}\t", "-text", "mixed", "text=auto eol=crlf");
        assert_eq!(binary, "i/-text w/mixed attr/text=auto eol=crlf\t");
    }

    #[test]
    fn crlf_action_spellings_match_get_convert_attr_ascii() {
        assert_eq!(CrlfAction::Undefined.as_str(), "");
        assert_eq!(CrlfAction::Binary.as_str(), "-text");
        assert_eq!(CrlfAction::Text.as_str(), "text");
        assert_eq!(CrlfAction::TextInput.as_str(), "text eol=lf");
        assert_eq!(CrlfAction::TextCrlf.as_str(), "text eol=crlf");
        assert_eq!(CrlfAction::Auto.as_str(), "text=auto");
        assert_eq!(CrlfAction::AutoCrlf.as_str(), "text=auto eol=crlf");
        assert_eq!(CrlfAction::AutoInput.as_str(), "text=auto eol=lf");
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
