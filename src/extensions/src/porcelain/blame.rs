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

/// `BLAME_DEFAULT_MOVE_SCORE` (`blame.h:13`), the score a bare `-M` installs.
pub(super) const BLAME_DEFAULT_MOVE_SCORE: u32 = 20;

/// `BLAME_DEFAULT_COPY_SCORE` (`blame.h:14`), the score a bare `-C` installs.
pub(super) const BLAME_DEFAULT_COPY_SCORE: u32 = 40;

/// git's `parse_score()`: `strtoul` over the whole argument, where anything it cannot consume
/// entirely yields 0.
///
/// A 0 is returned as `None` because git only overrides `sb->move_score` / `sb->copy_score`
/// `if (blame_move_score)` / `if (blame_copy_score)`, leaving the default in place otherwise.
fn parse_score(arg: &str) -> Option<u32> {
    arg.parse::<u32>().ok().filter(|&score| score != 0)
}

/// Byte-for-byte reproduction of `git blame`'s usage text, everything after the
/// `usage:` line — which [`print_usage`] writes first, naming the command as it
/// was invoked.
///
/// `cmd_annotate` builds its argv with `argv[0] == "annotate"`, and
/// `parse_options` renders `blame_opt_usage` through that name, so
/// `git annotate -h` differs from `git blame -h` in exactly that one line
/// (verified against git 2.55.0 with `diff <(git blame -h) <(git annotate -h)`).
/// `cmd_blame()`'s `struct option options[]` (builtin/blame.c), in table order,
/// as [`super::resolve_long`] reads it. `--diff-algorithm` is the only
/// `PARSE_OPT_NONEG` entry. `-b`, `-c`, `-t`, `-l`, `-s`, `-w`, `-S`, `-C`, `-M`
/// and `-L` are short-only and so have no entry, and the revision options
/// (`--date=`, `--reverse`, `--first-parent`, …) are not in this table at all:
/// git reaches them through `parse_revision_opt()` after `parse_options_step()`
/// returns `PARSE_OPT_UNKNOWN`, which is why they are never abbreviated.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "incremental", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "root", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "show-stats", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "progress", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "score-debug", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "show-name", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "show-number", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "porcelain", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "line-porcelain", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "show-email", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "diff-algorithm", neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "ignore-rev", neg: true, arg: super::Arg::Required },
    super::LongOpt { name: "ignore-revs-file", neg: true, arg: super::Arg::Required },
    super::LongOpt { name: "color-lines", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "color-by-age", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "minimal", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "contents", neg: true, arg: super::Arg::Required },
    super::LongOpt { name: "abbrev", neg: true, arg: super::Arg::Optional },
];

const USAGE_BODY: &str = concat!(
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

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE_BODY`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]minimal`.
/// Captured byte-for-byte from stock git 2.55.0's `git blame --help-all`, less
/// its synopsis line — which [`print_usage`] builds per command name, exactly
/// as `builtin/blame.c` does for `blame`, `annotate` and `pickaxe`.
const USAGE_BODY_ALL: &str = r#"
    <rev-opts> are documented in git-rev-list(1)

    --[no-]incremental    show blame entries as we find them, incrementally
    -b                    do not show object names of boundary commits (Default: off)
    --[no-]root           do not treat root commits as boundaries (Default: off)
    --[no-]show-stats     show work cost statistics
    --[no-]progress       force progress reporting
    --[no-]score-debug    show output score for blame entries
    -f, --[no-]show-name  show original filename (Default: auto)
    -n, --[no-]show-number
                          show original linenumber (Default: off)
    -p, --[no-]porcelain  show in a format designed for machine consumption
    --[no-]line-porcelain show porcelain format with per-line commit information
    -c                    use the same output mode as git-annotate (Default: off)
    -t                    show raw timestamp (Default: off)
    -l                    show long commit SHA1 (Default: off)
    -s                    suppress author name and timestamp (Default: off)
    -e, --[no-]show-email show author email instead of name (Default: off)
    -w                    ignore whitespace differences
    --diff-algorithm <algorithm>
                          choose a diff algorithm
    --[no-]ignore-rev <rev>
                          ignore <rev> when blaming
    --[no-]ignore-revs-file <file>
                          ignore revisions from <file>
    --[no-]color-lines    color redundant metadata from previous line differently
    --[no-]color-by-age   color lines by age
    --[no-]minimal        spend extra cycles to find a better match
    -S <file>             use revisions from <file> instead of calling git-rev-list
    --[no-]contents <file>
                          use <file>'s contents as the final image
    -C[<score>]           find line copies within and across files
    -M[<score>]           find line movements within and across files
    -L <range>            process only line range <start>,<end> or function :<funcname>
    --[no-]abbrev[=<n>]   use <n> digits to display object names

"#;

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

// git's `approxidate()`, which `parse_color_fields` runs over each heat threshold
// (`builtin/blame.c:421`) — the same shared parser every `--since`/`--until` goes through.
use crate::date::approxidate;

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
/// `--contents <file>` (and `--contents -` from stdin), `--diff-algorithm`
/// (every name git accepts, `patience` included) and its `--patience` spelling,
/// `-w`, `--date=relative`, `--color-lines`, `--color-by-age` and
/// `--progress`/`--no-progress`.
///
/// `-M[<score>]` is a port of `pass_blame()`'s `PICKAXE_BLAME_MOVE` block: once the
/// ordinary diff has handed each parent what it can, every entry still held by the
/// suspect is diffed against each parent's *whole* blob (`find_copy_in_blob()`), so
/// lines that were moved around inside the file are credited to where they came from.
/// The optional score is git's `sb->move_score` (20 by default) and, via
/// `blame_entry_score()`, keeps trivial lines from being credited to a chance match.
///
/// `-C[<score>]` is the `PICKAXE_BLAME_COPY` block, i.e. `find_copy_in_parent()`. Where
/// `-M` only re-offers a chunk to the blob the same file has in a parent, `-C` offers it
/// to *other* files there as well: a bare `-C` looks at the paths the commit changed or
/// removed, `-C -C` also at the files it left alone while the blamed path is new to the
/// parent, and `-C -C -C` at every file in every parent. Whichever file yields the least
/// trivial match becomes the chunk's *Source File*, so one commit can be the suspect for
/// chunks from several of its files at once — which is why `gix-blame`'s suspect is now a
/// `(commit, path)` origin, as git's `blame_origin` is, and why `assign_blame()`'s
/// one-origin-per-round loop is reproduced here. Any `-C` also turns `-M` on, as
/// `blame_copy_callback()` does. `sb->copy_score` defaults to 40.
///
/// The score of `-M`/`-C` is an *attached* argument that `parse_score()` reads with
/// `strtoul` over the whole of it, so `-CC` is a single `-C` whose score `"C"` reads as 0,
/// and a 0 — from `-C0` or from an unparsable score — leaves the default in place. This
/// was verified against git 2.55.0 rather than assumed: `git blame -C -C` finds a copy
/// from an untouched file where `git blame -CC` does not.
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
/// clear their `OPT_STRING_LIST`s, config-supplied entries included.
/// `--no-show-stats` clears its `OPT_BOOL` and `--no-score-debug` clears the
/// `OUTPUT_SHOW_SCORE` bit its `OPT_BIT` sets.
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
/// A long option no git parser knows — neither `options[]` nor the revision and
/// diff tables `parse_revision_opt()` forwards to, enumerated in
/// [`GIT_BLAME_LONG_OPTIONS`] — is answered with git's own
/// `error: unknown option` and usage on stderr, exit 129, including git's quirk
/// of losing the option's name to `overwrite_argv()` once an earlier option has
/// consumed an argv slot (see [`unknown_option_name`]).
///
/// Flags git *does* know but this port has not implemented are rejected with a
/// terse message rather than emitting wrong output, each for a concrete reason:
///   * `-S <revs-file>` — installs commit grafts that rewrite the ancestry the
///     walk follows.
///   * `--no-indent-heuristic` — the blame diffs run through
///     `gix_diff::blob::compact::change_compact`, git's `xdl_change_compact()`
///     with `XDF_INDENT_HEURISTIC` applied unconditionally, so the heuristic
///     cannot be switched off. The positive `--indent-heuristic` *is* accepted:
///     `diff.c:57` seeds `diff_indent_heuristic = 1`, so it asks for the state
///     the engine is already in.
///   * `--date=human` and the `-local` date variants.
///
/// `--first-parent` and `--minimal` are implemented: the first truncates every
/// commit's parent list to one entry inside `gix-blame`, which is what git's
/// `first_scapegoat()` does under `revs->first_parent_only`, and the second
/// selects the same `XDF_NEED_MINIMAL` diff `--diff-algorithm=minimal` selects.
///
/// `--reverse <range>`, `--show-stats` and `--score-debug` are implemented too,
/// and all three needed the same thing: `gix-blame` had to grow the parts of
/// git's `blame_origin` it did not model.
///   * `--reverse` inverts the direction of the whole algorithm. git builds a
///     `revs->children` decoration during `prepare_revision_walk()`,
///     `first_scapegoat()` returns *children* instead of parents and the commit
///     queue is ordered oldest-first, so a line is attributed to the last commit
///     that still had it. [`reverse_children`] builds that decoration over the
///     range the arguments name and hands it to `gix-blame` as
///     `Options::children`.
///   * `--show-stats` prints `blame_scoreboard`'s three counters, and `num read
///     blob` is the awkward one: it counts `fill_origin_blob()` calls that found
///     `blame_origin::file` empty, i.e. the *misses* of a per-origin blob cache
///     whose lifetime is the refcount graph. `gix-blame` now keeps that cache
///     (`OriginFiles`), so `pass_whole_blame()` hands the buffer over instead of
///     the parent re-reading it and `find_copy_in_parent()` drops a candidate's
///     blob again unless something kept the origin alive. The one origin outside
///     the walk is `fake_working_tree_commit()`, which is this file's overlay;
///     it is described to `gix-blame` as `Options::fake_commit`, and it is why a
///     working-tree blame reads one blob fewer than the same blame at `HEAD`
///     (verified against stock git: 2 versus 3 on the same file).
///   * `--score-debug` adds `blame_entry_score()` — one plus the entry's
///     alphanumeric bytes — and beside it `ent->suspect->refcnt`, read from that
///     same graph at output time. `Outcome::suspect_refcounts` reports it: an
///     origin is referenced once per coalesced entry naming it and once per live
///     origin whose `blame_origin::previous` points at it.
///
/// `-L` accepts every form `line-range.c` does: `<n>`, `<n>,<m>`, `<n>,+<m>`,
/// `<n>,-<m>`, the empty endpoints of `-L,<m>` and `-L<n>,`, `/<regex>/`,
/// `^/<regex>/` and `:<funcname>`, resolved against the final image after the
/// path is known, with multiple `-L`s threading git's anchor from one to the next.
/// The regexes are `regex::bytes` built with `multi_line` and without
/// `dot_matches_new_line`, which is `regcomp(…, REG_NEWLINE)`.
pub fn blame(args: &[String]) -> Result<ExitCode> {
    blame_with(args, "blame")
}

/// The single implementation behind `git blame`, `git annotate` and `git pickaxe`.
///
/// All three are `cmd_blame()` in git — `pickaxe` is the same command-table entry
/// under its pre-2006 name, and `cmd_annotate()` (`builtin/annotate.c`) is six
/// lines that splice `-c` in front of the user's argv and call `cmd_blame()`.
/// Keeping one body here is what keeps the three from drifting: every option,
/// every error-precedence rule and every output format is shared by construction
/// rather than by three parallel ports agreeing.
///
/// `cmd` is `argv[0]` as `parse_options` sees it, which only affects the `usage:`
/// line.
pub(super) fn blame_with(args: &[String], cmd: &str) -> Result<ExitCode> {
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

    let mut opts = match Options::parse(
        &repo,
        args,
        ConfigDefaults {
            show_email: show_email_default,
            show_root: show_root_default,
            blank_boundary: blank_boundary_default,
            ignore_revs_file: ignore_revs_file_default,
            mark_unblamable_lines,
            mark_ignored_lines,
        },
    )? {
        // `parse_options()` answers `-h` the moment it reaches it, before it looks
        // at anything that follows, so this returns from inside the parse loop.
        ParseOutcome::Help => return print_usage(cmd, true),
        ParseOutcome::HelpAll => return print_usage_all(cmd),
        // `parse_revision_opt()` prints the diagnostic and then
        // `usage_with_options()`, both on stderr, and exits 129.
        ParseOutcome::Unknown(name) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "error: unknown option `{name}'")?;
            err.flush()?;
            return print_usage(cmd, false);
        }
        // A rejected option *value*: only the callback's own `error()` line is
        // written, then `exit(129)`. No usage block, unlike the unknown-option
        // path above.
        ParseOutcome::OptError(msg) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "error: {msg}")?;
            err.flush()?;
            return Ok(ExitCode::from(129));
        }
        // `parse_long_opt()` reports the ambiguity with `error()` on stderr and
        // then returns `PARSE_OPT_HELP`, so the block that follows it lands on
        // *stdout* — the one rejection that splits its two halves across the two
        // streams.
        ParseOutcome::Ambiguous(body, first, second) => {
            let mut err = std::io::stderr().lock();
            writeln!(
                err,
                "error: ambiguous option: {body} (could be --{first} or --{second})"
            )?;
            err.flush()?;
            return print_usage(cmd, true);
        }
        // The parser has already written everything it had to say.
        ParseOutcome::Reported(code) => return Ok(code),
        ParseOutcome::Opts(opts) => *opts,
    };

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
        Targets::Usage => return print_usage(cmd, false),
        Targets::Fatal(code) => return Ok(code),
        Targets::Resolved => {}
    }

    // `setup_scoreboard()`'s first check (`blame.c:2778`), before anything else it does.
    if opts.reverse && opts.contents.is_some() {
        let mut err = std::io::stderr().lock();
        writeln!(err, "fatal: --contents and --reverse do not blend well.")?;
        err.flush()?;
        return Ok(ExitCode::from(128));
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
    //
    // Both checks demand a *blob*: `verify_working_tree_path` requires
    // `oid_object_info(...) == OBJ_BLOB` and `fill_blob_sha1_and_mode` requires
    // `odb_read_object_info(...) == OBJ_BLOB`, so naming a directory is "no such
    // path" rather than an attempt to blame a tree (verified: stock
    // `git blame src` prints `fatal: no such path 'src' in HEAD`, exit 128).
    // A `--reverse` blame always names its initial commit, so `setup_scoreboard()` never has to
    // invent a commit for the final image: `sb->final` is set and `sb->contents_from` is refused
    // above.
    let overlay = opts.contents.is_some() || (opts.rev.is_none() && !opts.reverse);
    let path_in_suspect = blob_at(&repo, &suspect, &rel_path).is_some();
    let mut index_only = false;
    if !path_in_suspect && overlay {
        let mut err = std::io::stderr().lock();
        if !path_in_index(&repo, &rel_path) {
            writeln!(err, "fatal: no such path '{rel_path}' in HEAD")?;
            err.flush()?;
            return Ok(ExitCode::from(128));
        }
        // The path is only in the index, so there is nothing to blame it against:
        // every line belongs to the synthetic commit holding the final image.
        index_only = true;
    }

    // `fake_working_tree_commit()`'s stat block (`blame.c:237-268`), the statements
    // after `verify_working_tree_path()` above:
    //
    // ```c
    // if (!contents_from || strcmp("-", contents_from)) {
    //         struct stat st;
    //         const char *read_from;
    //
    //         if (contents_from) {
    //                 if (stat(contents_from, &st) < 0)
    //                         die_errno("Cannot stat '%s'", contents_from);
    //                 read_from = contents_from;
    //         }
    //         else {
    //                 if (lstat(path, &st) < 0)
    //                         die_errno("Cannot lstat '%s'", path);
    //                 read_from = path;
    //         }
    //         …
    //         switch (st.st_mode & S_IFMT) {
    //         case S_IFREG: … case S_IFLNK: …
    //         default:
    //                 die("unsupported file type %s", read_from);
    //         }
    // }
    // ```
    //
    // It runs on the overlay path only — `setup_scoreboard()` builds the fake commit
    // nowhere else — and is skipped entirely for `--contents -`, which reads standard
    // input and never touches the filesystem. `path` is the prefix-joined,
    // repo-root-relative name `add_prefix()` built and `setup_work_tree()` has already
    // chdir'd for, which is `rel_path` here.
    //
    // Reading the file with `.ok()` and falling back to the suspect's blob — which is
    // what this did — turns a deleted or unreadable working-tree file into a silent
    // blame of `HEAD`, where git refuses outright.
    if overlay && opts.contents.as_deref() != Some("-") {
        // `stat()` for `--contents <file>` and `lstat()` for the working-tree copy:
        // a dangling symlink in the worktree is a file blame reads (`S_IFLNK` below
        // takes the link target as the image), not a missing one.
        let (read_from, verb, stat) = match opts.contents.as_deref() {
            Some(from) => (from.to_string(), "stat", std::fs::metadata(from)),
            None => {
                let path = repo.workdir().map(|w| w.join(&rel_path));
                let stat = match &path {
                    Some(p) => std::fs::symlink_metadata(p),
                    None => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
                };
                (rel_path.clone(), "lstat", stat)
            }
        };
        let mut err = std::io::stderr().lock();
        match stat {
            Err(e) => {
                writeln!(err, "fatal: Cannot {verb} '{read_from}': {}", errno_text(&e))?;
                err.flush()?;
                return Ok(ExitCode::from(128));
            }
            // The `S_IFMT` switch: only a regular file and a symlink have an image
            // to read, and everything else — a directory, a fifo, a device — is
            // refused by name and without quotes.
            Ok(st) if !st.is_file() && !st.file_type().is_symlink() => {
                writeln!(err, "fatal: unsupported file type {read_from}")?;
                err.flush()?;
                return Ok(ExitCode::from(128));
            }
            Ok(_) => {}
        }
    }

    // `prepare_revision_walk()`'s `limit_list()` (`revision.c:1448-1452`), which sits
    // between the fake working-tree commit above and the final blob read below:
    //
    // ```c
    // if (revs->ancestry_path_implicit_bottoms) {
    //         collect_bottom_commits(original_list, &revs->ancestry_path_bottoms);
    //         if (!revs->ancestry_path_bottoms)
    //                 die("--ancestry-path given but there are no bottom commits");
    // }
    // ```
    if opts.ancestry_path_pending {
        let mut err = std::io::stderr().lock();
        writeln!(err, "fatal: --ancestry-path given but there are no bottom commits")?;
        err.flush()?;
        return Ok(ExitCode::from(128));
    }

    // `setup_scoreboard()`'s `fill_blob_sha1_and_mode()` failure, which is the *other*
    // branch of the `if (sb->contents_from || !sb->final)` above: with a positive
    // revision and no overlay there is no working tree to fall back on, and the
    // diagnostic names the revision as the user typed it.
    if !path_in_suspect && !overlay {
        let mut err = std::io::stderr().lock();
        let rev = opts.rev.as_deref().unwrap_or("HEAD");
        writeln!(err, "fatal: no such path {rel_path} in {rev}")?;
        err.flush()?;
        return Ok(ExitCode::from(128));
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
        // `case S_IFLNK: strbuf_readlink(&buf, read_from, st.st_size)` — the image of
        // a symlink is the *link target text*, which is also what the index and the
        // tree hold for mode 120000. Reading through the link instead blames the
        // pointed-at file's contents against the symlink's blob.
        repo.workdir().map(|w| w.join(&rel_path)).and_then(|p| {
            match std::fs::symlink_metadata(&p) {
                Ok(st) if st.file_type().is_symlink() => {
                    std::fs::read_link(&p).ok().map(|t| t.into_os_string().into_encoded_bytes())
                }
                _ => std::fs::read(p).ok(),
            }
        })
    } else {
        None
    };

    // git's merge parents: `fake_working_tree_commit` gives the synthetic commit
    // holding the final image `HEAD` *and* every id in `MERGE_HEAD` as parents
    // (`blame.c:212-213`), so mid-merge a working-tree line that came from the
    // other side of the merge is blamed on that side rather than on nobody.
    // `--first-parent` drops them again in `first_scapegoat()`.
    //
    // The list is only consulted on the fake-commit path — `setup_scoreboard`
    // builds that commit only under `--contents` or with no positive rev
    // (`blame.c:2795`), which is exactly `overlay`.
    let merge_parents: Vec<ObjectId> = if overlay && !opts.first_parent {
        read_merge_heads(&repo)
    } else {
        Vec::new()
    };

    // git resolves `-L` against `sb->final_buf` — the final image — right after
    // `setup_scoreboard()` (`builtin/blame.c:1197-1223`), so the line numbers a
    // regex or `:funcname` spec resolves to are the ones the output will print,
    // and an out-of-range start is fatal (128) rather than an empty answer.
    let final_image: Vec<u8> = match &worktree_content {
        Some(content) => content.clone(),
        None => blob_at(&repo, &suspect, &rel_path)
            .and_then(|id| repo.find_object(id).ok())
            .map(|o| o.detach().data)
            .unwrap_or_default(),
    };
    match resolve_line_specs(&opts.line_specs, &final_image, &rel_path) {
        Ok(ranges) => opts.ranges = ranges,
        Err(LineSpecError::Usage) => return print_usage(cmd, false),
        Err(LineSpecError::Fatal(msg)) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "fatal: {msg}")?;
            err.flush()?;
            return Ok(ExitCode::from(128));
        }
    }

    // Blame the full file; `-L` is applied to the result so that the working-tree
    // overlay can be built in working-tree line coordinates, as git does.
    let ranges = if opts.ranges.is_empty() || worktree_content.is_some() {
        gix::blame::BlameRanges::default()
    } else {
        gix::blame::BlameRanges::from_one_based_inclusive_ranges(opts.ranges.clone())
            .map_err(|e| anyhow!("{e}"))?
    };
    // `revs->children` for a `--reverse` walk, built over the range the arguments named.
    let children = match (opts.reverse, opts.reverse_from) {
        (true, Some(from)) if opts.first_parent => {
            let latest = opts.reverse_tips[0];
            match reverse_first_parent_children(&repo, from, latest)? {
                Some(children) => Some(children),
                None => {
                    let mut err = std::io::stderr().lock();
                    writeln!(
                        err,
                        "fatal: --reverse --first-parent together require range along first-parent chain"
                    )?;
                    err.flush()?;
                    return Ok(ExitCode::from(128));
                }
            }
        }
        (true, Some(from)) => Some(reverse_children(&repo, from, &opts.reverse_tips)?),
        _ => None,
    };

    // git's `fake_working_tree_commit()`, which only exists on the overlay path. Both halves of
    // what `gix-blame` needs to account for it follow from the same comparison the overlay itself
    // is built from: whether the final image is `suspect`'s blob, and how many runs of lines no
    // scapegoat can claim. Under an in-progress merge the remaining scapegoats are `MERGE_HEAD`s,
    // whose blames are separate walks below and so keep their own origin caches; the counters
    // then add up rather than sharing one, as they would in git's single scoreboard.
    let fake_commit = match &worktree_content {
        Some(content) => {
            let scapegoat_blobs: Vec<Vec<u8>> = std::iter::once(suspect)
                .filter_map(|id| blob_at(&repo, &id, &rel_path))
                .filter_map(|blob| repo.find_object(blob).ok())
                .map(|o| o.detach().data)
                .collect();
            Some(gix::blame::FakeCommit {
                passes_whole_blame: scapegoat_blobs.first().is_some_and(|blob| blob == content),
            })
        }
        None => None,
    };

    let blame_options = gix::repository::blame_file::Options {
        diff_algorithm: opts.diff_algorithm,
        ranges,
        since: None,
        bottom: opts.bottom.clone(),
        rewrites: Some(gix::diff::Rewrites::default()),
        ignore_whitespace: opts.ignore_whitespace,
        detect_moved: opts.detect_moved,
        ignore_revs: ignore_revs.clone(),
        detect_copied: opts.detect_copied,
        first_parent: opts.first_parent,
        children,
        fake_commit,
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
    //
    // Neither are `--show-stats` and `--score-debug`: both report on the walk itself — the work it
    // did, and the `blame_origin` refcounts it ended up with — and a cache hit is precisely the
    // case where no walk happened. `--reverse` is a different traversal over a range the key does
    // not name, so it is kept out too.
    //
    // A *forward* range is kept out for exactly the same reason as `--reverse`: the key names the
    // commit dug from and not the range's bottom, so `git blame A..B -- f` and `git blame B -- f`
    // would share one entry while producing different attributions — and the cache lives in
    // `~/.zvcs/cache` keyed by commit id alone, so the collision is not even confined to the
    // repository that caused it.
    let algo_key = format!(
        "{:?}|w={}|M={:?}|C={:?}|1p={}",
        opts.diff_algorithm,
        opts.ignore_whitespace,
        opts.detect_moved,
        opts.detect_copied,
        opts.first_parent
    );
    let cache_key = (opts.ranges.is_empty()
        && ignore_revs.is_empty()
        && !index_only
        && !opts.incremental
        && !opts.show_stats
        && !opts.score_debug
        && !opts.reverse
        && opts.bottom.is_empty())
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
    // The `blame_scoreboard` counters `--show-stats` reports, accumulated over every walk this
    // blame runs — one, except mid-merge where each `MERGE_HEAD` scapegoat of the synthetic
    // working-tree commit is its own.
    let mut stats = gix::blame::Statistics::default();
    // `blame_origin::previous`, which the porcelain format reports and which walking forwards is a
    // *child*, so it cannot be rediscovered from a commit's parents the way [`find_previous`]
    // does it.
    let mut previous_origins: PreviousOrigins = std::collections::BTreeMap::new();
    // `(lines, blob content)` — the overlay path needs the blamed blob's bytes,
    // which come from the outcome on a miss and from the object on a hit.
    let (mut lines, blamed_bytes) = match cached {
        // Nothing to blame against: the file exists only in the index, so the empty
        // `HEAD` image below leaves every line with the synthetic commit. No walk runs, but
        // git's does not either — `setup_scoreboard()` reads the final image, and the fake
        // commit's `pass_blame()` reaches `sb->num_commits++` with every `sg_origin[]` left
        // NULL, since none of its scapegoats has the path to hand back.
        _ if index_only => {
            stats.num_read_blob = 1;
            stats.num_commits = 1;
            (Vec::new(), Vec::new())
        }
        Some((lines, bytes)) => (lines, bytes),
        None => {
            let outcome = repo
                .blame_file(rel_path.as_bytes().as_bstr(), suspect, blame_options.clone())
                .map_err(|e| anyhow!("{e}"))?;
            let lines = materialize_lines(&outcome);
            stats = outcome.statistics;
            collect_previous_origins(&mut previous_origins, &outcome);
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
        // The scapegoats of the synthetic commit, in git's order: the suspect
        // first, then each `MERGE_HEAD`. `pass_blame()` diffs against them in
        // turn and only ever hands a parent the lines no earlier parent took, so
        // a line present in both sides of the merge is credited to `HEAD`'s side,
        // exactly as here.
        let mut sources: Vec<(Vec<Line>, Vec<u8>)> = vec![(lines, blamed_bytes.clone())];
        for parent in &merge_parents {
            let Some(blob) = blob_at(&repo, parent, &rel_path) else {
                // `verify_working_tree_path` tolerates a parent without the path;
                // it simply has nothing to hand back.
                continue;
            };
            let Ok(object) = repo.find_object(blob) else {
                continue;
            };
            let bytes = object.detach().data;
            let outcome = repo
                .blame_file(rel_path.as_bytes().as_bstr(), *parent, blame_options.clone())
                .map_err(|e| anyhow!("{e}"))?;
            add_statistics(&mut stats, &outcome.statistics);
            collect_previous_origins(&mut previous_origins, &outcome);
            sources.push((materialize_lines(&outcome), bytes));
        }
        lines = overlay_worktree(
            &repo,
            &sources,
            content,
            opts.diff_algorithm,
            opts.ignore_whitespace,
        )?;
        if !opts.ranges.is_empty() {
            let keep = |n: u32| opts.ranges.iter().any(|r| r.contains(&n));
            lines.retain(|l| keep(l.final_no));
        }
    }

    // git's `stop_progress()`, which sits between `assign_blame` and `output`.
    finish_progress(show_progress, progress_started, lines.len())?;

    if lines.is_empty() {
        // `output()` has nothing to print, but `cmd_blame` still reaches the counters
        // (`builtin/blame.c:1293`) unless `--incremental` sent it to cleanup first.
        if opts.show_stats && !opts.incremental {
            print_stats(&stats)?;
        }
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
                opts.ignore_whitespace,
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

    // `ent->suspect->refcnt` as the output loop reads it: one reference per blame entry in
    // `sb->ent`, which is this line list grouped the way `blame_coalesce()` groups it, plus the
    // `blame_origin::previous` of every origin that survived. The counting happens here rather
    // than inside the walk because the entries the walk produced are not the entries git prints:
    // a working-tree overlay replaces some of them and `-L` drops others, and each of those is a
    // reference gained or lost.
    let refcounts = if opts.score_debug {
        let mut entry_counts: std::collections::BTreeMap<(ObjectId, Option<Vec<u8>>), u32> =
            std::collections::BTreeMap::new();
        for group in group_lines(&lines) {
            let entry = &lines[group.start];
            *entry_counts
                .entry((entry.commit_id, entry.source_name.clone()))
                .or_default() += 1;
        }
        // The fake working-tree commit's own edge: it only reaches `origin->previous` when it had
        // to diff against the suspect rather than hand the whole blame down.
        let mut previous = previous_origins.clone();
        if !index_only && blame_options.fake_commit.is_some_and(|fake| !fake.passes_whole_blame) {
            previous.insert((null_id, None), (suspect, None));
        }
        suspect_refcounts(&entry_counts, &previous)
    } else {
        std::collections::BTreeMap::new()
    };

    let code = if opts.porcelain {
        // The synthetic commit only records a `previous` origin when it actually
        // handed lines to one, which never happens when the path is index-only.
        let head_id = if index_only { None } else { head_id };
        emit_porcelain(&repo, &lines, &info, &rel_path, head_id, &null_id, &opts, &previous_origins)?
    } else {
        emit_human(&repo, &lines, &info, &rel_path, &opts, &colors, &refcounts)?
    };

    if opts.show_stats {
        print_stats(&stats)?;
    }

    Ok(code)
}

/// `--show-stats`, which is `cmd_blame`'s last act before cleanup
/// (`builtin/blame.c:1293-1297`): the counters follow the blame on stdout, in every format
/// `--incremental` did not skip past.
fn print_stats(stats: &gix::blame::Statistics) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "num read blob: {}", stats.num_read_blob)?;
    writeln!(out, "num get patch: {}", stats.num_get_patch)?;
    writeln!(out, "num commits: {}", stats.num_commits)?;
    out.flush()?;
    Ok(())
}

