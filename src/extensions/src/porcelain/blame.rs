use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;

/// git's smallest permitted abbreviation length.
const MINIMUM_ABBREV: usize = 4;

/// Byte-for-byte reproduction of `git blame`'s usage text, printed on stderr with
/// exit status 129 when no path is given (the only usage error we produce).
const USAGE: &str = concat!(
    "usage: git blame [<options>] [<rev-opts>] [<rev>] [--] <file>\n",
    "\n",
    "    <rev-opts> are documented in git-rev-list(1)\n",
    "\n",
    "    --[no-]incremental    show blame entries as we find them, incrementally\n",
    "    -b                    do not show object names of boundary commits (Default: off)\n",
    "    --[no-]root           do not treat root commits as boundaries (Default: off)\n",
    "    --[no-]show-stats     show work cost statistics\n",
    "    --[no-]progress       force progress reporting\n",
    "    --[no-]score-debug    show output score for blame entries\n",
    "    -f, --[no-]show-name  show original filename (Default: auto)\n",
    "    -n, --[no-]show-number\n",
    "                          show original linenumber (Default: off)\n",
    "    -p, --[no-]porcelain  show in a format designed for machine consumption\n",
    "    --[no-]line-porcelain show porcelain format with per-line commit information\n",
    "    -c                    use the same output mode as git-annotate (Default: off)\n",
    "    -t                    show raw timestamp (Default: off)\n",
    "    -l                    show long commit SHA1 (Default: off)\n",
    "    -s                    suppress author name and timestamp (Default: off)\n",
    "    -e, --[no-]show-email show author email instead of name (Default: off)\n",
    "    -w                    ignore whitespace differences\n",
    "    --diff-algorithm <algorithm>\n",
    "                          choose a diff algorithm\n",
    "    --[no-]ignore-rev <rev>\n",
    "                          ignore <rev> when blaming\n",
    "    --[no-]ignore-revs-file <file>\n",
    "                          ignore revisions from <file>\n",
    "    --[no-]color-lines    color redundant metadata from previous line differently\n",
    "    --[no-]color-by-age   color lines by age\n",
    "    -S <file>             use revisions from <file> instead of calling git-rev-list\n",
    "    --[no-]contents <file>\n",
    "                          use <file>'s contents as the final image\n",
    "    -C[<score>]           find line copies within and across files\n",
    "    -M[<score>]           find line movements within and across files\n",
    "    -L <range>            process only line range <start>,<end> or function :<funcname>\n",
    "    --[no-]abbrev[=<n>]   use <n> digits to display object names\n",
    "\n",
);

/// The synthetic author git attributes not-yet-committed lines to.
const NOT_COMMITTED_NAME: &[u8] = b"Not Committed Yet";
const NOT_COMMITTED_MAIL: &[u8] = b"not.committed.yet";

/// git's reset sequence — `ESC [ m`, not `ESC [ 0 m` (`GIT_COLOR_RESET`).
const COLOR_RESET: &str = "\x1b[m";

/// `GIT_COLOR_CYAN`, the built-in fallback for `color.blame.repeatedLines`.
const COLOR_CYAN: &str = "\x1b[36m";

/// git's `setup_default_color_by_age()` seed for the heat table.
const DEFAULT_COLOR_BY_AGE: &str = "blue,12 month ago,white,1 month ago,red";

/// Blame's two coloring modes, resolved from `blame.coloring`, `color.blame.*`
/// and the command line — git's `OUTPUT_COLOR_LINE` / `OUTPUT_SHOW_AGE_WITH_COLOR`
/// bits plus the `repeated_meta_color` / `colorfield` globals of
/// `builtin/blame.c`.
///
/// Note that blame colors unconditionally: unlike `git status` or `git diff` it
/// never consults `color.ui` / `want_color()`, so the SGR sequences appear even
/// when stdout is a pipe (verified against git 2.55.0).
struct BlameColors {
    /// `--color-lines` / `blame.coloring=repeatedLines`.
    color_lines: bool,
    /// `--color-by-age` / `blame.coloring=highlightRecent`.
    color_by_age: bool,
    /// `blame.coloring`'s contribution, applied only when the command line left
    /// both bits clear.
    config_lines: bool,
    /// `blame.coloring`'s contribution for the age mode.
    config_age: bool,
    /// `color.blame.repeatedLines`, empty when unset or unparseable.
    repeated: String,
    /// git's `colorfield`: `(hop, sgr)` in table order, the last entry carrying
    /// `i64::MAX` so it always matches.
    heat: Vec<(i64, String)>,
}

impl BlameColors {
    /// Read the coloring configuration exactly as `git_blame_config` does.
    ///
    /// A `color.blame.highlightRecent` git rejects is fatal (128) here as it is
    /// there; a `color.blame.repeatedLines` it rejects only warns and leaves the
    /// built-in cyan in place; an unknown `blame.coloring` only warns.
    fn from_config(repo: &gix::Repository) -> Result<Self, ExitCode> {
        let snapshot = repo.config_snapshot();

        // `setup_default_color_by_age()` runs before the config callback, so a
        // repository without `color.blame.highlightRecent` gets git's table.
        let mut heat = match parse_color_fields(DEFAULT_COLOR_BY_AGE) {
            ColorFields::Ok(fields) => fields,
            ColorFields::Fatal(code) => return Err(code),
        };
        if let Some(value) = snapshot.string("color.blame.highlightRecent") {
            match parse_color_fields(&value.to_str_lossy()) {
                ColorFields::Ok(fields) => heat = fields,
                ColorFields::Fatal(code) => return Err(code),
            }
        }

        // `color_parse_mem` failing leaves `repeated_meta_color` untouched (so
        // the cyan default below still applies) after two diagnostics.
        let repeated = match snapshot.string("color.blame.repeatedLines") {
            Some(value) => {
                let spec = value.to_str_lossy().into_owned();
                match super::color::parse_color_spec(&spec) {
                    Some(sgr) => sgr,
                    None => {
                        eprintln!("error: invalid color value: {spec}");
                        eprintln!(
                            "warning: invalid value for 'color.blame.repeatedLines': '{spec}'"
                        );
                        String::new()
                    }
                }
            }
            None => String::new(),
        };

        // `blame.coloring` ORs into `coloring_mode`; `none` clears both bits. The
        // config callback sees every occurrence in file order, so all values are
        // folded in rather than just the last one.
        let (mut config_lines, mut config_age) = (false, false);
        for value in snapshot
            .plumbing()
            .strings("blame.coloring")
            .unwrap_or_default()
            .iter()
            .map(|v| v.to_str_lossy().into_owned())
        {
            match value.as_str() {
                "repeatedLines" => config_lines = true,
                "highlightRecent" => config_age = true,
                "none" => {
                    config_lines = false;
                    config_age = false;
                }
                other => eprintln!("warning: invalid value for 'blame.coloring': '{other}'"),
            }
        }

        Ok(BlameColors {
            color_lines: false,
            color_by_age: false,
            config_lines,
            config_age,
            repeated,
            heat,
        })
    }

    /// cmd_blame's final coloring decision, taken between `blame_coalesce()` and
    /// `output()`:
    ///   * `blame.coloring` only applies when *neither* bit is set — so
    ///     `--no-color-lines` clears the bit and thereby re-enables the config
    ///     value, which stock git does too;
    ///   * an unset `color.blame.repeatedLines` falls back to cyan, but only when
    ///     line coloring is on and the format is not porcelain;
    ///   * the annotate-compat format (`-c`) clears both bits last.
    fn apply_command_line(&mut self, opts: &Options) {
        self.color_lines = opts.color_lines;
        self.color_by_age = opts.color_by_age;
        if !self.color_lines && !self.color_by_age {
            self.color_lines = self.config_lines;
            self.color_by_age = self.config_age;
        }
        if !opts.porcelain && self.repeated.is_empty() && self.color_lines {
            self.repeated = COLOR_CYAN.to_string();
        }
        if opts.annotate_compat {
            self.color_lines = false;
            self.color_by_age = false;
        }
    }

    /// git's `determine_line_heat`: the first bucket whose hop is not older than
    /// the author time wins, and the sentinel bucket catches everything newer.
    fn heat_for(&self, author_time: i64) -> &str {
        let mut i = 0;
        while i + 1 < self.heat.len() && author_time > self.heat[i].0 {
            i += 1;
        }
        &self.heat[i].1
    }

    /// The `(color, reset)` pair `emit_other` would put around the metadata of
    /// the `cnt`-th line of a blame entry authored at `author_time`.
    fn for_line(&self, cnt: usize, author_time: i64) -> (Option<&str>, Option<&str>) {
        let default_color = self.color_by_age.then(|| self.heat_for(author_time));
        let (mut color, mut reset) = match default_color {
            Some(c) => (Some(c), Some(COLOR_RESET)),
            None => (None, None),
        };
        if self.color_lines {
            if cnt > 0 {
                color = Some(self.repeated.as_str());
                reset = Some(COLOR_RESET);
            } else {
                color = default_color;
                reset = default_color.map(|_| COLOR_RESET);
            }
        }
        (color, reset)
    }
}

/// The outcome of parsing a `color.blame.highlightRecent` value.
enum ColorFields {
    Ok(Vec<(i64, String)>),
    /// git already reported the failure; return this status (128).
    Fatal(ExitCode),
}

/// git's `parse_color_fields` (`builtin/blame.c`): a comma-separated list that
/// alternates color, date, color, date, …, and must end on a color. Each date is
/// an approxidate "hop"; the trailing color is stored with `TIME_MAX` so the
/// lookup always terminates. Items are used verbatim — git splits on `,` without
/// trimming, and its color parser skips the leading blanks itself.
fn parse_color_fields(spec: &str) -> ColorFields {
    let mut fields: Vec<(i64, String)> = Vec::new();
    let mut pending = String::new();
    // git's `next`, seeded to EXPECT_COLOR.
    let mut expect_color = true;
    for item in spec.split(',') {
        if expect_color {
            match super::color::parse_color_spec(item) {
                Some(sgr) => pending = sgr,
                None => {
                    eprintln!("error: invalid color value: {item}");
                    eprintln!("fatal: expecting a color: {item}");
                    return ColorFields::Fatal(ExitCode::from(128));
                }
            }
        } else {
            fields.push((approxidate(item), std::mem::take(&mut pending)));
        }
        expect_color = !expect_color;
    }
    // Ending on a date leaves git expecting a color, which it refuses to invent.
    if expect_color {
        eprintln!("fatal: must end with a color");
        return ColorFields::Fatal(ExitCode::from(128));
    }
    fields.push((i64::MAX, pending));
    ColorFields::Ok(fields)
}

/// git's approxidate, as `parse_color_fields` uses it for each heat threshold:
/// an absolute or relative date resolved against `GIT_TEST_DATE_NOW`/now, with
/// an unparseable value falling back to now — the same rule `log`'s
/// `--since`/`--until` handling follows.
fn approxidate(value: &str) -> i64 {
    let now_s = crate::date::now_seconds();
    if value.trim() == "now" {
        return now_s;
    }
    let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(now_s.max(0) as u64);
    gix::date::parse(value, Some(now))
        .map(|t| t.seconds)
        .unwrap_or(now_s)
}

/// `git blame` — line-by-line last-modifying commit, backed by `gix-blame`.
///
/// Implemented invocation forms, reproducing stock `git blame` byte for byte:
///   * `git blame <file>`                     — blame the working-tree file
///   * `git blame <rev> [--] <file>`          — blame `<rev>:<file>`
///   * `-L <start>,<end>` / `-L <start>` / `-L <start>,+<n>` / `-L ,<end>`
///   * `-l`/`--long`, `-s`, `-e`/`--show-email`, `-f`/`--show-name`,
///     `-n`/`--show-number`, `--abbrev=<n>`
///   * `-p`/`--porcelain`, `--line-porcelain`
///
/// With no `<rev>`, the working-tree copy of the file is blamed the way git does
/// it: a synthetic commit holding the working-tree content sits on top of `HEAD`,
/// so lines that differ from `HEAD` are reported against the all-zero object id
/// with author `Not Committed Yet`.
///
/// Whole-file rename following is on (matching git's default), so the source
/// filename column appears exactly when git would show it. Boundary commits
/// (roots) are prefixed with `^` as git does.
///
/// Also implemented: `-b`, `--root`, `-t`, `-c` (annotate-compat), `-l`,
/// `--contents <file>` (and `--contents -` from stdin), `--diff-algorithm`,
/// `-w`, `--date=relative`, `--color-lines`, `--color-by-age` and
/// `--progress`/`--no-progress`.
///
/// Coloring follows `builtin/blame.c` exactly: `blame.coloring` supplies the
/// default mode when neither color option is given, `color.blame.repeatedLines`
/// (default cyan) paints the metadata of every line after the first in an entry,
/// and `color.blame.highlightRecent` (default
/// `blue,12 month ago,white,1 month ago,red`) buckets lines by author date.
/// Blame never consults `color.ui`, so the sequences are emitted even into a
/// pipe, and the porcelain and annotate-compat formats are never colored.
///
/// The `--[no-]` negation forms git advertises are honored with git's exact
/// bit-clearing semantics: `--no-show-name`, `--no-show-number`, `--no-porcelain`
/// (clears the porcelain bit only), `--no-line-porcelain` (clears both porcelain
/// bits, so it also cancels a preceding `-p`), and `--no-abbrev` (equivalent to
/// `--abbrev=0`, i.e. the full hash). `--no-ignore-rev` / `--no-ignore-revs-file`
/// clear their `OPT_STRING_LIST`s, config-supplied entries included. The `--no-`
/// forms of the unimplemented options (`--no-incremental`, `--no-show-stats`,
/// `--no-score-debug`) each select git's default, which this port already
/// produces, so they are accepted as no-ops.
///
/// `--ignore-rev <rev>`, `--ignore-revs-file <file>` and `blame.ignoreRevsFile`
/// are implemented against a port of git's fingerprint matcher: once the ordinary
/// diff has handed everything it can to the parents, the lines still held by an
/// ignored commit are matched to the parents' lines by byte-pair similarity
/// (`guess_line_blames()`), and whichever find a match are handed over too.
/// `blame.markIgnoredLines` marks the re-attributed lines with `?` and
/// `blame.markUnblamableLines` marks the lines that found no match with `*`, each
/// spending one column of the object-name field, and both emit their own
/// `ignored` / `unblamable` line in the porcelain formats.
///
/// Flags that are not implemented are rejected with a terse message rather than
/// emitting wrong output, each for a concrete reason:
///   * `--incremental` — git streams *uncoalesced* entries in walk order
///     (`blame_coalesce()` runs afterwards), while `gix-blame` only exposes the
///     coalesced attribution, so the entry list would differ.
///   * `--show-stats` — the counters are git's own walk instrumentation
///     (`num_read_blob` / `num_get_patch` / `num_commits`); `gix-blame` counts
///     different events and cannot be mapped onto them.
///   * `--score-debug` — the second column is `ent->suspect->refcnt`, the live
///     reference count of git's `blame_origin` graph, which depends on which
///     origins the walk kept alive rather than on the attribution itself.
///   * `-M` / `-C` — line move/copy detection happens inside the walk
///     (`find_move_in_parent` / `find_copy_in_parent`), splitting entries against
///     *other* origins of the same commit; `gix-blame` tracks one source path per
///     hunk and has no scoreboard of origins to split against.
///   * `-S <revs-file>` — installs commit grafts that rewrite the ancestry the
///     walk follows.
///   * `--reverse`, regex/function `-L` forms, `--date=human` and the `-local`
///     date variants.
pub fn blame(args: &[String]) -> Result<ExitCode> {
    let mut repo = gix::discover(".")?;
    // Object-heavy path: give gix the caches it does not enable by default —
    // a decoded-object cache and a git-sized delta-base cache (gix ships a
    // 64-entry linked list; git's core.deltaBaseCacheLimit default is 96MB).
    repo.object_cache_size_if_unset(16 * 1024 * 1024);
    repo.objects.set_pack_cache(|| {
        Box::new(gix::odb::pack::cache::lru::MemoryCappedHashmap::new(96 * 1024 * 1024))
    });

    // git reads blame.showEmail as the default for `-e`/`--show-email`, still
    // overridable on the command line (including `--no-show-email`).
    let show_email_default = repo.config_snapshot().boolean("blame.showEmail") == Some(true);

    // git reads blame.showRoot / blame.blankBoundary as the defaults for `--root`
    // and `-b`, still overridable on the command line.
    let show_root_default = repo.config_snapshot().boolean("blame.showRoot") == Some(true);
    let blank_boundary_default =
        repo.config_snapshot().boolean("blame.blankBoundary") == Some(true);

    // git reads blame.date as the default date mode for the human-format
    // timestamp column, still overridable by `--date=<mode>`. git validates the
    // config value at read time (before argument parsing), so an invalid mode
    // there is fatal even when a valid `--date` is also on the command line.
    let date_default = match repo.config_snapshot().string("blame.date") {
        Some(v) => match resolve_date_mode(&v.to_str_lossy())? {
            DateOutcome::Mode(m) => m,
            DateOutcome::Fatal(code) => return Ok(code),
        },
        None => DateMode::Iso8601,
    };

    // git's `setup_default_color_by_age()` and the three coloring config keys,
    // all read by `git_blame_config` *before* `parse_options` runs — so a
    // malformed `color.blame.highlightRecent` is fatal even when the command
    // line also has an unknown option (verified against git 2.55.0).
    let mut colors = match BlameColors::from_config(&repo) {
        Ok(c) => c,
        Err(code) => return Ok(code),
    };

    // `blame.ignoreRevsFile` is an `OPT_STRING_LIST` fed from the config callback, so
    // every occurrence in file order contributes and an empty value clears what came
    // before it. git resolves each through `git_config_pathname`, which expands `~`.
    let ignore_revs_file_default: Vec<String> = repo
        .config_snapshot()
        .plumbing()
        .strings("blame.ignoreRevsFile")
        .unwrap_or_default()
        .iter()
        .map(|v| v.to_str_lossy().into_owned())
        .fold(Vec::new(), |mut acc, value| {
            if value.is_empty() {
                acc.clear();
            } else {
                acc.push(expand_tilde(&value));
            }
            acc
        });
    let mark_unblamable_lines =
        repo.config_snapshot().boolean("blame.markUnblamableLines") == Some(true);
    let mark_ignored_lines = repo.config_snapshot().boolean("blame.markIgnoredLines") == Some(true);

    let mut opts = Options::parse(
        args,
        ConfigDefaults {
            show_email: show_email_default,
            show_root: show_root_default,
            blank_boundary: blank_boundary_default,
            ignore_revs_file: ignore_revs_file_default,
            mark_unblamable_lines,
            mark_ignored_lines,
        },
    )?;

    // git's `--progress` handling, run straight after `parse_options` and before
    // any path or revision is resolved: the machine formats refuse it outright,
    // and otherwise an unspecified value means `isatty(2)`.
    let show_progress = if opts.porcelain || opts.incremental {
        if opts.show_progress == Some(true) {
            let mut err = std::io::stderr().lock();
            writeln!(
                err,
                "fatal: --progress can't be used with --incremental or porcelain formats"
            )?;
            err.flush()?;
            return Ok(ExitCode::from(128));
        }
        false
    } else {
        opts.show_progress
            .unwrap_or_else(|| std::io::stderr().is_terminal())
    };
    let progress_started = std::time::Instant::now();

    // `--date=<mode>` overrides blame.date; git validates it the same way.
    opts.date_mode = match opts.date_arg.take() {
        Some(s) => match resolve_date_mode(&s)? {
            DateOutcome::Mode(m) => m,
            DateOutcome::Fatal(code) => return Ok(code),
        },
        None => date_default,
    };
    // `-t` (OUTPUT_RAW_TIMESTAMP) makes git's `format_time` ignore the date mode
    // and print the raw `<seconds> <tz>`. Modelling it as the raw mode reproduces
    // that byte-for-byte, including the fixed column width.
    if opts.raw_timestamp {
        opts.date_mode = DateMode::Raw;
    }

    // Split the positional arguments into a revision and a single path following
    // git blame's DWIM grammar, then resolve the revision. This may short-circuit
    // with git's usage text (129) or a `bad revision` / `More than one commit`
    // fatal (128); those cases print to stderr and return the code here.
    match resolve_targets(&repo, &mut opts)? {
        Targets::Usage => return print_usage(),
        Targets::Fatal(code) => return Ok(code),
        Targets::Resolved => {}
    }

    // Resolve the suspect commit (default HEAD). The overlay (working tree or
    // `--contents`) is layered on top of the suspect, so `head_id` — the commit a
    // not-yet-committed line points back to via the porcelain `previous` field —
    // is the suspect itself. An unborn HEAD stays tolerable as long as an explicit
    // revision was given.
    let (suspect, head_id) = match opts.suspect_id {
        Some(id) => (id, Some(id)),
        None => {
            let id = repo.head_id()?.detach();
            (id, Some(id))
        }
    };

    // git's `build_ignorelist`, run before `setup_scoreboard`, so a bad ignore list is
    // reported before a bad path is.
    let ignore_revs = match build_ignorelist(&repo, &opts) {
        Ok(set) => set,
        Err(code) => return Ok(code),
    };

    // Translate the user's path (relative to CWD) into a repo-root-relative path.
    let rel_path = repo_relative_path(&repo, &opts.file)?;

    // git refuses a path the blamed commit does not have. Which diagnostic it uses
    // depends on whether a final image is overlaid on top of that commit:
    //   * with an overlay (no revision, or `--contents`) the fake commit's
    //     `verify_working_tree_path` looks in its parents' trees and then in the
    //     index, and only then dies with the quoted, always-`HEAD` form;
    //   * without one, `setup_scoreboard` fails to fill the blob and names the
    //     revision as the user typed it.
    let overlay = opts.contents.is_some() || opts.rev.is_none();
    let path_in_suspect = repo
        .rev_parse_single(format!("{suspect}:{rel_path}").as_str())
        .is_ok();
    let mut index_only = false;
    if !path_in_suspect {
        let mut err = std::io::stderr().lock();
        if !overlay {
            let rev = opts.rev.as_deref().unwrap_or("HEAD");
            writeln!(err, "fatal: no such path {rel_path} in {rev}")?;
            err.flush()?;
            return Ok(ExitCode::from(128));
        }
        if !path_in_index(&repo, &rel_path) {
            writeln!(err, "fatal: no such path '{rel_path}' in HEAD")?;
            err.flush()?;
            return Ok(ExitCode::from(128));
        }
        // The path is only in the index, so there is nothing to blame it against:
        // every line belongs to the synthetic commit holding the final image.
        index_only = true;
    }

    // The final image is overlaid on top of the suspect when either no revision
    // was given (git blames the working-tree copy) or `--contents` supplies an
    // explicit image. `--contents -` reads standard input; `--contents <file>`
    // reads that file. Lines shared with the suspect keep its blame; the rest
    // belong to a synthetic commit (the null object id).
    let worktree_content = if let Some(from) = &opts.contents {
        let bytes = if from == "-" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut buf)?;
            buf
        } else {
            std::fs::read(from).map_err(|e| anyhow!("Cannot open '{from}': {e}"))?
        };
        Some(bytes)
    } else if opts.rev.is_none() {
        repo.workdir()
            .map(|w| w.join(&rel_path))
            .and_then(|p| std::fs::read(p).ok())
    } else {
        None
    };

    // Blame the full file; `-L` is applied to the result so that the working-tree
    // overlay can be built in working-tree line coordinates, as git does.
    let ranges = if opts.ranges.is_empty() || worktree_content.is_some() {
        gix::blame::BlameRanges::default()
    } else {
        gix::blame::BlameRanges::from_one_based_inclusive_ranges(opts.ranges.clone())
            .map_err(|e| anyhow!("{e}"))?
    };
    let blame_options = gix::repository::blame_file::Options {
        diff_algorithm: opts.diff_algorithm,
        ranges,
        since: None,
        rewrites: Some(gix::diff::Rewrites::default()),
        ignore_whitespace: opts.ignore_whitespace,
        ignore_revs: ignore_revs.clone(),
    };

    // A blame of (commit, path) is a pure function of immutable objects, so the
    // result is memoised in the ledger. gix re-runs the whole history walk and
    // per-step diff on every invocation; git does too, but roughly twice as
    // fast — caching sidesteps the race entirely, and the entry is valid in any
    // clone holding those commits.
    //
    // Only a full-file blame is cached: `-L` narrows the walk, so its outcome is
    // not the whole file's attribution.
    // The key must separate `-w` from a plain blame: they diff differently, so their
    // attributions differ and must not share a cache entry.
    //
    // An `--ignore-rev` blame is not cached at all: its result depends on the ignore
    // set *and* carries per-line `ignored`/`unblamable` flags the run encoding has no
    // room for, so a cache entry would either collide with a plain blame or lose the
    // flags.
    //
    // `--incremental` is not cached either: it needs the walk-order, uncoalesced entries,
    // which the run encoding does not preserve.
    let algo_key = format!("{:?}|w={}", opts.diff_algorithm, opts.ignore_whitespace);
    let cache_key = (opts.ranges.is_empty()
        && ignore_revs.is_empty()
        && !index_only
        && !opts.incremental)
        .then(|| (suspect.to_string(), rel_path.clone(), algo_key));
    // The blamed blob identifies the file content the attribution belongs to.
    let blamed_blob = repo
        .rev_parse_single(format!("{suspect}:{rel_path}").as_str())
        .ok()
        .map(|id| id.detach());
    let cached = cache_key.as_ref().zip(blamed_blob).and_then(|((c, p, a), blob)| {
        let (blob_hex, runs) = crate::rcache::blame_load(c, p, a)?;
        if blob_hex != blob.to_string() {
            return None; // the path holds different content at this commit
        }
        lines_from_cache(&repo, blob, runs)
    });

    // The entries in the order the walk finalized them, which only `--incremental`
    // needs — and which is empty on a cache hit, where the walk never ran.
    let mut uncoalesced: Vec<gix::blame::BlameEntry> = Vec::new();
    // `(lines, blob content)` — the overlay path needs the blamed blob's bytes,
    // which come from the outcome on a miss and from the object on a hit.
    let (mut lines, blamed_bytes) = match cached {
        // Nothing to blame against: the file exists only in the index, so the empty
        // `HEAD` image below leaves every line with the synthetic commit.
        _ if index_only => (Vec::new(), Vec::new()),
        Some((lines, bytes)) => (lines, bytes),
        None => {
            let outcome = repo
                .blame_file(rel_path.as_bytes().as_bstr(), suspect, blame_options)
                .map_err(|e| anyhow!("{e}"))?;
            let lines = materialize_lines(&outcome);
            if let (Some((c, p, a)), Some(blob)) = (&cache_key, blamed_blob) {
                // Queued, not written here: the blame is already computed and
                // about to be printed.
                crate::rcache::cache_write(crate::rcache::CacheWrite::Blame {
                    commit: c.clone(),
                    path: p.clone(),
                    algo: a.clone(),
                    blob: blob.to_string(),
                    runs: encode_runs(&lines),
                });
            }
            uncoalesced = outcome.uncoalesced_entries.clone();
            (lines, outcome.blob.clone())
        }
    };

    if let Some(content) = &worktree_content {
        lines = overlay_worktree(&repo, lines, &blamed_bytes, content, opts.diff_algorithm)?;
        if !opts.ranges.is_empty() {
            let keep = |n: u32| opts.ranges.iter().any(|r| r.contains(&n));
            lines.retain(|l| keep(l.final_no));
        }
    }

    // git's `stop_progress()`, which sits between `assign_blame` and `output`.
    finish_progress(show_progress, progress_started, lines.len())?;

    if lines.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let null_id = ObjectId::null(repo.object_hash());

    // `cmd_blame` jumps straight to cleanup after `assign_blame()` under
    // `--incremental`: the entries were streamed as the walk found them, so neither the
    // pager nor `blame_sort_final()`, `blame_coalesce()` or any output format runs.
    if opts.incremental {
        let mapped = match &worktree_content {
            Some(content) => Some(worktree_line_map(
                &repo,
                &blamed_bytes,
                content,
                opts.diff_algorithm,
            )?),
            None => None,
        };
        let mut entries = incremental_entries(&uncoalesced, mapped.as_deref(), &null_id);
        // With an overlay the walk ran over the whole file so that the overlay could be
        // built in final-image coordinates, so `-L` still has to be applied here.
        if !opts.ranges.is_empty() && worktree_content.is_some() {
            entries = clip_to_ranges(entries, &opts.ranges);
        }
        let info = collect_commit_info(&repo, &lines, &opts, &null_id, &rel_path)?;
        let head_id = if index_only { None } else { head_id };
        return emit_incremental(&repo, &entries, &info, &rel_path, head_id, &null_id);
    }

    // cmd_blame folds `blame.coloring` in only when neither color bit survived
    // argument parsing, then clears both bits for the annotate-compat format.
    colors.apply_command_line(&opts);

    let info = collect_commit_info(&repo, &lines, &opts, &null_id, &rel_path)?;

    if opts.porcelain {
        // The synthetic commit only records a `previous` origin when it actually
        // handed lines to one, which never happens when the path is index-only.
        let head_id = if index_only { None } else { head_id };
        emit_porcelain(&repo, &lines, &info, &rel_path, head_id, &null_id, &opts)
    } else {
        emit_human(&repo, &lines, &info, &rel_path, &opts, &colors)
    }
}