/// Add one walk's counters onto the running total — see the `stats` binding in [`blame_with`].
fn add_statistics(total: &mut gix::blame::Statistics, one: &gix::blame::Statistics) {
    total.commits_traversed += one.commits_traversed;
    total.trees_decoded += one.trees_decoded;
    total.trees_diffed += one.trees_diffed;
    total.trees_diffed_with_rewrites += one.trees_diffed_with_rewrites;
    total.blobs_diffed += one.blobs_diffed;
    total.num_read_blob += one.num_read_blob;
    total.num_get_patch += one.num_get_patch;
    total.num_commits += one.num_commits;
}

/// The origin each origin points at through `blame_origin::previous`, keyed the way the output
/// lines name one.
type PreviousOrigins =
    std::collections::BTreeMap<(ObjectId, Option<Vec<u8>>), (ObjectId, Option<Vec<u8>>)>;

/// Re-key one walk's `blame_origin::previous` map onto the `(commit, source file name)` pair the
/// output lines carry.
fn collect_previous_origins(into: &mut PreviousOrigins, outcome: &gix::blame::Outcome) {
    for (origin, parent) in &outcome.suspect_previous {
        let owned = |(commit_id, name): &gix::blame::OriginKey| {
            (*commit_id, name.as_ref().map(|name| name.to_vec()))
        };
        into.entry(owned(origin)).or_insert_with(|| owned(parent));
    }
}

/// [`gix::blame::suspect_refcounts`] over the keys this file uses, which own their path bytes.
fn suspect_refcounts(
    entries: &std::collections::BTreeMap<(ObjectId, Option<Vec<u8>>), u32>,
    previous: &PreviousOrigins,
) -> std::collections::BTreeMap<(ObjectId, Option<Vec<u8>>), u32> {
    use gix::bstr::BString;
    let to_gix = |(commit_id, name): &(ObjectId, Option<Vec<u8>>)| -> gix::blame::OriginKey {
        (*commit_id, name.as_ref().map(|name| BString::from(name.clone())))
    };
    let entries = entries.iter().map(|(key, count)| (to_gix(key), *count)).collect();
    let previous = previous.iter().map(|(key, parent)| (to_gix(key), to_gix(parent))).collect();
    gix::blame::suspect_refcounts(&entries, &previous)
        .into_iter()
        .map(|((commit_id, name), count)| ((commit_id, name.map(|name| name.to_vec())), count))
        .collect()
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

/// Rebase the blames of the synthetic working-tree commit's parents onto the
/// working-tree content.
///
/// git blames the working tree by putting a synthetic commit holding the
/// working-tree blob on top of its parents and running its usual algorithm.
/// `pass_blame()` walks those parents in order, diffing the final image against
/// each and handing over only the lines no earlier parent already claimed; what
/// survives every parent stays with the synthetic commit (the null object id).
///
/// `sources` is that parent list in the same order — `(blame of the parent, the
/// parent's blob)` — which is `[HEAD]` for an ordinary working-tree blame and
/// `[HEAD, MERGE_HEAD…]` mid-merge.
fn overlay_worktree(
    repo: &gix::Repository,
    sources: &[(Vec<Line>, Vec<u8>)],
    worktree: &[u8],
    diff_algorithm: Option<gix::diff::blob::Algorithm>,
    ignore_whitespace: bool,
) -> Result<Vec<Line>> {
    let tokens: Vec<&[u8]> = gix::diff::blob::sources::byte_lines(worktree).collect();
    let null_id = ObjectId::null(repo.object_hash());

    let mut out: Vec<Line> = tokens
        .into_iter()
        .enumerate()
        .map(|(i, token)| {
            let mut content = token.to_vec();
            if content.last() == Some(&b'\n') {
                content.pop();
            }
            let final_no = i as u32 + 1;
            Line {
                commit_id: null_id,
                final_no,
                orig_no: final_no,
                source_name: None,
                content,
                ignored: false,
                unblamable: false,
            }
        })
        .collect();

    for (parent_lines, parent_blob) in sources {
        let mapped =
            worktree_line_map(repo, parent_blob, worktree, diff_algorithm, ignore_whitespace)?;
        for (i, line) in out.iter_mut().enumerate() {
            // Already claimed by an earlier parent, exactly as `blame_chunk()`
            // skips the entries a previous scapegoat took.
            if line.commit_id != null_id {
                continue;
            }
            let Some(src) = mapped.get(i).copied().flatten().and_then(|n| parent_lines.get(n as usize))
            else {
                continue;
            };
            line.commit_id = src.commit_id;
            line.orig_no = src.orig_no;
            line.source_name = src.source_name.clone();
            line.ignored = src.ignored;
            line.unblamable = src.unblamable;
        }
    }
    Ok(out)
}

/// The blob `path` names in `commit`, or `None` when the entry is missing or is
/// not a blob.
///
/// git demands a blob in both places it resolves the blamed path —
/// `verify_working_tree_path()` (`blame.c`) and `fill_blob_sha1_and_mode()` —
/// so a directory operand is "no such path" rather than an attempt to blame a
/// tree.
fn blob_at(repo: &gix::Repository, commit: &ObjectId, path: &str) -> Option<ObjectId> {
    let id = repo
        .rev_parse_single(format!("{commit}:{path}").as_str())
        .ok()?;
    let header = repo.find_header(id).ok()?;
    header.kind().is_blob().then(|| id.detach())
}

/// The ids in `MERGE_HEAD`, empty when no merge is in progress.
///
/// `append_merge_parents()` (`blame.c:145-168`) reads the file line by line and
/// appends each as a parent of the synthetic working-tree commit. It dies on a
/// line that is not a hex object name; a malformed file is treated as absent
/// here, because refusing to blame is a worse answer than blaming without the
/// other side.
fn read_merge_heads(repo: &gix::Repository) -> Vec<ObjectId> {
    let Ok(body) = std::fs::read_to_string(repo.common_dir().join("MERGE_HEAD")) else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| ObjectId::from_hex(line.trim().as_bytes()).ok())
        .collect()
}