/// git's delayed "Blaming lines" progress meter (`start_delayed_progress` /
/// `stop_progress`), reported on stderr.
///
/// git only starts displaying once the meter has outlived `GIT_PROGRESS_DELAY`
/// (default 2) one-second ticks, and `stop_progress` prints nothing at all when
/// it never started — which is every blame that finishes promptly, so the common
/// case is reproduced exactly (verified: stock git writes 0 bytes to stderr for
/// `blame --progress` on a small file).
///
/// The per-entry ticks git emits once the meter *has* started come from its
/// `found_guilty_entry` callback, which fires as the history walk assigns blame.
/// `gix-blame` computes the whole attribution in one call and exposes no such
/// hook, so a long blame prints the final `100% (n/n), done.` line without the
/// intermediate `\r`-terminated updates. stdout is unaffected either way.
fn finish_progress(
    show_progress: bool,
    started: std::time::Instant,
    num_lines: usize,
) -> Result<()> {
    if !show_progress {
        return Ok(());
    }
    let delay = std::env::var("GIT_PROGRESS_DELAY")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2);
    if started.elapsed() < std::time::Duration::from_secs(delay) {
        return Ok(());
    }
    let mut err = std::io::stderr().lock();
    writeln!(err, "Blaming lines: 100% ({num_lines}/{num_lines}), done.")?;
    err.flush()?;
    Ok(())
}

/// Run-length encode an attribution: consecutive lines from the same commit,
/// advancing together in both files, collapse to one record
/// `final_start,orig_start,count,commit[,source_name]`.
fn encode_runs(lines: &[Line]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let start = &lines[i];
        let mut n = 1;
        while i + n < lines.len() {
            let l = &lines[i + n];
            let contiguous = l.commit_id == start.commit_id
                && l.final_no == start.final_no + n as u32
                && l.orig_no == start.orig_no + n as u32
                && l.source_name == start.source_name;
            if !contiguous {
                break;
            }
            n += 1;
        }
        let name = start
            .source_name
            .as_ref()
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        out.push(format!(
            "{},{},{},{},{}",
            start.final_no, start.orig_no, n, start.commit_id, name
        ));
        i += n;
    }
    out.join(";")
}

/// Rebuild the attribution from a cached encoding, taking line CONTENT from the
/// blamed blob. Returns `None` when the record does not parse or the blob is
/// gone, so a damaged entry falls back to a real blame rather than lying.
fn lines_from_cache(
    repo: &gix::Repository,
    blob: ObjectId,
    runs: &str,
) -> Option<(Vec<Line>, Vec<u8>)> {
    let data = repo.find_object(blob).ok()?.detach().data;
    let mut content: Vec<Vec<u8>> = Vec::new();
    for chunk in data.split_inclusive(|b| *b == b'\n') {
        let mut c = chunk.to_vec();
        if c.last() == Some(&b'\n') {
            c.pop();
        }
        content.push(c);
    }

    let mut lines: Vec<Line> = Vec::new();
    for rec in runs.split(';').filter(|r| !r.is_empty()) {
        let mut f = rec.splitn(5, ',');
        let final_no: u32 = f.next()?.parse().ok()?;
        let orig_no: u32 = f.next()?.parse().ok()?;
        let count: u32 = f.next()?.parse().ok()?;
        let commit_id = ObjectId::from_hex(f.next()?.as_bytes()).ok()?;
        let name = f.next().unwrap_or("");
        let source_name = (!name.is_empty()).then(|| name.as_bytes().to_vec());
        for k in 0..count {
            let idx = (final_no + k) as usize - 1;
            lines.push(Line {
                commit_id,
                final_no: final_no + k,
                orig_no: orig_no + k,
                source_name: source_name.clone(),
                content: content.get(idx)?.clone(),
                ignored: false,
                unblamable: false,
            });
        }
    }
    (!lines.is_empty()).then_some((lines, data))
}

/// One output line: which commit it came from and where it sits in both files.
struct Line {
    commit_id: ObjectId,
    final_no: u32,
    orig_no: u32,
    source_name: Option<Vec<u8>>,
    content: Vec<u8>,
    /// git's `blame_entry::ignored`: the line reached this commit through the
    /// `--ignore-rev` similarity match rather than through a diff.
    ignored: bool,
    /// git's `blame_entry::unblamable`: the line belongs to an ignored commit but
    /// no similar line was found in any parent.
    unblamable: bool,
}

/// Flatten `gix-blame`'s hunks into one `Line` per line of the blamed file.
fn materialize_lines(outcome: &gix::blame::Outcome) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    for (entry, tokens) in outcome.entries_with_lines() {
        let blamed_start = entry.start_in_blamed_file;
        let source_start = entry.start_in_source_file;
        let source_name = entry.source_file_name.as_ref().map(|n| n.to_vec());
        for (i, token) in tokens.into_iter().enumerate() {
            let i = i as u32;
            // Line tokens include their trailing '\n'; strip exactly one so the
            // writer below can re-add it (git also terminates a final line that
            // had no newline of its own).
            let mut content = token.to_vec();
            if content.last() == Some(&b'\n') {
                content.pop();
            }
            lines.push(Line {
                commit_id: entry.commit_id,
                final_no: blamed_start + i + 1,
                orig_no: source_start + i + 1,
                source_name: source_name.clone(),
                content,
                ignored: entry.ignored,
                unblamable: entry.unblamable,
            });
        }
    }
    lines
}

/// Rebase a `HEAD` blame onto the working-tree content.
///
/// git blames the working tree by putting a synthetic commit holding the
/// working-tree blob on top of `HEAD` and running its usual algorithm; the first
/// diff that commit takes part in is exactly `HEAD:<path>` against the working
/// tree. Lines that survive that diff unchanged carry `HEAD`'s blame result,
/// lines that don't stay with the synthetic commit (the null object id).
fn overlay_worktree(
    repo: &gix::Repository,
    head_lines: Vec<Line>,
    head_blob: &[u8],
    worktree: &[u8],
    diff_algorithm: Option<gix::diff::blob::Algorithm>,
) -> Result<Vec<Line>> {
    let mapped = worktree_line_map(repo, head_blob, worktree, diff_algorithm)?;
    let after_len = mapped.len() as u32;

    let null_id = ObjectId::null(repo.object_hash());
    let tokens: Vec<&[u8]> = gix::diff::blob::sources::byte_lines(worktree).collect();

    let mut out = Vec::with_capacity(after_len as usize);
    for (i, token) in tokens.into_iter().enumerate() {
        let mut content = token.to_vec();
        if content.last() == Some(&b'\n') {
            content.pop();
        }
        let final_no = i as u32 + 1;
        match mapped[i].and_then(|h| head_lines.get(h as usize)) {
            Some(src) => out.push(Line {
                commit_id: src.commit_id,
                final_no,
                orig_no: src.orig_no,
                source_name: src.source_name.clone(),
                content,
                ignored: src.ignored,
                unblamable: src.unblamable,
            }),
            None => out.push(Line {
                commit_id: null_id,
                final_no,
                orig_no: final_no,
                source_name: None,
                content,
                ignored: false,
                unblamable: false,
            }),
        }
    }
    Ok(out)
}

/// The diff the synthetic working-tree commit takes part in, as a per-line map: for
/// every line of the final image, the (0-based) line of `head_blob` it is unchanged
/// from, or `None` when the line is new and therefore stays with that commit.
fn worktree_line_map(
    repo: &gix::Repository,
    head_blob: &[u8],
    worktree: &[u8],
    diff_algorithm: Option<gix::diff::blob::Algorithm>,
) -> Result<Vec<Option<u32>>> {
    let input = gix::diff::blob::InternedInput::new(head_blob, worktree);
    // `--diff-algorithm` applies to the fake-commit diff too, matching git which
    // threads its `xdl_opts` through every diff in the blame.
    let algorithm = match diff_algorithm {
        Some(a) => a,
        None => repo.diff_algorithm()?,
    };
    let mut diff = gix::diff::blob::Diff::compute(algorithm, &input);
    diff.postprocess_lines(&input);

    let after_len = input.after.len() as u32;
    let mut mapped: Vec<Option<u32>> = vec![None; after_len as usize];
    let (mut before, mut after) = (0u32, 0u32);
    for hunk in diff.hunks() {
        while after < hunk.after.start {
            mapped[after as usize] = Some(before);
            after += 1;
            before += 1;
        }
        after = hunk.after.end;
        before = hunk.before.end;
    }
    while after < after_len {
        mapped[after as usize] = Some(before);
        after += 1;
        before += 1;
    }
    Ok(mapped)
}

/// Everything about a commit that either output format can need.
struct CommitInfo {
    /// Human-format author column: name, or `<email>` under `-e`.
    display_author: Vec<u8>,
    /// Human-format date column.
    display_date: String,
    boundary: bool,
    hex: String,
    author_name: Vec<u8>,
    author_mail: Vec<u8>,
    author_time: i64,
    author_tz: String,
    committer_name: Vec<u8>,
    committer_mail: Vec<u8>,
    committer_time: i64,
    committer_tz: String,
    summary: Vec<u8>,
}

fn collect_commit_info(
    repo: &gix::Repository,
    lines: &[Line],
    opts: &Options,
    null_id: &ObjectId,
    rel_path: &str,
) -> Result<HashMap<ObjectId, CommitInfo>> {
    let mut info: HashMap<ObjectId, CommitInfo> = HashMap::new();
    for line in lines {
        if info.contains_key(&line.commit_id) {
            continue;
        }
        let ci = if &line.commit_id == null_id {
            not_committed_info(line.commit_id, opts, rel_path)
        } else {
            let commit = repo.find_commit(line.commit_id)?;
            let author = commit.author()?;
            let committer = commit.committer()?;
            let author_time = author.time().ok();
            let committer_time = committer.time().ok();
            // Reduced to owned values before the struct literal: the iterator
            // and the summary both borrow `commit`, which drops at the end of
            // this block while the literal's temporaries are still live.
            // `--root` (git's `show_root`) stops root commits counting as
            // boundaries, dropping both the `^` marker and the porcelain
            // `boundary` field for them.
            let boundary = !opts.show_root && commit.parent_ids().next().is_none();
            let summary = Vec::from(commit.message()?.summary().into_owned());
            CommitInfo {
                display_author: display_author(author.name, author.email, opts.show_email),
                display_date: author_time
                    .map(|t| opts.date_mode.format_time(t.seconds, t.offset))
                    .unwrap_or_else(|| author.time.to_string()),
                boundary,
                hex: line.commit_id.to_hex().to_string(),
                author_name: author.name.to_vec(),
                author_mail: author.email.to_vec(),
                author_time: author_time.map(|t| t.seconds).unwrap_or(0),
                author_tz: format_tz(author_time.map(|t| t.offset).unwrap_or(0)),
                committer_name: committer.name.to_vec(),
                committer_mail: committer.email.to_vec(),
                committer_time: committer_time.map(|t| t.seconds).unwrap_or(0),
                committer_tz: format_tz(committer_time.map(|t| t.offset).unwrap_or(0)),
                summary,
            }
        };
        info.insert(line.commit_id, ci);
    }
    Ok(info)
}

/// The synthetic commit git invents for the final image (working tree, or the
/// `--contents` file). git's `fake_working_tree_commit` uses a different author
/// identity and message `from` field when `--contents` supplies the image.
fn not_committed_info(id: ObjectId, opts: &Options, rel_path: &str) -> CommitInfo {
    let now = gix::date::Time::now_local_or_utc();
    // git: `"External file (--contents)" / "external.file"` for `--contents`,
    // else `"Not Committed Yet" / "not.committed.yet"`.
    let (name, mail): (&[u8], &[u8]) = if opts.contents.is_some() {
        (b"External file (--contents)", b"external.file")
    } else {
        (NOT_COMMITTED_NAME, NOT_COMMITTED_MAIL)
    };
    // git's message: `"Version of %s from %s"` where the second `%s` is the path,
    // or the `--contents` argument (`"standard input"` for `-`).
    let from = match opts.contents.as_deref() {
        Some("-") => "standard input".to_string(),
        Some(f) => f.to_string(),
        None => rel_path.to_string(),
    };
    CommitInfo {
        display_author: display_author(name.as_bstr(), mail.as_bstr(), opts.show_email),
        display_date: opts.date_mode.format_time(now.seconds, now.offset),
        boundary: false,
        hex: id.to_hex().to_string(),
        author_name: name.to_vec(),
        author_mail: mail.to_vec(),
        author_time: now.seconds,
        author_tz: format_tz(now.offset),
        committer_name: name.to_vec(),
        committer_mail: mail.to_vec(),
        committer_time: now.seconds,
        committer_tz: format_tz(now.offset),
        summary: format!("Version of {rel_path} from {from}").into_bytes(),
    }
}

fn display_author(name: &gix::bstr::BStr, email: &gix::bstr::BStr, show_email: bool) -> Vec<u8> {
    if show_email {
        let mut v = Vec::with_capacity(email.len() + 2);
        v.push(b'<');
        v.extend_from_slice(email);
        v.push(b'>');
        v
    } else {
        name.to_vec()
    }
}

/// Effective object-name width, following git: `-l` forces the full hash,
/// otherwise `--abbrev`/`core.abbrev` applies and one extra digit is reserved so
/// the boundary caret can take a slot without shrinking the column.
fn object_name_width(repo: &gix::Repository, opts: &Options) -> usize {
    let hexsz = repo.object_hash().len_in_hex();
    let mut width = if opts.long {
        hexsz
    } else {
        match opts.abbrev {
            // `--abbrev=0` means "no abbreviation" to git.
            Some(0) => hexsz,
            Some(n) => n.clamp(MINIMUM_ABBREV, hexsz),
            None => configured_abbrev(repo, hexsz).clamp(MINIMUM_ABBREV, hexsz),
        }
    };
    if width < hexsz {
        width += 1;
    }
    width
}

/// Emit the object-name column into `buf`, following git's `emit_other`, which
/// spends the column budget on markers first and prints what is left of the hex:
///   * `-b` (`blank_boundary`) blanks a boundary commit's name to spaces — and
///     also suppresses the `^` marker, but does *not* give the column back, so
///     the blanked run is still `name_width` wide.
///   * otherwise a boundary commit takes one column for `^` (never in
///     annotate-compat mode, which prints no marker).
///   * `blame.markUnblamableLines` then takes a column for `*`, and
///     `blame.markIgnoredLines` one for `?`, in that order.
///   * whatever budget remains is filled with hex digits.
fn emit_object_name(buf: &mut Vec<u8>, ci: &CommitInfo, line: &Line, name_width: usize, opts: &Options) {
    let mut length = name_width;
    let blanked = ci.boundary && opts.blank_boundary;
    if ci.boundary && !blanked && !opts.annotate_compat {
        buf.push(b'^');
        length -= 1;
    }
    if opts.mark_unblamable_lines && line.unblamable {
        buf.push(b'*');
        length -= 1;
    }
    if opts.mark_ignored_lines && line.ignored {
        buf.push(b'?');
        length -= 1;
    }
    if blanked {
        pad(buf, length);
    } else {
        buf.extend_from_slice(&ci.hex.as_bytes()[..length]);
    }
}

/// Right-justify `s` into `buf` to a minimum byte width of `min`, matching C's
/// `%*s` (spaces on the left, no truncation).
fn pad_left(buf: &mut Vec<u8>, s: &[u8], min: usize) {
    pad(buf, min.saturating_sub(s.len()));
    buf.extend_from_slice(s);
}