/// The diff the synthetic working-tree commit takes part in, as a per-line map: for
/// every line of the final image, the (0-based) line of `head_blob` it is unchanged
/// from, or `None` when the line is new and therefore stays with that commit.
fn worktree_line_map(
    repo: &gix::Repository,
    head_blob: &[u8],
    worktree: &[u8],
    diff_algorithm: Option<gix::diff::blob::Algorithm>,
    ignore_whitespace: bool,
) -> Result<Vec<Option<u32>>> {
    // `-w` (`XDF_IGNORE_WHITESPACE`) is part of `revs.diffopt.xdl_opts`, which git threads through
    // *every* diff in the blame — the synthetic working-tree commit's first diff included. Without
    // it, a worktree that differs from `HEAD` only in indentation has all of those lines blamed on
    // "Not Committed Yet" under `-w`, where git keeps `HEAD`'s attribution. Normalizing per line
    // preserves the line count, so the hunk indices still map one-to-one onto the original lines.
    let (head_norm, worktree_norm);
    let (head_cmp, worktree_cmp): (&[u8], &[u8]) = if ignore_whitespace {
        head_norm = gix::blame::strip_whitespace_per_line(head_blob);
        worktree_norm = gix::blame::strip_whitespace_per_line(worktree);
        (&head_norm, &worktree_norm)
    } else {
        (head_blob, worktree)
    };
    let input = gix::diff::blob::InternedInput::new(head_cmp, worktree_cmp);
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
    // `cmd_blame` calls `read_mailmap()` unconditionally (`builtin/blame.c:1255`) and
    // `get_ac_line()` runs `map_user()` over both the author and the committer
    // (`builtin/blame.c:177-184`), so every identity blame prints — human column,
    // porcelain `author`/`author-mail`, `committer`/`committer-mail` — is the mapped one.
    // There is no `--no-use-mailmap` to turn it off; blame has no such option.
    let mailmap = repo.open_mailmap();
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
            // `map_user()` rewrites only the name and the e-mail; the timestamp it
            // is handed is left alone, so the date columns keep the commit's own.
            let author_id = mailmap.resolve_cow(author);
            let committer_id = mailmap.resolve_cow(committer);
            let (author_name, author_mail) =
                (author_id.name.as_ref().to_vec(), author_id.email.as_ref().to_vec());
            let (committer_name, committer_mail) = (
                committer_id.name.as_ref().to_vec(),
                committer_id.email.as_ref().to_vec(),
            );
            let author_time = author.time().ok();
            let committer_time = committer.time().ok();
            // Reduced to owned values before the struct literal: the iterator
            // and the summary both borrow `commit`, which drops at the end of
            // this block while the literal's temporaries are still live.
            // `--root` (git's `show_root`) stops root commits counting as
            // boundaries, dropping both the `^` marker and the porcelain
            // `boundary` field for them.
            //
            // A `--reverse` range marks its initial commit `UNINTERESTING` through the `^A` the
            // arguments named, which is a different flag from the root rule and so is not
            // affected by `--root`: a line the range's oldest commit kept, because the next commit
            // changed it, prints with the boundary marker.
            //
            // A forward range marks its bottom commits `UNINTERESTING` the same way
            // — `assign_blame()` sets the flag on every commit it refuses to pass
            // blame from, and `emit_other()` reads that one flag for the marker — so
            // `git blame A..B` prints `^` against whichever of `A`'s ancestors ended
            // up holding a line the range did not touch.
            let boundary = (!opts.show_root && commit.parent_ids().next().is_none())
                || opts.reverse_from.is_some_and(|from| from == line.commit_id)
                || opts.bottom.contains(&line.commit_id);
            let summary = Vec::from(commit.message()?.summary().into_owned());
            CommitInfo {
                display_author: display_author(
                    author_name.as_slice().into(),
                    author_mail.as_slice().into(),
                    opts.show_email,
                ),
                display_date: author_time
                    .map(|t| opts.date_mode.format_time(t.seconds, t.offset))
                    .unwrap_or_else(|| author.time.to_string()),
                boundary,
                hex: line.commit_id.to_hex().to_string(),
                author_name,
                author_mail,
                author_time: author_time.map(|t| t.seconds).unwrap_or(0),
                author_tz: format_tz(author_time.map(|t| t.offset).unwrap_or(0)),
                committer_name,
                committer_mail,
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
            // `parse_abbrev_cb` has already applied git's `MINIMUM_ABBREV` floor.
            // There is deliberately no ceiling: `cmd_blame` only adds the boundary
            // column when `abbrev < hexsz`, and `emit_other`'s `%.*s` then prints
            // however much of the hash the budget covers. So `--abbrev=40` spends a
            // column on `^` and shows 39 digits, while `--abbrev=41` and up show all
            // 40 — a ceiling here would collapse the two.
            Some(n) => n,
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
    // git prints `%.*s` over a NUL-terminated hex buffer, so a budget wider than the
    // hash (`--abbrev=41` and up) simply stops at the hash — and `-b`'s `memset`
    // blanks exactly `strlen(hex)` bytes, so the blank run is capped the same way.
    let length = length.min(ci.hex.len());
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
    refcounts: &std::collections::BTreeMap<(ObjectId, Option<Vec<u8>>), u32>,
) -> Result<ExitCode> {
    let name_width = object_name_width(repo, opts);

    if opts.annotate_compat {
        return emit_annotate_compat(lines, info, name_width, opts);
    }

    // `emit_other` colors per blame entry: `cnt` counts lines within the entry,
    // and only `cnt > 0` takes the repeated-metadata color. The entries are the
    // coalesced groups the porcelain format also emits.
    let mut cnt_in_entry: Vec<usize> = vec![0; lines.len()];
    // `blame_entry_score()` (`blame.c:1991`): one plus the alphanumeric bytes of the entry's lines
    // in the final image, cached on the entry so every line of it prints the same number.
    // `find_alignment()` computes it for every entry whether or not `--score-debug` asked
    // (`builtin/blame.c:680`), and takes `max_score_digits` from the largest.
    let mut score_of_line: Vec<u32> = vec![0; lines.len()];
    for group in group_lines(lines) {
        let entry = &lines[group.start..group.start + group.len];
        let score = 1 + entry
            .iter()
            .map(|line| line.content.iter().filter(|b| b.is_ascii_alphanumeric()).count() as u32)
            .sum::<u32>();
        for k in 0..group.len {
            cnt_in_entry[group.start + k] = k;
            score_of_line[group.start + k] = score;
        }
    }
    let w_score = decimal_width(score_of_line.iter().copied().max().unwrap_or(1));

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

        // `--score-debug`: `printf(" %*d %02d", max_score_digits, ent->score,
        // ent->suspect->refcnt)` (`builtin/blame.c:533-535`).
        if opts.score_debug {
            let s = score_of_line[idx].to_string();
            buf.push(b' ');
            pad(&mut buf, w_score.saturating_sub(s.len()));
            buf.extend_from_slice(s.as_bytes());
            let refcnt = refcounts
                .get(&(line.commit_id, line.source_name.clone()))
                .copied()
                .unwrap_or_default();
            buf.extend_from_slice(format!(" {refcnt:02}").as_bytes());
        }

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
    previous_origins: &PreviousOrigins,
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
            } else if opts.reverse {
                // Walking forwards, `origin->previous` is the first *child* the origin handed
                // entries to, which no amount of looking at the commit's parents will find.
                previous_origins
                    .get(&(first.commit_id, first.source_name.clone()))
                    .map(|(commit_id, name)| {
                        (
                            commit_id.to_hex().to_string(),
                            name.clone().unwrap_or_else(|| current_path.to_vec()),
                        )
                    })
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

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_name(name: &[u8]) -> Vec<u8> {
    crate::quote::quoted_name_bytes(name)
}

/// Format a UTC offset the way git writes the `author-tz`/`committer-tz` field.
fn format_tz(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let abs = offset_seconds.unsigned_abs();
    format!("{sign}{:02}{:02}", abs / 3600, (abs % 3600) / 60)
}

/// Print the usage text for `cmd` and yield `parse_options`' exit status (129).
///
/// git splits the two usage paths by stream, which the parity contract sees
/// because stdout is compared: `usage_with_options()` from the `-h` handler
/// writes to **stdout** (`parse-options.c` passes `stdout` when the request was
/// explicit), while `usage(str_usage)` for a structurally invalid command line
/// writes to **stderr**. Both exit 129. Verified against git 2.55.0:
/// `git blame -h` wrote 2097 bytes to stdout and 0 to stderr; `git blame` with
/// no operand wrote 0 to stdout and the same 2097 bytes to stderr.
fn print_usage(cmd: &str, to_stdout: bool) -> Result<ExitCode> {
    write_usage(cmd, USAGE_BODY, to_stdout)
}

/// `--help-all`: the same synopsis over [`USAGE_BODY_ALL`], always on stdout —
/// only a help request reaches `USAGE_FULL`, and a help request is never a
/// rejection.
fn print_usage_all(cmd: &str) -> Result<ExitCode> {
    write_usage(cmd, USAGE_BODY_ALL, true)
}

/// The synopsis line, named for the command that was actually invoked, followed
/// by whichever option block was asked for.
fn write_usage(cmd: &str, body: &str, to_stdout: bool) -> Result<ExitCode> {
    let text = format!("usage: git {cmd} [<options>] [<rev-opts>] [<rev>] [--] <file>\n{body}");
    if to_stdout {
        let mut out = std::io::stdout().lock();
        out.write_all(text.as_bytes())?;
        out.flush()?;
    } else {
        let mut err = std::io::stderr().lock();
        err.write_all(text.as_bytes())?;
        err.flush()?;
    }
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

/// `strerror(errno)` as `die_errno()` renders it: Rust spells an OS error
/// `<strerror> (os error <n>)` and git spells only the first half.
fn errno_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    text.split(" (os error ").next().unwrap_or(&text).to_string()
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

/// git's `is_a_rev` (`builtin/blame.c`), which decides whether the trailing
/// positional of `git blame <a> <b>` is a revision or a path:
///
/// ```c
/// static int is_a_rev(const char *name)
/// {
///         struct object_id oid;
///
///         if (repo_get_oid(the_repository, name, &oid))
///                 return 0;
///         return OBJ_NONE < odb_read_object_info(the_repository->objects, &oid, NULL);
/// }
/// ```
///
/// Both halves matter and they pull opposite ways: `repo_get_oid()` is the
/// full-length-hex rule, so an absent hex gets past it, and then the explicit
/// object-info lookup rejects it. So — unlike every other operand blame
/// resolves — a well-formed absent hex is *not* a rev here, and
/// `git blame <absent-hex>` is a request to blame a **path** by that name.
fn is_a_rev(repo: &gix::Repository, name: &str) -> bool {
    match crate::objname::resolve(repo, name) {
        Some(id) => repo.find_object(id).is_ok(),
        None => false,
    }
}

/// The fatal `setup_revisions()` raises for one revision operand, complete with
/// any line an inner lookup printed first, or `None` when the operand is not one
/// of the shapes that rule covers and blame's own diagnosis stands.
///
/// `cmd_blame()` hands its operands to `setup_revisions()` untouched, so blame
/// inherits both halves of `get_oid_basic()`'s full-length-hex rule: a bare
/// absent hex resolves and then dies in `get_reference()` as `bad object`, and a
/// range whose endpoints resolve to objects the repository does not have dies in
/// `dotdot_missing()` naming the whole token. `gix`'s parser consults the object
/// database, so without this both come back as `bad revision '<operand>'`.
fn setup_revisions_fatal(repo: &gix::Repository, token: &str) -> Option<String> {
    if crate::objname::split_range(token).is_some() {
        return crate::objname::dotdot_fatal(repo, token);
    }
    crate::objname::bad_object_name(repo, token).map(|name| format!("fatal: bad object {name}\n"))
}

/// One entry of git's `revs->pending`: the object an operand named, under the
/// name `setup_revisions()` recorded for it.
///
/// The name is carried because every diagnostic `find_single_final()` raises
/// quotes it rather than the object id, and it is not always the operand: a ref
/// selector queues `refs_for_each_ref()`'s refname, `--reflog` and
/// `--indexed-objects` queue the empty string, and the merge bases `A...B`
/// contributes are queued under their hex ids.
struct Pending {
    name: String,
    /// The object the name resolved to, before `deref_tag()`. Carried because
    /// `UNINTERESTING` is a flag on the *object*, not on the pending entry, so
    /// two entries naming the same object share it — see [`Revs::uninteresting`].
    id: ObjectId,
    object: crate::sequencer::Side,
}

/// git's `struct rev_info` as far as `cmd_blame()` uses it: everything
/// `setup_revisions()` leaves behind for `setup_scoreboard()` to read.
#[derive(Default)]
struct Revs {
    /// `revs->pending`, in the order the operands and pseudo-options queued it.
    pending: Vec<Pending>,
    /// The object ids carrying `UNINTERESTING`.
    ///
    /// A set of *ids* rather than a flag per pending entry because git keeps the
    /// flag on the `struct object`, which two entries share when two operands name
    /// the same object. `A...B` is where that becomes observable: it queues the
    /// merge bases as uninteresting and then `A` as the symmetric-left end, and
    /// when `A` *is* the merge base — every `<ancestor>...<descendant>` — that one
    /// object is uninteresting for both entries, so `find_single_final()` skips
    /// them both and the range behaves exactly like `A..B`.
    uninteresting: std::collections::HashSet<ObjectId>,
    /// `revs->ancestry_path` together with `revs->ancestry_path_implicit_bottoms`,
    /// which `--ancestry-path` (without a `=<commit>`) sets in one breath
    /// (`revision.c:2406-2411`).
    ancestry_path: bool,
    /// `revs->show_merge`, which `--merge` sets and `prepare_show_merge()` acts on
    /// at the end of `setup_revisions()`.
    show_merge: bool,
    /// `revs->diffopt.flags.follow_renames` *as `setup_revisions()` leaves it*.
    ///
    /// Only the trailing slot can set it: an operand-position `--follow` is
    /// consumed by `parse_revision_opt()` inside `cmd_blame()`'s own parse loop,
    /// and the line right after that loop clears the flag again
    /// (`builtin/blame.c:1035 revs.diffopt.flags.follow_renames = 0;`), so it never
    /// reaches `diff_setup_done()`'s check.
    follow: bool,
}

/// `handle_revision_arg()`'s view of one revision operand, as
/// `find_single_final()` needs it: the commit it peels to, the fact that it
/// resolves to something that is not commit-ish, or that it does not resolve.
///
/// This is `repo_get_oid()` — [`crate::objname::resolve`], so the full-length-hex
/// rule and `get_oid_basic()`'s ambiguity warning apply — followed by
/// `deref_tag()`, which is [`crate::sequencer::peel_id`].
fn pending_object(repo: &gix::Repository, rev: &str) -> crate::sequencer::Side {
    match crate::objname::resolve(repo, rev) {
        Some(id) => crate::sequencer::peel_id(repo, id),
        None => crate::sequencer::Side::Unresolved,
    }
}

/// Resolve a revision to the commit it names (peeling tags), or `None` if it is
/// not a valid revision — git's `get_oid` followed by a peel to commit.
///
/// This is `repo_get_oid_committish()` as `build_ignorelist()` calls it: it
/// answers `None` both for a name that does not resolve and for one that
/// resolves to an object the repository lacks, which is the single
/// `cannot find revision %s to ignore` that caller wants either way.
fn resolve_commit(repo: &gix::Repository, rev: &str) -> Option<ObjectId> {
    match pending_object(repo, rev) {
        crate::sequencer::Side::Commit(id) => Some(id),
        _ => None,
    }
}

/// `find_single_final()` (`blame.c:2663-2686`), which picks the one commit a
/// forward blame digs from out of `revs->pending`:
///
/// ```c
/// for (i = 0; i < revs->pending.nr; i++) {
///         struct object *obj = revs->pending.objects[i].item;
///         if (obj->flags & UNINTERESTING)
///                 continue;
///         obj = deref_tag(revs->repo, obj, NULL, 0);
///         if (!obj || obj->type != OBJ_COMMIT)
///                 die("Non commit %s?", revs->pending.objects[i].name);
///         if (found)
///                 die("More than one commit to dig from %s and %s?",
///                     revs->pending.objects[i].name, name);
///         found = (struct commit *)obj;
///         name = revs->pending.objects[i].name;
/// }
/// ```
///
/// Note which way round the second diagnostic names its two operands: the one
/// just reached comes first and the one already held second, so
/// `git blame HEAD~1 HEAD -- f` says `from HEAD and HEAD~1?`.
///
/// The `UNINTERESTING` skip is what makes a range a range here: `A..B` queues
/// both ends, and only `B` is a candidate to dig from.
///
/// `Err` is the `die()` text without its `fatal: ` prefix; `Ok(None)` is the
/// empty-pending case `setup_scoreboard()` answers with a fake working-tree
/// commit.
fn find_single_final(revs: &Revs) -> Result<Option<(String, ObjectId)>, String> {
    let mut found: Option<(String, ObjectId)> = None;
    for entry in &revs.pending {
        if revs.uninteresting.contains(&entry.id) {
            continue;
        }
        let id = match entry.object {
            crate::sequencer::Side::Commit(id) => id,
            // `deref_tag()` returning NULL and a non-commit object are the same
            // arm: an operand naming a tree or a blob is queued by
            // `handle_revision_arg()` and only refused here.
            _ => return Err(format!("Non commit {}?", entry.name)),
        };
        if let Some((first, _)) = &found {
            return Err(format!(
                "More than one commit to dig from {} and {first}?",
                entry.name
            ));
        }
        found = Some((entry.name.clone(), id));
    }
    Ok(found)
}

/// `find_single_initial()` (`blame.c:2726-2757`), the `--reverse` counterpart:
/// the same loop over `revs->pending` with the test inverted, so it picks the one
/// *negative* commit — the range's oldest end, whose file is the final image.
///
/// ```c
/// for (i = 0; i < revs->pending.nr; i++) {
///         struct object *obj = revs->pending.objects[i].item;
///         if (!(obj->flags & UNINTERESTING))
///                 continue;
///         obj = deref_tag(revs->repo, obj, NULL, 0);
///         if (!obj || obj->type != OBJ_COMMIT)
///                 die("Non commit %s?", revs->pending.objects[i].name);
///         if (found)
///                 die("More than one commit to dig up from, %s and %s?",
///                     revs->pending.objects[i].name, name);
///         found = (struct commit *) obj;
///         name = revs->pending.objects[i].name;
/// }
/// ```
///
/// The comma in `dig up from, %s` is git's and is not in the forward wording.
///
/// `Ok(None)` is `!name`, which the caller answers with `dwim_reverse_initial()`
/// and then `die("No commit to dig up from?")`.
fn find_single_initial(revs: &Revs) -> Result<Option<(String, ObjectId)>, String> {
    let mut found: Option<(String, ObjectId)> = None;
    for entry in &revs.pending {
        if !revs.uninteresting.contains(&entry.id) {
            continue;
        }
        let id = match entry.object {
            crate::sequencer::Side::Commit(id) => id,
            _ => return Err(format!("Non commit {}?", entry.name)),
        };
        if let Some((first, _)) = &found {
            return Err(format!(
                "More than one commit to dig up from, {} and {first}?",
                entry.name
            ));
        }
        found = Some((entry.name.clone(), id));
    }
    Ok(found)
}

/// The long options reachable from `setup_revisions()` that take a *separate*
/// value argument and are reported by `parse-options` when it is missing:
/// ``error: option `<name>' requires a value``, exit 129. These are the entries of
/// the `parseopts[]` table `diff.c:prep_parse_options()` builds.
static REV_OPT_PARSE_OPTIONS_VALUE: &[&str] = &[
    "anchored", "color-moved-ws", "diff-algorithm", "diff-filter", "dst-prefix", "find-object",
    "ignore-matching-lines", "inter-hunk-context", "line-prefix", "max-depth", "output",
    "output-indicator-context", "output-indicator-new", "output-indicator-old", "rotate-to",
    "skip-to", "src-prefix", "stat-count", "stat-graph-width", "stat-name-width", "stat-width",
    "word-diff-regex", "ws-error-highlight",
];

/// The same, for the options `revision.c`'s own hand-rolled matcher owns: it
/// `die()`s with `fatal: Option '--<name>' requires a value` at exit 128.
static REV_OPT_REVISION_VALUE: &[&str] = &[
    "after", "author", "before", "committer", "date", "encoding", "exclude", "exclude-hidden",
    "glob", "grep", "grep-reflog", "max-age", "max-count", "max-count-oldest", "min-age", "since",
    "since-as-filter", "skip", "until",
];

/// Which of git's value parsers an option hands its value to, for the options
/// whose *rejections* this port reproduces.
///
/// Everything outside this table is still accepted and dropped: blame ignores
/// every diff and revision knob that does not change which commit it digs from,
/// so a value git merely *stores* is one this port has nothing to do with. What
/// it cannot drop is a value git *refuses*, because refusing is observable.
enum RevOptValue {
    /// `parse_count()` (`revision.c:2277`) — `fatal: '<v>': not an integer`, 128.
    Count,
    /// `blame_diff_algorithm_callback()` (`builtin/blame.c:868`) and
    /// `diff_opt_diff_algorithm()` (`diff.c`), which share one message.
    DiffAlgorithm,
    /// `diff_opt_find_object()` (`diff.c:5522`) — ``error: unable to resolve '<v>'``,
    /// 129. It resolves with `repo_get_oid()`, so a full-length hex that is also a
    /// refname warns here exactly as it would anywhere else.
    FindObject,
    /// `handle_revision_opt()`'s `--date=<mode>`, which is `parse_date_format()` —
    /// `fatal: unknown date format <v>`, 128.
    Date,
    /// `diff_opt_break_rewrites()` (`diff.c:5569`).
    BreakRewrites,
    /// `diff_opt_find_renames()` / `diff_opt_find_copies()` (`diff.c:5732`,
    /// `diff.c:5752`), whose message names the option: ``error: invalid argument to
    /// <long-name>``, 129.
    RenameScore(&'static str),
    /// parse-options' `OPT_INTEGER_F(..., PARSE_OPT_NONEG)`, whose two failures are
    /// separate messages: an empty value "expects a numerical value", anything else
    /// "expects a non-negative integer value with an optional k/m/g suffix".
    Integer,
    /// `diff.c`'s hand-rolled width parsers, which spell the same complaint without
    /// the backticks and without the leading dashes: `error: <name> expects a
    /// numerical value`.
    StatWidth,
    /// `-U<n>` / `--unified=<n>`, which spells it with the dashes:
    /// `error: --unified expects a numerical value`.
    Unified,
}

/// The parser [`RevOptValue`] names for one long-option *name* (no `--`, no value).
fn rev_opt_value_parser(name: &str) -> Option<RevOptValue> {
    Some(match name {
        "max-count" | "max-count-oldest" | "skip" | "min-parents" | "max-parents"
        | "graph-lane-limit" => RevOptValue::Count,
        "diff-algorithm" => RevOptValue::DiffAlgorithm,
        "find-object" => RevOptValue::FindObject,
        "date" => RevOptValue::Date,
        "break-rewrites" => RevOptValue::BreakRewrites,
        "find-renames" => RevOptValue::RenameScore("find-renames"),
        "find-copies" => RevOptValue::RenameScore("find-copies"),
        "inter-hunk-context" => RevOptValue::Integer,
        "stat-width" | "stat-name-width" | "stat-count" | "stat-graph-width" => {
            RevOptValue::StatWidth
        }
        "unified" => RevOptValue::Unified,
        _ => return None,
    })
}

/// Run one option's value through [`rev_opt_value_parser`]'s choice, answering the
/// diagnostic and exit status when it is rejected.
///
/// `name` is the long name as the message spells it; for the two short spellings
/// that share a parser (`-B`, `-U`) it is the long option they alias.
fn rev_opt_value_refusal(
    repo: &gix::Repository,
    name: &str,
    value: &str,
) -> Result<Option<(String, ExitCode)>> {
    let Some(parser) = rev_opt_value_parser(name) else {
        return Ok(None);
    };
    Ok(match parser {
        RevOptValue::Count => parse_count(value)
            .err()
            .map(|message| (format!("fatal: {message}\n"), ExitCode::from(128))),
        RevOptValue::DiffAlgorithm => parse_diff_algorithm(value)
            .is_none()
            .then(|| (format!("error: {DIFF_ALGORITHM_ERROR}\n"), ExitCode::from(129))),
        RevOptValue::FindObject => crate::objname::resolve(repo, value)
            .is_none()
            .then(|| (format!("error: unable to resolve '{value}'\n"), ExitCode::from(129))),
        RevOptValue::Date => match resolve_date_mode(value)? {
            DateOutcome::Mode(_) => None,
            // `resolve_date_mode` has already written the `fatal:` line, so there is
            // nothing left to print — only the status to carry back.
            DateOutcome::Fatal(_) => Some((String::new(), ExitCode::from(128))),
        },
        RevOptValue::BreakRewrites => (!break_rewrites_ok(value))
            .then(|| ("error: break-rewrites expects <n>/<m> form\n".to_string(), ExitCode::from(129))),
        RevOptValue::RenameScore(long) => (!rename_score_consumed(value))
            .then(|| (format!("error: invalid argument to {long}\n"), ExitCode::from(129))),
        RevOptValue::Integer => match git_parse_unsigned(value) {
            Unsigned::Ok => None,
            Unsigned::Empty => Some((
                format!("error: option `{name}' expects a numerical value\n"),
                ExitCode::from(129),
            )),
            // `errno == ERANGE`, which parse-options reports before the generic
            // complaint and which names the bound the option's `precision` sets —
            // 4 bytes for `--inter-hunk-context`, so `[0,4294967295]`.
            Unsigned::Range => Some((
                format!("error: value {value} for option `{name}' not in range [0,4294967295]\n"),
                ExitCode::from(129),
            )),
            Unsigned::Bad => Some((
                format!(
                    "error: option `{name}' expects a non-negative integer value \
                     with an optional k/m/g suffix\n"
                ),
                ExitCode::from(129),
            )),
        },
        // `strtoul()`, so a leading `-` is consumed and wraps rather than being
        // refused, and an out-of-range value is `ULONG_MAX` with nothing left over:
        // `--stat-width=-1` and `--stat-width=99999999999999999999` are both accepted
        // by git 2.55.0 while `--stat-width=0x10` is not.
        RevOptValue::StatWidth => strtol_consumed(value)
            .is_none()
            .then(|| (format!("error: {name} expects a numerical value\n"), ExitCode::from(129))),
        // ```c
        // long val = strtol(arg, &s, 10);
        // if (*s)
        //         return error(_("%s expects a numerical value"), "--unified");
        // if (val < 0)
        //         return error(_("%s expects a non-negative integer"), "--unified");
        // ```
        //
        // Two separate refusals, and the second is reachable only because `strtol`
        // takes a sign: `-U-1` is a *number*, so it clears the first test and is
        // caught by the second.
        RevOptValue::Unified => match strtol_consumed(value) {
            None => Some((
                "error: --unified expects a numerical value\n".to_string(),
                ExitCode::from(129),
            )),
            Some(val) if val < 0 => Some((
                "error: --unified expects a non-negative integer\n".to_string(),
                ExitCode::from(129),
            )),
            Some(_) => None,
        },
    })
}

/// `strtol(s, &end, 10)` followed by git's near-universal `if (*end)` check: the
/// value when the whole string was consumed, `None` when anything was left over.
///
/// `strtol` skips leading whitespace, takes an optional sign, and on overflow
/// returns `LONG_MAX`/`LONG_MIN` with `end` still past the digits — so the sign of
/// an out-of-range value survives, which is what makes
/// `--unified=-99999999999999999999` a *negative* value rather than a malformed
/// one. An empty string converts nothing, leaving `end == s` and therefore
/// `*end == 0`, so it is accepted as 0 — which is why `git blame -- f --unified=`
/// is not an error while `--max-count=` is (that one is [`strtol_i`], whose extra
/// `p == s` test rejects it).
fn strtol_consumed(s: &str) -> Option<i64> {
    let rest = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (negative, digits) = match rest.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, rest.strip_prefix('+').unwrap_or(rest)),
    };
    let taken: String = digits.chars().take_while(char::is_ascii_digit).collect();
    // `end == s` when nothing converted, so only a wholly empty argument passes the
    // `*end` test; a sign or whitespace with no digits behind it does not.
    if taken.is_empty() {
        return s.is_empty().then_some(0);
    }
    if taken.len() != digits.len() {
        return None;
    }
    let magnitude = taken.parse::<i64>().unwrap_or(i64::MAX);
    Some(if negative { -magnitude } else { magnitude })
}

/// What `git_parse_unsigned()` made of a value, as `OPTION_UNSIGNED`
/// (`parse-options.c:294-311`) distinguishes the three failures.
enum Unsigned {
    /// The value parsed. Blame never reads the number — `--inter-hunk-context`
    /// only reaches a diff this port does not produce — so only the fact that
    /// there was one is carried.
    Ok,
    /// `!*arg` — tested before the parser runs at all.
    Empty,
    /// `errno == ERANGE`: a well-formed number past the option's `precision`.
    Range,
    Bad,
}

/// `git_parse_unsigned(value, ret, max)` (`parse.c:53-86`), with `max` the
/// `UINT_MAX` an `OPT_UNSIGNED` of `sizeof(unsigned) == 4` sets:
///
/// ```c
/// /* negative values would be accepted by strtoumax */
/// if (strchr(value, '-')) { errno = EINVAL; return 0; }
/// errno = 0;
/// val = strtoumax(value, &end, 0);
/// if (errno == ERANGE) return 0;
/// if (end == value) { errno = EINVAL; return 0; }
/// factor = get_unit_factor(end);
/// if (!factor) { errno = EINVAL; return 0; }
/// if (unsigned_mult_overflows(factor, val) || factor * val > max) {
///         errno = ERANGE; return 0;
/// }
/// ```
///
/// The base is **0**, not 10, so `0x10` is 16 and `010` is 8 — which is why
/// `git blame -- f --inter-hunk-context=0x10` succeeds where the base-10
/// `--max-count=0x10` is `fatal: '0x10': not an integer`. `get_unit_factor()`
/// accepts only an empty tail or a case-insensitive `k`/`m`/`g`, so the suffix is
/// not merely stripped: anything else makes the whole value invalid.
fn git_parse_unsigned(value: &str) -> Unsigned {
    if value.is_empty() {
        return Unsigned::Empty;
    }
    if value.contains('-') {
        return Unsigned::Bad;
    }
    let Some((val, end)) = strtoumax0(value) else {
        return Unsigned::Bad;
    };
    let Some(val) = val else {
        // `strtoumax` set `ERANGE`; the tail is still whatever followed the digits,
        // and git returns before ever looking at it.
        return Unsigned::Range;
    };
    let factor: u64 = match end {
        "" => 1,
        _ if end.eq_ignore_ascii_case("k") => 1024,
        _ if end.eq_ignore_ascii_case("m") => 1024 * 1024,
        _ if end.eq_ignore_ascii_case("g") => 1024 * 1024 * 1024,
        _ => return Unsigned::Bad,
    };
    match val.checked_mul(factor).filter(|n| *n <= u64::from(u32::MAX)) {
        Some(_) => Unsigned::Ok,
        None => Unsigned::Range,
    }
}

/// `strtoumax(value, &end, 0)`: leading whitespace and an optional `+`, then a
/// base chosen from the prefix — `0x`/`0X` hexadecimal, a leading `0` octal,
/// anything else decimal.
///
/// `None` is `end == value`, i.e. nothing was converted. `Some((None, end))` is
/// `ERANGE`, which git tests before it looks at `end` at all.
fn strtoumax0(value: &str) -> Option<(Option<u64>, &str)> {
    let body = value.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let body = body.strip_prefix('+').unwrap_or(body);
    let (radix, digits) = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        // `0x` with no hex digit behind it converts just the `0`, leaving `x…`.
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_hexdigit()) => (16, rest),
        Some(_) => (10, body),
        None if body.starts_with('0') => (8, body),
        None => (10, body),
    };
    let taken: String = digits.chars().take_while(|c| c.is_digit(radix)).collect();
    if taken.is_empty() {
        return None;
    }
    let end = &digits[taken.len()..];
    Some((u64::from_str_radix(&taken, radix).ok(), end))
}