/// git-annotate-compatible output (`-c` / OUTPUT_ANNOTATE_COMPAT): one line per
/// blamed line, `<name>` right-justified to 10, the date to 10, tab-separated, and
/// the final 1-based line number, all inside a single `(...)`. `-f`/`-n`/`-s` do
/// not apply in this mode.
fn emit_annotate_compat(
    lines: &[Line],
    info: &HashMap<ObjectId, CommitInfo>,
    name_width: usize,
    opts: &Options,
) -> Result<ExitCode> {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut buf: Vec<u8> = Vec::with_capacity(128);

    for line in lines {
        let ci = &info[&line.commit_id];
        buf.clear();

        emit_object_name(&mut buf, ci, line, name_width, opts);

        // format_time pads the date to `blame_date_width` first; the trailing
        // `%10s` then never fires because every mode's width is >= 10.
        let mut date = ci.display_date.clone().into_bytes();
        pad(&mut date, opts.date_mode.width().saturating_sub(ci.display_date.chars().count()));

        buf.extend_from_slice(b"\t(");
        pad_left(&mut buf, &ci.display_author, 10);
        buf.push(b'\t');
        pad_left(&mut buf, &date, 10);
        buf.push(b'\t');
        buf.extend_from_slice(line.final_no.to_string().as_bytes());
        buf.push(b')');
        buf.extend_from_slice(&line.content);
        buf.push(b'\n');

        out.write_all(&buf)?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn emit_human(
    repo: &gix::Repository,
    lines: &[Line],
    info: &HashMap<ObjectId, CommitInfo>,
    rel_path: &str,
    opts: &Options,
    colors: &BlameColors,
) -> Result<ExitCode> {
    let name_width = object_name_width(repo, opts);

    if opts.annotate_compat {
        return emit_annotate_compat(lines, info, name_width, opts);
    }

    // `emit_other` colors per blame entry: `cnt` counts lines within the entry,
    // and only `cnt > 0` takes the repeated-metadata color. The entries are the
    // coalesced groups the porcelain format also emits.
    let mut cnt_in_entry: Vec<usize> = vec![0; lines.len()];
    for group in group_lines(lines) {
        for k in 0..group.len {
            cnt_in_entry[group.start + k] = k;
        }
    }

    let show_file = opts.show_name || lines.iter().any(|l| l.source_name.is_some());
    let current_path = rel_path.as_bytes();
    let w_line = decimal_width(lines.iter().map(|l| l.final_no).max().unwrap_or(1));
    let w_orig = decimal_width(lines.iter().map(|l| l.orig_no).max().unwrap_or(1));
    let w_file = if show_file {
        lines
            .iter()
            .map(|l| l.source_name.as_deref().unwrap_or(current_path).len())
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let w_author = if opts.suppress {
        0
    } else {
        lines
            .iter()
            .map(|l| info[&l.commit_id].display_author.len())
            .max()
            .unwrap_or(0)
    };

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut buf: Vec<u8> = Vec::with_capacity(128);

    for (idx, line) in lines.iter().enumerate() {
        let ci = &info[&line.commit_id];
        buf.clear();

        // The color prefix wraps the whole metadata block, boundary marker
        // included, and the reset lands after the `") "` that ends it.
        let (color, reset) = colors.for_line(cnt_in_entry[idx], ci.author_time);
        if let Some(color) = color {
            buf.extend_from_slice(color.as_bytes());
        }

        // Object name (boundary `^` marker, `-b` blanking).
        emit_object_name(&mut buf, ci, line, name_width, opts);

        // Source filename column (left-justified).
        if show_file {
            let name = line.source_name.as_deref().unwrap_or(current_path);
            buf.push(b' ');
            buf.extend_from_slice(name);
            pad(&mut buf, w_file.saturating_sub(name.len()));
        }

        // Original line number in the source commit (right-justified).
        if opts.show_number {
            let s = line.orig_no.to_string();
            buf.push(b' ');
            pad(&mut buf, w_orig.saturating_sub(s.len()));
            buf.extend_from_slice(s.as_bytes());
        }

        // Author/date block (omitted entirely by `-s`, mirroring git which then
        // leaves the closing paren of the line-number field unmatched).
        if !opts.suppress {
            buf.extend_from_slice(b" (");
            buf.extend_from_slice(&ci.display_author);
            pad(&mut buf, w_author.saturating_sub(ci.display_author.len()));
            buf.push(b' ');
            // The date column is left-justified in a fixed, per-mode width
            // (git's `blame_date_width`), so shorter renderings are padded out.
            buf.extend_from_slice(ci.display_date.as_bytes());
            pad(
                &mut buf,
                opts.date_mode
                    .width()
                    .saturating_sub(ci.display_date.chars().count()),
            );
        }

        // Final line number (right-justified) + content.
        let s = line.final_no.to_string();
        buf.push(b' ');
        pad(&mut buf, w_line.saturating_sub(s.len()));
        buf.extend_from_slice(s.as_bytes());
        buf.extend_from_slice(b") ");
        if let Some(reset) = reset {
            buf.extend_from_slice(reset.as_bytes());
        }
        buf.extend_from_slice(&line.content);
        buf.push(b'\n');

        out.write_all(&buf)?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn emit_porcelain(
    repo: &gix::Repository,
    lines: &[Line],
    info: &HashMap<ObjectId, CommitInfo>,
    rel_path: &str,
    head_id: Option<ObjectId>,
    null_id: &ObjectId,
    opts: &Options,
) -> Result<ExitCode> {
    let current_path = rel_path.as_bytes();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    // git prints a commit's detail block once per output (`--porcelain`) or once
    // per line (`--line-porcelain`).
    let mut shown: HashSet<ObjectId> = HashSet::new();
    type PreviousCache = HashMap<(ObjectId, Vec<u8>), Option<(String, Vec<u8>)>>;
    let mut previous_cache: PreviousCache = HashMap::new();

    for group in group_lines(lines) {
        let first = &lines[group.start];
        let ci = &info[&first.commit_id];
        let path = first.source_name.as_deref().unwrap_or(current_path);

        let key = (first.commit_id, path.to_vec());
        if !previous_cache.contains_key(&key) {
            let previous = if &first.commit_id == null_id {
                head_id.map(|h| (h.to_hex().to_string(), current_path.to_vec()))
            } else {
                find_previous(repo, first.commit_id, path)?
            };
            previous_cache.insert(key.clone(), previous);
        }
        let previous = &previous_cache[&key];

        for (i, line) in lines[group.start..group.start + group.len].iter().enumerate() {
            if i == 0 {
                writeln!(
                    out,
                    "{} {} {} {}",
                    ci.hex, line.orig_no, line.final_no, group.len
                )?;
            } else {
                writeln!(out, "{} {} {}", ci.hex, line.orig_no, line.final_no)?;
            }
            if (i == 0 || opts.line_porcelain)
                && (opts.line_porcelain || shown.insert(first.commit_id)) {
                    write_detail(&mut out, ci, previous.as_ref(), path)?;
                }
            // git's `emit_porcelain_per_line_details`, printed for every line right
            // after the (possibly absent) commit detail block.
            if opts.mark_unblamable_lines && line.unblamable {
                out.write_all(b"unblamable\n")?;
            }
            if opts.mark_ignored_lines && line.ignored {
                out.write_all(b"ignored\n")?;
            }
            out.write_all(b"\t")?;
            out.write_all(&line.content)?;
            out.write_all(b"\n")?;
        }
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// One entry of `git blame --incremental`: a stretch of the final image that one
/// commit took responsibility for in one step of the walk, *before* `blame_coalesce()`
/// would have merged it with an adjacent stretch of the same commit.
struct IncrementalEntry {
    commit_id: ObjectId,
    /// 1-based first line in the *Source File* (git's `ent->s_lno + 1`).
    orig_no: u32,
    /// 1-based first line in the final image (git's `ent->lno + 1`).
    final_no: u32,
    num_lines: u32,
    source_name: Option<Vec<u8>>,
}

/// Turn `gix-blame`'s walk-order entries into the entries git's `found_guilty_entry()`
/// sees, optionally rebased onto the working-tree image.
///
/// Without an overlay this is a straight 0-based to 1-based translation. With one, git
/// runs the whole walk on top of the synthetic commit holding the final image, so the
/// entries it emits are these entries cut down to the lines that survived that first
/// diff; the lines that did not survive stay with the synthetic commit, which the walk
/// pops first and which therefore contributes the leading entries.
fn incremental_entries(
    entries: &[gix::blame::BlameEntry],
    mapped: Option<&[Option<u32>]>,
    null_id: &ObjectId,
) -> Vec<IncrementalEntry> {
    let Some(mapped) = mapped else {
        return entries
            .iter()
            .map(|e| IncrementalEntry {
                commit_id: e.commit_id,
                orig_no: e.start_in_source_file + 1,
                final_no: e.start_in_blamed_file + 1,
                num_lines: e.len.get(),
                source_name: e.source_file_name.as_ref().map(|n| n.to_vec()),
            })
            .collect();
    };

    let mut out: Vec<IncrementalEntry> = Vec::new();
    // The synthetic commit keeps every line that the diff against the blamed blob
    // reports as new, in one entry per contiguous run.
    let mut i = 0usize;
    while i < mapped.len() {
        if mapped[i].is_some() {
            i += 1;
            continue;
        }
        let start = i;
        while i < mapped.len() && mapped[i].is_none() {
            i += 1;
        }
        out.push(IncrementalEntry {
            commit_id: *null_id,
            orig_no: start as u32 + 1,
            final_no: start as u32 + 1,
            num_lines: (i - start) as u32,
            source_name: None,
        });
    }

    // The inverse map, so an entry stated in blamed-blob lines can be restated in lines
    // of the final image.
    let head_lines = mapped.iter().flatten().copied().max().map_or(0, |m| m as usize + 1);
    let mut final_of_head: Vec<Option<u32>> = vec![None; head_lines];
    for (final_no, head_no) in mapped.iter().enumerate() {
        if let Some(head_no) = head_no {
            final_of_head[*head_no as usize] = Some(final_no as u32);
        }
    }

    for entry in entries {
        let mut run: Option<(u32, u32, u32)> = None; // (orig_no, final_no, len)
        for offset in 0..entry.len.get() {
            let head_no = entry.start_in_blamed_file + offset;
            let this = final_of_head.get(head_no as usize).copied().flatten();
            match (this, &mut run) {
                // The run continues only while both files stay contiguous, which is
                // exactly what `blame_chunk()` splits an entry on.
                (Some(f), Some((_, first_final, len))) if *first_final + *len == f => *len += 1,
                (Some(f), slot) => {
                    if let Some((orig_no, final_no, len)) = slot.take() {
                        out.push(IncrementalEntry {
                            commit_id: entry.commit_id,
                            orig_no,
                            final_no: final_no + 1,
                            num_lines: len,
                            source_name: entry.source_file_name.as_ref().map(|n| n.to_vec()),
                        });
                    }
                    *slot = Some((entry.start_in_source_file + offset + 1, f, 1));
                }
                (None, slot) => {
                    if let Some((orig_no, final_no, len)) = slot.take() {
                        out.push(IncrementalEntry {
                            commit_id: entry.commit_id,
                            orig_no,
                            final_no: final_no + 1,
                            num_lines: len,
                            source_name: entry.source_file_name.as_ref().map(|n| n.to_vec()),
                        });
                    }
                }
            }
        }
        if let Some((orig_no, final_no, len)) = run {
            out.push(IncrementalEntry {
                commit_id: entry.commit_id,
                orig_no,
                final_no: final_no + 1,
                num_lines: len,
                source_name: entry.source_file_name.as_ref().map(|n| n.to_vec()),
            });
        }
    }
    out
}

/// Keep only the parts of each entry that fall inside `ranges`, splitting an entry that
/// straddles a boundary — what git gets for free by narrowing the scoreboard to `-L`
/// before the walk starts.
fn clip_to_ranges(
    entries: Vec<IncrementalEntry>,
    ranges: &[std::ops::RangeInclusive<u32>],
) -> Vec<IncrementalEntry> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut run: Option<(u32, u32, u32)> = None; // (orig_no, final_no, len)
        let mut flush = |run: &mut Option<(u32, u32, u32)>, out: &mut Vec<IncrementalEntry>| {
            if let Some((orig_no, final_no, num_lines)) = run.take() {
                out.push(IncrementalEntry {
                    commit_id: entry.commit_id,
                    orig_no,
                    final_no,
                    num_lines,
                    source_name: entry.source_name.clone(),
                });
            }
        };
        for offset in 0..entry.num_lines {
            let final_no = entry.final_no + offset;
            if ranges.iter().any(|r| r.contains(&final_no)) {
                match &mut run {
                    Some((_, _, len)) => *len += 1,
                    slot => *slot = Some((entry.orig_no + offset, final_no, 1)),
                }
            } else {
                flush(&mut run, &mut out);
            }
        }
        flush(&mut run, &mut out);
    }
    out
}

/// git's `found_guilty_entry()` under `--incremental`: stream every entry as the walk
/// finalizes it, with the commit's detail block emitted only the first time that commit
/// appears and the path information repeated for every entry.
fn emit_incremental(
    repo: &gix::Repository,
    entries: &[IncrementalEntry],
    info: &HashMap<ObjectId, CommitInfo>,
    rel_path: &str,
    head_id: Option<ObjectId>,
    null_id: &ObjectId,
) -> Result<ExitCode> {
    let current_path = rel_path.as_bytes();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    let mut shown: HashSet<ObjectId> = HashSet::new();
    type PreviousCache = HashMap<(ObjectId, Vec<u8>), Option<(String, Vec<u8>)>>;
    let mut previous_cache: PreviousCache = HashMap::new();

    for entry in entries {
        let ci = &info[&entry.commit_id];
        let path = entry.source_name.as_deref().unwrap_or(current_path);
        writeln!(
            out,
            "{} {} {} {}",
            ci.hex, entry.orig_no, entry.final_no, entry.num_lines
        )?;
        if shown.insert(entry.commit_id) {
            write_suspect_detail(&mut out, ci)?;
        }
        let key = (entry.commit_id, path.to_vec());
        if !previous_cache.contains_key(&key) {
            let previous = if &entry.commit_id == null_id {
                head_id.map(|h| (h.to_hex().to_string(), current_path.to_vec()))
            } else {
                find_previous(repo, entry.commit_id, path)?
            };
            previous_cache.insert(key.clone(), previous);
        }
        write_filename_info(&mut out, previous_cache[&key].as_ref(), path)?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// git's `emit_one_suspect_detail()`: everything about a commit that does not depend on
/// the path.
fn write_suspect_detail(out: &mut impl Write, ci: &CommitInfo) -> Result<()> {
    write_field(out, b"author", &ci.author_name)?;
    out.write_all(b"author-mail <")?;
    out.write_all(&ci.author_mail)?;
    out.write_all(b">\n")?;
    writeln!(out, "author-time {}", ci.author_time)?;
    writeln!(out, "author-tz {}", ci.author_tz)?;
    write_field(out, b"committer", &ci.committer_name)?;
    out.write_all(b"committer-mail <")?;
    out.write_all(&ci.committer_mail)?;
    out.write_all(b">\n")?;
    writeln!(out, "committer-time {}", ci.committer_time)?;
    writeln!(out, "committer-tz {}", ci.committer_tz)?;
    write_field(out, b"summary", &ci.summary)?;
    if ci.boundary {
        out.write_all(b"boundary\n")?;
    }
    Ok(())
}

/// git's `write_filename_info()`: the suspect information that does depend on the path,
/// which is why it is repeated for every group rather than once per commit.
fn write_filename_info(
    out: &mut impl Write,
    previous: Option<&(String, Vec<u8>)>,
    path: &[u8],
) -> Result<()> {
    if let Some((hex, prev_path)) = previous {
        out.write_all(b"previous ")?;
        out.write_all(hex.as_bytes())?;
        out.write_all(b" ")?;
        out.write_all(&quote_name(prev_path))?;
        out.write_all(b"\n")?;
    }
    out.write_all(b"filename ")?;
    out.write_all(&quote_name(path))?;
    out.write_all(b"\n")?;
    Ok(())
}

/// The porcelain formats' commit block: git emits the path-independent detail and the
/// path information together there, because it only prints the latter when it printed
/// the former.
fn write_detail(
    out: &mut impl Write,
    ci: &CommitInfo,
    previous: Option<&(String, Vec<u8>)>,
    path: &[u8],
) -> Result<()> {
    write_suspect_detail(out, ci)?;
    write_filename_info(out, previous, path)
}

fn write_field(out: &mut impl Write, key: &[u8], value: &[u8]) -> Result<()> {
    out.write_all(key)?;
    out.write_all(b" ")?;
    out.write_all(value)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// A run of consecutive output lines sharing one commit, one source path and a
/// contiguous stretch of the source file — git's `blame_coalesce()` rule.
struct Group {
    start: usize,
    len: usize,
}

fn group_lines(lines: &[Line]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let extends = groups.last().is_some_and(|g: &Group| {
            let prev = &lines[g.start + g.len - 1];
            prev.commit_id == line.commit_id
                && prev.source_name == line.source_name
                && prev.orig_no + 1 == line.orig_no
                && prev.final_no + 1 == line.final_no
                // `blame_coalesce()` also refuses to merge across these.
                && prev.ignored == line.ignored
                && prev.unblamable == line.unblamable
        });
        if extends {
            let last = groups.len() - 1;
            groups[last].len += 1;
        } else {
            groups.push(Group { start: i, len: 1 });
        }
    }
    groups
}

/// The `previous <commit> <path>` field: the first parent of `commit` in which
/// `path` still exists. git records the origin it found in the first parent it
/// looked at; when the file is not in that parent (the commit added it, or the
/// commit is a root) there is no `previous` line at all.
///
/// A commit that both renamed and modified the file in one step would need
/// rename detection against the parent to name the pre-rename path; that case is
/// not covered here and yields no `previous` line.
fn find_previous(
    repo: &gix::Repository,
    commit: ObjectId,
    path: &[u8],
) -> Result<Option<(String, Vec<u8>)>> {
    let commit = repo.find_commit(commit)?;
    let Some(parent) = commit.parent_ids().next() else {
        return Ok(None);
    };
    let parent_id = parent.detach();
    let Ok(path_str) = std::str::from_utf8(path) else {
        return Ok(None);
    };
    let tree = repo.find_commit(parent_id)?.tree()?;
    if tree
        .lookup_entry_by_path(std::path::Path::new(path_str))?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some((parent_id.to_hex().to_string(), path.to_vec())))
}

/// git's `quote_c_style`: paths containing control, quote, backslash or non-ASCII
/// bytes are emitted as a C-style quoted string, everything else verbatim.
fn quote_name(name: &[u8]) -> Vec<u8> {
    let needs_quoting = name
        .iter()
        .any(|&b| !(0x20..0x7f).contains(&b) || b == b'"' || b == b'\\');
    if !needs_quoting {
        return name.to_vec();
    }
    let mut out = Vec::with_capacity(name.len() + 2);
    out.push(b'"');
    for &b in name {
        match b {
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x0b => out.extend_from_slice(b"\\v"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b if !(0x20..0x7f).contains(&b) => out.extend_from_slice(format!("\\{b:03o}").as_bytes()),
            b => out.push(b),
        }
    }
    out.push(b'"');
    out
}

/// Format a UTC offset the way git writes the `author-tz`/`committer-tz` field.
fn format_tz(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let abs = offset_seconds.unsigned_abs();
    format!("{sign}{:02}{:02}", abs / 3600, (abs % 3600) / 60)
}

/// Print git blame's usage text on stderr and yield its exit status (129).
fn print_usage() -> Result<ExitCode> {
    let mut err = std::io::stderr().lock();
    err.write_all(USAGE.as_bytes())?;
    err.flush()?;
    Ok(ExitCode::from(129))
}

/// Outcome of splitting the positionals into `[<rev>...] <file>` and resolving
/// the revision, mirroring `cmd_blame`'s argument grammar in git.
enum Targets {
    /// The positional shape is not a valid blame invocation: print usage (129).
    Usage,
    /// A fatal error (`bad revision` / `More than one commit`) was already
    /// written to stderr; return this exit code (128).
    Fatal(ExitCode),
    /// `opts.file`, `opts.rev` and `opts.suspect_id` are now populated.
    Resolved,
}

/// Expand a leading `~` / `~/` to `$HOME`, which is what `git_config_pathname` does
/// for `blame.ignoreRevsFile`.
fn expand_tilde(value: &str) -> String {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return value.to_string();
    };
    let expanded = if value == "~" {
        home
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        return value.to_string();
    };
    expanded.to_string_lossy().into_owned()
}

/// git's `verify_working_tree_path` fallback: the path is tracked in the index even
/// though the blamed commit does not have it (it was just `git add`ed, or is
/// unmerged). Blame then still works, against an empty history.
fn path_in_index(repo: &gix::Repository, rel_path: &str) -> bool {
    let Ok(index) = repo.index_or_empty() else {
        return false;
    };
    // git looks for the exact name at any stage, so an unmerged path counts too.
    let entries = index.entries();
    entries
        .iter()
        .any(|e| &**e.path(&index) == rel_path.as_bytes())
}

/// git's `peel_to_commit_oid`: peel tags until a commit is reached. Anything that is
/// not a commit and not a tag chain ending in one yields `None`, which
/// `oidset_parse_file_carefully` treats as "skip this line".
fn peel_ignored_oid(repo: &gix::Repository, oid: ObjectId) -> Option<ObjectId> {
    repo.find_object(oid)
        .ok()?
        .peel_to_commit()
        .ok()
        .map(|c| c.id().detach())
}

/// git's `build_ignorelist`: the revision files first (in the order config and
/// command line contributed them), then the `--ignore-rev` arguments.
///
/// A file that cannot be opened, and a line in one that is not a full object name,
/// are fatal; a well-formed object name that is not a commit is skipped silently. An
/// `--ignore-rev` argument that names nothing is fatal.
fn build_ignorelist(
    repo: &gix::Repository,
    opts: &Options,
) -> Result<HashSet<ObjectId>, ExitCode> {
    let mut set: HashSet<ObjectId> = HashSet::new();

    for path in &opts.ignore_revs_file {
        let Ok(data) = std::fs::read(path) else {
            eprintln!("fatal: could not open object name list: {path}");
            return Err(ExitCode::from(128));
        };
        for raw in data.split(|b| *b == b'\n') {
            // `strbuf_getline` drops the terminator and a trailing CR; then trailing
            // comments and surrounding whitespace go, and blank lines are skipped.
            let line = raw.strip_suffix(b"\r").unwrap_or(raw);
            let line = match line.iter().position(|b| *b == b'#') {
                Some(hash) => &line[..hash],
                None => line,
            };
            let line = line.trim_ascii();
            if line.is_empty() {
                continue;
            }
            let Ok(oid) = ObjectId::from_hex(line) else {
                eprintln!(
                    "fatal: invalid object name: {}",
                    String::from_utf8_lossy(line)
                );
                return Err(ExitCode::from(128));
            };
            if let Some(commit) = peel_ignored_oid(repo, oid) {
                set.insert(commit);
            }
        }
    }

    for rev in &opts.ignore_rev {
        match resolve_commit(repo, rev) {
            Some(id) => {
                set.insert(id);
            }
            None => {
                eprintln!("fatal: cannot find revision {rev} to ignore");
                return Err(ExitCode::from(128));
            }
        }
    }

    Ok(set)
}

/// git's `is_a_rev`: the name resolves to some object in the repository.
fn is_a_rev(repo: &gix::Repository, name: &str) -> bool {
    repo.rev_parse_single(name).is_ok()
}

/// Resolve a revision to the commit it names (peeling tags), or `None` if it is
/// not a valid revision — git's `get_oid` followed by a peel to commit.
fn resolve_commit(repo: &gix::Repository, rev: &str) -> Option<ObjectId> {
    repo.rev_parse_single(rev)
        .ok()?
        .object()
        .ok()?
        .peel_to_commit()
        .ok()
        .map(|c| c.id().detach())
}

/// Split the collected positionals into `[<rev>...] <file>` following git
/// blame's DWIM rules, then resolve the revision. Reproduces `cmd_blame`'s
/// argument handling for the presence/absence of the `--` separator.
fn resolve_targets(repo: &gix::Repository, opts: &mut Options) -> Result<Targets> {
    // Determine the revision arguments (in order) and the single path.
    let (revs, file): (Vec<String>, String) = match opts.post.take() {
        // `--` was present: everything after it is a pathspec. blame accepts
        // exactly one path; a trailing second token is DWIM'd as a revision.
        Some(post) => {
            let pre = std::mem::take(&mut opts.pre);
            match post.len() {
                0 => return Ok(Targets::Usage),
                1 => (pre, post.into_iter().next().unwrap()),
                // `blame -- <file> <rev>`: only legal with no revs before `--`.
                2 if pre.is_empty() => {
                    let mut it = post.into_iter();
                    let file = it.next().unwrap();
                    let rev = it.next().unwrap();
                    (vec![rev], file)
                }
                _ => return Ok(Targets::Usage),
            }
        }
        // No `--`: the last positional is the path, the rest are revisions.
        None => {
            let mut pos = std::mem::take(&mut opts.pre);
            match pos.len() {
                0 => return Ok(Targets::Usage),
                1 => (vec![], pos.pop().unwrap()),
                // Two positionals: `blame <path> <rev>` if the last is a rev,
                // otherwise `blame <rev> <path>`.
                2 => {
                    if is_a_rev(repo, &pos[1]) {
                        let rev = pos.pop().unwrap();
                        let file = pos.pop().unwrap();
                        (vec![rev], file)
                    } else {
                        let file = pos.pop().unwrap();
                        let rev = pos.pop().unwrap();
                        (vec![rev], file)
                    }
                }
                _ => {
                    let file = pos.pop().unwrap();
                    (pos, file)
                }
            }
        }
    };

    // Resolve the revisions in order, matching git: the first that fails to
    // resolve is a `bad revision`; a second one that succeeds is `More than one
    // commit to dig from`.
    let mut suspect: Option<(String, ObjectId)> = None;
    for r in &revs {
        match resolve_commit(repo, r) {
            Some(id) => {
                if let Some((first, _)) = &suspect {
                    let mut err = std::io::stderr().lock();
                    writeln!(err, "fatal: More than one commit to dig from {first} and {r}?")?;
                    err.flush()?;
                    return Ok(Targets::Fatal(ExitCode::from(128)));
                }
                suspect = Some((r.clone(), id));
            }
            None => {
                let mut err = std::io::stderr().lock();
                writeln!(err, "fatal: bad revision '{r}'")?;
                err.flush()?;
                return Ok(Targets::Fatal(ExitCode::from(128)));
            }
        }
    }

    opts.rev = suspect.as_ref().map(|(n, _)| n.clone());
    opts.suspect_id = suspect.map(|(_, id)| id);
    opts.file = file;
    Ok(Targets::Resolved)
}

/// The date-formatting modes zvcs blame reproduces byte-for-byte from git's
/// `show_date`. git accepts a few more (`human`, `format:<strftime>`, and every
/// `-local` variant); those need machinery blame.rs does not have (an strftime
/// renderer, per-timestamp local-timezone conversion), so they are rejected
/// rather than emitting wrong bytes — matching this file's policy for
/// unimplemented features. `relative` is fully supported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DateMode {
    /// git `DATE_NORMAL` (`default`): `Thu Oct 19 16:00:04 2006 -0700`.
    Normal,
    /// git `DATE_ISO8601` (`iso`/`iso8601`): `2006-10-19 16:00:04 -0700`. blame's default.
    Iso8601,
    /// git `DATE_ISO8601_STRICT` (`iso-strict`): `2006-10-19T16:00:04-07:00` (`Z` at UTC).
    Iso8601Strict,
    /// git `DATE_RFC2822` (`rfc`): `Thu, 19 Oct 2006 16:00:04 -0700`.
    Rfc2822,
    /// git `DATE_SHORT` (`short`): `2006-10-19`.
    Short,
    /// git `DATE_RAW` (`raw`): `1161298804 -0700`.
    Raw,
    /// git `DATE_UNIX` (`unix`): `1161298804`.
    Unix,
    /// git `DATE_RELATIVE` (`relative`): `3 days ago`, computed against the current
    /// time. Independent of the recorded timezone offset.
    Relative,
}

impl DateMode {
    /// git's fixed `blame_date_width` per mode: the width the date column is
    /// left-justified into (`sizeof(reference) - 1`, i.e. the reference length).
    fn width(self) -> usize {
        match self {
            DateMode::Normal => "Thu Oct 19 16:00:04 2006 -0700".len(),
            DateMode::Iso8601 => "2006-10-19 16:00:04 -0700".len(),
            DateMode::Iso8601Strict => "2006-10-19T16:00:04-07:00".len(),
            DateMode::Rfc2822 => "Thu, 19 Oct 2006 16:00:04 -0700".len(),
            DateMode::Short => "2006-10-19".len(),
            DateMode::Raw => "1161298804 -0700".len(),
            DateMode::Unix => "1161298804".len(),
            // git: `utf8_strwidth("4 years, 11 months ago") + 1`, then the shared
            // `blame_date_width -= 1` (strip the NUL) leaves the string width.
            DateMode::Relative => "4 years, 11 months ago".len(),
        }
    }

    /// Render `<seconds> @ <offset>` the way git's `show_date` does for this mode.
    fn format_time(self, seconds: i64, offset: i32) -> String {
        use gix::date::time::format;
        let t = gix::date::Time { seconds, offset };
        match self {
            DateMode::Normal => t.format_or_unix(format::DEFAULT),
            DateMode::Iso8601 => t.format_or_unix(format::ISO8601),
            DateMode::Iso8601Strict => {
                // git prints `Z` for a zero UTC offset; jiff's `%:z` (used by gix)
                // would print `+00:00`, so fix that one case up to match.
                let s = t.format_or_unix(format::ISO8601_STRICT);
                if offset == 0 {
                    if let Some(head) = s.strip_suffix("+00:00") {
                        return format!("{head}Z");
                    }
                }
                s
            }
            // gix's `RFC2822` zero-pads the day; git's `%-d` form (`GIT_RFC2822`)
            // matches git's `show_date` exactly.
            DateMode::Rfc2822 => t.format_or_unix(format::GIT_RFC2822),
            DateMode::Short => t.format_or_unix(format::SHORT),
            DateMode::Raw => t.format_or_unix(format::RAW),
            DateMode::Unix => t.format_or_unix(format::UNIX),
            DateMode::Relative => show_date_relative(seconds),
        }
    }
}

/// git's `show_date_relative` (date.c), via the shared port — so `--date=relative`
/// in blame honors `GIT_TEST_DATE_NOW` and matches every other command exactly.
/// The recorded timezone offset is irrelevant, as in git.
fn show_date_relative(seconds: i64) -> String {
    crate::date::show_date_relative(seconds, crate::date::now_seconds())
}

/// git's `date_mode_type`, restricted to what the parser needs to classify.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DateType {
    Relative,
    Human,
    IsoStrict,
    Iso,
    Rfc,
    Short,
    Normal,
    Raw,
    Unix,
    Strftime,
}

/// The result of classifying a `--date` / blame.date value against git's grammar.
enum DateClass {
    /// A mode blame.rs renders byte-for-byte.
    Supported(DateMode),
    /// A mode git accepts but blame.rs does not implement; carries the effective
    /// format string for the diagnostic.
    Unsupported(String),
    /// Not a recognized git date format → `fatal: unknown date format <s>`.
    UnknownFormat(String),
    /// A `format`/`format-local` mode with no `:` → git's missing-colon fatal.
    MissingColon(String),
}

/// Classify a date-format string exactly the way git's `parse_date_format` /
/// `parse_date_type` do: prefix-match the type in git's order, consume an
/// optional `-local` suffix, then require the remainder to be empty (or a `:`
/// for `format`). `auto:` and the `local` alias are handled first as git does.
fn classify_date(input: &str) -> DateClass {
    // `auto:foo` → foo when stdout is a terminal, else `default`.
    let format = if let Some(rest) = input.strip_prefix("auto:") {
        if std::io::stdout().is_terminal() {
            rest.to_string()
        } else {
            "default".to_string()
        }
    } else {
        input.to_string()
    };
    // Historical alias: `local` means `default-local`.
    let format = if format == "local" {
        "default-local".to_string()
    } else {
        format
    };

    // parse_date_type: first matching prefix wins, in git's exact order.
    let f = format.as_str();
    let (ty, rest) = if let Some(r) = f.strip_prefix("relative") {
        (DateType::Relative, r)
    } else if let Some(r) = f.strip_prefix("iso8601-strict").or_else(|| f.strip_prefix("iso-strict"))
    {
        (DateType::IsoStrict, r)
    } else if let Some(r) = f.strip_prefix("iso8601").or_else(|| f.strip_prefix("iso")) {
        (DateType::Iso, r)
    } else if let Some(r) = f.strip_prefix("rfc2822").or_else(|| f.strip_prefix("rfc")) {
        (DateType::Rfc, r)
    } else if let Some(r) = f.strip_prefix("short") {
        (DateType::Short, r)
    } else if let Some(r) = f.strip_prefix("default") {
        (DateType::Normal, r)
    } else if let Some(r) = f.strip_prefix("human") {
        (DateType::Human, r)
    } else if let Some(r) = f.strip_prefix("raw") {
        (DateType::Raw, r)
    } else if let Some(r) = f.strip_prefix("unix") {
        (DateType::Unix, r)
    } else if let Some(r) = f.strip_prefix("format") {
        (DateType::Strftime, r)
    } else {
        return DateClass::UnknownFormat(format);
    };

    // Optional `-local` suffix sets local mode on any type.
    let (local, rest) = match rest.strip_prefix("-local") {
        Some(r) => (true, r),
        None => (false, rest),
    };

    if ty == DateType::Strftime {
        // `format:<strftime>` requires a colon; the strftime renderer is not
        // implemented, so a valid one is still "unsupported".
        if !rest.starts_with(':') {
            return DateClass::MissingColon(format);
        }
        return DateClass::Unsupported(format);
    }

    // Any other trailing text is not a valid format.
    if !rest.is_empty() {
        return DateClass::UnknownFormat(format);
    }

    // `-local` needs timezone conversion blame.rs does not do.
    if local {
        return DateClass::Unsupported(format);
    }

    match ty {
        DateType::Iso => DateClass::Supported(DateMode::Iso8601),
        DateType::IsoStrict => DateClass::Supported(DateMode::Iso8601Strict),
        DateType::Rfc => DateClass::Supported(DateMode::Rfc2822),
        DateType::Short => DateClass::Supported(DateMode::Short),
        DateType::Normal => DateClass::Supported(DateMode::Normal),
        DateType::Raw => DateClass::Supported(DateMode::Raw),
        DateType::Unix => DateClass::Supported(DateMode::Unix),
        DateType::Relative => DateClass::Supported(DateMode::Relative),
        // `human` needs a time-relative renderer that also folds the current
        // time into local-timezone broken-down form; not implemented.
        DateType::Human => DateClass::Unsupported(format),
        DateType::Strftime => unreachable!("strftime handled above"),
    }
}

/// Outcome of resolving a date-format value: a mode to use, or a fatal exit
/// (git's `128` for a malformed format, already reported to stderr).
enum DateOutcome {
    Mode(DateMode),
    Fatal(ExitCode),
}

/// Resolve a `--date` / blame.date value, reproducing git's fatal messages and
/// exit code for malformed formats and rejecting valid-but-unimplemented modes.
fn resolve_date_mode(input: &str) -> Result<DateOutcome> {
    match classify_date(input) {
        DateClass::Supported(m) => Ok(DateOutcome::Mode(m)),
        DateClass::Unsupported(f) => bail!("unsupported --date mode: {f}"),
        DateClass::UnknownFormat(f) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "fatal: unknown date format {f}")?;
            err.flush()?;
            Ok(DateOutcome::Fatal(ExitCode::from(128)))
        }
        DateClass::MissingColon(f) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "fatal: date format missing colon separator: {f}")?;
            err.flush()?;
            Ok(DateOutcome::Fatal(ExitCode::from(128)))
        }
    }
}

struct Options {
    rev: Option<String>,
    file: String,
    suspect_id: Option<ObjectId>,
    /// Positionals before the `--` separator (or all of them if absent).
    pre: Vec<String>,
    /// Positionals after `--`; `None` when no `--` was given.
    post: Option<Vec<String>>,
    ranges: Vec<RangeInclusive<u32>>,
    long: bool,
    suppress: bool,
    show_email: bool,
    show_name: bool,
    show_number: bool,
    abbrev: Option<usize>,
    porcelain: bool,
    line_porcelain: bool,
    /// `-b`: blank the object name of boundary commits instead of showing it.
    blank_boundary: bool,
    /// `--root`: do not treat root commits as boundaries.
    show_root: bool,
    /// `-t`: show the raw timestamp (overrides the resolved date mode).
    raw_timestamp: bool,
    /// `-c`: git-annotate-compatible output format.
    annotate_compat: bool,
    /// `--color-lines` (git's `OUTPUT_COLOR_LINE`): color the metadata of every
    /// line after the first in a blame entry.
    color_lines: bool,
    /// `--color-by-age` (git's `OUTPUT_SHOW_AGE_WITH_COLOR`): color the metadata
    /// by the author date's bucket in the `color.blame.highlightRecent` table.
    color_by_age: bool,
    /// `--progress` / `--no-progress`; `None` is git's default (`isatty(2)`).
    show_progress: Option<bool>,
    /// `--incremental`: stream every entry as the walk finalizes it, uncoalesced,
    /// instead of printing the sorted-and-coalesced attribution at the end.
    incremental: bool,
    /// `-w`: ignore whitespace differences when diffing revisions.
    ignore_whitespace: bool,
    /// `--diff-algorithm=<algo>`; `None` falls back to `diff.algorithm`.
    diff_algorithm: Option<gix::diff::blob::Algorithm>,
    /// `--contents <file>`: use `<file>`'s contents (or stdin for `-`) as the
    /// final image, on top of the suspect commit (default HEAD).
    contents: Option<String>,
    /// `--ignore-rev <rev>`, in command-line order. `--no-ignore-rev` clears it,
    /// matching `OPT_STRING_LIST`'s negation.
    ignore_rev: Vec<String>,
    /// `blame.ignoreRevsFile` values followed by `--ignore-revs-file <file>`
    /// values, which git keeps in one list. `--no-ignore-revs-file` clears it,
    /// config-supplied entries included.
    ignore_revs_file: Vec<String>,
    /// `blame.markUnblamableLines`.
    mark_unblamable_lines: bool,
    /// `blame.markIgnoredLines`.
    mark_ignored_lines: bool,
    /// Raw `--date` value before repo-side validation; `None` if not given.
    date_arg: Option<String>,
    /// Resolved date mode for the human-format timestamp column, after applying
    /// blame.date and any `--date` override.
    date_mode: DateMode,
}