/// git's `strtol_i(s, 10, &result)` (`git-compat-util.h`), the whole of which is a
/// base-10 `strtol()` plus `if (errno || *p || p == s || (int) ul != ul) return -1`.
///
/// `strtol()` skips leading whitespace and takes an optional sign; the guard then
/// demands the whole string was consumed and that the value fits an `int`. So `-1`
/// and `+3` pass while `3x`, `0x10` (base 10 stops at the `x`), the empty string and
/// `99999999999999999999` do not — all five verified against git 2.55.0 through
/// `git blame -- <path> --max-count=<v>`.
fn strtol_i(s: &str) -> Option<i32> {
    // Rust's own parser is the guard: it rejects leading whitespace, trailing
    // characters, an empty string and anything outside `i32`, and accepts exactly
    // the optional sign `strtol` does. Only the whitespace skip has to be added.
    s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']).parse::<i32>().ok()
}

/// `parse_count()` (`revision.c:2277`), which is [`strtol_i`] and a `die()`:
///
/// ```c
/// static int parse_count(const char *arg)
/// {
///         int count;
///
///         if (strtol_i(arg, 10, &count) < 0)
///                 die("'%s': not an integer", arg);
///         return count;
/// }
/// ```
///
/// `Err` is the `die()` text without its `fatal: ` prefix.
fn parse_count(arg: &str) -> Result<i32, String> {
    strtol_i(arg).ok_or_else(|| format!("'{arg}': not an integer"))
}

/// git's answer to a `-`-leading token in the revision slot of
/// `git blame -- <path> <token>`, which `setup_revisions()` parses as an option.
///
/// `Ok(None)` means the option parsed — for this port's purposes, that it was
/// consumed and had no effect on which commit is blamed. `Ok(Some(code))` means
/// its value was missing: the diagnostic is already on stderr and `code` is the
/// status to exit with. Which of the three diagnostics fires depends on which
/// parser owns the name; all three were read off git v2.55.0 by feeding every
/// option `git blame` accepts through this position.
///
/// Value *validation* — as opposed to a value being absent — is
/// [`rev_opt_value_refusal`], which the callers run first because a value that is
/// present and wrong is rejected wherever the option stands, while "requires a
/// value" is only reachable at the end of the line.
///
/// Shared with `log`, `show` and the other `setup_revisions()` verbs rather than
/// copied into each: the split between the two tables — parse-options' `error:`
/// at 129 and `revision.c`'s `die()` at 128 — is the whole content of this
/// function, and a second copy is a second chance to get that split wrong.
pub(super) fn trailing_option_missing_value(arg: &str) -> Result<Option<ExitCode>> {
    let mut err = std::io::stderr().lock();
    if let Some(body) = arg.strip_prefix("--") {
        // `--name=<value>` carries its value with it, so it is never missing one.
        if body.contains('=') {
            return Ok(None);
        }
        if REV_OPT_PARSE_OPTIONS_VALUE.contains(&body) {
            writeln!(err, "error: option `{body}' requires a value")?;
            err.flush()?;
            return Ok(Some(ExitCode::from(129)));
        }
        if REV_OPT_REVISION_VALUE.contains(&body) {
            writeln!(err, "fatal: Option '--{body}' requires a value")?;
            err.flush()?;
            return Ok(Some(ExitCode::from(128)));
        }
        // `--default <rev>` names the revision to fall back on; without one,
        // `revision.c` rejects the empty name it was left holding.
        if body == "default" {
            writeln!(err, "error: bad --default argument")?;
            err.flush()?;
            return Ok(Some(ExitCode::from(128)));
        }
        return Ok(None);
    }
    // A short option only lacks its value when nothing is attached to it: `-l5`
    // and `-Sfoo` are complete, `-l` and `-S` are not. A bare `-` is not an option
    // name at all and is dropped like any other unrecognised one.
    match &arg[1..] {
        // `handle_revision_opt()` checks `-n`'s argument itself, before
        // `parse-options` ever sees it.
        "n" => {
            writeln!(err, "error: -n requires an argument")?;
            err.flush()?;
            Ok(Some(ExitCode::from(128)))
        }
        c @ ("l" | "G" | "I" | "O" | "S") => {
            writeln!(err, "error: switch `{c}' requires a value")?;
            err.flush()?;
            Ok(Some(ExitCode::from(129)))
        }
        _ => Ok(None),
    }
}

/// The short options that alias a long one whose *attached* value this port
/// validates: `-M<score>`, `-C<score>`, `-B<n>/<m>`, `-U<n>` and `-<count>`.
///
/// `git blame`'s own `options[]` claims `-M` and `-C` for itself, and
/// `blame_copy_callback()`'s `parse_score()` silently yields 0 for a score it
/// cannot read — so those two only reach `diff_opt_find_renames()` in the
/// *revision* slot, which is why this is asked there and not in the parse loop.
fn short_opt_value(arg: &str) -> Option<(&'static str, &str)> {
    for (flag, long) in [
        ("-M", "find-renames"),
        ("-C", "find-copies"),
        ("-B", "break-rewrites"),
        ("-U", "unified"),
    ] {
        if let Some(value) = arg.strip_prefix(flag) {
            return Some((long, value));
        }
    }
    // `else if ((*arg == '-') && isdigit(arg[1]))` (`revision.c:2364`): the
    // traditional `head`-style count, whose argument is everything after the dash.
    let rest = arg.strip_prefix('-')?;
    rest.starts_with(|c: char| c.is_ascii_digit()).then_some(("max-count", rest))
}

/// Whether git's `parse_rename_score()` (`diff.c`) consumes `score` whole, which is
/// the test `-C<score>` / `-M<score>` apply to their argument.
fn rename_score_consumed(score: &str) -> bool {
    rename_score_rest(score).is_empty()
}

/// What `parse_rename_score()` leaves unconsumed.
///
/// The scanner takes digits and at most one `.`; a `%` is "always at the end", so it
/// stops the scan and anything after it is left over. Everything else stops the scan
/// too, which is how `C`, `+3`, `1e3` and a second `.` end up rejected while `.`,
/// `%`, `0` and `50%` are accepted whole.
fn rename_score_rest(score: &str) -> &str {
    let mut dot = false;
    for (idx, c) in score.char_indices() {
        match c {
            '.' if !dot => dot = true,
            '%' => return &score[idx + c.len_utf8()..],
            '0'..='9' => {}
            _ => return &score[idx..],
        }
    }
    ""
}

/// `diff_opt_break_rewrites()` (`diff.c:5569`), the `-B` / `--break-rewrites`
/// value parser:
///
/// ```c
/// opt1 = parse_rename_score(&arg);
/// if (*arg == 0)
///         opt2 = 0;
/// else if (*arg != '/')
///         return error(_("%s expects <n>/<m> form"), "break-rewrites");
/// else {
///         arg++;
///         opt2 = parse_rename_score(&arg);
/// }
/// if (*arg != 0)
///         return error(_("%s expects <n>/<m> form"), "break-rewrites");
/// ```
///
/// So a bare `-B`, `-B50`, `-B50%`, `-B/` and `-B20/60` are all accepted while
/// `-BB` and `-Bx/y` are not — verified against git 2.55.0.
fn break_rewrites_ok(arg: &str) -> bool {
    let rest = rename_score_rest(arg);
    match rest.strip_prefix('/') {
        Some(after) => rename_score_rest(after).is_empty(),
        None => rest.is_empty(),
    }
}

/// git's `wildmatch(pattern, text, 0)`: no `WM_PATHNAME`, so `*` spans `/` too.
/// This is what `ref_excluded()` (`revision.c`) matches an exclusion with.
fn wildmatch0(pattern: &str, text: &str) -> bool {
    gix::glob::wildmatch(
        pattern.as_bytes().as_bstr(),
        text.as_bytes().as_bstr(),
        gix::glob::wildmatch::Mode::empty(),
    )
}

/// `handle_revision_pseudo_opt()`'s ref selectors (`revision.c:2808-2896`), which
/// all reduce to one `refs_for_each_ref_ext()` call under a prefix, a trim and a
/// pattern:
///
/// | option | prefix | trimmed | pattern |
/// |---|---|---|---|
/// | `--all` | none | no | none |
/// | `--branches` / `--branches=<p>` | `refs/heads/` | yes | `<p>` |
/// | `--tags` / `--tags=<p>` | `refs/tags/` | yes | `<p>` |
/// | `--remotes` / `--remotes=<p>` | `refs/remotes/` | yes | `<p>` |
/// | `--glob=<p>` | none | no | `<p>` |
/// | `--bisect` | `refs/bisect/` | no | see below |
///
/// `handle_one_ref()` queues the ref under the name the iteration handed it — the
/// *trimmed* one where a prefix was trimmed, which is why `--branches` says
/// `main` where `--all` says `refs/heads/main` — and it queues the object the ref
/// points at without peeling, so an annotated tag arrives as the tag object and is
/// only dereferenced by `find_single_final()`.
///
/// `ref_excluded()` is applied to that same (trimmed) name, and every one of these
/// branches ends with `clear_ref_exclusions()`, so a `--exclude=<p>` covers the
/// next selector and nothing after it.
///
/// The pattern, though, is matched against the *untrimmed* name — see
/// [`glob_ref_pattern`], which is what makes `--branches=main` select nothing.
fn queue_refs(
    repo: &gix::Repository,
    revs: &mut Revs,
    not: bool,
    prefix: Option<&str>,
    pattern: Option<&str>,
    excludes: &mut Vec<String>,
) -> Result<()> {
    let pattern = pattern.map(|p| glob_ref_pattern(prefix, p));
    let mut named: Vec<(String, ObjectId)> = Vec::new();
    for reference in repo.references()?.all()? {
        let Ok(reference) = reference else { continue };
        let full = reference.name().as_bstr().to_str_lossy().into_owned();
        // `refs_for_each_ref()` hands out resolved ids; a symbolic or broken ref
        // that resolves to nothing is skipped rather than queued.
        let Some(id) = reference.target().try_id().map(|id| id.to_owned()) else {
            continue;
        };
        // `refs_ref_iterator_begin(refs, prefix, …)` restricts the iteration before
        // the pattern is ever consulted, so a ref outside the prefix is not a
        // candidate however the pattern reads.
        let name = match prefix {
            Some(p) => match full.strip_prefix(p) {
                Some(rest) => rest.to_string(),
                None => continue,
            },
            None => full.clone(),
        };
        // `for_each_filter_refs()` (`refs.c`) tests the composed pattern against
        // `ref->name` *before* trimming: "We need to trim the prefix in the callback
        // function as the pattern is expected to match on the full refname."
        if pattern.as_deref().is_some_and(|p| !wildmatch0(p, &full)) {
            continue;
        }
        if excludes.iter().any(|p| wildmatch0(p, &name)) {
            continue;
        }
        named.push((name, id));
    }
    // `refs_for_each_ref()` iterates in refname order, and that order is what
    // decides which two names `find_single_final()`'s second diagnostic quotes.
    named.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, id) in named {
        queue_pending(repo, revs, not, name, id);
    }
    excludes.clear();
    Ok(())
}

/// `refs_for_each_ref_ext()`'s `real_pattern` (`refs.c:1900-1913`), the pattern a
/// ref selector actually matches with:
///
/// ```c
/// if (opts->pattern) {
///         if (!opts->prefix && !starts_with(opts->pattern, "refs/"))
///                 strbuf_addstr(&real_pattern, "refs/");
///         else if (opts->prefix)
///                 strbuf_addstr(&real_pattern, opts->prefix);
///         strbuf_addstr(&real_pattern, opts->pattern);
///
///         if (!has_glob_specials(opts->pattern)) {
///                 /* Append implied '/' '*' if not present. */
///                 strbuf_complete(&real_pattern, '/');
///                 /* No need to check for '*', there is none. */
///                 strbuf_addch(&real_pattern, '*');
///         }
/// ```
///
/// Two rules, and both are load-bearing for what a blame ends up digging from:
///
/// * The pattern is rooted — under the selector's own prefix, or under `refs/` for
///   a `--glob=` that does not already name one — and matched against the whole
///   refname, not the trimmed one the entry is later *queued* under.
/// * A pattern with none of `?`, `*` or `[` (`has_glob_specials()` is
///   `strpbrk(pattern, "?*[")`) is a *directory* prefix, not a name: it gains `/*`.
///   So `--branches=main` is `refs/heads/main/*` and selects nothing at all, which
///   is why `git blame --branches=main -- f.txt` blames the working tree and
///   `git blame --reverse --branches=main -- f.txt` is
///   `fatal: No commit to dig up from?` — both verified against git 2.55.0.
fn glob_ref_pattern(prefix: Option<&str>, pattern: &str) -> String {
    let mut real = match prefix {
        Some(p) => p.to_string(),
        None if !pattern.starts_with("refs/") => "refs/".to_string(),
        None => String::new(),
    };
    real.push_str(pattern);
    if !pattern.contains(['?', '*', '[']) {
        if !real.ends_with('/') {
            real.push('/');
        }
        real.push('*');
    }
    real
}

/// `add_pending_object()` for an id whose name is already decided: record the
/// entry and, when the flags carry it, the object's `UNINTERESTING` bit.
fn queue_pending(repo: &gix::Repository, revs: &mut Revs, not: bool, name: String, id: ObjectId) {
    if not {
        revs.uninteresting.insert(id);
    }
    revs.pending.push(Pending {
        name,
        id,
        object: crate::sequencer::peel_id(repo, id),
    });
}

/// `handle_revision_pseudo_opt()` (`revision.c:2778`) restricted to what a blame
/// can observe, plus the two `handle_revision_opt()` flags whose effect only shows
/// up at the end of `setup_revisions()` (`--merge`, `--follow`).
///
/// Every branch here changes `revs->pending` or kills the command; the pseudo-options
/// that only set a walk knob blame never reads (`--no-walk`, `--do-walk`,
/// `--single-worktree`, `--filter=`) are accepted and dropped, which is what they
/// amount to for a command that never runs `get_revision()`.
///
/// Returns `true` when the token was one of these, so the caller knows not to treat
/// it as an ordinary revision operand.
fn pseudo_revision_opt(
    repo: &gix::Repository,
    revs: &mut Revs,
    arg: &str,
    not: &mut bool,
    excludes: &mut Vec<String>,
) -> Result<Option<bool>> {
    let handled = match arg {
        "--all" => {
            queue_refs(repo, revs, *not, None, None, excludes)?;
            // `handle_refs(refs, revs, *flags, refs_head_ref)` right after the
            // for-each-ref pass, which is why `HEAD` comes last however it sorts.
            if let Ok(head) = repo.head_id() {
                queue_pending(repo, revs, *not, "HEAD".to_string(), head.detach());
            }
            true
        }
        "--branches" => {
            queue_refs(repo, revs, *not, Some("refs/heads/"), None, excludes)?;
            true
        }
        "--tags" => {
            queue_refs(repo, revs, *not, Some("refs/tags/"), None, excludes)?;
            true
        }
        "--remotes" => {
            queue_refs(repo, revs, *not, Some("refs/remotes/"), None, excludes)?;
            true
        }
        // `for_each_bad_bisect_ref` / `for_each_good_bisect_ref`, which read the
        // terms from `.git/BISECT_TERMS`. With no bisect in progress there are no
        // such refs and the option contributes nothing, which is the only shape a
        // blame fixture ever has.
        "--bisect" => {
            queue_refs(repo, revs, *not, Some("refs/bisect/"), None, excludes)?;
            true
        }
        // `add_reflogs_to_pending()` (`revision.c:1728`): every entry of every
        // reflog, old id and new id, each queued under the *empty* name
        // (`add_pending_object(cb->all_revs, o, "")`, `revision.c:1670`) — which is
        // why `git blame --reflog` says `More than one commit to dig from  and ?`.
        "--reflog" => {
            queue_reflogs(repo, revs, *not)?;
            true
        }
        // `add_index_objects_to_pending()` (`revision.c:1829`): the index's blobs,
        // also under the empty name, and blobs are never commits — so this is
        // always `fatal: Non commit ?` for a non-empty index.
        "--indexed-objects" => {
            queue_index_objects(repo, revs, *not)?;
            true
        }
        // `*flags ^= UNINTERESTING | BOTTOM`: everything after it swaps sides.
        "--not" => {
            *not = !*not;
            true
        }
        // `add_alternate_refs_to_pending()`. A repository with no alternate object
        // database has no alternate refs to add, and this port has no alternates.
        "--alternate-refs" => true,
        "--no-walk" | "--do-walk" | "--single-worktree" | "--no-filter" => true,
        // `revs->show_merge = 1`, acted on by `prepare_show_merge()` at the end of
        // `setup_revisions()` — so it outranks every diagnostic `setup_scoreboard()`
        // raises but not an operand this loop has already refused.
        "--merge" => {
            revs.show_merge = true;
            true
        }
        // `revs->ancestry_path = 1; … revs->ancestry_path_implicit_bottoms = 1`,
        // whose refusal comes much later, from `limit_list()`.
        "--ancestry-path" => {
            revs.ancestry_path = true;
            true
        }
        // `diff_opt_parse()`'s `--follow`, checked by `diff_setup_done()` at the end
        // of `setup_revisions()` — see [`Revs::follow`] for why only this position
        // can set it.
        "--follow" => {
            revs.follow = true;
            true
        }
        "--no-follow" => {
            revs.follow = false;
            true
        }
        _ => false,
    };
    if handled {
        return Ok(Some(true));
    }
    for (opt, prefix) in [
        ("--branches=", Some("refs/heads/")),
        ("--tags=", Some("refs/tags/")),
        ("--remotes=", Some("refs/remotes/")),
        ("--glob=", None),
    ] {
        if let Some(pattern) = arg.strip_prefix(opt) {
            queue_refs(repo, revs, *not, prefix, Some(pattern), excludes)?;
            return Ok(Some(true));
        }
    }
    // `add_ref_exclusion()`, which the *next* selector consults and then clears.
    if let Some(pattern) = arg.strip_prefix("--exclude=") {
        excludes.push(pattern.to_string());
        return Ok(Some(true));
    }
    if arg.starts_with("--no-walk=") || arg.starts_with("--filter=") {
        return Ok(Some(true));
    }
    Ok(None)
}