/// The `blame.*` config keys `git_blame_config` reads before `parse_options` runs,
/// each of which the command line can still override.
struct ConfigDefaults {
    /// `blame.showEmail`.
    show_email: bool,
    /// `blame.showRoot`.
    show_root: bool,
    /// `blame.blankBoundary`.
    blank_boundary: bool,
    /// `blame.ignoreRevsFile`, in file order; an empty value clears the list.
    ignore_revs_file: Vec<String>,
    /// `blame.markUnblamableLines`.
    mark_unblamable_lines: bool,
    /// `blame.markIgnoredLines`.
    mark_ignored_lines: bool,
}

impl Options {
    fn parse(args: &[String], defaults: ConfigDefaults) -> Result<Options> {
        let ConfigDefaults {
            show_email: show_email_default,
            show_root: show_root_default,
            blank_boundary: blank_boundary_default,
            ignore_revs_file: ignore_revs_file_default,
            mark_unblamable_lines,
            mark_ignored_lines,
        } = defaults;
        let mut ranges: Vec<RangeInclusive<u32>> = Vec::new();
        let mut long = false;
        let mut suppress = false;
        let mut show_email = show_email_default;
        let mut show_name = false;
        let mut show_number = false;
        let mut abbrev: Option<usize> = None;
        let mut porcelain = false;
        let mut line_porcelain = false;
        let mut blank_boundary = blank_boundary_default;
        let mut show_root = show_root_default;
        let mut raw_timestamp = false;
        let mut annotate_compat = false;
        let mut color_lines = false;
        let mut color_by_age = false;
        let mut show_progress: Option<bool> = None;
        let mut incremental = false;
        let mut ignore_whitespace = false;
        let mut diff_algorithm: Option<gix::diff::blob::Algorithm> = None;
        let mut contents: Option<String> = None;
        let mut ignore_rev: Vec<String> = Vec::new();
        let mut ignore_revs_file: Vec<String> = ignore_revs_file_default;
        // Raw `--date` value (last one wins); resolved against the repo in `blame`.
        let mut date_arg: Option<String> = None;
        // Positionals before the first `--`; `post` collects those after it.
        // `post.is_some()` means a `--` separator was seen.
        let mut pre: Vec<String> = Vec::new();
        let mut post: Option<Vec<String>> = None;

        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if let Some(paths) = post.as_mut() {
                paths.push(a.to_string());
                i += 1;
                continue;
            }
            match a {
                // The first `--` ends option parsing; everything after it is a
                // pathspec, including a further `--`.
                "--" => post = Some(Vec::new()),
                "-l" | "--long" => long = true,
                "-s" => suppress = true,
                "-e" | "--show-email" => show_email = true,
                "--no-show-email" => show_email = false,
                "-f" | "--show-name" => show_name = true,
                // git's `--[no-]show-name` clears OUTPUT_SHOW_NAME; auto-detection
                // in `find_alignment` still re-shows the column when a rename put a
                // differing source path on a line, exactly as git does.
                "--no-show-name" => show_name = false,
                "-n" | "--show-number" => show_number = true,
                "--no-show-number" => show_number = false,
                // `-b` blanks boundary object names; there is no `--no-b`.
                "-b" => blank_boundary = true,
                // `--root` stops treating root commits as boundaries.
                "--root" => show_root = true,
                "--no-root" => show_root = false,
                // `-t` forces the raw timestamp regardless of the date mode.
                "-t" => raw_timestamp = true,
                // `-c` selects git-annotate-compatible output.
                "-c" => annotate_compat = true,
                // git declares both color options with `OPT_BIT`, so the `--no-`
                // form clears the bit — and a cleared bit lets `blame.coloring`
                // apply again (verified against stock git: `-c blame.coloring=
                // repeatedLines blame --no-color-lines` is still colored).
                "--color-lines" => color_lines = true,
                "--no-color-lines" => color_lines = false,
                "--color-by-age" => color_by_age = true,
                "--no-color-by-age" => color_by_age = false,
                // git's `OPT_BOOL(0, "progress", &show_progress, …)` over a
                // tri-state seeded to -1: unset means "auto" (`isatty(2)`).
                "--progress" => show_progress = Some(true),
                "--no-progress" => show_progress = Some(false),
                // git's `OPT_BOOL(0, "incremental", &incremental, …)`.
                "--incremental" => incremental = true,
                "-w" => ignore_whitespace = true,
                // git's `--porcelain` and `--line-porcelain` are bit flags on one
                // field, so `--line-porcelain` wins no matter the order.
                "-p" | "--porcelain" => porcelain = true,
                // git's `--no-porcelain` clears only the OUTPUT_PORCELAIN bit,
                // leaving OUTPUT_LINE_PORCELAIN untouched; the output selector keys
                // off OUTPUT_PORCELAIN, so this drops back to the human format.
                "--no-porcelain" => porcelain = false,
                "--line-porcelain" => {
                    porcelain = true;
                    line_porcelain = true;
                }
                // `--line-porcelain`'s OPT_BIT value is OUTPUT_PORCELAIN |
                // OUTPUT_LINE_PORCELAIN, so its `--no-` form clears BOTH bits — even
                // after a bare `-p` (verified against stock git: `-p
                // --no-line-porcelain` yields the human format).
                "--no-line-porcelain" => {
                    porcelain = false;
                    line_porcelain = false;
                }
                "-L" => {
                    i += 1;
                    let spec = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `-L` requires a value"))?;
                    parse_line_range(spec, &mut ranges)?;
                }
                "--abbrev" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--abbrev` requires a value"))?;
                    abbrev = Some(v.parse().map_err(|_| anyhow!("invalid --abbrev value: {v}"))?);
                }
                // `--date <mode>` / `--date=<mode>` set the default date format for
                // the human-format timestamp column (validated against the repo in
                // `blame`, so the last one wins here and errors surface there).
                "--date" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--date` requires a value"))?;
                    date_arg = Some(v.clone());
                }
                _ if a.starts_with("--date=") => {
                    date_arg = Some(a["--date=".len()..].to_string());
                }
                "--diff-algorithm" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--diff-algorithm` requires a value"))?;
                    diff_algorithm = Some(parse_diff_algorithm(v)?);
                }
                _ if a.starts_with("--diff-algorithm=") => {
                    diff_algorithm = Some(parse_diff_algorithm(&a["--diff-algorithm=".len()..])?);
                }
                "--contents" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--contents` requires a value"))?;
                    contents = Some(v.clone());
                }
                _ if a.starts_with("--contents=") => {
                    contents = Some(a["--contents=".len()..].to_string());
                }
                "--no-contents" => contents = None,
                // git's OPT__ABBREV `--no-abbrev` sets abbrev to 0, which its
                // post-parse `else if (!abbrev) abbrev = hexsz` turns into the full
                // hash. `object_name_width` already treats `Some(0)` as "no
                // abbreviation", so `--no-abbrev` is exactly `--abbrev=0` (verified
                // identical to `-l` on stock git).
                "--no-abbrev" => abbrev = Some(0),
                // git declares both as `OPT_STRING_LIST`, so each occurrence appends
                // and the `--no-` form clears the whole list — including, for
                // `--ignore-revs-file`, the entries `blame.ignoreRevsFile` put there.
                "--ignore-rev" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--ignore-rev` requires a value"))?;
                    ignore_rev.push(v.clone());
                }
                _ if a.starts_with("--ignore-rev=") => {
                    ignore_rev.push(a["--ignore-rev=".len()..].to_string());
                }
                "--no-ignore-rev" => ignore_rev.clear(),
                "--ignore-revs-file" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--ignore-revs-file` requires a value"))?;
                    ignore_revs_file.push(v.clone());
                }
                _ if a.starts_with("--ignore-revs-file=") => {
                    ignore_revs_file.push(a["--ignore-revs-file=".len()..].to_string());
                }
                "--no-ignore-revs-file" => ignore_revs_file.clear(),
                // The `--no-` forms of options whose positive form needs substrate
                // this port does not have (`--incremental`, `--show-stats`,
                // `--score-debug`). Each positive default is off, so the negated
                // form requests exactly the behavior this port already produces;
                // stock git emits byte-identical stdout for them (verified), so they
                // are accepted as no-ops rather than rejected. The positive forms
                // remain refused below.
                "--no-incremental" => incremental = false,
                "--no-show-stats" | "--no-score-debug" => {}
                _ if a.starts_with("-L") => parse_line_range(&a[2..], &mut ranges)?,
                _ if a.starts_with("--abbrev=") => {
                    let v = &a["--abbrev=".len()..];
                    abbrev = Some(v.parse().map_err(|_| anyhow!("invalid --abbrev value: {v}"))?);
                }
                _ if a.starts_with('-') && a.len() > 1 => {
                    bail!("unsupported option: {a}")
                }
                _ => pre.push(a.to_string()),
            }
            i += 1;
        }

        // The revision/path split and its validation happen against the repo in
        // `resolve_targets`, since git's DWIM (`is_a_rev`) needs the object db.
        Ok(Options {
            rev: None,
            file: String::new(),
            suspect_id: None,
            pre,
            post,
            ranges,
            long,
            suppress,
            show_email,
            show_name,
            show_number,
            abbrev,
            porcelain,
            line_porcelain,
            blank_boundary,
            show_root,
            raw_timestamp,
            annotate_compat,
            color_lines,
            color_by_age,
            show_progress,
            incremental,
            ignore_whitespace,
            diff_algorithm,
            contents,
            ignore_rev,
            ignore_revs_file,
            mark_unblamable_lines,
            mark_ignored_lines,
            date_arg,
            // Overwritten in `blame` once blame.date / `--date` are resolved.
            date_mode: DateMode::Iso8601,
        })
    }
}