/// `add_reflogs_to_pending()` — see the `--reflog` arm of [`pseudo_revision_opt`].
///
/// `handle_one_reflog_ent()` offers both ids of every entry to
/// `handle_one_reflog_commit()`, which drops the null ones (the `old` id of a ref's
/// first entry) and queues the rest under the empty name.
fn queue_reflogs(repo: &gix::Repository, revs: &mut Revs, not: bool) -> Result<()> {
    let mut names: Vec<String> = vec!["HEAD".to_string()];
    for reference in repo.references()?.all()? {
        let Ok(reference) = reference else { continue };
        names.push(reference.name().as_bstr().to_str_lossy().into_owned());
    }
    names.sort();
    names.dedup();
    let null = ObjectId::null(repo.object_hash());
    for name in names {
        let Some(reference) = repo.try_find_reference(name.as_str()).ok().flatten() else {
            continue;
        };
        let mut log = reference.log_iter();
        let Ok(Some(iter)) = log.all() else {
            continue;
        };
        for line in iter {
            let Ok(line) = line else { continue };
            for id in [line.previous_oid(), line.new_oid()] {
                if id != null && repo.find_object(id).is_ok() {
                    queue_pending(repo, revs, not, String::new(), id);
                }
            }
        }
    }
    Ok(())
}

/// `add_index_objects_to_pending()` — see the `--indexed-objects` arm of
/// [`pseudo_revision_opt`]. Gitlinks are skipped (`S_ISGITLINK(ce->ce_mode)`); the
/// cache tree's trees are not modelled, because the first blob already decides the
/// only thing a blame ever gets to say about this option.
fn queue_index_objects(repo: &gix::Repository, revs: &mut Revs, not: bool) -> Result<()> {
    let Ok(index) = repo.index_or_empty() else {
        return Ok(());
    };
    for entry in index.entries() {
        if entry.mode.is_submodule() {
            continue;
        }
        queue_pending(repo, revs, not, String::new(), entry.id);
    }
    Ok(())
}

/// `handle_revision_arg_1()` (`revision.c:2155`) for one operand that is not an
/// option: the range grammar first, then the `^` exclusion mark, then the name.
///
/// `Ok(Some(code))` means the operand was refused and the diagnostic is already on
/// stderr.
fn queue_revision_arg(
    repo: &gix::Repository,
    revs: &mut Revs,
    arg: &str,
    not: bool,
) -> Result<Option<ExitCode>> {
    // ```c
    // if (get_oid_with_context(revs->repo, a_name, oc_flags, &a_oid, a_oc) ||
    //     get_oid_with_context(revs->repo, b_name, oc_flags, &b_oid, b_oc))
    //         return -1;
    // ```
    //
    // An endpoint carrying a parent mark cannot clear that: `^@`, `^!` and `^-<n>`
    // are `handle_revision_arg_1()`'s own grammar and `get_oid_1()` has no case for
    // them, so `main^!..main` is not a range — and, its `^!` not being the last two
    // characters of the operand either, not a mark, which leaves the whole word to
    // be resolved and refused. gitoxide's parser *does* accept `^!`, so without this
    // the operand would come back as a working range.
    if crate::objname::split_range(arg)
        .is_some_and(|r| crate::objname::parents_only_base(r.a) == r.a
            && crate::objname::parents_only_base(r.b) == r.b)
    {
        // `handle_dotdot_1()` resolves both endpoints with
        // `get_oid_with_context()`, so the ambiguity warning belongs to the
        // *first* of them that gets that far — the `||` short-circuits.
        crate::objname::warn_dotdot_endpoints(repo, arg);
        if let Some(message) = crate::objname::dotdot_fatal(repo, arg) {
            let mut err = std::io::stderr().lock();
            write!(err, "{message}")?;
            err.flush()?;
            return Ok(Some(ExitCode::from(128)));
        }
        let crate::objname::Dotdot::Ok { a, b } = crate::objname::dotdot(repo, arg) else {
            // Neither endpoint resolved: `handle_dotdot_1()` returns -1 and the
            // operand falls through to the single-name path below, which reports it.
            return queue_single_name(repo, revs, arg, not);
        };
        let range = crate::objname::split_range(arg).expect("split_range agreed above");
        // ```c
        // if (!symmetric) { b_flags = flags; a_flags = flags_exclude; }
        // else { … add_pending_commit_list(revs, exclude, flags_exclude);
        //        b_flags = flags; a_flags = flags | SYMMETRIC_LEFT; }
        // ```
        // `flags_exclude` is `flags ^ (UNINTERESTING | BOTTOM)`, so under a
        // preceding `--not` the two ends swap which one is the bottom.
        if range.symmetric {
            for base in merge_bases(repo, a, b) {
                queue_pending(repo, revs, !not, base.to_string(), base);
            }
            queue_pending(repo, revs, not, range.a.to_string(), a);
        } else {
            queue_pending(repo, revs, !not, range.a.to_string(), a);
        }
        queue_pending(repo, revs, not, range.b.to_string(), b);
        return Ok(None);
    }
    // The three parent marks, in git's order and with git's three different
    // outcomes:
    //
    // ```c
    // mark = strstr(arg, "^@");
    // if (mark && !mark[2]) {
    //         arg_minus_at = xmemdupz(arg, mark - arg);
    //         if (add_parents_only(revs, arg_minus_at, flags, 0)) { ret = 0; goto out; }
    // }
    // mark = strstr(arg, "^!");
    // if (mark && !mark[2]) {
    //         arg_minus_excl = xmemdupz(arg, mark - arg);
    //         if (add_parents_only(revs, arg_minus_excl, flags ^ (UNINTERESTING | BOTTOM), 0))
    //                 arg = arg_minus_excl;
    // }
    // mark = strstr(arg, "^-");
    // if (mark) {
    //         int exclude_parent = 1;
    //         if (mark[2]) {
    //                 if (strtol_i(mark + 2, 10, &exclude_parent) || exclude_parent < 1) {
    //                         ret = -1; goto out;
    //                 }
    //         }
    //         arg_minus_dash = xmemdupz(arg, mark - arg);
    //         if (add_parents_only(revs, arg_minus_dash, flags ^ (UNINTERESTING | BOTTOM), exclude_parent))
    //                 arg = arg_minus_dash;
    // }
    // ```
    //
    // `^@` *replaces* the operand when it succeeds — `handle_revision_arg_1()`
    // returns, so the commit itself is never queued and only its parents are. The
    // other two only *prepend* to it: the parents go in with the flags flipped and
    // the operand carries on to the name path below under the truncated spelling,
    // which is why `git blame HEAD^! -- f` is the range `HEAD^..HEAD` while
    // `git blame HEAD^@ -- f` digs from `HEAD`'s sole parent.
    //
    // A mark whose `add_parents_only()` answers 0 leaves `arg` alone, and the
    // operand is then handed to `get_oid_with_context()` with the mark still
    // attached. `get_oid_1()` has no case for `^@`, `^!` or `^-<n>` — they are
    // `handle_revision_arg_1()`'s grammar, not the revision parser's — so that call
    // cannot do anything but fail, and the operand comes back as
    // `fatal: bad revision '<arg>'`. `git blame <tree>^! -- f` is that path.
    let base = crate::objname::parents_only_base(arg);
    let mark = &arg[base.len()..];
    let arg = if mark.is_empty() {
        arg
    } else {
        // `^@` is parent 0 (all of them) and *replaces* the operand on success;
        // `^!` is also 0 but only prepends; `^-<n>` is the `n`th, read by
        // [`crate::objname::parents_only_parent`].
        let (nth, replaces) = match mark {
            "^@" => (0usize, true),
            "^!" => (0, false),
            // `if (… || exclude_parent < 1) { ret = -1; goto out; }` — a parent
            // number below one is refused before `add_parents_only()` is reached.
            _ => match crate::objname::parents_only_parent(&mark[2..]) {
                Some(n) if n >= 1 => (n as usize, false),
                _ => return bad_revision(arg),
            },
        };
        // `^@` keeps `flags`; the other two flip them for the parents they queue.
        if add_parents_only(repo, revs, base, if replaces { not } else { !not }, nth) {
            if replaces {
                return Ok(None);
            }
            base
        } else {
            return bad_revision(arg);
        }
    };
    queue_single_name(repo, revs, arg, not)
}

/// `setup_revisions()`'s `die(_("bad revision '%s'"), arg)`, which quotes the
/// operand as typed — mark, leading `^` and all.
fn bad_revision(arg: &str) -> Result<Option<ExitCode>> {
    let mut err = std::io::stderr().lock();
    writeln!(err, "fatal: bad revision '{arg}'")?;
    err.flush()?;
    Ok(Some(ExitCode::from(128)))
}

/// `add_parents_only()` (`revision.c:2098-2140`), which queues the parents of the
/// commit a marked operand names:
///
/// ```c
/// if (*arg == '^') { flags ^= UNINTERESTING | BOTTOM; arg++; }
/// if (repo_get_oid_committish(the_repository, arg, &oid))
///         return 0;
/// while (1) { it = get_reference(revs, arg, &oid, 0); … if (it->type != OBJ_TAG) break; … }
/// if (it->type != OBJ_COMMIT)
///         return 0;
/// commit = (struct commit *)it;
/// if (exclude_parent && exclude_parent > commit_list_count(commit->parents))
///         return 0;
/// for (parents = commit->parents, parent_number = 1; parents;
///      parents = parents->next, parent_number++) {
///         if (exclude_parent && parent_number != exclude_parent)
///                 continue;
///         it = &parents->item->object;
///         it->flags |= flags;
///         add_rev_cmdline(revs, it, arg_, REV_CMD_PARENTS_ONLY, flags);
///         add_pending_object(revs, it, arg);
/// }
/// return 1;
/// ```
///
/// `exclude_parent` is 1-based and 0 means "every parent". Every entry is queued
/// under `arg` — the *base*, with its own leading `^` already stripped — not under
/// the operand as typed.
///
/// A root commit queues nothing and still answers `true`: the loop body simply
/// never runs, and `return 1` is unconditional. That is what makes
/// `git blame <root>^@ -- <path>` an empty `revs->pending` rather than an error.
fn add_parents_only(
    repo: &gix::Repository,
    revs: &mut Revs,
    arg_: &str,
    not: bool,
    exclude_parent: usize,
) -> bool {
    let (arg, marked) = crate::objname::uninteresting_mark(arg_);
    let not = not ^ marked;
    // `repo_get_oid_committish()` and the tag-peeling loop, which is
    // [`crate::objname::resolve`] followed by [`crate::sequencer::peel_id`] — and
    // this is the resolution the ambiguity warning is counted for.
    let Some(id) = crate::objname::resolve(repo, arg) else {
        return false;
    };
    let crate::sequencer::Side::Commit(id) = crate::sequencer::peel_id(repo, id) else {
        return false;
    };
    let Ok(commit) = repo.find_commit(id) else {
        return false;
    };
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    if exclude_parent != 0 && exclude_parent > parents.len() {
        return false;
    }
    for (n, parent) in parents.iter().enumerate() {
        if exclude_parent != 0 && n + 1 != exclude_parent {
            continue;
        }
        queue_pending(repo, revs, not, arg.to_string(), *parent);
    }
    true
}

/// The tail of `handle_revision_arg_1()`: strip one leading `^`, resolve what is
/// left, and queue it under *that* name — which is why `git blame --reverse ^A ^B`
/// names `A` and `B` and not `^A` and `^B`.
///
/// ```c
/// local_flags = 0;
/// if (*arg == '^') {
///         local_flags = UNINTERESTING | BOTTOM;
///         arg++;
/// }
/// …
/// object = get_reference(revs, arg, &oid, flags ^ local_flags);
/// add_pending_object_with_path(revs, object, arg, oc.mode, oc.path);
/// ```
fn queue_single_name(
    repo: &gix::Repository,
    revs: &mut Revs,
    arg: &str,
    not: bool,
) -> Result<Option<ExitCode>> {
    let (name, marked) = crate::objname::uninteresting_mark(arg);
    // `get_reference()`'s `die("bad object %s", name)` for a well-formed but absent
    // full-length hex, which resolves without the object database being consulted.
    if let Some(message) = setup_revisions_fatal(repo, name) {
        let mut err = std::io::stderr().lock();
        write!(err, "{message}")?;
        err.flush()?;
        return Ok(Some(ExitCode::from(128)));
    }
    let Some(id) = crate::objname::resolve(repo, name) else {
        let mut err = std::io::stderr().lock();
        writeln!(err, "fatal: bad revision '{arg}'")?;
        err.flush()?;
        return Ok(Some(ExitCode::from(128)));
    };
    queue_pending(repo, revs, not ^ marked, name.to_string(), id);
    Ok(None)
}

/// `repo_get_merge_bases(the_repository, a, b, &exclude)`, whose result `A...B`
/// queues as the range's uninteresting end.
///
/// Non-commit endpoints cannot get here: [`crate::objname::dotdot`] has already
/// put a symmetric range's ends through `lookup_commit_reference()` and answered
/// `Missing` when either failed.
fn merge_bases(repo: &gix::Repository, a: ObjectId, b: ObjectId) -> Vec<ObjectId> {
    match repo.merge_bases_many(a, &[b]) {
        Ok(bases) => bases.into_iter().map(|id| id.detach()).collect(),
        Err(_) => Vec::new(),
    }
}

/// `setup_revisions()` (`revision.c:2960`) as `cmd_blame()` reaches it: the operand
/// list with the path already cut out of it, read left to right, each token either
/// an option one of the revision parsers owns or a revision to queue.
///
/// The two end-of-function refusals come last and in git's order —
/// `prepare_show_merge()` (`revision.c:3124`) before `diff_setup_done()`'s
/// `diff_check_follow_pathspec()` (`diff.c:5219`) — and both after every operand has
/// been read, which is why a bad revision anywhere on the line outranks them.
fn setup_revisions(
    repo: &gix::Repository,
    operands: &[String],
    revs: &mut Revs,
) -> Result<Option<ExitCode>> {
    let mut not = false;
    let mut excludes: Vec<String> = Vec::new();
    let mut i = 0;
    while i < operands.len() {
        let arg = operands[i].as_str();
        // A bare `-` is not an option name; `setup_revisions()` hands it to
        // `handle_revision_arg()` like any other word.
        if arg.starts_with('-') && arg.len() > 1 {
            if let Some(code) = revision_option(repo, revs, operands, &mut i, &mut not, &mut excludes)? {
                return Ok(Some(code));
            }
            i += 1;
            continue;
        }
        if let Some(code) = queue_revision_arg(repo, revs, arg, not)? {
            return Ok(Some(code));
        }
        i += 1;
    }
    if revs.show_merge && lookup_other_head(repo).is_none() {
        let mut err = std::io::stderr().lock();
        writeln!(
            err,
            "fatal: --merge requires one of the pseudorefs MERGE_HEAD, CHERRY_PICK_HEAD, \
             REVERT_HEAD or REBASE_HEAD"
        )?;
        err.flush()?;
        return Ok(Some(ExitCode::from(128)));
    }
    // `diff_check_follow_pathspec(&revs->prune_data, 1)`: `cmd_blame()` strips the
    // path out of argv before calling `setup_revisions()`, so `prune_data.nr` is
    // always 0 here and `--follow` in this position is always fatal.
    if revs.follow {
        let mut err = std::io::stderr().lock();
        writeln!(err, "fatal: --follow requires exactly one pathspec")?;
        err.flush()?;
        return Ok(Some(ExitCode::from(128)));
    }
    Ok(None)
}

/// `lookup_other_head()` (`revision.c:1975`): the first of the four pseudo-refs
/// `--merge` accepts that exists.
fn lookup_other_head(repo: &gix::Repository) -> Option<&'static str> {
    ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "REBASE_HEAD"]
        .into_iter()
        .find(|name| repo.try_find_reference(*name).ok().flatten().is_some())
}

/// One `-`-leading operand, routed the way `setup_revisions()` routes it: first
/// `handle_revision_pseudo_opt()`, then the value parsers, then the
/// "requires a value" tables.
///
/// `i` is advanced past a separate value argument the option consumed.
fn revision_option(
    repo: &gix::Repository,
    revs: &mut Revs,
    operands: &[String],
    i: &mut usize,
    not: &mut bool,
    excludes: &mut Vec<String>,
) -> Result<Option<ExitCode>> {
    let arg = operands[*i].as_str();
    if pseudo_revision_opt(repo, revs, arg, not, excludes)?.is_some() {
        return Ok(None);
    }
    if let Some(code) = rev_option_value_check(repo, arg, operands.get(*i + 1).map(String::as_str))? {
        return Ok(Some(code));
    }
    // A separate value the option consumed must not be read again as a revision.
    if takes_next_slot(arg) && *i + 1 < operands.len() {
        *i += 1;
        return Ok(None);
    }
    trailing_option_missing_value(arg).map(|code| code)
}

/// Whether the option spends the *next* argv slot on its value, which is the two
/// "requires a value" tables plus the short options that spell it that way.
fn takes_next_slot(arg: &str) -> bool {
    if let Some(body) = arg.strip_prefix("--") {
        return !body.contains('=')
            && (REV_OPT_PARSE_OPTIONS_VALUE.contains(&body) || REV_OPT_REVISION_VALUE.contains(&body));
    }
    matches!(&arg[1..], "n" | "l" | "G" | "I" | "O" | "S")
}

/// The value validation half of [`revision_option`], split out because both
/// argument positions need it: a value that is present and wrong is rejected
/// wherever the option stands, as `git blame --max-count=abc -- f.txt` and
/// `git blame -- f.txt --max-count=abc` both show (both `fatal: 'abc': not an
/// integer`, exit 128).
fn rev_option_value_check(
    repo: &gix::Repository,
    arg: &str,
    next: Option<&str>,
) -> Result<Option<ExitCode>> {
    let (name, value) = if let Some(body) = arg.strip_prefix("--") {
        match body.split_once('=') {
            Some((name, value)) => (name.to_string(), value.to_string()),
            None if rev_opt_value_parser(body).is_some() && takes_next_slot(arg) => {
                match next {
                    Some(value) => (body.to_string(), value.to_string()),
                    // No value to check; `trailing_option_missing_value` reports it.
                    None => return Ok(None),
                }
            }
            None => return Ok(None),
        }
    } else {
        match short_opt_value(arg) {
            Some((long, value)) => (long.to_string(), value.to_string()),
            None => return Ok(None),
        }
    };
    let Some((message, code)) = rev_opt_value_refusal(repo, &name, &value)? else {
        return Ok(None);
    };
    if !message.is_empty() {
        let mut err = std::io::stderr().lock();
        write!(err, "{message}")?;
        err.flush()?;
    }
    Ok(Some(code))
}