/// git's `diff-algorithm` value parser: the names git accepts for `-A/--diff-algorithm`.
/// `patience` is a valid git algorithm the vendored `gix-diff` does not implement, so
/// it is reported as such rather than silently substituted.
fn parse_diff_algorithm(name: &str) -> Result<gix::diff::blob::Algorithm> {
    use gix::diff::blob::Algorithm;
    if name.eq_ignore_ascii_case("myers") || name.eq_ignore_ascii_case("default") {
        Ok(Algorithm::Myers)
    } else if name.eq_ignore_ascii_case("minimal") {
        Ok(Algorithm::MyersMinimal)
    } else if name.eq_ignore_ascii_case("histogram") {
        Ok(Algorithm::Histogram)
    } else if name.eq_ignore_ascii_case("patience") {
        bail!("diff algorithm 'patience' is not implemented")
    } else {
        bail!(
            "option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\""
        )
    }
}

/// Parse one `-L` spec into a 1-based inclusive range. Only numeric forms are
/// supported; regex (`/re/`) and function (`:name`) forms are rejected.
fn parse_line_range(spec: &str, ranges: &mut Vec<RangeInclusive<u32>>) -> Result<()> {
    if spec.starts_with('/') || spec.starts_with(':') {
        bail!("unsupported -L form: only numeric ranges are supported");
    }
    let (start_part, end_part) = match spec.split_once(',') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };

    let start: u32 = if start_part.is_empty() {
        1
    } else {
        start_part
            .parse()
            .map_err(|_| anyhow!("invalid -L range: {spec}"))?
    };
    if start == 0 {
        bail!("invalid -L range: line numbers are 1-based");
    }

    let end: u32 = match end_part {
        None => u32::MAX,
        Some("") => u32::MAX,
        Some(e) if e.starts_with('+') => {
            let count: u32 = e[1..]
                .parse()
                .map_err(|_| anyhow!("invalid -L range: {spec}"))?;
            start.saturating_add(count.saturating_sub(1))
        }
        Some(e) if e.starts_with('-') => {
            bail!("unsupported -L form: relative end offsets are not supported")
        }
        Some(e) => e.parse().map_err(|_| anyhow!("invalid -L range: {spec}"))?,
    };

    ranges.push(start..=end.max(start));
    Ok(())
}

use crate::abbrev::configured_abbrev;

/// Number of decimal digits needed to print `n` (at least 1).
fn decimal_width(n: u32) -> usize {
    n.to_string().len()
}

/// Append `n` spaces to `buf`.
fn pad(buf: &mut Vec<u8>, n: usize) {
    buf.resize(buf.len() + n, b' ');
}

/// Turn a CWD-relative user path into a repo-root-relative path, so blame works
/// from any subdirectory of the worktree (git resolves pathspecs the same way).
fn repo_relative_path(repo: &gix::Repository, user_path: &str) -> Result<String> {
    let joined = match repo.workdir() {
        Some(workdir) => {
            let cwd = std::env::current_dir()?;
            let workdir_abs = workdir.canonicalize().unwrap_or_else(|_| workdir.to_path_buf());
            let cwd_abs = cwd.canonicalize().unwrap_or(cwd);
            match cwd_abs.strip_prefix(&workdir_abs) {
                Ok(prefix) => prefix.join(user_path),
                Err(_) => PathBuf::from(user_path),
            }
        }
        None => PathBuf::from(user_path),
    };

    let s = joined
        .to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {user_path}"))?;
    Ok(s.strip_prefix("./").unwrap_or(s).to_string())
}