/// Split the collected positionals into `[<rev>...] <file>` following git
/// blame's DWIM rules, then resolve the revision. Reproduces `cmd_blame`'s
/// argument handling for the presence/absence of the `--` separator.
fn resolve_targets(repo: &gix::Repository, opts: &mut Options) -> Result<Targets> {
    // Determine the revision arguments (in order) and the single path.
    //
    // `revs` is `cmd_blame()`'s argv tail after it has cut the path out, so it
    // still holds the pseudo-revision options `handle_revision_opt()` left in
    // place — their position among the operands is what decides `revs->pending`
    // order, and that order is what `find_single_final()`'s diagnostics quote.
    let (revs, file): (Vec<String>, String) = match opts.post.take() {
        // `--` was present: everything after it is a pathspec. blame accepts
        // exactly one path; a trailing second token is DWIM'd as a revision.
        Some(post) => {
            let pre = std::mem::take(&mut opts.pre);
            match post.len() {
                0 => return Ok(Targets::Usage),
                1 => (pre, post.into_iter().next().unwrap()),
                // `blame -- <file> <rev>`: only legal with no revs before `--`
                // (git's `if (argc != 4) usage_with_options(...)`).
                2 if pre.is_empty() => {
                    let mut it = post.into_iter();
                    let file = it.next().unwrap();
                    let rev = it.next().unwrap();
                    // `cmd_blame` reorders this shape to `<rev> -- <path>` and hands
                    // the whole array to `setup_revisions()`, which routes anything
                    // starting with `-` to `handle_revision_opt()` / `diff_opt_parse()`
                    // instead of `get_oid()`.
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

    // `setup_revisions()` queues every operand into `revs->pending` first, dying on
    // the first one it cannot resolve; only then does `setup_scoreboard()` run
    // `find_single_final()` / `find_single_initial()` over what was queued. So a bad
    // revision anywhere on the line outranks either of those diagnostics, whichever
    // order the operands are in.
    let mut queued = Revs {
        // `--merge` and `--ancestry-path` set their flag from either position; the
        // parse loop has already recorded an occurrence in front of the `--`.
        show_merge: opts.show_merge,
        ancestry_path: opts.ancestry_path,
        ..Revs::default()
    };
    if let Some(code) = setup_revisions(repo, &revs, &mut queued)? {
        return Ok(Targets::Fatal(code));
    }

    if opts.reverse {
        return resolve_reverse_targets(repo, opts, queued, file);
    }

    let suspect = match find_single_final(&queued) {
        Ok(found) => found,
        Err(message) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "fatal: {message}")?;
            err.flush()?;
            return Ok(Targets::Fatal(ExitCode::from(128)));
        }
    };

    // `prepare_revision_walk()`'s `limit_list()`, which marks every ancestor of a
    // bottom commit `UNINTERESTING` before `assign_blame()` reads the flag
    // (`revision.c:1448-1452` for the `--ancestry-path` refusal that shares the
    // same list). `assign_blame()` then stops at any commit carrying it.
    opts.bottom = uninteresting_closure(repo, &queued)?;
    if queued.ancestry_path && opts.bottom.is_empty() {
        // `die("--ancestry-path given but there are no bottom commits")`, raised from
        // inside `prepare_revision_walk()` — so after `find_single_final()` and after
        // the fake working-tree commit's `lstat`, which is why it is reported here
        // rather than in `setup_revisions()`.
        opts.ancestry_path_pending = true;
    }

    opts.rev = suspect.as_ref().map(|(n, _)| n.clone());
    opts.suspect_id = suspect.map(|(_, id)| id);
    opts.file = file;
    Ok(Targets::Resolved)
}

/// Every commit reachable from a bottom commit, bottoms included — git's
/// `UNINTERESTING` flag once `limit_list()` and `mark_parents_uninteresting()`
/// have finished spreading it.
///
/// The closure is computed up front rather than propagated as the blame walk
/// reaches each bottom, because ancestry can re-enter through the far side of a
/// merge: a commit the blame reaches by another path is already marked in git and
/// would not be if the mark only travelled along the blame's own chain.
fn uninteresting_closure(
    repo: &gix::Repository,
    revs: &Revs,
) -> Result<std::collections::HashSet<ObjectId>> {
    let mut out: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();
    // Only commit-ish bottoms limit a walk; a tree or blob queued as uninteresting
    // (`--indexed-objects --not`) has no ancestry to spread the flag along.
    let bottoms: Vec<ObjectId> = revs
        .pending
        .iter()
        .filter(|p| revs.uninteresting.contains(&p.id))
        .filter_map(|p| match p.object {
            crate::sequencer::Side::Commit(id) => Some(id),
            _ => None,
        })
        .collect();
    if bottoms.is_empty() {
        return Ok(out);
    }
    let walk = repo
        .rev_walk(bottoms)
        .sorting(gix::revision::walk::Sorting::BreadthFirst)
        .all()?;
    for info in walk {
        let Ok(info) = info else { continue };
        out.insert(info.id);
    }
    Ok(out)
}

/// The `--reverse` half of `setup_scoreboard()`: `find_single_initial()` picks the one
/// negative commit, `dwim_reverse_initial()` invents one when the line named a single
/// positive commit and nothing else, and the remaining positives are where the forward
/// walk stops.
fn resolve_reverse_targets(
    repo: &gix::Repository,
    opts: &mut Options,
    mut queued: Revs,
    file: String,
) -> Result<Targets> {
    let mut from = match find_single_initial(&queued) {
        Ok(found) => found,
        Err(message) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "fatal: {message}")?;
            err.flush()?;
            return Ok(Targets::Fatal(ExitCode::from(128)));
        }
    };

    // `dwim_reverse_initial()` (`blame.c:2688`): with `revs->pending.nr == 1` and that
    // sole entry commit-ish, `git blame --reverse ONE -- PATH` means `ONE..HEAD` — the
    // entry is marked `UNINTERESTING` and `HEAD` is queued behind it.
    if from.is_none() && queued.pending.len() == 1 {
        let only = &queued.pending[0];
        if let crate::sequencer::Side::Commit(id) = only.object {
            if let Ok(head) = repo.head_id() {
                if let crate::sequencer::Side::Commit(head) =
                    crate::sequencer::peel_id(repo, head.detach())
                {
                    from = Some((only.name.clone(), id));
                    queued.uninteresting.insert(only.id);
                    queue_pending(repo, &mut queued, false, "HEAD".to_string(), head);
                }
            }
        }
    }

    let Some((from_name, from_id)) = from else {
        return no_commit_to_dig_up_from();
    };

    // The forward walk's tips: `revs->pending` minus the negative one, which is what
    // `prepare_revision_walk()` builds `revs->children` over.
    let tips: Vec<ObjectId> = queued
        .pending
        .iter()
        .filter(|p| !queued.uninteresting.contains(&p.id))
        .filter_map(|p| match p.object {
            crate::sequencer::Side::Commit(id) => Some(id),
            _ => None,
        })
        .collect();

    // `setup_scoreboard()`: `--reverse --first-parent` needs a single latest commit to build the
    // first-parent chain decoration from (`blame.c:2828-2832`).
    if opts.first_parent && tips.len() != 1 {
        let mut err = std::io::stderr().lock();
        writeln!(
            err,
            "fatal: --reverse and --first-parent together require specified latest commit"
        )?;
        err.flush()?;
        return Ok(Targets::Fatal(ExitCode::from(128)));
    }

    opts.rev = Some(from_name);
    opts.suspect_id = Some(from_id);
    opts.reverse_from = Some(from_id);
    opts.reverse_tips = tips;
    opts.file = file;
    Ok(Targets::Resolved)
}

fn no_commit_to_dig_up_from() -> Result<Targets> {
    let mut err = std::io::stderr().lock();
    writeln!(err, "fatal: No commit to dig up from?")?;
    err.flush()?;
    Ok(Targets::Fatal(ExitCode::from(128)))
}

/// git's `revs->children` decoration for a `--reverse` range, which is what `first_scapegoat()`
/// returns instead of a commit's parents.
///
/// `set_children()` (`revision.c`) walks `revs->commits` — the commits the range selected, newest
/// first — and inserts each at the *front* of its parents' child lists while
/// `prepare_revision_walk()` runs, so a commit's children come out oldest first. That order is the
/// order `pass_blame()` offers the scapegoats in, which decides which of them gets a chunk that
/// both could claim and which one becomes `blame_origin::previous`.
fn reverse_children(
    repo: &gix::Repository,
    from: ObjectId,
    tips: &[ObjectId],
) -> Result<gix::blame::Children> {
    let mut children: gix::blame::Children = gix::blame::Children::default();
    let walk = repo
        .rev_walk(tips.iter().copied())
        .with_hidden(Some(from))
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ))
        .all()?;
    for info in walk {
        let info = info?;
        let child = info.id;
        for parent in info.parent_ids() {
            children.entry(parent.detach()).or_default().insert(0, child);
        }
    }
    Ok(children)
}

/// `setup_scoreboard()`'s `--reverse --first-parent` decoration (`blame.c:2842-2859`): instead of
/// every child in the range, each commit on the first-parent chain from the latest commit down to
/// `from` is recorded as the sole child of the commit before it.
fn reverse_first_parent_children(
    repo: &gix::Repository,
    from: ObjectId,
    latest: ObjectId,
) -> Result<Option<gix::blame::Children>> {
    let mut children: gix::blame::Children = gix::blame::Children::default();
    let mut c = latest;
    loop {
        if c == from {
            return Ok(Some(children));
        }
        let commit = repo.find_commit(c)?;
        let Some(parent) = commit.parent_ids().next() else {
            // git leaves the loop on a parentless commit and then finds it is not the initial one.
            return Ok(None);
        };
        children.insert(parent.detach(), vec![c]);
        c = parent.detach();
    }
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
        DateClass::Unsupported(f) => anyhow::bail!("unsupported --date mode: {f}"),
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
    /// The raw `-L` arguments in command-line order. git keeps them as an
    /// `OPT_STRING_LIST` and only interprets them once the final image is in
    /// hand, because every form but the plain numeric one needs the file's text.
    line_specs: Vec<String>,
    /// `line_specs` resolved against the final image; empty until then.
    ranges: Vec<RangeInclusive<u32>>,
    /// `--first-parent`: follow only the first parent of every commit, which is
    /// git's `revs->first_parent_only` applied in `first_scapegoat()`.
    first_parent: bool,
    /// `--reverse`: git's `sb->reverse`. The walk runs forwards through the given range, so a
    /// line is attributed to the last commit that still had it rather than the first that
    /// introduced it.
    reverse: bool,
    /// The negative endpoint of a `--reverse` range, i.e. git's `find_single_initial()`: the
    /// commit whose version of the file is the final image and where the forward walk starts.
    reverse_from: Option<ObjectId>,
    /// The positive endpoints of a `--reverse` range — where the forward walk stops. git's
    /// `revs->pending` minus the negative one.
    reverse_tips: Vec<ObjectId>,
    /// git's `UNINTERESTING` closure for a forward blame: the bottom commits a range
    /// named and everything they reach. `assign_blame()` does not pass blame from a
    /// commit carrying the flag, so these keep the lines the range did not touch and
    /// print with the boundary marker.
    bottom: std::collections::HashSet<ObjectId>,
    /// `--merge` seen in front of the `--`, where `parse_revision_opt()` consumes it
    /// into `revs->show_merge`; `prepare_show_merge()` acts on it either way.
    show_merge: bool,
    /// `--ancestry-path` seen in front of the `--`, same reasoning.
    ancestry_path: bool,
    /// `--ancestry-path` with nothing for `collect_bottom_commits()` to find, whose
    /// `die()` `prepare_revision_walk()` raises — later than every diagnostic
    /// `resolve_targets` can, which is why it is carried rather than reported there.
    ancestry_path_pending: bool,
    /// `--show-stats`: print `blame_scoreboard`'s three work counters after the blame.
    show_stats: bool,
    /// `--score-debug` (git's `OUTPUT_SHOW_SCORE`): add `blame_entry_score()` and the entry's
    /// `blame_origin::refcnt` to every human-format line.
    score_debug: bool,
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
    /// `-M[<score>]`: the value is git's `sb->move_score`.
    detect_moved: Option<u32>,
    /// `-C[<score>]`, `-C -C` and `-C -C -C`: git's `PICKAXE_BLAME_COPY*` bits together with
    /// `sb->copy_score`.
    detect_copied: Option<gix::blame::CopyDetection>,
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

/// What one `parse_options()` pass produced: either the parsed command line, or
/// the `-h` short-circuit git answers from inside the loop.
enum ParseOutcome {
    Help,
    /// `--help-all`: the same block with the `PARSE_OPT_HIDDEN` `--minimal`
    /// left in, on the same stream at the same 129.
    HelpAll,
    /// An option neither `options[]` nor `handle_revision_opt()` recognises.
    /// The payload is the name git puts in its `unknown option` diagnostic —
    /// see [`unknown_option_name`] for why that is sometimes `(null)`.
    Unknown(String),
    /// An option *callback* rejected its value: `parse-options` turns the
    /// callback's non-zero return into `PARSE_OPT_ERROR`, which `cmd_blame`'s
    /// own `parse_options_step` loop answers with a bare `exit(129)`
    /// (`builtin/blame.c:1016`) — the callback's `error()` line is the whole
    /// diagnostic, with no usage block after it. The payload is that line's
    /// text without the `error: ` prefix.
    OptError(String),
    /// An abbreviation that two entries of `options[]` both answer to, as
    /// `(<body as typed>, <first candidate>, <second candidate>)`. The
    /// explanation goes to stderr and the usage block to stdout, which is what
    /// makes it its own outcome rather than an [`ParseOutcome::Unknown`].
    Ambiguous(String, String, String),
    /// A refusal whose whole diagnostic is already on stderr — the value parsers
    /// `handle_revision_opt()` and `diff_opt_parse()` reach, which spell their own
    /// message and whose status is 128 or 129 depending on which of them owns the
    /// option. Nothing further is printed, in particular no usage block.
    Reported(ExitCode),
    Opts(Box<Options>),
}

/// The operand of git's `error: unknown option \`%s'` for the argument at
/// `args[idx]`, given that `positionals` positional arguments preceded it.
///
/// `cmd_blame` drives `parse_options_step` itself and hands the leftover to
/// `parse_revision_opt()`, which prints `ctx->argv[0]`. Before printing,
/// `handle_revision_opt()` has already called `overwrite_argv()`, which moves the
/// argument down into `ctx->out[ctx->cpidx]` — the same array as `ctx->argv` —
/// and NULs out the slot it came from. The two indices only differ once a
/// recognised option has consumed an argv slot without pushing anything to
/// `out`, and `overwrite_argv` short-circuits when they coincide, so the name
/// survives for an unknown option that is the first option on the line and is
/// lost for every later one. Verified against git 2.55.0:
/// `git blame --no-bogus a.txt` names the option, `git blame -w --no-bogus a.txt`
/// prints `(null)`.
fn unknown_option_name(args: &[String], idx: usize, positionals: usize) -> String {
    if idx > positionals {
        "(null)".to_string()
    } else {
        args[idx].clone()
    }
}

/// Every long option name `git blame` accepts, whether or not this port
/// implements it: `builtin/blame.c`'s own `options[]`, plus everything
/// `parse_revision_opt()` forwards to — the names `revision.c`'s
/// `handle_revision_opt()` matches and the `parseopts[]` table
/// `diff.c:prep_parse_options()` builds — all read off git v2.55.0.
///
/// The set exists to keep the two failure modes apart: an option outside it is a
/// name git itself rejects, so the port must reproduce git's
/// `error: unknown option` and its exit 129, while an option inside it that this
/// port has no implementation for must be refused on its own terms rather than
/// misreported as a typo.
static GIT_BLAME_LONG_OPTIONS: &[&str] = &[
    "abbrev", "abbrev-commit", "after", "all", "all-match", "alternate-refs", "always",
    "ancestry-path", "anchored", "author", "author-date-order", "basic-regexp", "before",
    "binary", "bisect", "boundary", "branches", "break-rewrites", "check", "cherry",
    "cherry-mark", "cherry-pick", "children", "color", "color-by-age", "color-lines",
    "color-moved", "color-moved-ws", "color-words", "committer", "compact-summary", "contents",
    "count", "cumulative", "date", "date-order", "default", "default-prefix", "dense",
    "diff-algorithm", "diff-filter", "dirstat", "dirstat-by-file", "do-walk", "dst-prefix",
    "encode-email-headers", "encoding", "exclude", "exclude-first-parent-only",
    "exclude-hidden", "exclude-promisor-objects", "exit-code", "expand-tabs", "ext-diff",
    "extended-regexp", "find-copies", "find-copies-harder", "find-object", "find-renames",
    "first-parent", "fixed-strings", "follow", "format", "full-diff", "full-history",
    "full-index", "function-context", "glob", "graph", "graph-lane-limit", "grep",
    "grep-reflog", "histogram", "ignore-all-space", "ignore-blank-lines", "ignore-cr-at-eol",
    "ignore-matching-lines", "ignore-missing", "ignore-rev", "ignore-revs-file",
    "ignore-space-at-eol", "ignore-space-change", "ignore-submodules", "in-commit-order",
    "incremental", "indent-heuristic", "indexed-objects", "inter-hunk-context", "invert-grep",
    "irreversible-delete", "ita-invisible-in-index", "ita-visible-in-index", "left-only",
    "left-right", "line-porcelain", "line-prefix", "log-size", "max-age", "max-count",
    "max-count-oldest", "max-depth", "max-parents", "maximal-only", "merge", "merges",
    "min-age", "min-parents", "minimal", "name-only", "name-status", "no-abbrev",
    "no-abbrev-commit", "no-commit-id", "no-encode-email-headers", "no-expand-tabs",
    "no-graph", "no-kept-objects", "no-max-parents", "no-merges", "no-min-parents", "no-notes",
    "no-patch", "no-prefix", "no-renames", "no-show-signature", "no-standard-notes", "no-walk",
    "not", "notes", "numstat", "objects", "objects-edge", "objects-edge-aggressive", "oneline",
    "output", "output-indicator-context", "output-indicator-new", "output-indicator-old",
    "parents", "patch", "patch-with-raw", "patch-with-stat", "patience", "perl-regexp",
    "pickaxe-all", "pickaxe-regex", "porcelain", "pretty", "progress", "quiet", "raw",
    "reflog", "regexp-ignore-case", "relative", "relative-date", "remotes", "remove-empty",
    "rename-empty", "reverse", "right-only", "root", "rotate-to", "score-debug", "shortstat",
    "show-email", "show-linear-break", "show-name", "show-notes", "show-notes-by-default",
    "show-number", "show-pulls", "show-signature", "show-stats", "simplify-by-decoration",
    "simplify-merges", "since", "since-as-filter", "skip", "skip-to", "sparse", "src-prefix",
    "standard-notes", "stat", "stat-count", "stat-graph-width", "stat-name-width",
    "stat-width", "submodule", "summary", "tags", "text", "textconv", "topo-order", "unified",
    "unpacked", "until", "verify-objects", "walk-reflogs", "word-diff", "word-diff-regex",
    "ws-error-highlight",
];

/// Whether one of git's own parsers would recognise the long option `a`, in any
/// of the spellings `parse_long_opt()` accepts: `--name`, `--name=<value>` and
/// the `--no-name` negation.
fn git_knows_long_option(a: &str) -> bool {
    let Some(body) = a.strip_prefix("--") else {
        return false;
    };
    // This command's own table decides first, and it decides negatability too:
    // `--diff-algorithm` is `PARSE_OPT_NONEG`, so `--no-diff-algorithm` is an
    // unknown option however familiar the stem looks.
    if !matches!(super::resolve_long(LONG_OPTS, body), super::Resolved::Unknown) {
        return true;
    }
    let name = body.split('=').next().unwrap_or(body);
    let stem = name.strip_prefix("no-").unwrap_or(name);
    if LONG_OPTS.iter().any(|o| o.name == stem) {
        return false;
    }
    // Everything else is `handle_revision_opt()`'s, which matches its names
    // exactly (no abbreviation) and spells its own negations.
    GIT_BLAME_LONG_OPTIONS.contains(&name)
        || name
            .strip_prefix("no-")
            .is_some_and(|n| GIT_BLAME_LONG_OPTIONS.contains(&n))
}

/// git's "pseudo revision arguments" — the first block of `handle_revision_opt()`
/// (`revision.c:2325-2340`), which does not act on them at all:
///
/// ```c
/// if (!strcmp(arg, "--all") || !strcmp(arg, "--branches") ||
///     !strcmp(arg, "--tags") || !strcmp(arg, "--remotes") ||
///     !strcmp(arg, "--reflog") || !strcmp(arg, "--not") ||
///     !strcmp(arg, "--no-walk") || !strcmp(arg, "--do-walk") ||
///     !strcmp(arg, "--bisect") || starts_with(arg, "--glob=") ||
///     !strcmp(arg, "--indexed-objects") ||
///     !strcmp(arg, "--alternate-refs") ||
///     starts_with(arg, "--exclude=") || starts_with(arg, "--exclude-hidden=") ||
///     starts_with(arg, "--branches=") || starts_with(arg, "--tags=") ||
///     starts_with(arg, "--remotes=") || starts_with(arg, "--no-walk="))
/// {
///         overwrite_argv(unkc, unkv, &argv[0], opt);
///         return 1;
/// }
/// ```
///
/// `overwrite_argv()` moves the token down into the kept-argv array, so it survives
/// the parse loop and `setup_revisions()` reads it again *in place* — which is what
/// makes `git blame HEAD --all -- f` queue `HEAD` before `--all`'s refs and
/// `git blame --all HEAD -- f` queue them the other way round. Keeping them among
/// the positionals here is that same `overwrite_argv()`.
fn is_pseudo_revision_arg(a: &str) -> bool {
    matches!(
        a,
        "--all"
            | "--branches"
            | "--tags"
            | "--remotes"
            | "--reflog"
            | "--not"
            | "--no-walk"
            | "--do-walk"
            | "--bisect"
            | "--indexed-objects"
            | "--alternate-refs"
    ) || ["--glob=", "--exclude=", "--exclude-hidden=", "--branches=", "--tags=", "--remotes=", "--no-walk="]
        .iter()
        .any(|p| a.starts_with(p))
}

/// Whether [`rev_option_value_check`] knows the value parser this token's option
/// uses, i.e. whether its value is one git *refuses* rather than merely stores.
fn rev_opt_value_checked(arg: &str) -> bool {
    if let Some(body) = arg.strip_prefix("--") {
        let name = body.split('=').next().unwrap_or(body);
        return rev_opt_value_parser(name).is_some();
    }
    short_opt_value(arg).is_some()
}

impl Options {
    fn parse(
        repo: &gix::Repository,
        args: &[String],
        defaults: ConfigDefaults,
    ) -> Result<ParseOutcome> {
        let ConfigDefaults {
            show_email: show_email_default,
            show_root: show_root_default,
            blank_boundary: blank_boundary_default,
            ignore_revs_file: ignore_revs_file_default,
            mark_unblamable_lines,
            mark_ignored_lines,
        } = defaults;
        let mut line_specs: Vec<String> = Vec::new();
        let mut first_parent = false;
        let mut reverse = false;
        let mut show_merge = false;
        let mut ancestry_path = false;
        let mut show_stats = false;
        let mut score_debug = false;
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
        let mut detect_moved: Option<u32> = None;
        // How many `-C`s were given and the last explicit `-C<score>` / `-M<score>`, which git
        // keeps in `blame_copy_score` / `blame_move_score` and only applies when non-zero.
        let mut copy_levels = 0u32;
        let mut copy_score: Option<u32> = None;
        let mut move_score: Option<u32> = None;
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
            // Resolve the long name the way `parse_long_opt()` resolves it. Only
            // names this command's own table claims are rewritten; anything it
            // does not claim is handed back untouched and goes on to the
            // revision-option arms below, exactly as `parse_options_step()`
            // hands `PARSE_OPT_UNKNOWN` to `parse_revision_opt()`.
            let resolved = match super::canonical_long(a, LONG_OPTS) {
                super::Long::Name(name) => name,
                super::Long::Ambiguous(first, second) => {
                    let body = a.strip_prefix("--").unwrap_or(a).to_string();
                    return Ok(ParseOutcome::Ambiguous(body, first, second));
                }
            };
            let a = resolved.as_ref();
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
                // `parse_options()` answers `-h` where it finds it, without
                // looking at the rest of the command line.
                "-h" => return Ok(ParseOutcome::Help),
                // `if (internal_help && !strcmp(arg + 2, "help-all"))`
                // (parse-options.c:1122), the same answer over `USAGE_FULL`.
                "--help-all" => return Ok(ParseOutcome::HelpAll),
                // `--first-parent` is a rev-list option blame forwards to
                // `setup_revisions()`, which sets `revs->first_parent_only`.
                "--first-parent" => first_parent = true,
                "--no-first-parent" => first_parent = false,
                // `cmd_blame` rewrites `--reverse` to `--children` before handing the argument to
                // `handle_revision_opt()` and sets its own `reverse` flag
                // (`builtin/blame.c:1027-1029`), which is what turns the whole algorithm around.
                "--reverse" => reverse = true,
                // git's `OPT_BOOL(0, "show-stats", …)` and `OPT_BIT(0, "score-debug", …,
                // OUTPUT_SHOW_SCORE)`, each with the negation its declaration implies.
                "--show-stats" => show_stats = true,
                "--score-debug" => score_debug = true,
                // `--minimal` is `XDF_NEED_MINIMAL` in `revs.diffopt.xdl_opts`,
                // which is what `Algorithm::MyersMinimal` is: Myers followed by
                // the exhaustive pass that removes the remaining non-minimal
                // placements. It is the same knob `--diff-algorithm=minimal`
                // sets, so the last of the two on the command line wins.
                "--minimal" => diff_algorithm = Some(gix::diff::blob::Algorithm::MyersMinimal),
                // `--patience` is `XDF_PATIENCE_DIFF` in the same `xdl_opts` word,
                // i.e. the knob `--diff-algorithm=patience` sets, so the last of the
                // two on the command line wins — the exact shape of `--minimal`
                // above.
                "--patience" => diff_algorithm = Some(gix::diff::blob::Algorithm::Patience),
                // `--indent-heuristic` sets `XDF_INDENT_HEURISTIC`, which
                // `diff.indentHeuristic` already sets by default
                // (`diff.c:57 static int diff_indent_heuristic = 1;`), so the
                // flag asks for the state the engine is already in: the blame
                // diffs run through `gix_diff::blob::compact::change_compact`,
                // git's `xdl_change_compact()` with the heuristic applied
                // unconditionally. `--no-indent-heuristic` would turn it *off*
                // and has no path through that code, so it stays refused below
                // rather than being accepted as a no-op.
                "--indent-heuristic" => {}
                // `optname()` names a short option by its character, so this is
                // ``switch `L'`` and not ``option `-L'``; the refusal is
                // parse-options' own `error:` line at 129, never a `zvcs:` gap
                // message at exit 1 (parse-options.c:30-45, :59-60).
                "-L" => {
                    i += 1;
                    line_specs.push(super::value_at(args, i, a)?.to_string());
                }
                // `OPT__ABBREV` is `PARSE_OPT_OPTARG`, so a bare `--abbrev` never
                // reaches for the next argument: the callback runs with a NULL arg
                // and stores `DEFAULT_ABBREV`, which is -1 unless `core.abbrev` names
                // a number — the same "work it out" state as no `--abbrev` at all.
                // `git blame --abbrev 8 f` therefore leaves `8` as a revision, which
                // is why stock answers it with `fatal: bad revision '8'`.
                "--abbrev" => abbrev = None,
                // `--date <mode>` / `--date=<mode>` set the default date format for
                // the human-format timestamp column (validated against the repo in
                // `blame`, so the last one wins here and errors surface there).
                "--date" => {
                    i += 1;
                    let Some(v) = args.get(i) else {
                        // `--date` is `revision.c`'s option, not one of blame's own
                        // `options[]`: `handle_revision_opt()` matches it by hand and
                        // words its refusal `fatal: Option '--<name>' requires a value`
                        // at 128 — not parse-options' `error: option `<name>'` at 129.
                        // The two tables in [`trailing_option_missing_value`] are what
                        // keep the halves apart, so the answer comes from there.
                        let code = trailing_option_missing_value(a)?
                            .expect("--date is in REV_OPT_REVISION_VALUE");
                        return Ok(ParseOutcome::Reported(code));
                    };
                    date_arg = Some(v.clone());
                }
                _ if a.starts_with("--date=") => {
                    date_arg = Some(a["--date=".len()..].to_string());
                }
                "--diff-algorithm" => {
                    i += 1;
                    let v = super::value_at(args, i, a)?;
                    match parse_diff_algorithm(v) {
                        Some(alg) => diff_algorithm = Some(alg),
                        None => {
                            return Ok(ParseOutcome::OptError(DIFF_ALGORITHM_ERROR.to_string()))
                        }
                    }
                }
                _ if a.starts_with("--diff-algorithm=") => {
                    match parse_diff_algorithm(&a["--diff-algorithm=".len()..]) {
                        Some(alg) => diff_algorithm = Some(alg),
                        None => {
                            return Ok(ParseOutcome::OptError(DIFF_ALGORITHM_ERROR.to_string()))
                        }
                    }
                }
                "--contents" => {
                    i += 1;
                    contents = Some(super::value_at(args, i, a)?.to_string());
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
                    ignore_rev.push(super::value_at(args, i, a)?.to_string());
                }
                _ if a.starts_with("--ignore-rev=") => {
                    ignore_rev.push(a["--ignore-rev=".len()..].to_string());
                }
                "--no-ignore-rev" => ignore_rev.clear(),
                "--ignore-revs-file" => {
                    i += 1;
                    ignore_revs_file.push(super::value_at(args, i, a)?.to_string());
                }
                _ if a.starts_with("--ignore-revs-file=") => {
                    ignore_revs_file.push(a["--ignore-revs-file=".len()..].to_string());
                }
                "--no-ignore-revs-file" => ignore_revs_file.clear(),
                // The `--no-` forms of `--show-stats` and `--score-debug`, whose
                // positive forms need substrate this port does not have. Each
                // positive default is off, so the negated form requests exactly the
                // behavior this port already produces; stock git emits
                // byte-identical stdout for them (verified), so they are accepted as
                // no-ops rather than rejected. The positive forms remain refused.
                // `-M[<score>]` (`blame_move_callback`): the score is an optional *attached*
                // argument, and `parse_score` yields 0 for anything `strtoul` cannot consume whole.
                // git only overrides `sb->move_score` `if (blame_move_score)`, so a 0 — whether
                // written as `-M0` or produced by an unparsable argument — keeps the default.
                "-M" => detect_moved = Some(detect_moved.unwrap_or(BLAME_DEFAULT_MOVE_SCORE)),
                _ if a.starts_with("-M") => {
                    move_score = parse_score(&a["-M".len()..]);
                    detect_moved = Some(move_score.unwrap_or(BLAME_DEFAULT_MOVE_SCORE));
                }
                // `-C[<score>]` (`blame_copy_callback`): each further `-C` widens the search, and
                // any `-C` also turns `-M` on.
                "-C" => copy_levels += 1,
                _ if a.starts_with("-C") => {
                    copy_levels += 1;
                    copy_score = parse_score(&a["-C".len()..]);
                }
                "--no-incremental" => incremental = false,
                "--no-show-stats" => show_stats = false,
                "--no-score-debug" => score_debug = false,
                _ if a.starts_with("-L") => line_specs.push(a[2..].to_string()),
                // `--encoding=<enc>` (`OPT_STRING(0, "encoding", ...)`): the encoding the
                // author names and summaries are written in. Everything here is already
                // UTF-8, so `utf-8` and `none` are what the output is either way; any
                // other encoding would need a transcoding step that is not ported, and
                // silently emitting UTF-8 under a Latin-1 request would be wrong.
                "--encoding" => {
                    let v = args.get(i + 1).cloned().unwrap_or_default();
                    i += 1;
                    if !encoding_is_passthrough(&v) {
                        bail!(
                            "unsupported option: --encoding={v} (only utf-8 and none are \
                             ported; transcoding author names is not)"
                        );
                    }
                }
                _ if a.starts_with("--encoding=") => {
                    let v = &a["--encoding=".len()..];
                    if !encoding_is_passthrough(v) {
                        bail!(
                            "unsupported option: {a} (only utf-8 and none are ported; \
                             transcoding author names is not)"
                        );
                    }
                }
                _ if a.starts_with("--abbrev=") => {
                    match parse_abbrev_cb(&a["--abbrev=".len()..]) {
                        Ok(v) => abbrev = Some(v),
                        Err(msg) => return Ok(ParseOutcome::OptError(msg.to_string())),
                    }
                }
                // `revs->show_merge` and `revs->ancestry_path`, both of which
                // `handle_revision_opt()` consumes here and neither of which is
                // acted on until much later — so an occurrence in this position is
                // indistinguishable from one in the revision slot.
                "--merge" => show_merge = true,
                "--ancestry-path" => ancestry_path = true,
                // `--follow` is consumed the same way, but the line right after
                // `cmd_blame()`'s parse loop clears `follow_renames` again
                // (`builtin/blame.c:1035`), so unlike the two above it has no effect
                // at all from in front of the `--`.
                "--follow" | "--no-follow" => {}
                // `handle_revision_opt()`'s pseudo-revision block, which keeps the
                // token among the operands for `setup_revisions()` — see
                // [`is_pseudo_revision_arg`].
                _ if is_pseudo_revision_arg(a) => pre.push(a.to_string()),
                // An option whose *value* one of git's parsers refuses. This is
                // asked in both argument positions because git refuses in both:
                // `git blame --max-count=abc -- f` and `git blame -- f --max-count=abc`
                // are the same `fatal: 'abc': not an integer`.
                _ if rev_opt_value_checked(a) => {
                    let next = args.get(i + 1).map(String::as_str);
                    if let Some(code) = rev_option_value_check(repo, a, next)? {
                        return Ok(ParseOutcome::Reported(code));
                    }
                    if takes_next_slot(a) && i + 1 < args.len() {
                        i += 1;
                    }
                }
                // A long option no git parser knows: git answers with its own
                // diagnostic and the usage, so reproduce that rather than
                // claiming the option merely is not ported.
                _ if a.starts_with("--") && !git_knows_long_option(a) => {
                    return Ok(ParseOutcome::Unknown(unknown_option_name(args, i, pre.len())));
                }
                // A short option that git's *revision* parser owns and whose value
                // is simply absent — `-S`, `-G`, `-I`, `-O`, `-n`. `get_arg()`
                // reports it as ``switch `<c>' requires a value`` at 129 (and `-n`
                // through `handle_revision_opt()`'s own check at 128); reaching
                // the gap message below instead claimed the option was unported
                // when it is merely incomplete.
                _ if a.starts_with('-') && a.len() > 1 => {
                    if let Some(code) = trailing_option_missing_value(a)? {
                        return Ok(ParseOutcome::Reported(code));
                    }
                    bail!("unsupported option: {a}")
                }
                _ => pre.push(a.to_string()),
            }
            i += 1;
        }

        // `blame_copy_callback`: the first `-C` turns on copy detection *and* `-M`, the second
        // adds COPY_HARDER and the third COPY_HARDEST. `sb->copy_score` is only overridden by a
        // non-zero `-C<score>`, exactly as `sb->move_score` is by `-M<score>`.
        let detect_copied = (copy_levels > 0).then(|| gix::blame::CopyDetection {
            score: copy_score.unwrap_or(BLAME_DEFAULT_COPY_SCORE),
            harder: copy_levels >= 2,
            hardest: copy_levels >= 3,
        });
        if detect_copied.is_some() {
            detect_moved = Some(move_score.unwrap_or(BLAME_DEFAULT_MOVE_SCORE));
        }

        // The revision/path split and its validation happen against the repo in
        // `resolve_targets`, since git's DWIM (`is_a_rev`) needs the object db.
        Ok(ParseOutcome::Opts(Box::new(Options {
            rev: None,
            file: String::new(),
            suspect_id: None,
            pre,
            post,
            line_specs,
            ranges: Vec::new(),
            first_parent,
            reverse,
            reverse_from: None,
            reverse_tips: Vec::new(),
            bottom: std::collections::HashSet::new(),
            show_merge,
            ancestry_path,
            ancestry_path_pending: false,
            show_stats,
            score_debug,
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
            detect_moved,
            detect_copied,
            diff_algorithm,
            contents,
            ignore_rev,
            ignore_revs_file,
            mark_unblamable_lines,
            mark_ignored_lines,
            date_arg,
            // Overwritten in `blame` once blame.date / `--date` are resolved.
            date_mode: DateMode::Iso8601,
        })))
    }
}

/// The text of `blame_diff_algorithm_callback`'s `error()`
/// (`builtin/blame.c:868`), reported without a usage block at exit 129.
const DIFF_ALGORITHM_ERROR: &str =
    "option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"";

/// git's `diff-algorithm` value parser: the names `parse_algorithm_value()` accepts.
///
/// `None` is git's `parse_algorithm_value() < 0`, which
/// `blame_diff_algorithm_callback` turns into [`DIFF_ALGORITHM_ERROR`] and
/// `parse-options` into exit 129 — *not* a `die()`, so it must not be reported as
/// `fatal:`. Every name git accepts maps to an algorithm the vendored
/// `gix-imara-diff` implements, `patience` included (its `patience.rs` is git's
/// `xdiff/xpatience.c`), so no valid value is refused here — refusing one during
/// option parsing would also fire *before* `cmd_blame` has validated the
/// positional shape, turning git's usage error (129) into an unrelated failure.
fn parse_diff_algorithm(name: &str) -> Option<gix::diff::blob::Algorithm> {
    use gix::diff::blob::Algorithm;
    if name.eq_ignore_ascii_case("myers") || name.eq_ignore_ascii_case("default") {
        Some(Algorithm::Myers)
    } else if name.eq_ignore_ascii_case("minimal") {
        Some(Algorithm::MyersMinimal)
    } else if name.eq_ignore_ascii_case("histogram") {
        Some(Algorithm::Histogram)
    } else if name.eq_ignore_ascii_case("patience") {
        Some(Algorithm::Patience)
    } else {
        None
    }
}

/// `parse_opt_abbrev_cb()` (`parse-options-cb.c:19`) for an *attached* value.
///
/// `Err` is the callback's `error()` — reported as
/// ``option `abbrev' expects a numerical value`` at exit 129 — which fires for an
/// empty value and for anything `strtol()` cannot consume in full (so `0x10` stops
/// at `x` and is rejected, and a lone tab has no digits at all).
///
/// The value itself is C's, quirks included: `strtol` saturates at `LONG_MAX`, the
/// result is stored into an `int`, and only *then* is a non-zero value below
/// `MINIMUM_ABBREV` raised to it. That is why `--abbrev=-1` and
/// `--abbrev=99999999999999999999999999` both mean 4 — the latter truncates to `-1`
/// — while `--abbrev=999999999` survives whole and later prints the full hash.
fn parse_abbrev_cb(arg: &str) -> Result<usize, &'static str> {
    const ERR: &str = "option `abbrev' expects a numerical value";
    // The `strtol`-then-narrow quirks are shared with every other command that
    // takes `--abbrev`; see [`crate::abbrev::parse_opt_abbrev_value`].
    let mut v = crate::abbrev::parse_opt_abbrev_value(arg).ok_or(ERR)?;
    if v != 0 && v < MINIMUM_ABBREV as i32 {
        v = MINIMUM_ABBREV as i32;
    }
    Ok(v as usize)
}

/// Why a `-L` spec could not be turned into a range.
#[derive(Debug)]
enum LineSpecError {
    /// `parse_range_arg()` returned non-zero, which `cmd_blame` answers with
    /// `usage(str_usage)` — the stderr usage block and exit 129.
    Usage,
    /// git dies with this message and exit 128.
    Fatal(String),
}

/// `builtin/blame.c:1202-1223` — resolve every `-L` argument against the final
/// image, in order, and turn each into a 1-based inclusive range.
///
/// The anchor threading is git's: `anchor` starts at 1 and becomes `top + 1`
/// after each spec, so a second `-L` with a relative or regex start searches
/// from where the previous one ended. The clamping after `parse_range_arg` is
/// also git's: a `bottom` past the end of the file is fatal, while a `top` past
/// the end is silently pulled back to the last line (verified against git
/// 2.55.0 on a 2-line file: `-L 9,9` → `fatal: file src/lib.rs has only 2
/// lines` exit 128, `-L 1,9` → both lines, exit 0).
fn resolve_line_specs(
    specs: &[String],
    image: &[u8],
    path: &str,
) -> Result<Vec<RangeInclusive<u32>>, LineSpecError> {
    let lines = split_lines(image);
    let num_lines = lines.len() as u32;
    let mut out: Vec<RangeInclusive<u32>> = Vec::with_capacity(specs.len());
    let mut anchor: u32 = 1;
    for spec in specs {
        let (bottom, top) = parse_range_arg(spec, image, &lines, num_lines, anchor)?;
        // `if ((!lno && (top || bottom)) || lno < bottom)` — an empty file with a
        // non-empty request, or a start past the last line.
        if (num_lines == 0 && (top != 0 || bottom != 0)) || num_lines < bottom {
            return Err(LineSpecError::Fatal(plural_line_count(path, num_lines)));
        }
        let bottom = bottom.max(1);
        let top = if top < 1 || num_lines < top { num_lines } else { top };
        out.push(bottom..=top);
        anchor = top + 1;
    }
    Ok(out)
}

/// `line-range.c:parse_range_arg()`. Returns git's `(begin, end)`, both 1-based
/// and both possibly 0 for "unset", which the caller clamps.
fn parse_range_arg(
    spec: &str,
    image: &[u8],
    lines: &[&[u8]],
    num_lines: u32,
    anchor: u32,
) -> Result<(u32, u32), LineSpecError> {
    // `if (anchor < 1) anchor = 1; if (anchor > lines) anchor = lines + 1;`
    let anchor = anchor.clamp(1, num_lines + 1);

    if spec.starts_with(':') || spec.starts_with("^:") {
        return parse_range_funcname(spec, image, lines, num_lines, anchor);
    }

    // `parse_loc(arg, …, -anchor, begin)` then, after a comma,
    // `parse_loc(arg + 1, …, *begin + 1, end)`.
    let (start_spec, end_spec) = match spec.split_once(',') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };
    let begin = parse_loc(start_spec, image, lines, num_lines, -(anchor as i64))?;
    let end = match end_spec {
        None => 0,
        Some(e) => parse_loc(e, image, lines, num_lines, begin as i64 + 1)?,
    };

    // `if (*begin && *end && *end < *begin) SWAP(*end, *begin);`
    if begin != 0 && end != 0 && end < begin {
        Ok((end, begin))
    } else {
        Ok((begin, end))
    }
}

/// `line-range.c:parse_loc()` — one endpoint of a `-L` spec.
///
/// `begin` carries git's two-role encoding: negative means "this is the start
/// endpoint, and the relative anchor is `-begin`"; positive means "this is the
/// end endpoint, anchored one past the resolved start".
fn parse_loc(
    spec: &str,
    image: &[u8],
    lines: &[&[u8]],
    num_lines: u32,
    begin: i64,
) -> Result<u32, LineSpecError> {
    // An endpoint git's `strtol` cannot start on and that is not a regex leaves
    // `*ret` at 0 — `-L,5` and `-L2,` both take this path — and the caller
    // clamps 0 to the file's bounds.
    if spec.is_empty() {
        return Ok(0);
    }

    // `if (1 <= begin && (spec[0] == '+' || spec[0] == '-'))` — the `,+N` / `,-N`
    // forms, valid only as the end endpoint.
    if begin >= 1 {
        if let Some(rest) = spec.strip_prefix(['+', '-']) {
            if let Some(magnitude) = whole_number(rest) {
                if magnitude == 0 {
                    return Err(LineSpecError::Fatal("-L invalid empty range".into()));
                }
                let num = if spec.starts_with('-') {
                    -(magnitude as i64)
                } else {
                    magnitude as i64
                };
                // `*ret = begin + num - 2` for a `+N`; `begin + num > 0 ? … : 1`
                // for a `-N`.
                let value = if num > 0 {
                    begin + num - 2
                } else {
                    (begin + num).max(1)
                };
                return Ok(value.max(0) as u32);
            }
        }
    }

    // `num = strtol(spec, &term, 10); if (term != spec)` — a plain line number,
    // sign included, which is how `-L-1` reaches the `num <= 0` die.
    if let Some(signed) = spec.strip_prefix('-') {
        if let Some(num) = whole_number(signed) {
            return Err(LineSpecError::Fatal(format!(
                "-L invalid line number: -{num}"
            )));
        }
    } else if let Some(num) = whole_number(spec.strip_prefix('+').unwrap_or(spec)) {
        if num == 0 {
            return Err(LineSpecError::Fatal("-L invalid line number: 0".into()));
        }
        return Ok(num);
    }

    // `if (begin < 0) { if (spec[0] != '^') begin = -begin; else { begin = 1; spec++; } }`
    let (mut search_from, spec) = if begin < 0 {
        match spec.strip_prefix('^') {
            Some(rest) => (1i64, rest),
            None => (-begin, spec),
        }
    } else {
        (begin, spec)
    };

    // `if (spec[0] != '/') return spec;` — anything else is a parse failure, which
    // `parse_range_arg` reports by leaving unconsumed input behind.
    let Some(body) = spec.strip_prefix('/') else {
        return Err(LineSpecError::Usage);
    };
    let Some(pattern) = regex_body(body) else {
        return Err(LineSpecError::Usage);
    };

    // `begin--; line = nth_line(data, begin);` — the search starts at the byte
    // offset of the anchor line and runs to the end of the buffer, so the answer
    // is the line the *match start* falls in.
    search_from -= 1;
    let start_line = search_from.clamp(0, num_lines as i64) as usize;
    let offset = line_offset(lines, image, start_line);
    let re = compile_line_regex(pattern).map_err(|_| {
        LineSpecError::Fatal(format!(
            "-L parameter '{pattern}' starting at line {}: invalid regex",
            start_line + 1
        ))
    })?;
    let Some(m) = re.find(&image[offset..]) else {
        return Err(LineSpecError::Fatal(format!(
            "-L parameter '{pattern}' starting at line {}: regexec() failed to match",
            start_line + 1
        )));
    };
    let match_at = offset + m.start();
    let mut lno = start_line;
    while lno + 1 <= lines.len() && line_offset(lines, image, lno + 1) <= match_at {
        lno += 1;
    }
    Ok(lno as u32 + 1)
}

/// `line-range.c:parse_range_funcname()` — `-L:<regex>[:<file>]` and its `^`
/// variant. The trailing `:<file>` form is not reachable from blame, which takes
/// exactly one path operand.
fn parse_range_funcname(
    spec: &str,
    image: &[u8],
    lines: &[&[u8]],
    num_lines: u32,
    anchor: u32,
) -> Result<(u32, u32), LineSpecError> {
    // `if (*arg == '^') { anchor = 1; arg++; }`
    let (anchor, spec) = match spec.strip_prefix('^') {
        Some(rest) => (1u32, rest),
        None => (anchor, spec),
    };
    let body = &spec[1..];
    // `while (*term && *term != ':') { if (*term == '\\' && *(term+1)) term++; term++; }`
    let mut end = body.len();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            end = i;
            break;
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 1;
        }
        i += 1;
    }
    let pattern = &body[..end];
    // `if (term == arg+1) return NULL;` — an empty pattern is a parse failure.
    if pattern.is_empty() || end != body.len() {
        return Err(LineSpecError::Usage);
    }

    let start_line = anchor.saturating_sub(1).min(num_lines) as usize;
    let re = compile_line_regex(pattern).map_err(|_| {
        LineSpecError::Fatal(format!("-L parameter '{pattern}': invalid regex"))
    })?;

    // `find_funcname_matching_regexp()`: take each match in turn, widen it to the
    // line it starts on, and accept the first such line that is also a funcname
    // line.
    let mut search = line_offset(lines, image, start_line);
    let begin = loop {
        let Some(m) = re.find(&image[search..]) else {
            return Err(LineSpecError::Fatal(format!(
                "-L parameter '{pattern}' starting at line {}: no match",
                start_line + 1
            )));
        };
        let match_at = search + m.start();
        let mut lno = start_line;
        while lno + 1 <= lines.len() && line_offset(lines, image, lno + 1) <= match_at {
            lno += 1;
        }
        if lno < lines.len() && is_funcname_line(lines[lno]) {
            break lno as u32;
        }
        // `start = eol` — resume after the line the match ended on.
        let match_end = search + m.end();
        let mut after = match_end;
        while after < image.len() && image[after] != b'\n' {
            after += 1;
        }
        if after >= image.len() {
            return Err(LineSpecError::Fatal(format!(
                "-L parameter '{pattern}' starting at line {}: no match",
                start_line + 1
            )));
        }
        search = after + 1;
    };

    // `if (*begin >= lines) die("-L parameter '%s' matches at EOF", pattern);`
    if begin >= num_lines {
        return Err(LineSpecError::Fatal(format!(
            "-L parameter '{pattern}' matches at EOF"
        )));
    }
    // `*end = *begin+1; while (*end < lines && !match_funcname(…)) (*end)++;`
    let mut end_line = begin + 1;
    while end_line < num_lines && !is_funcname_line(lines[end_line as usize]) {
        end_line += 1;
    }
    // `(*begin)++` compensates for the 0-based scan.
    Ok((begin + 1, end_line))
}

/// The body of a `/…/` regex, honouring the backslash escaping git's scan does
/// (`for (term = spec + 1; *term && *term != '/'; term++) if (*term == '\\') term++;`).
/// `None` when the closing `/` is missing, which git reports as a parse failure.
fn regex_body(body: &str) -> Option<&str> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            // git requires the regex to be the *whole* remaining argument.
            return (i + 1 == bytes.len()).then(|| &body[..i]);
        }
        if bytes[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    None
}

/// `regcomp(&regexp, pattern, REG_NEWLINE)`: `.` and negated classes stop at a
/// newline, and `^`/`$` match at line boundaries. `regex::bytes` spells that
/// `multi_line(true).dot_matches_new_line(false)`.
fn compile_line_regex(pattern: &str) -> Result<regex::bytes::Regex, regex::Error> {
    regex::bytes::RegexBuilder::new(pattern)
        .multi_line(true)
        .dot_matches_new_line(false)
        .build()
}

/// Byte offset of 0-based line `n` in `image`; the end of the buffer for `n`
/// past the last line, which is what `blame_nth_line()` returns.
fn line_offset(lines: &[&[u8]], image: &[u8], n: usize) -> usize {
    match lines.get(n) {
        Some(line) => line.as_ptr() as usize - image.as_ptr() as usize,
        None => image.len(),
    }
}

/// A run of digits that is the *whole* string.
///
/// git parses with `strtol` and then rejects whatever it could not consume
/// (`if (*arg) return -1` in `parse_range_arg`), so an endpoint with trailing
/// text is a usage error rather than a truncated number. Requiring the whole
/// string here collapses those two steps.
fn whole_number(s: &str) -> Option<u32> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// xdiff's default function-line test (`xdiff/xemit.c:def_ff()` via
/// `line-range.c:match_funcname()`): a non-empty line starting with a letter,
/// `_`, or `$`. Used whenever the path has no userdiff driver with a `funcname`
/// pattern.
fn is_funcname_line(line: &[u8]) -> bool {
    matches!(line.first(), Some(&c) if c.is_ascii_alphabetic() || c == b'_' || c == b'$')
}

/// git's `Q_("file %s has only %lu line", "file %s has only %lu lines", lines)`.
fn plural_line_count(path: &str, lines: u32) -> String {
    if lines == 1 {
        format!("file {path} has only 1 line")
    } else {
        format!("file {path} has only {lines} lines")
    }
}

/// Lines of `image`, without terminators. A trailing incomplete line counts, an
/// empty piece after a final `\n` does not — which is how `sb->num_lines` counts.
fn split_lines(image: &[u8]) -> Vec<&[u8]> {
    let mut out: Vec<&[u8]> = image.split(|&b| b == b'\n').collect();
    if out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10-line file whose lines 1, 6 and 10 pass xdiff's default funcname test
    /// (a line starting with a letter, `_` or `$`).
    ///
    /// Every expectation below was captured from stock git 2.55.0 by committing
    /// exactly these bytes as `g.txt` and reading the line numbers back out of
    /// `git blame -s -L <spec> g.txt`, not derived from reading `line-range.c`.
    const FILE: &[u8] = b"fn one() {\n    1\n}\n\n\nfn two() {\n    2\n}\n\nx\n";

    fn ranges(specs: &[&str]) -> Result<Vec<RangeInclusive<u32>>, LineSpecError> {
        let specs: Vec<String> = specs.iter().map(|s| s.to_string()).collect();
        resolve_line_specs(&specs, FILE, "g.txt")
    }

    fn fatal(specs: &[&str]) -> String {
        match ranges(specs) {
            Err(LineSpecError::Fatal(msg)) => msg,
            Err(LineSpecError::Usage) => panic!("expected a fatal, got a usage error"),
            Ok(r) => panic!("expected a fatal, got {r:?}"),
        }
    }

    /// `--abbrev=<v>`, whose value parser is C's `strtol` plus one clamp and so
    /// answers several plausible values counter-intuitively.
    ///
    /// Every expectation was read off stock git 2.55.0, not off `parse-options-cb.c`:
    /// each value was run as `git blame --abbrev=<v> README.md` against a repository
    /// whose sole commit is a boundary commit, and the width was counted from the
    /// hex digits printed after the `^` marker (which spends one column of the
    /// budget). Rejections were read from the exit code and the `error:` line.
    #[test]
    fn abbrev_callback_matches_git() {
        // Accepted, used as given once the `MINIMUM_ABBREV` floor is applied.
        assert_eq!(parse_abbrev_cb("8"), Ok(8));
        assert_eq!(parse_abbrev_cb("40"), Ok(40));
        assert_eq!(parse_abbrev_cb("999999999"), Ok(999999999));
        // `0` is "no abbreviation", and is the one value the floor lets through.
        assert_eq!(parse_abbrev_cb("0"), Ok(0));
        // The floor is applied *after* the read, so a below-minimum or negative
        // value becomes 4 rather than being rejected: stock prints 4 hex digits for
        // both of these.
        assert_eq!(parse_abbrev_cb("1"), Ok(4));
        assert_eq!(parse_abbrev_cb("-1"), Ok(4));
        // `strtol` saturates at `LONG_MAX`, which truncates to -1 in the `int` the
        // callback stores it in, which the floor then raises to 4. Stock agrees: it
        // prints 4 digits for this, not the full hash.
        assert_eq!(parse_abbrev_cb("99999999999999999999999999"), Ok(4));
        // Rejected: nothing at all, or anything `strtol` cannot consume whole —
        // `0x10` stops at `x`, and a lone tab never reaches a digit.
        for bad in ["", "abc", "true", "false", "v1", "=", "%H%n", "\t", "0x10", "8x"] {
            assert!(parse_abbrev_cb(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    /// The `-C<score>` / `-M<score>` argument test `setup_revisions()` applies to a
    /// trailing `git blame -- <path> <token>`, whose accept/reject split is
    /// `parse_rename_score()`'s scanner rather than a numeric parse.
    ///
    /// Every expectation was read off stock git 2.55.0 as
    /// `git blame -- <path> -C<score>`: exit 0 for the accepted ones, and
    /// `error: invalid argument to find-copies` at exit 129 for the rejected ones.
    #[test]
    fn rename_score_argument_matches_git() {
        // Digits, one `.`, and a `%` that ends the number. A bare `-C` arrives here
        // as the empty string, which the scanner consumes trivially.
        for good in ["", "0", "40", "50%", ".5", ".", "%", "1.5", "1.5%"] {
            assert!(rename_score_consumed(good), "-C{good} should be accepted");
        }
        // Anything the scanner stops on and cannot consume: a letter (`-CC` is the
        // one that reaches this from a real command line), a sign, an exponent, a
        // second `.`, and anything trailing the `%` that ends the scan.
        for bad in ["C", "foo", "4x", "+3", "-1", "1e3", "1.2.3", "50%x", "10%%", " "] {
            assert!(!rename_score_consumed(bad), "-C{bad} should be rejected");
        }
    }

    /// `--diff-algorithm=<name>`: every name git accepts maps to an algorithm, and
    /// only a name git itself rejects yields `None` (git's
    /// `parse_algorithm_value() < 0`, reported as [`DIFF_ALGORITHM_ERROR`]).
    #[test]
    fn diff_algorithm_names_match_git() {
        use gix::diff::blob::Algorithm;
        assert_eq!(parse_diff_algorithm("myers"), Some(Algorithm::Myers));
        assert_eq!(parse_diff_algorithm("default"), Some(Algorithm::Myers));
        assert_eq!(parse_diff_algorithm("minimal"), Some(Algorithm::MyersMinimal));
        assert_eq!(parse_diff_algorithm("histogram"), Some(Algorithm::Histogram));
        // The one that used to be refused: `gix-imara-diff`'s `patience.rs` is git's
        // `xdiff/xpatience.c`, so the name maps like the other three.
        assert_eq!(parse_diff_algorithm("patience"), Some(Algorithm::Patience));
        assert_eq!(parse_diff_algorithm("PATIENCE"), Some(Algorithm::Patience));
        for bad in ["", "pat", "patience2", "none", "0"] {
            assert_eq!(parse_diff_algorithm(bad), None, "{bad:?} is not a git name");
        }
    }

    /// The numeric `-L` forms, including the two relative ends and the clamping
    /// `builtin/blame.c:1211-1218` applies.
    #[test]
    fn numeric_line_specs_match_git() {
        assert_eq!(ranges(&["3"]).unwrap(), vec![3..=10]);
        assert_eq!(ranges(&["3,5"]).unwrap(), vec![3..=5]);
        assert_eq!(ranges(&["3,+2"]).unwrap(), vec![3..=4]);
        assert_eq!(ranges(&["10,-5"]).unwrap(), vec![6..=10]);
        assert_eq!(ranges(&["10,-15"]).unwrap(), vec![1..=10]);
        assert_eq!(ranges(&["3,-1"]).unwrap(), vec![3..=3]);
        // `-L,<m>` and `-L<n>,` leave the missing endpoint at git's 0, which the
        // clamp turns into the first and the last line respectively.
        assert_eq!(ranges(&[",4"]).unwrap(), vec![1..=4]);
        assert_eq!(ranges(&["4,"]).unwrap(), vec![4..=10]);
        // An inverted range is swapped, not rejected.
        assert_eq!(ranges(&["5,2"]).unwrap(), vec![2..=5]);
        // A `top` past the end is pulled back; a `bottom` past the end is fatal.
        assert_eq!(ranges(&["1,99"]).unwrap(), vec![1..=10]);
        assert_eq!(fatal(&["99,99"]), "file g.txt has only 10 lines");
        assert_eq!(fatal(&["0,0"]), "-L invalid line number: 0");
        assert_eq!(fatal(&["10,-0"]), "-L invalid empty range");
    }

    /// `/<regex>/` resolves to the line the match *starts* on, searching forward
    /// from the anchor, and a second `-L` picks up where the first left off
    /// (`anchor = top + 1`, `builtin/blame.c:1221`).
    #[test]
    fn regex_line_specs_match_git() {
        assert_eq!(ranges(&["/two/,+1"]).unwrap(), vec![6..=6]);
        // `^/re/` re-anchors the search at line 1 regardless of the running
        // anchor, so the second spec finds the *first* `fn`, not the next one.
        assert_eq!(
            ranges(&["/two/,+1", "^/fn/,+1"]).unwrap(),
            vec![6..=6, 1..=1]
        );
        // Without `^`, the same second spec searches from line 7 onward and
        // finds nothing.
        assert_eq!(
            fatal(&["/two/,+1", "/fn/,+1"]),
            "-L parameter 'fn' starting at line 7: regexec() failed to match"
        );
    }

    /// `:<funcname>` takes the first funcname line matching the pattern and runs
    /// to just before the next funcname line (`parse_range_funcname`).
    #[test]
    fn funcname_line_specs_match_git() {
        assert_eq!(ranges(&[":one"]).unwrap(), vec![1..=5]);
        // Line 10 is `x`, which passes the funcname test, so the second function
        // ends at line 9 rather than at the end of the file.
        assert_eq!(ranges(&[":two"]).unwrap(), vec![6..=9]);
        // The pattern also matches inside line 6 only; `find_funcname_matching_regexp`
        // widens each match to its line and keeps the first that is a funcname line.
        assert_eq!(ranges(&[":fn t"]).unwrap(), vec![6..=9]);
        assert_eq!(
            fatal(&[":nowhere"]),
            "-L parameter 'nowhere' starting at line 1: no match"
        );
    }

    /// The three tables that decide whether an option is a typo or a porting
    /// gap: blame's own, revision.c's, and diff.c's. A miss here would make the
    /// port claim git rejects an option git actually accepts.
    #[test]
    fn known_long_options_span_all_three_tables() {
        // `builtin/blame.c:options[]`.
        assert!(git_knows_long_option("--show-stats"));
        assert!(git_knows_long_option("--ignore-revs-file=x"));
        // `revision.c:handle_revision_opt()`.
        assert!(git_knows_long_option("--first-parent"));
        assert!(git_knows_long_option("--children"));
        // `diff.c:prep_parse_options()`.
        assert!(git_knows_long_option("--ignore-space-change"));
        assert!(git_knows_long_option("--find-copies-harder"));
        // `parse_long_opt()` accepts the negation of a negatable option, and the
        // explicit `--no-` names revision.c matches in their own right.
        assert!(git_knows_long_option("--no-show-stats"));
        assert!(git_knows_long_option("--no-walk"));
        // Not an option of any of the three, which is what the tested case
        // `git blame -s --no-use-mailmap` turns on: blame has no `use-mailmap`.
        assert!(!git_knows_long_option("--use-mailmap"));
        assert!(!git_knows_long_option("--no-use-mailmap"));
        assert!(!git_knows_long_option("--bogus"));
        // Short options never take this path.
        assert!(!git_knows_long_option("-w"));
    }

    /// `overwrite_argv()` clears `ctx->argv[0]` exactly when an earlier
    /// recognised option has consumed an argv slot, so the name git prints is
    /// the option itself only for the first option on the line.
    ///
    /// Captured from stock git 2.55.0: `git blame --no-bogus a.txt` prints
    /// ``unknown option `--no-bogus'``, `git blame a.txt --no-bogus` prints the
    /// same, and `git blame -w --no-bogus a.txt` prints ``unknown option
    /// `(null)'``.
    #[test]
    fn unknown_option_name_matches_gits_argv_shuffle() {
        let argv = |v: &[&str]| -> Vec<String> { v.iter().map(|s| (*s).to_owned()).collect() };

        // First thing on the line: nothing has desynchronised `out` from `argv`.
        assert_eq!(unknown_option_name(&argv(&["--no-bogus", "a.txt"]), 0, 0), "--no-bogus");
        // A positional before it advances both indices together.
        assert_eq!(unknown_option_name(&argv(&["a.txt", "--no-bogus"]), 1, 1), "--no-bogus");
        // A recognised option advances only `argv`, and the name is lost.
        assert_eq!(unknown_option_name(&argv(&["-w", "--no-bogus", "a.txt"]), 1, 0), "(null)");
        // An option with a detached value consumes two slots; still lost.
        assert_eq!(
            unknown_option_name(&argv(&["-L", "1,1", "--no-bogus", "a.txt"]), 2, 0),
            "(null)"
        );
    }
}

/// Whether `--encoding=<enc>` asks for the bytes this port already produces: UTF-8 in
/// any of its spellings, or `none` (git's "do not convert"). git also falls back to
/// passing the bytes through when the platform's iconv does not know the name, but that
/// is a silent fallback rather than a promise, so only these are accepted here.
///
/// Shared with the history commands, which take the same option for commit messages —
/// every IDE passes `--encoding=UTF-8` to `log` and `blame` alike.
pub(crate) fn encoding_is_passthrough(enc: &str) -> bool {
    let lower = enc.trim().to_ascii_lowercase();
    matches!(lower.as_str(), "utf-8" | "utf8" | "none" | "")
}
