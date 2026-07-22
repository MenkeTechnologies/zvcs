//! `git diff-pairs` — compare the content and mode of blob pairs read from stdin.
//!
//! The input is the NUL-terminated raw diff format produced by `git diff-tree -z -r --raw`:
//! `:<omode> <nmode> <ooid> <noid> <status>\0<path>\0` with a second path field for
//! rename/copy statuses. A lone NUL where a record header would start closes a *batch*:
//! the diffs accumulated so far are run through `diffcore_std` and emitted, then a NUL is
//! written to delimit them.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). The blob diff runs through
//! `gix::diff::blob` exactly as [`super::diff`] does; the file headers are assembled here
//! to match `diff.c`'s `fill_metainfo` ordering (mode lines, similarity/rename lines,
//! `index` line, `---`/`+++`).
//!
//! ### Covered (byte-identical stdout, stderr and exit code against stock git)
//!
//! * patch output (the default), including
//!   - `new file mode` / `deleted file mode` / `old mode`+`new mode`
//!   - `similarity index <n>%` with `rename from`/`rename to` and `copy from`/`copy to`
//!   - the `index <old>..<new>[ <mode>]` line, omitted when both sides hash equal
//!   - `Binary files ... differ`, `\ No newline at end of file`
//!   - `@@ -a,b +c,d @@ <function>` hunk headers, using git's built-in `def_ff`
//!     function-name heuristic (a preceding line starting with a letter, `_` or `$`,
//!     truncated to 80 bytes and right-trimmed)
//!   - type changes (`T`), split into a deletion patch followed by a creation patch
//!   - gitlinks (`160000`), rendered as `Subproject commit <oid>` line diffs
//!   - `builtin_diff`'s lazy header: a modification whose content compares equal
//!     (a whitespace-only change under `-w`) and whose mode is unchanged prints nothing
//! * `--raw` (echoes the pair with full object ids), `--name-only`, `--name-status`,
//!   `--numstat`, `--stat`/`--stat=<w>[,<n>[,<c>]]`, `--shortstat`, `--summary`,
//!   `--compact-summary`, `-s`/`--no-patch`
//! * `-p`/`-u`/`--patch`, `--patch-with-raw`, `--patch-with-stat`, `-U<n>`/`--unified[=<n>]`
//! * `--full-index`, `--no-prefix`, `--default-prefix`, `--src-prefix=`, `--dst-prefix=`
//! * `-R` (reverse), `--diff-filter=<letters>` (with `*` and lowercase exclusion)
//! * `-w`/`--ignore-all-space`, `-b`/`--ignore-space-change`, `--ignore-space-at-eol`,
//!   `--ignore-cr-at-eol`; `-I<re>`/`--ignore-matching-lines` for non-patch formats
//! * the pickaxe: `-S<string>` (with `--pickaxe-regex`), `-G<regex>`, `--find-object=<id>`,
//!   `--pickaxe-all`
//! * `--relative[=<prefix>]` / `--no-relative`, `--rotate-to=<p>` / `--skip-to=<p>`
//! * `--exit-code`, `--quiet` (implies `-s` and `--exit-code`)
//! * `--abbrev[=<n>]` — accepted and ignored, which is what stock git does here.
//!   `core.abbrev` itself *is* honoured.
//! * `-h` (usage on stdout, exit 129); running without `-z` (usage line on stderr, exit 129)
//! * the fatal paths: `invalid raw diff input`, `unable to parse object id: ...`,
//!   `tree objects not supported`, `unable to read <oid>` — all exit 128
//!
//! ### Honest limitations (bailed on with a precise message, never silently ignored)
//!
//! * Tree-object pairs (`040000` on either side) are rejected with `tree objects not
//!   supported`, exit 128 — this matches stock git's `builtin/diff-pairs.c`, which dies
//!   with the same message rather than recursing.
//! * `--color`, `--color-moved`, `--word-diff`, `--check`, `--binary`, `--line-prefix`,
//!   `--output`, `--output-indicator-*`, `--inter-hunk-context`, `-a`/`--text`,
//!   `-W`/`--function-context`, `--dirstat`, `--diff-algorithm`/`--patience`/`--histogram`/
//!   `--minimal`/`--anchored`, `-O`/`--order`, `--textconv`/`--ext-diff`, `--submodule`,
//!   and `-I` combined with a *patch* format (git marks the ignorable hunks via
//!   `xdl_mark_ignorable`, which the vendored differ does not expose).
//! * The rename/copy options (`-M`, `-C`, `-B`, `-l`, ...) are meaningless for this command
//!   — the pairs arrive pre-computed — and are rejected rather than quietly dropped.
//! * `gitattributes` diff drivers: neither external commands nor custom `funcname`
//!   patterns are applied, so hunk headers use git's built-in heuristic only.
//! * `--stat`/`--summary` file names are not C-quoted, so a path containing a byte that
//!   git would escape is emitted verbatim.

use anyhow::{bail, Result};
use std::cmp::Ordering;
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, InternedInput, ResourceKind, UnifiedDiff};
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;
use gix::prelude::ObjectIdExt;
use regex::bytes::Regex;

/// Stock git's `diff-pairs` usage block, byte-for-byte including the trailing blank
/// line. Printed on `-h` (stdout, exit 129).
const USAGE: &str = r#"usage: git diff-pairs -z [<diff-options>]

Diff output format options
    -p, --patch           generate patch
    -s, --no-patch        suppress diff output
    -u                    generate patch
    -U, --unified[=<n>]   generate diffs with <n> lines context
    -W, --[no-]function-context
                          generate diffs with <n> lines context
    --raw                 generate the diff in raw format
    --patch-with-raw      synonym for '-p --raw'
    --patch-with-stat     synonym for '-p --stat'
    --numstat             machine friendly --stat
    --shortstat           output only the last line of --stat
    -X, --dirstat[=<param1>,<param2>...]
                          output the distribution of relative amount of changes for each sub-directory
    --cumulative          synonym for --dirstat=cumulative
    --dirstat-by-file[=<param1>,<param2>...]
                          synonym for --dirstat=files,<param1>,<param2>...
    --check               warn if changes introduce conflict markers or whitespace errors
    --summary             condensed summary such as creations, renames and mode changes
    --name-only           show only names of changed files
    --name-status         show only names and status of changed files
    --stat[=<width>[,<name-width>[,<count>]]]
                          generate diffstat
    --stat-width <width>  generate diffstat with a given width
    --stat-name-width <width>
                          generate diffstat with a given name width
    --stat-graph-width <width>
                          generate diffstat with a given graph width
    --stat-count <count>  generate diffstat with limited lines
    --[no-]compact-summary
                          generate compact summary in diffstat
    --binary              output a binary diff that can be applied
    --[no-]full-index     show full pre- and post-image object names on the "index" lines
    --[no-]color[=<when>] show colored diff
    --ws-error-highlight <kind>
                          highlight whitespace errors in the 'context', 'old' or 'new' lines in the diff
    -z                    do not munge pathnames and use NULs as output field terminators in --raw or --numstat
    --[no-]abbrev[=<n>]   use <n> digits to display object names
    --src-prefix <prefix> show the given source prefix instead of "a/"
    --dst-prefix <prefix> show the given destination prefix instead of "b/"
    --line-prefix <prefix>
                          prepend an additional prefix to every line of output
    --no-prefix           do not show any source or destination prefix
    --default-prefix      use default prefixes a/ and b/
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    --output-indicator-new <char>
                          specify the character to indicate a new line instead of '+'
    --output-indicator-old <char>
                          specify the character to indicate an old line instead of '-'
    --output-indicator-context <char>
                          specify the character to indicate a context instead of ' '

Diff rename options
    -B, --break-rewrites[=<n>[/<m>]]
                          break complete rewrite changes into pairs of delete and create
    -M, --find-renames[=<n>]
                          detect renames
    -D, --irreversible-delete
                          omit the preimage for deletes
    -C, --find-copies[=<n>]
                          detect copies
    --[no-]find-copies-harder
                          use unmodified files as source to find copies
    --no-renames          disable rename detection
    --[no-]rename-empty   use empty blobs as rename source
    --[no-]follow         continue listing the history of a file beyond renames
    -l <n>                prevent rename/copy detection if the number of rename/copy targets exceeds given limit

Diff algorithm options
    --minimal             produce the smallest possible diff
    -w, --ignore-all-space
                          ignore whitespace when comparing lines
    -b, --ignore-space-change
                          ignore changes in amount of whitespace
    --ignore-space-at-eol ignore changes in whitespace at EOL
    --ignore-cr-at-eol    ignore carrier-return at the end of line
    --ignore-blank-lines  ignore changes whose lines are all blank
    -I, --[no-]ignore-matching-lines <regex>
                          ignore changes whose all lines match <regex>
    --[no-]indent-heuristic
                          heuristic to shift diff hunk boundaries for easy reading
    --patience            generate diff using the "patience diff" algorithm
    --histogram           generate diff using the "histogram diff" algorithm
    --diff-algorithm <algorithm>
                          choose a diff algorithm
    --anchored <text>     generate diff using the "anchored diff" algorithm
    --word-diff[=<mode>]  show word diff, using <mode> to delimit changed words
    --word-diff-regex <regex>
                          use <regex> to decide what a word is
    --color-words[=<regex>]
                          equivalent to --word-diff=color --word-diff-regex=<regex>
    --[no-]color-moved[=<mode>]
                          moved lines of code are colored differently
    --[no-]color-moved-ws <mode>
                          how white spaces are ignored in --color-moved

Other diff options
    --[no-]relative[=<prefix>]
                          when run from subdir, exclude changes outside and show relative paths
    -a, --[no-]text       treat all files as text
    -R                    swap two inputs, reverse the diff
    --[no-]exit-code      exit with 1 if there were differences, 0 otherwise
    --[no-]quiet          disable all output of the program
    --[no-]ext-diff       allow an external diff helper to be executed
    --[no-]textconv       run external text conversion filters when comparing binary files
    --ignore-submodules[=<when>]
                          ignore changes to submodules in the diff generation
    --submodule[=<format>]
                          specify how differences in submodules are shown
    --ita-invisible-in-index
                          hide 'git add -N' entries from the index
    --ita-visible-in-index
                          treat 'git add -N' entries as real in the index
    -S <string>           look for differences that change the number of occurrences of the specified string
    -G <regex>            look for differences that change the number of occurrences of the specified regex
    --pickaxe-all         show all changes in the changeset with -S or -G
    --pickaxe-regex       treat <string> in -S as extended POSIX regular expression
    -O <file>             control the order in which files appear in the output
    --rotate-to <path>    show the change in the specified path first
    --skip-to <path>      skip the output to the specified path
    --find-object <object-id>
                          look for differences that change the number of occurrences of the specified object
    --diff-filter [(A|C|D|M|R|T|U|X|B)...[*]]
                          select files by diff type
    --max-depth <depth>   maximum tree depth to recurse
    --output <file>       output to a specific file

"#;

/// git's `S_IFMT` mask: `100644` and `100755` share a type, `120000` and `160000` do not.
const IFMT: u32 = 0o170000;

/// `def_ff`'s scratch-buffer size in `xdiff/xutils.c`; the function name is truncated to it.
const FUNCNAME_MAX: usize = 80;

/// How lines are compared, mirroring xdiff's `XDF_*` whitespace flags.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Whitespace {
    Keep,
    /// `-w` / `--ignore-all-space`: every whitespace byte is ignored.
    IgnoreAll,
    /// `-b` / `--ignore-space-change`: runs of whitespace collapse to one space,
    /// trailing whitespace is ignored.
    IgnoreChange,
    /// `--ignore-space-at-eol`: only trailing whitespace is ignored.
    IgnoreAtEol,
    /// `--ignore-cr-at-eol`: a single CR before the line terminator is ignored.
    IgnoreCrAtEol,
}

/// The `--relative[=<p>]` / `--no-relative` selection.
enum Relative {
    /// git's default: paths stay repository-root relative.
    No,
    /// Bare `--relative`: use the current directory's prefix within the worktree.
    Cwd,
    /// `--relative=<p>`: use the given directory as the prefix.
    Path(BString),
}

/// Where the listing should be re-anchored, per `--rotate-to`/`--skip-to`.
enum Anchor {
    /// `--rotate-to=<p>`: move everything before `<p>` to the end.
    Rotate(BString),
    /// `--skip-to=<p>`: drop everything before `<p>`.
    Skip(BString),
}

/// A search pattern: a literal substring (git's kwset path for a plain `-S`) or a
/// compiled regular expression (git's `-G` and `-S --pickaxe-regex`, which call
/// `regcomp` with `REG_EXTENDED | REG_NEWLINE`).
enum Needle {
    Literal(Vec<u8>),
    Regex(Regex),
}

impl Needle {
    /// Whether `hay` contains a match — used by `-G` on each changed line.
    fn is_match(&self, hay: &[u8]) -> bool {
        match self {
            Needle::Literal(n) => count_occurrences(hay, n) > 0,
            Needle::Regex(re) => re.is_match(hay),
        }
    }

    /// Non-overlapping match count — used by `-S` to compare the two sides.
    fn count(&self, hay: &[u8]) -> usize {
        match self {
            Needle::Literal(n) => count_occurrences(hay, n),
            Needle::Regex(re) => re.find_iter(hay).count(),
        }
    }
}

/// Compile a `-G`/`-I`/`-S --pickaxe-regex` pattern the way git's `regcomp` does: on
/// bytes, without Unicode mode so the byte semantics carry git's C locale, and with
/// multi-line mode standing in for `REG_NEWLINE`.
fn compile_regex(pat: &[u8]) -> std::result::Result<Regex, String> {
    let s = std::str::from_utf8(pat).map_err(|_| "invalid byte sequence in pattern".to_owned())?;
    regex::bytes::RegexBuilder::new(s)
        .unicode(false)
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())
}

fn matches_any(pats: &[Needle], line: &[u8]) -> bool {
    pats.iter().any(|p| p.is_match(line))
}

fn strip_terminator(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\n') {
        &line[..line.len() - 1]
    } else {
        line
    }
}

fn count_occurrences(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

/// `-S<string>` counts occurrences; `-G<pattern>` looks at the changed lines;
/// `--find-object=<id>` keeps a pair that touches one of the named object ids.
enum PickaxeKind {
    Occurrences(Needle),
    Grep(Needle),
    ObjFind(Vec<ObjectId>),
}

struct Pickaxe {
    kind: PickaxeKind,
    /// `--pickaxe-all`: keep every pair when any one of them matches.
    all: bool,
}

/// `--diff-filter=<letters>`.
struct Filter {
    /// Upper-cased status letters to keep.
    keep: Vec<u8>,
    /// Upper-cased status letters to exclude (lowercase input).
    exclude: Vec<u8>,
    /// `*`: all-or-none.
    all_or_none: bool,
    /// Every letter was an exclusion, so the base set is "everything but these".
    only_exclude: bool,
}

impl Filter {
    fn matches(&self, status: u8) -> bool {
        if self.all_or_none {
            return self.keep.contains(&status);
        }
        if self.only_exclude {
            return !self.exclude.contains(&status);
        }
        self.keep.contains(&status)
    }
}

/// The `--stat` geometry, in git's own `-1 == unset` encoding.
struct StatWidths {
    width: i64,
    name_width: i64,
    graph_width: i64,
    count: i64,
    /// `--compact-summary`: annotate names with `(gone)`, `(new)`, `(mode +x)`, …
    with_summary: bool,
}

impl Default for StatWidths {
    fn default() -> Self {
        StatWidths {
            width: -1,
            name_width: -1,
            graph_width: -1,
            count: 0,
            with_summary: false,
        }
    }
}

/// Which output formats are active. git ORs these together and only falls back to a
/// patch when none was requested.
#[derive(Default)]
struct Formats {
    patch: bool,
    raw: bool,
    name_only: bool,
    name_status: bool,
    numstat: bool,
    stat: bool,
    shortstat: bool,
    summary: bool,
    no_output: bool,
}

impl Formats {
    /// Whether any format was requested explicitly (so the patch default does not apply).
    fn requested(&self) -> bool {
        self.patch
            || self.raw
            || self.name_only
            || self.name_status
            || self.numstat
            || self.stat
            || self.shortstat
            || self.summary
            || self.no_output
    }

    /// Whether one of the per-pair "name" formats runs before the stat block.
    fn name_group(&self) -> bool {
        self.raw || self.name_only || self.name_status
    }
}

/// Parsed command-line options for a single `diff-pairs` invocation.
struct Opts {
    formats: Formats,
    ctx: u32,                  // -U<n>
    full_index: bool,          // --full-index
    src_prefix: BString,       // --src-prefix / --no-prefix
    dst_prefix: BString,       // --dst-prefix / --no-prefix
    exit_code: bool,           // --exit-code / --quiet
    ws: Whitespace,            // -w / -b / --ignore-space-at-eol / --ignore-cr-at-eol
    ignore_lines: Vec<Needle>, // -I<re>
    reverse: bool,             // -R
    filter: Option<Filter>,    // --diff-filter
    pickaxe: Option<Pickaxe>,  // -S / -G / --find-object (finalized after parse)
    relative: Relative,
    anchor: Option<Anchor>, // --rotate-to / --skip-to
    stat: StatWidths,
}

/// One raw-format record: a pre-computed file pair.
#[derive(Clone)]
struct Pair {
    old_mode: u32,
    new_mode: u32,
    old_id: ObjectId,
    new_id: ObjectId,
    /// The status token verbatim, e.g. `M`, `A`, `R100`.
    status: BString,
    old_path: BString,
    /// Equal to `old_path` unless the status carries a second path (rename/copy).
    new_path: BString,
}

impl Pair {
    fn kind(&self) -> u8 {
        self.status[0]
    }

    /// The similarity/dissimilarity score encoded in the status token, if any.
    fn score(&self) -> u32 {
        std::str::from_utf8(&self.status[1..])
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    fn old_valid(&self) -> bool {
        self.old_mode != 0
    }

    fn new_valid(&self) -> bool {
        self.new_mode != 0
    }

    /// git's `DIFF_PAIR_TYPE_CHANGED`: both sides exist but their `S_IFMT` differs.
    fn type_changed(&self) -> bool {
        self.old_valid() && self.new_valid() && (self.old_mode & IFMT) != (self.new_mode & IFMT)
    }
}

/// Per-pair blob analysis: line counts, whether the content is binary, the two raw
/// blob buffers (used by the pickaxe) and the rendered hunks (empty when the two sides
/// compare equal under the active whitespace rules).
struct Analysis {
    add: u32,
    del: u32,
    binary: bool,
    old_data: Vec<u8>,
    new_data: Vec<u8>,
    hunks: Vec<u8>,
}

/// `git diff-pairs` — see the module documentation for the covered surface.
pub fn diff_pairs(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("diff-pairs") => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        formats: Formats::default(),
        ctx: 3,
        full_index: false,
        src_prefix: BString::from("a/"),
        dst_prefix: BString::from("b/"),
        exit_code: false,
        ws: Whitespace::Keep,
        ignore_lines: Vec::new(),
        reverse: false,
        filter: None,
        pickaxe: None,
        relative: Relative::No,
        anchor: None,
        stat: StatWidths::default(),
    };
    let mut nul = false;
    // Deferred until the whole line is read so `--pickaxe-regex`/`--pickaxe-all`, which
    // may follow the `-S`/`-G`, can fold in. `b'S'` counts occurrences, `b'G'` greps.
    let mut pickaxe_pending: Option<(u8, Vec<u8>)> = None;
    let mut find_object_args: Vec<String> = Vec::new();
    let mut pickaxe_all = false;
    let mut pickaxe_regex = false;

    let mut i = 0usize;
    while i < args.len() {
        let s = args[i].as_str();
        // Fetch the value of a `--opt=v` / `--opt v` / `-Xv` / `-X v` option, advancing
        // the cursor past a separate value argument.
        macro_rules! want_value {
            ($prefix_len:expr) => {{
                let prefix_len: usize = $prefix_len;
                if s.len() > prefix_len {
                    s[prefix_len..].to_string()
                } else {
                    i += 1;
                    match args.get(i) {
                        Some(v) => v.clone(),
                        None => bail!("option {s:?} requires an argument"),
                    }
                }
            }};
        }
        match s {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-z" => nul = true,
            "-p" | "-u" | "--patch" => opts.formats.patch = true,
            "-s" | "--no-patch" => opts.formats.no_output = true,
            "--raw" => opts.formats.raw = true,
            "--name-only" => opts.formats.name_only = true,
            "--name-status" => opts.formats.name_status = true,
            "--numstat" => opts.formats.numstat = true,
            "--shortstat" => opts.formats.shortstat = true,
            "--summary" => opts.formats.summary = true,
            "--stat" => opts.formats.stat = true,
            _ if s.starts_with("--stat=") => {
                opts.formats.stat = true;
                parse_stat_geometry(&mut opts.stat, &s["--stat=".len()..]);
            }
            "--compact-summary" => {
                opts.formats.stat = true;
                opts.stat.with_summary = true;
            }
            "--no-compact-summary" => opts.stat.with_summary = false,
            _ if s.starts_with("--stat-width=") => {
                opts.formats.stat = true;
                opts.stat.width = parse_i64(&s["--stat-width=".len()..]);
            }
            _ if s.starts_with("--stat-name-width=") => {
                opts.formats.stat = true;
                opts.stat.name_width = parse_i64(&s["--stat-name-width=".len()..]);
            }
            _ if s.starts_with("--stat-graph-width=") => {
                opts.formats.stat = true;
                opts.stat.graph_width = parse_i64(&s["--stat-graph-width=".len()..]);
            }
            _ if s.starts_with("--stat-count=") => {
                opts.formats.stat = true;
                opts.stat.count = parse_i64(&s["--stat-count=".len()..]);
            }
            "--patch-with-raw" => {
                opts.formats.patch = true;
                opts.formats.raw = true;
            }
            "--patch-with-stat" => {
                opts.formats.patch = true;
                opts.formats.stat = true;
            }
            "--full-index" => opts.full_index = true,
            "--no-full-index" => opts.full_index = false,
            "--no-prefix" => {
                opts.src_prefix.clear();
                opts.dst_prefix.clear();
            }
            "--default-prefix" => {
                opts.src_prefix = BString::from("a/");
                opts.dst_prefix = BString::from("b/");
            }
            "--exit-code" => opts.exit_code = true,
            "--no-exit-code" => opts.exit_code = false,
            "--quiet" => {
                opts.exit_code = true;
                opts.formats.no_output = true;
            }
            // -R swaps the two prefixes and, per pair, the two sides at render time.
            "-R" => opts.reverse = true,
            // Whitespace comparison flags.
            "-w" | "--ignore-all-space" => opts.ws = Whitespace::IgnoreAll,
            "-b" | "--ignore-space-change" => opts.ws = Whitespace::IgnoreChange,
            "--ignore-space-at-eol" => opts.ws = Whitespace::IgnoreAtEol,
            "--ignore-cr-at-eol" => opts.ws = Whitespace::IgnoreCrAtEol,
            "-I" | "--ignore-matching-lines" => {
                let v = want_value!(s.len());
                let re = compile_regex(v.as_bytes())
                    .map_err(|e| anyhow::anyhow!("invalid regex given to -I: {e}"))?;
                opts.ignore_lines.push(Needle::Regex(re));
            }
            _ if s.starts_with("-I") => {
                let re = compile_regex(s[2..].as_bytes())
                    .map_err(|e| anyhow::anyhow!("invalid regex given to -I: {e}"))?;
                opts.ignore_lines.push(Needle::Regex(re));
            }
            _ if s.starts_with("--ignore-matching-lines=") => {
                let re = compile_regex(s["--ignore-matching-lines=".len()..].as_bytes())
                    .map_err(|e| anyhow::anyhow!("invalid regex given to -I: {e}"))?;
                opts.ignore_lines.push(Needle::Regex(re));
            }
            // --diff-filter
            "--diff-filter" => {
                let v = want_value!(s.len());
                opts.filter = Some(parse_filter(&v));
            }
            _ if s.starts_with("--diff-filter=") => {
                opts.filter = Some(parse_filter(&s["--diff-filter=".len()..]));
            }
            // Pickaxe.
            "-S" => pickaxe_pending = Some((b'S', want_value!(s.len()).into_bytes())),
            _ if s.starts_with("-S") => pickaxe_pending = Some((b'S', s[2..].as_bytes().to_vec())),
            "-G" => pickaxe_pending = Some((b'G', want_value!(s.len()).into_bytes())),
            _ if s.starts_with("-G") => pickaxe_pending = Some((b'G', s[2..].as_bytes().to_vec())),
            "--pickaxe-all" => pickaxe_all = true,
            "--pickaxe-regex" => pickaxe_regex = true,
            "--find-object" => find_object_args.push(want_value!(s.len())),
            _ if s.starts_with("--find-object=") => {
                find_object_args.push(s["--find-object=".len()..].to_string());
            }
            // --relative / --no-relative
            "--relative" => opts.relative = Relative::Cwd,
            "--no-relative" => opts.relative = Relative::No,
            _ if s.starts_with("--relative=") => {
                opts.relative = Relative::Path(BString::from(s["--relative=".len()..].as_bytes()));
            }
            // --rotate-to / --skip-to
            "--rotate-to" => opts.anchor = Some(Anchor::Rotate(want_value!(s.len()).into())),
            _ if s.starts_with("--rotate-to=") => {
                opts.anchor = Some(Anchor::Rotate(BString::from(
                    s["--rotate-to=".len()..].as_bytes(),
                )));
            }
            "--skip-to" => opts.anchor = Some(Anchor::Skip(want_value!(s.len()).into())),
            _ if s.starts_with("--skip-to=") => {
                opts.anchor = Some(Anchor::Skip(BString::from(
                    s["--skip-to=".len()..].as_bytes(),
                )));
            }
            // Accepted and ignored, matching stock git: `--abbrev=<n>` has no effect on
            // this command's `index` lines (only `core.abbrev` does).
            "--abbrev" | "--no-abbrev" => {}
            _ if s.starts_with("--abbrev=") => {}
            "-U" => {
                opts.ctx = parse_ctx(&want_value!(s.len()))?;
                opts.formats.patch = true;
            }
            _ if s.starts_with("-U") => {
                opts.ctx = parse_ctx(&s[2..])?;
                opts.formats.patch = true;
            }
            _ if s.starts_with("--unified=") => {
                opts.ctx = parse_ctx(&s["--unified=".len()..])?;
                opts.formats.patch = true;
            }
            "--unified" => opts.formats.patch = true,
            _ if s.starts_with("--src-prefix=") => {
                opts.src_prefix = BString::from(s["--src-prefix=".len()..].as_bytes());
            }
            _ if s.starts_with("--dst-prefix=") => {
                opts.dst_prefix = BString::from(s["--dst-prefix=".len()..].as_bytes());
            }
            _ => bail!(
                "unsupported flag {s:?} (ported: -z, -p/-u/--patch, -s/--no-patch, --raw, \
                 --name-only, --name-status, --numstat, --stat[=<w>], --shortstat, --summary, \
                 --compact-summary, --patch-with-raw, --patch-with-stat, -U<n>/--unified[=<n>], \
                 --full-index, --no-prefix, --default-prefix, --src-prefix=, --dst-prefix=, \
                 -R, --diff-filter=<f>, -w/-b/--ignore-space-at-eol/--ignore-cr-at-eol, \
                 -I<re>, -S<s>/-G<re>/--find-object=<id>/--pickaxe-regex/--pickaxe-all, \
                 --relative[=<p>]/--no-relative, --rotate-to=<p>/--skip-to=<p>, \
                 --exit-code, --quiet, --abbrev[=<n>], -h)"
            ),
        }
        i += 1;
    }

    if !nul {
        eprintln!("usage: working without -z is not supported");
        return Ok(ExitCode::from(129));
    }
    if !opts.formats.requested() {
        opts.formats.patch = true;
    }
    // `-I` combined with a patch format needs `xdl_mark_ignorable`, which the vendored
    // differ does not expose. It stays exact for the non-patch formats.
    if !opts.ignore_lines.is_empty() && opts.formats.patch {
        bail!("-I/--ignore-matching-lines with a patch format is not supported");
    }

    let repo = match gix::discover(".") {
        Ok(r) => r,
        Err(_) => {
            eprintln!("fatal: not a git repository (or any of the parent directories): .git");
            return Ok(ExitCode::from(128));
        }
    };

    // Finalize the pickaxe now that the whole line has been read.
    opts.pickaxe = match finalize_pickaxe(
        &repo,
        pickaxe_pending,
        find_object_args,
        pickaxe_all,
        pickaxe_regex,
    ) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(ExitCode::from(128));
        }
    };

    // -R swaps the two prefixes once, globally.
    if opts.reverse {
        std::mem::swap(&mut opts.src_prefix, &mut opts.dst_prefix);
    }

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;

    let hexsz = repo.object_hash().len_in_hex();
    let base_abbrev = base_abbrev(&repo);
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut any_pair = false;
    let mut batch: Vec<Pair> = Vec::new();
    let mut cursor = 0usize;

    // Records are NUL-terminated fields; a zero-length header field closes a batch.
    while cursor < input.len() {
        let Some(end) = input[cursor..].iter().position(|&b| b == 0) else {
            return Ok(fatal("invalid raw diff input"));
        };
        let header = &input[cursor..cursor + end];
        cursor += end + 1;

        if header.is_empty() {
            match flush(&mut out, &repo, &mut cache, &batch, &opts, base_abbrev)? {
                Ok(()) => {}
                Err(code) => return Ok(code),
            }
            out.write_all(b"\0")?;
            out.flush()?;
            batch.clear();
            continue;
        }

        let pair = match parse_header(header, hexsz) {
            Ok(p) => p,
            Err(msg) => return Ok(fatal(&msg)),
        };
        let (old_path, rest) = match take_field(&input, cursor) {
            Some(v) => v,
            None => return Ok(fatal("invalid raw diff input")),
        };
        cursor = rest;
        let new_path = if matches!(pair.0, b'R' | b'C') {
            let (p, rest) = match take_field(&input, cursor) {
                Some(v) => v,
                None => return Ok(fatal("invalid raw diff input")),
            };
            cursor = rest;
            p
        } else {
            old_path.clone()
        };

        let (_, mut pair) = pair;
        pair.old_path = old_path;
        pair.new_path = new_path;
        any_pair = true;
        batch.push(pair);
    }

    match flush(&mut out, &repo, &mut cache, &batch, &opts, base_abbrev)? {
        Ok(()) => {}
        Err(code) => return Ok(code),
    }
    out.flush()?;

    Ok(if opts.exit_code && any_pair {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Report a git-style fatal error and yield its exit code.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

fn parse_ctx(s: &str) -> Result<u32> {
    s.parse::<u32>()
        .map_err(|_| anyhow::anyhow!("invalid context line count {s:?}"))
}

/// Parse a bare integer for the `--stat-*` width options; git treats a bad value as unset.
fn parse_i64(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(-1)
}

/// Parse `--stat=<width>[,<name-width>[,<count>]]`.
fn parse_stat_geometry(sw: &mut StatWidths, spec: &str) {
    let mut it = spec.split(',');
    if let Some(w) = it.next() {
        if let Ok(v) = w.trim().parse::<i64>() {
            sw.width = v;
        }
    }
    if let Some(n) = it.next() {
        if let Ok(v) = n.trim().parse::<i64>() {
            sw.name_width = v;
        }
    }
    if let Some(c) = it.next() {
        if let Ok(v) = c.trim().parse::<i64>() {
            sw.count = v;
        }
    }
}

/// `--diff-filter=<letters>`: uppercase includes, lowercase excludes, `*` is all-or-none.
fn parse_filter(spec: &str) -> Filter {
    let mut keep = Vec::new();
    let mut exclude = Vec::new();
    let mut all_or_none = false;
    let mut has_include = false;
    for c in spec.bytes() {
        if c == b'*' {
            all_or_none = true;
            continue;
        }
        if c.is_ascii_lowercase() {
            exclude.push(c.to_ascii_uppercase());
        } else {
            keep.push(c);
            has_include = true;
        }
    }
    Filter {
        only_exclude: !has_include && !all_or_none && !exclude.is_empty(),
        keep,
        exclude,
        all_or_none,
    }
}

/// Resolve the deferred pickaxe request into a [`Pickaxe`]. The last of `-S`/`-G`/
/// `--find-object` wins (they share one option slot in git).
fn finalize_pickaxe(
    repo: &gix::Repository,
    pending: Option<(u8, Vec<u8>)>,
    find_object_args: Vec<String>,
    all: bool,
    regex: bool,
) -> std::result::Result<Option<Pickaxe>, String> {
    if !find_object_args.is_empty() {
        let mut ids = Vec::new();
        for arg in &find_object_args {
            match repo.rev_parse_single(arg.as_str()) {
                Ok(id) => ids.push(id.detach()),
                Err(_) => return Err(format!("error: unable to resolve '{arg}'")),
            }
        }
        return Ok(Some(Pickaxe {
            kind: PickaxeKind::ObjFind(ids),
            all,
        }));
    }
    let Some((which, pat)) = pending else {
        return Ok(None);
    };
    let kind = match which {
        b'G' => {
            let re = compile_regex(&pat).map_err(|e| format!("fatal: invalid regex: {e}"))?;
            PickaxeKind::Grep(Needle::Regex(re))
        }
        _ => {
            let needle = if regex {
                let re = compile_regex(&pat).map_err(|e| format!("fatal: invalid regex: {e}"))?;
                Needle::Regex(re)
            } else {
                Needle::Literal(pat)
            };
            PickaxeKind::Occurrences(needle)
        }
    };
    Ok(Some(Pickaxe { kind, all }))
}

/// Read the NUL-terminated field starting at `at`, returning it and the next offset.
fn take_field(input: &[u8], at: usize) -> Option<(BString, usize)> {
    let end = input.get(at..)?.iter().position(|&b| b == 0)?;
    Some((BString::from(&input[at..at + end]), at + end + 1))
}

/// Parse `:<omode> <nmode> <ooid> <noid> <status>` into a pair with empty path fields.
fn parse_header(header: &[u8], hexsz: usize) -> Result<(u8, Pair), String> {
    let invalid = || "invalid raw diff input".to_string();
    if header.first() != Some(&b':') {
        return Err(invalid());
    }
    let body = &header[1..];
    let mode_end = 6;
    if body.len() < 6 + 1 + 6 + 1 {
        return Err(invalid());
    }
    let old_mode = parse_mode(&body[..mode_end]).ok_or_else(invalid)?;
    if body[6] != b' ' {
        return Err(invalid());
    }
    let new_mode = parse_mode(&body[7..13]).ok_or_else(invalid)?;
    if body[13] != b' ' {
        return Err(invalid());
    }

    let oid_at = 14;
    let old_id = parse_oid(body, oid_at, hexsz)?;
    let new_at = oid_at + hexsz + 1;
    let new_id = parse_oid(body, new_at, hexsz)?;

    let status_at = new_at + hexsz + 1;
    if status_at >= body.len() {
        return Err(invalid());
    }
    let status = BString::from(&body[status_at..]);
    if !status[0].is_ascii_uppercase() {
        return Err(invalid());
    }

    if old_mode & IFMT == 0o040000 || new_mode & IFMT == 0o040000 {
        return Err("tree objects not supported".to_string());
    }

    let kind = status[0];
    Ok((
        kind,
        Pair {
            old_mode,
            new_mode,
            old_id,
            new_id,
            status,
            old_path: BString::default(),
            new_path: BString::default(),
        },
    ))
}

fn parse_mode(field: &[u8]) -> Option<u32> {
    let s = std::str::from_utf8(field).ok()?;
    u32::from_str_radix(s, 8).ok()
}

/// Parse the full-length hex id at `at`, followed by a single space.
fn parse_oid(body: &[u8], at: usize, hexsz: usize) -> Result<ObjectId, String> {
    let fail = || {
        format!(
            "unable to parse object id: {}",
            body.get(at..).unwrap_or_default().as_bstr()
        )
    };
    let end = at + hexsz;
    if body.len() <= end || body[end] != b' ' {
        return Err(fail());
    }
    ObjectId::from_hex(&body[at..end]).map_err(|_| fail())
}

/// Render one batch of pairs after running git's `diffcore_std` pipeline over it.
///
/// The inner `Result` carries a git fatal exit code (e.g. an unreadable blob) so the
/// caller can stop after the bytes already written.
#[allow(clippy::type_complexity)]
fn flush(
    out: &mut impl Write,
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    batch: &[Pair],
    opts: &Opts,
    base_abbrev: usize,
) -> Result<std::result::Result<(), ExitCode>> {
    if batch.is_empty() || opts.formats.no_output {
        return Ok(Ok(()));
    }

    let mut pairs: Vec<Pair> = batch.to_vec();

    // ---- diffcore_std order: pickaxe -> rotate -> apply_filter ----
    if let Some(px) = &opts.pickaxe {
        let mut keep = Vec::with_capacity(pairs.len());
        for p in &pairs {
            match pickaxe_hit(repo, cache, px, p, opts) {
                Ok(hit) => keep.push(hit),
                Err(code) => return Ok(Err(code)),
            }
        }
        if px.all {
            if !keep.iter().any(|k| *k) {
                pairs.clear();
            }
        } else {
            let mut idx = 0usize;
            pairs.retain(|_| {
                let k = keep[idx];
                idx += 1;
                k
            });
        }
    }

    if let Some(anchor) = &opts.anchor {
        rotate(&mut pairs, anchor);
    }

    if let Some(f) = &opts.filter {
        if f.all_or_none {
            if !pairs.iter().any(|p| f.matches(p.kind())) {
                pairs.clear();
            }
        } else {
            pairs.retain(|p| f.matches(p.kind()));
        }
    }

    // `--relative` re-anchors the rendered paths and drops what falls outside.
    apply_relative(repo, &mut pairs, &opts.relative)?;

    // `-R`: swap each pair's two sides for display (the prefixes were swapped globally).
    if opts.reverse {
        for p in &mut pairs {
            reverse_pair(p);
        }
    }

    if pairs.is_empty() {
        return Ok(Ok(()));
    }

    let mut buf: Vec<u8> = Vec::new();

    // ---- name/raw group ----
    if opts.formats.name_group() {
        for p in &pairs {
            render_name(&mut buf, p, &opts.formats);
        }
    }

    // ---- content analyses (numstat/stat/shortstat share these) ----
    let stats_wanted = opts.formats.numstat || opts.formats.stat || opts.formats.shortstat;
    let files: Vec<StatFile> = if stats_wanted {
        let mut analyses = Vec::with_capacity(pairs.len());
        for p in &pairs {
            match analyze(repo, cache, p, opts) {
                Ok(a) => analyses.push(a),
                Err(code) => {
                    out.write_all(&buf)?;
                    return Ok(Err(code));
                }
            }
        }
        compute_diffstat(&pairs, &analyses, opts)
    } else {
        Vec::new()
    };

    // ---- stat block (numstat, then diffstat, then shortstat), no internal separators ----
    if opts.formats.numstat {
        render_numstat(&mut buf, &files);
    }
    if opts.formats.stat {
        render_stat(&mut buf, &files, &opts.stat);
    }
    if opts.formats.shortstat {
        render_shortstat(&mut buf, &files);
    }

    // ---- summary ----
    let summary_shown = opts.formats.summary && !summary_is_empty(&pairs);
    if summary_shown {
        for p in &pairs {
            render_summary(&mut buf, p);
        }
    }

    // ---- patch ----
    // git sets `separator` whenever any non-patch format is requested (even if it
    // produced no bytes), so `--stat -p` with an empty stat still emits the NUL.
    if opts.formats.patch {
        let separator = opts.formats.name_group()
            || opts.formats.numstat
            || opts.formats.stat
            || opts.formats.shortstat
            || summary_shown;
        if separator {
            buf.push(b'\0');
        }
        for p in &pairs {
            if let Err(code) = render_patch(&mut buf, repo, cache, p, opts, base_abbrev) {
                out.write_all(&buf)?;
                return Ok(Err(code));
            }
        }
    }

    out.write_all(&buf)?;
    Ok(Ok(()))
}

/// `diffcore_rotate`: `--rotate-to` moves everything before the anchor to the end;
/// `--skip-to` drops it. The anchor is the first path (`p->two->path`) not less than
/// the target; when the target sorts after every path, git leaves the queue untouched
/// (it does not error, unlike `git diff`).
fn rotate(pairs: &mut Vec<Pair>, anchor: &Anchor) {
    if pairs.is_empty() {
        return;
    }
    let (target, skip): (&BString, bool) = match anchor {
        Anchor::Rotate(t) => (t, false),
        Anchor::Skip(t) => (t, true),
    };
    let mut idx = pairs.len();
    for (i, p) in pairs.iter().enumerate() {
        match target.as_slice().cmp(p.new_path.as_slice()) {
            Ordering::Equal | Ordering::Less => {
                idx = i;
                break;
            }
            Ordering::Greater => {}
        }
    }
    let idx = if idx == pairs.len() { 0 } else { idx };
    if skip {
        pairs.drain(..idx);
    } else {
        pairs.rotate_left(idx);
    }
}

/// `-R`: swap the two sides of a pair for rendering. git applies reverse at the emit
/// layer, so the raw status letter is *not* recomputed — only the modes, ids and paths
/// move — which is why a reversed deletion still prints its `D` in `--raw`.
fn reverse_pair(p: &mut Pair) {
    std::mem::swap(&mut p.old_mode, &mut p.new_mode);
    std::mem::swap(&mut p.old_id, &mut p.new_id);
    std::mem::swap(&mut p.old_path, &mut p.new_path);
}

/// `--relative[=<p>]`: keep only records under `<p>`, with that prefix stripped from
/// the rendered paths.
fn apply_relative(
    repo: &gix::Repository,
    pairs: &mut Vec<Pair>,
    relative: &Relative,
) -> Result<()> {
    let prefix: BString = match relative {
        Relative::No => return Ok(()),
        Relative::Path(p) => p.clone(),
        Relative::Cwd => match repo.prefix()? {
            Some(p) => gix::path::into_bstr(p).into_owned(),
            None => return Ok(()),
        },
    };
    if prefix.is_empty() {
        return Ok(());
    }
    let mut needle: Vec<u8> = prefix.into();
    if needle.last() != Some(&b'/') {
        needle.push(b'/');
    }
    pairs.retain_mut(|p| {
        // git filters on the destination path and strips the prefix from both.
        match p.new_path.strip_prefix(needle.as_slice()) {
            Some(rest) => {
                let rest = rest.to_vec();
                if let Some(o) = p.old_path.strip_prefix(needle.as_slice()) {
                    p.old_path = o.to_vec().into();
                }
                p.new_path = rest.into();
                true
            }
            None => false,
        }
    });
    Ok(())
}

/// The "delete the old side" half of a type-change patch.
fn as_deletion(p: &Pair) -> Pair {
    Pair {
        old_mode: p.old_mode,
        new_mode: 0,
        old_id: p.old_id,
        new_id: ObjectId::null(p.old_id.kind()),
        status: BString::from("D"),
        old_path: p.old_path.clone(),
        new_path: p.old_path.clone(),
    }
}

/// The "create the new side" half of a type-change patch.
fn as_creation(p: &Pair) -> Pair {
    Pair {
        old_mode: 0,
        new_mode: p.new_mode,
        old_id: ObjectId::null(p.new_id.kind()),
        new_id: p.new_id,
        status: BString::from("A"),
        old_path: p.new_path.clone(),
        new_path: p.new_path.clone(),
    }
}

/// `--raw` / `--name-only` / `--name-status` for one pair (`--name-status` wins when
/// several are set, matching `flush_one_pair`'s precedence).
fn render_name(out: &mut Vec<u8>, p: &Pair, f: &Formats) {
    let two_paths = matches!(p.kind(), b'R' | b'C');
    if f.name_status {
        out.extend_from_slice(&p.status);
        out.push(b'\0');
    } else if f.name_only {
        out.extend_from_slice(&p.new_path);
        out.push(b'\0');
        return;
    } else {
        out.extend_from_slice(
            format!(
                ":{:06o} {:06o} {} {} ",
                p.old_mode,
                p.new_mode,
                p.old_id.to_hex(),
                p.new_id.to_hex()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&p.status);
        out.push(b'\0');
    }
    out.extend_from_slice(&p.old_path);
    out.push(b'\0');
    if two_paths {
        out.extend_from_slice(&p.new_path);
        out.push(b'\0');
    }
}

// ---------------------------------------------------------------------------
// blob analysis
// ---------------------------------------------------------------------------

/// A pair whose surviving side is a submodule link; those never touch the object database.
fn is_gitlink(p: &Pair) -> bool {
    (p.old_valid() && p.old_mode & IFMT == 0o160000)
        || (p.new_valid() && p.new_mode & IFMT == 0o160000)
}

fn gitlink_counts(p: &Pair) -> (u32, u32) {
    (u32::from(p.new_valid()), u32::from(p.old_valid()))
}

/// The `Subproject commit <oid>` pseudo-diff git emits for `160000` entries.
fn gitlink_hunks(p: &Pair) -> Vec<u8> {
    let line = |id: &ObjectId| format!("Subproject commit {}\n", id.to_hex());
    let mut hunks = Vec::new();
    match (p.old_valid(), p.new_valid()) {
        (true, true) => {
            hunks.extend_from_slice(b"@@ -1 +1 @@\n");
            hunks.extend_from_slice(format!("-{}", line(&p.old_id)).as_bytes());
            hunks.extend_from_slice(format!("+{}", line(&p.new_id)).as_bytes());
        }
        (false, true) => {
            hunks.extend_from_slice(b"@@ -0,0 +1 @@\n");
            hunks.extend_from_slice(format!("+{}", line(&p.new_id)).as_bytes());
        }
        (true, false) => {
            hunks.extend_from_slice(b"@@ -1 +0,0 @@\n");
            hunks.extend_from_slice(format!("-{}", line(&p.old_id)).as_bytes());
        }
        (false, false) => {}
    }
    hunks
}

/// Verify both non-null sides are present in the object database, as git does before
/// producing any patch body.
fn check_readable(repo: &gix::Repository, p: &Pair) -> std::result::Result<(), ExitCode> {
    for id in [p.old_id, p.new_id] {
        if id.is_null() {
            continue;
        }
        match repo.try_find_header(id) {
            Ok(Some(_)) => {}
            _ => return Err(fatal(&format!("unable to read {}", id.to_hex()))),
        }
    }
    Ok(())
}

/// Diff the pair's two blobs through the gitoxide blob platform, honouring the active
/// whitespace comparison rules and `-I` line filters.
fn analyze(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    p: &Pair,
    opts: &Opts,
) -> std::result::Result<Analysis, ExitCode> {
    if is_gitlink(p) {
        let (add, del) = gitlink_counts(p);
        return Ok(Analysis {
            add,
            del,
            binary: false,
            old_data: Vec::new(),
            new_data: Vec::new(),
            hunks: gitlink_hunks(p),
        });
    }
    check_readable(repo, p)?;

    let old_kind = mode_kind(if p.old_valid() {
        p.old_mode
    } else {
        p.new_mode
    });
    let new_kind = mode_kind(if p.new_valid() {
        p.new_mode
    } else {
        p.old_mode
    });
    let set = |cache: &mut gix::diff::blob::Platform| -> Result<()> {
        cache.set_resource(
            p.old_id,
            old_kind,
            p.old_path.as_bstr(),
            ResourceKind::OldOrSource,
            &repo.objects,
        )?;
        cache.set_resource(
            p.new_id,
            new_kind,
            p.new_path.as_bstr(),
            ResourceKind::NewOrDestination,
            &repo.objects,
        )?;
        Ok(())
    };
    if set(cache).is_err() {
        return Err(fatal("unable to diff blob pair"));
    }
    let prep = match cache.prepare_diff() {
        Ok(p) => p,
        Err(_) => return Err(fatal("unable to diff blob pair")),
    };
    let old_data = prep.old.data.as_slice().unwrap_or_default().to_vec();
    let new_data = prep.new.data.as_slice().unwrap_or_default().to_vec();

    match prep.operation {
        Operation::SourceOrDestinationIsBinary => Ok(Analysis {
            add: 0,
            del: 0,
            binary: true,
            old_data,
            new_data,
            hunks: Vec::new(),
        }),
        Operation::ExternalCommand { .. } => Err(fatal("external diff drivers are not supported")),
        Operation::InternalDiff { algorithm } => {
            let before: Vec<&[u8]> = byte_lines(&old_data);
            let after: Vec<&[u8]> = byte_lines(&new_data);
            let mut input: InternedInput<Vec<u8>> = InternedInput::default();
            input.update_before(before.iter().map(|l| normalize(l, opts.ws)));
            input.update_after(after.iter().map(|l| normalize(l, opts.ws)));

            let diff = diff_with_slider_heuristics(algorithm, &input);
            // `xdl_mark_ignorable_regex`: a change group whose every removed and added
            // line matches an `-I` pattern contributes nothing to the counts.
            let (add, del) = if opts.ignore_lines.is_empty() {
                (diff.count_additions(), diff.count_removals())
            } else {
                let mut a = 0u32;
                let mut d = 0u32;
                for hunk in diff.hunks() {
                    let ignorable = hunk
                        .before
                        .clone()
                        .all(|i| matches_any(&opts.ignore_lines, before[i as usize]))
                        && hunk
                            .after
                            .clone()
                            .all(|i| matches_any(&opts.ignore_lines, after[i as usize]));
                    if ignorable {
                        continue;
                    }
                    d += hunk.before.clone().count() as u32;
                    a += hunk.after.clone().count() as u32;
                }
                (a, d)
            };

            // Hunks render the *original* line bytes, indexed by the cursors the hunk
            // header establishes, so whitespace-normalized comparison never leaks into
            // the printed patch. `-I` is rejected alongside a patch format, so the raw
            // (unmarked) hunks are correct whenever they are actually emitted.
            let hunks = if diff.hunks().next().is_some() {
                let sink = PatchSink {
                    buf: Vec::new(),
                    before: &before,
                    after: &after,
                    func_prev: -1,
                };
                match UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(opts.ctx))
                    .consume()
                {
                    Ok(h) => h,
                    Err(_) => return Err(fatal("unable to diff blob pair")),
                }
            } else {
                Vec::new()
            };

            drop(before);
            drop(after);
            Ok(Analysis {
                add,
                del,
                binary: false,
                old_data,
                new_data,
                hunks,
            })
        }
    }
}

/// Split `data` into lines the way `imara_diff::sources::byte_lines` does: the
/// terminator stays attached, and a final line without one is still a line.
fn byte_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let len = rest.find_byte(b'\n').map_or(rest.len(), |i| i + 1);
        let (line, tail) = rest.split_at(len);
        out.push(line);
        rest = tail;
    }
    out
}

/// The form of a line used for *comparison* only; the original bytes are always printed.
fn normalize(line: &[u8], ws: Whitespace) -> Vec<u8> {
    let is_space = |b: u8| matches!(b, b' ' | b'\t' | b'\x0b' | b'\x0c' | b'\r' | b'\n');
    match ws {
        Whitespace::Keep => line.to_vec(),
        Whitespace::IgnoreAll => line.iter().copied().filter(|b| !is_space(*b)).collect(),
        Whitespace::IgnoreAtEol => {
            let end = line
                .iter()
                .rposition(|b| !is_space(*b))
                .map_or(0, |i| i + 1);
            line[..end].to_vec()
        }
        Whitespace::IgnoreCrAtEol => {
            let body = strip_terminator(line);
            let end = body.len() - usize::from(body.last() == Some(&b'\r'));
            body[..end].to_vec()
        }
        Whitespace::IgnoreChange => {
            let end = line
                .iter()
                .rposition(|b| !is_space(*b))
                .map_or(0, |i| i + 1);
            let mut out = Vec::with_capacity(end);
            let mut in_space = false;
            for &b in &line[..end] {
                if is_space(b) {
                    in_space = true;
                    continue;
                }
                if in_space {
                    out.push(b' ');
                    in_space = false;
                }
                out.push(b);
            }
            out
        }
    }
}

/// `has_changes()` for `-S`, `diff_grep()` for `-G`, and `pickaxe_match()`'s objfind
/// branch for `--find-object`.
fn pickaxe_hit(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    px: &Pickaxe,
    p: &Pair,
    opts: &Opts,
) -> std::result::Result<bool, ExitCode> {
    if let PickaxeKind::ObjFind(ids) = &px.kind {
        return Ok((p.old_valid() && ids.contains(&p.old_id))
            || (p.new_valid() && ids.contains(&p.new_id)));
    }
    if !p.old_valid() && !p.new_valid() {
        return Ok(false);
    }
    let an = analyze(repo, cache, p, opts)?;
    Ok(match &px.kind {
        PickaxeKind::Occurrences(needle) => {
            if let Needle::Literal(n) = needle {
                if n.is_empty() {
                    return Ok(false);
                }
            }
            let old = if p.old_valid() {
                needle.count(&an.old_data)
            } else {
                0
            };
            let new = if p.new_valid() {
                needle.count(&an.new_data)
            } else {
                0
            };
            match (p.old_valid(), p.new_valid()) {
                (false, true) => new != 0,
                (true, false) => old != 0,
                _ => old != new,
            }
        }
        PickaxeKind::Grep(needle) => {
            if !p.old_valid() {
                return Ok(needle.is_match(&an.new_data));
            }
            if !p.new_valid() {
                return Ok(needle.is_match(&an.old_data));
            }
            byte_lines(&an.hunks).iter().any(|l| {
                matches!(l.first().copied(), Some(b'+') | Some(b'-')) && needle.is_match(&l[1..])
            })
        }
        PickaxeKind::ObjFind(_) => unreachable!("objfind handled above"),
    })
}

// ---------------------------------------------------------------------------
// diffstat (--numstat / --stat / --shortstat)
// ---------------------------------------------------------------------------

/// One `struct diffstat_file`.
struct StatFile {
    /// `M`, `A`, `D`, `T`, `R`, `C`.
    status: u8,
    old_path: BString,
    new_path: BString,
    /// The name as printed by `--stat`, `pprint_rename`d and possibly `--compact-summary`
    /// annotated.
    print_name: Vec<u8>,
    added: u32,
    deleted: u32,
    binary: bool,
}

/// `compute_diffstat()`, including `builtin_diffstat()`'s rule that a plain `M` entry
/// with no added, no deleted and an unchanged mode is dropped outright.
fn compute_diffstat(pairs: &[Pair], analyses: &[Analysis], opts: &Opts) -> Vec<StatFile> {
    let mut out = Vec::new();
    for (p, an) in pairs.iter().zip(analyses) {
        let (added, deleted) = if an.binary {
            // Binary counts are byte sizes, not lines.
            (an.new_data.len() as u32, an.old_data.len() as u32)
        } else {
            (an.add, an.del)
        };
        if p.kind() == b'M' && added == 0 && deleted == 0 && p.old_mode == p.new_mode && !an.binary
        {
            continue;
        }
        out.push(StatFile {
            status: p.kind(),
            old_path: p.old_path.clone(),
            new_path: p.new_path.clone(),
            print_name: stat_print_name(p, opts.stat.with_summary),
            added,
            deleted,
            binary: an.binary,
        });
    }
    out
}

/// `fill_print_name()` plus `get_compact_summary()`.
fn stat_print_name(p: &Pair, with_summary: bool) -> Vec<u8> {
    let mut name = if matches!(p.kind(), b'R' | b'C') {
        pprint_rename(&p.old_path, &p.new_path)
    } else {
        p.new_path.to_vec()
    };
    if with_summary {
        if let Some(comment) = compact_summary_comment(p) {
            name.extend_from_slice(b" (");
            name.extend_from_slice(comment.as_bytes());
            name.push(b')');
        }
    }
    name
}

/// `get_compact_summary()`: the `(new)`, `(gone)`, `(mode +x)`, … annotation
/// `--compact-summary` appends to a name.
fn compact_summary_comment(p: &Pair) -> Option<&'static str> {
    match p.kind() {
        b'A' => Some(match p.new_mode {
            0o120000 => "new +l",
            0o100755 => "new +x",
            _ => "new",
        }),
        b'D' => Some("gone"),
        _ => {
            if p.old_mode == 0o120000 && p.new_mode != 0o120000 {
                Some("mode -l")
            } else if p.old_mode != 0o120000 && p.new_mode == 0o120000 {
                Some("mode +l")
            } else if p.old_mode == 0o100644 && p.new_mode == 0o100755 {
                Some("mode +x")
            } else if p.old_mode == 0o100755 && p.new_mode == 0o100644 {
                Some("mode -x")
            } else {
                None
            }
        }
    }
}

/// `show_numstat()` with git's `-z` field layout.
fn render_numstat(out: &mut Vec<u8>, files: &[StatFile]) {
    for f in files {
        if f.binary {
            out.extend_from_slice(b"-\t-\t");
        } else {
            out.extend_from_slice(format!("{}\t{}\t", f.added, f.deleted).as_bytes());
        }
        if matches!(f.status, b'R' | b'C') {
            out.push(b'\0');
            out.extend_from_slice(&f.old_path);
            out.push(b'\0');
            out.extend_from_slice(&f.new_path);
        } else {
            out.extend_from_slice(&f.new_path);
        }
        out.push(b'\0');
    }
}

/// `show_shortstats()`.
fn render_shortstat(out: &mut Vec<u8>, files: &[StatFile]) {
    if files.is_empty() {
        return;
    }
    let (total, adds, dels) = stat_totals(files);
    stat_summary(out, total, adds, dels);
}

fn stat_totals(files: &[StatFile]) -> (u32, u32, u32) {
    let total = files.len() as u32;
    let (mut adds, mut dels) = (0u32, 0u32);
    for f in files {
        if !f.binary {
            adds += f.added;
            dels += f.deleted;
        }
    }
    (total, adds, dels)
}

/// `print_stat_summary_inserts_deletes()`.
fn stat_summary(out: &mut Vec<u8>, files: u32, insertions: u32, deletions: u32) {
    if files == 0 {
        out.extend_from_slice(b" 0 files changed\n");
        return;
    }
    out.extend_from_slice(
        format!(" {files} file{} changed", if files == 1 { "" } else { "s" }).as_bytes(),
    );
    if insertions != 0 || deletions == 0 {
        out.extend_from_slice(
            format!(
                ", {insertions} insertion{}(+)",
                if insertions == 1 { "" } else { "s" }
            )
            .as_bytes(),
        );
    }
    if deletions != 0 || insertions == 0 {
        out.extend_from_slice(
            format!(
                ", {deletions} deletion{}(-)",
                if deletions == 1 { "" } else { "s" }
            )
            .as_bytes(),
        );
    }
    out.push(b'\n');
}

fn decimal_width(n: u32) -> i64 {
    let mut w = 1i64;
    let mut n = n / 10;
    while n > 0 {
        w += 1;
        n /= 10;
    }
    w
}

/// `scale_linear()` from `diff.c`.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// `show_stats()`. `stat_width == -1` means "terminal width", which is 80 for a
/// non-tty just like git's `term_columns()` fallback.
fn render_stat(out: &mut Vec<u8>, files: &[StatFile], sw: &StatWidths) {
    if files.is_empty() {
        return;
    }
    let mut count: i64 = if sw.count != 0 {
        sw.count
    } else {
        files.len() as i64
    };

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    let mut i: i64 = 0;
    while i < count && i < files.len() as i64 {
        let f = &files[i as usize];
        let change = (f.added + f.deleted) as i64;
        i += 1;
        max_len = max_len.max(f.print_name.len() as i64);
        if f.binary {
            let w = 14 + decimal_width(f.added) + decimal_width(f.deleted);
            bin_width = bin_width.max(w);
            number_width = 3;
            continue;
        }
        max_change = max_change.max(change);
    }
    count = i;

    let mut width: i64 = if sw.width == -1 {
        80
    } else if sw.width != 0 {
        sw.width
    } else {
        80
    };
    number_width = number_width.max(decimal_width(max_change as u32));
    let stat_name_width = if sw.name_width == -1 {
        0
    } else {
        sw.name_width
    };
    let stat_graph_width = if sw.graph_width == -1 {
        0
    } else {
        sw.graph_width
    };

    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = if max_change + 4 > bin_width {
        max_change
    } else {
        bin_width - 4
    };
    if stat_graph_width > 0 && stat_graph_width < graph_width {
        graph_width = stat_graph_width;
    }
    let mut name_width = if stat_name_width > 0 && stat_name_width < max_len {
        stat_name_width
    } else {
        max_len
    };

    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = width * 3 / 8 - number_width - 6;
            if graph_width < 6 {
                graph_width = 6;
            }
        }
        if stat_graph_width > 0 && graph_width > stat_graph_width {
            graph_width = stat_graph_width;
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    for f in files.iter().take(count.max(0) as usize) {
        let (added, deleted) = (f.added as i64, f.deleted as i64);

        let full = &f.print_name;
        let (prefix, name): (&str, &[u8]) = if name_width < full.len() as i64 {
            let len = (name_width - 3).max(0);
            let start = full.len() - len as usize;
            let tail = &full[start..];
            let tail = match tail.iter().position(|b| *b == b'/') {
                Some(pos) => &tail[pos..],
                None => tail,
            };
            ("...", tail)
        } else {
            ("", full.as_slice())
        };
        let padding = (name_width - prefix.len() as i64 - name.len() as i64).max(0) as usize;

        out.push(b' ');
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        out.extend_from_slice(b" | ");

        if f.binary {
            out.extend_from_slice(
                format!("{:>width$}", "Bin", width = number_width.max(0) as usize).as_bytes(),
            );
            if added == 0 && deleted == 0 {
                out.push(b'\n');
                continue;
            }
            out.extend_from_slice(format!(" {deleted} -> {added} bytes\n").as_bytes());
            continue;
        }

        let (mut add, mut del) = (added, deleted);
        if graph_width <= max_change {
            let mut total = scale_linear(add + del, graph_width, max_change);
            if total < 2 && add > 0 && del > 0 {
                total = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = total - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = total - del;
            }
        }
        out.extend_from_slice(
            format!(
                "{:>width$}",
                added + deleted,
                width = number_width.max(0) as usize
            )
            .as_bytes(),
        );
        if added + deleted != 0 {
            out.push(b' ');
        }
        out.extend_from_slice(&b"+".repeat(add.max(0) as usize));
        out.extend_from_slice(&b"-".repeat(del.max(0) as usize));
        out.push(b'\n');
    }

    if (count as usize) < files.len() {
        out.extend_from_slice(b" ...\n");
    }

    let (total, adds, dels) = stat_totals(files);
    stat_summary(out, total, adds, dels);
}

// ---------------------------------------------------------------------------
// --summary
// ---------------------------------------------------------------------------

fn summary_is_empty(pairs: &[Pair]) -> bool {
    for p in pairs {
        match p.kind() {
            b'A' | b'D' | b'C' | b'R' => return false,
            _ => {
                if p.old_mode != 0 && p.new_mode != 0 && p.old_mode != p.new_mode {
                    return false;
                }
            }
        }
    }
    true
}

/// `diff_summary()`.
fn render_summary(out: &mut Vec<u8>, p: &Pair) {
    match p.kind() {
        b'D' => summary_mode_name(out, "delete", p.old_mode, &p.old_path),
        b'A' => summary_mode_name(out, "create", p.new_mode, &p.new_path),
        b'C' => summary_rename_copy(out, "copy", p),
        b'R' => summary_rename_copy(out, "rename", p),
        _ => summary_mode_change(out, p, true),
    }
}

/// `show_file_mode_name()`.
fn summary_mode_name(out: &mut Vec<u8>, verb: &str, mode: u32, path: &BString) {
    if mode != 0 {
        out.extend_from_slice(format!(" {verb} mode {:06o} ", mode).as_bytes());
    } else {
        out.extend_from_slice(format!(" {verb} ").as_bytes());
    }
    out.extend_from_slice(path);
    out.push(b'\n');
}

/// `show_rename_copy()`.
fn summary_rename_copy(out: &mut Vec<u8>, verb: &str, p: &Pair) {
    out.push(b' ');
    out.extend_from_slice(verb.as_bytes());
    out.push(b' ');
    out.extend_from_slice(&pprint_rename(&p.old_path, &p.new_path));
    out.extend_from_slice(format!(" ({}%)\n", p.score()).as_bytes());
    summary_mode_change(out, p, false);
}

/// `show_mode_change()`: emit the ` mode change ...` line when the modes differ.
/// `show_name` appends the path (the plain-modification case); rename/copy omit it.
fn summary_mode_change(out: &mut Vec<u8>, p: &Pair, show_name: bool) {
    if p.old_mode != 0 && p.new_mode != 0 && p.old_mode != p.new_mode {
        out.extend_from_slice(
            format!(" mode change {:06o} => {:06o}", p.old_mode, p.new_mode).as_bytes(),
        );
        if show_name {
            out.push(b' ');
            out.extend_from_slice(&p.new_path);
        }
        out.push(b'\n');
    }
}

/// `pprint_rename()`: compress the common leading directory and trailing suffix of a
/// rename/copy into `pfx{old-mid => new-mid}sfx`.
fn pprint_rename(a: &[u8], b: &[u8]) -> Vec<u8> {
    let (la, lb) = (a.len(), b.len());
    let at = |s: &[u8], i: usize| -> u8 {
        if i < s.len() {
            s[i]
        } else {
            0 // virtual NUL terminator, matching git's pointer walk
        }
    };

    // Common prefix, recorded up to and including the last shared slash.
    let mut pfx = 0usize;
    {
        let mut i = 0;
        while i < la && i < lb && a[i] == b[i] {
            if a[i] == b'/' {
                pfx = i + 1;
            }
            i += 1;
        }
    }

    // Common suffix, from the (virtual) terminators backwards, stopping at the prefix.
    let mut sfx = 0usize;
    {
        let pfx_adjust = if pfx > 0 { 1isize } else { 0 };
        let lo = pfx as isize - pfx_adjust;
        let mut oa = la as isize;
        let mut ob = lb as isize;
        while oa >= lo && ob >= lo && at(a, oa as usize) == at(b, ob as usize) {
            if at(a, oa as usize) == b'/' {
                sfx = la - oa as usize;
            }
            oa -= 1;
            ob -= 1;
        }
    }

    let a_mid = (la as isize - pfx as isize - sfx as isize).max(0) as usize;
    let b_mid = (lb as isize - pfx as isize - sfx as isize).max(0) as usize;

    let mut out = Vec::new();
    if pfx + sfx > 0 {
        out.extend_from_slice(&a[..pfx]);
        out.push(b'{');
        out.extend_from_slice(&a[pfx..pfx + a_mid]);
        out.extend_from_slice(b" => ");
        out.extend_from_slice(&b[pfx..pfx + b_mid]);
        out.push(b'}');
        out.extend_from_slice(&a[la - sfx..]);
    } else {
        out.extend_from_slice(a);
        out.extend_from_slice(b" => ");
        out.extend_from_slice(b);
    }
    out
}

// ---------------------------------------------------------------------------
// patch
// ---------------------------------------------------------------------------

/// Render one pair as one or two `diff --git` file sections (a type change splits into
/// a deletion patch followed by a creation patch, exactly as `run_diff()` does).
fn render_patch(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    p: &Pair,
    opts: &Opts,
    base_abbrev: usize,
) -> std::result::Result<(), ExitCode> {
    let steps: Vec<Pair> = if p.type_changed() {
        vec![as_deletion(p), as_creation(p)]
    } else {
        vec![p.clone()]
    };
    for step in &steps {
        let an = analyze(repo, cache, step, opts)?;
        emit_patch(out, repo, step, &an, opts, base_abbrev);
    }
    Ok(())
}

/// Emit a single file section for `p` using its precomputed [`Analysis`].
fn emit_patch(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    p: &Pair,
    an: &Analysis,
    opts: &Opts,
    base_abbrev: usize,
) {
    let kind = p.kind();
    let renamed = matches!(kind, b'R' | b'C');

    // `builtin_diff()` only emits the header once it has something to attach to it. A
    // plain modification whose content compares equal (a whitespace-only change under
    // `-w`) and whose mode is unchanged produces nothing at all.
    let must_show = !p.old_valid()
        || !p.new_valid()
        || renamed
        || p.old_mode != p.new_mode
        || an.binary
        || !an.hunks.is_empty();
    if !must_show {
        return;
    }

    let old_disp = if p.old_valid() || renamed {
        p.old_path.clone()
    } else {
        p.new_path.clone()
    };
    let new_disp = p.new_path.clone();

    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&opts.src_prefix);
    out.extend_from_slice(&old_disp);
    out.push(b' ');
    out.extend_from_slice(&opts.dst_prefix);
    out.extend_from_slice(&new_disp);
    out.push(b'\n');

    if !p.old_valid() {
        out.extend_from_slice(format!("new file mode {:06o}\n", p.new_mode).as_bytes());
    } else if !p.new_valid() {
        out.extend_from_slice(format!("deleted file mode {:06o}\n", p.old_mode).as_bytes());
    } else if p.old_mode != p.new_mode {
        out.extend_from_slice(
            format!("old mode {:06o}\nnew mode {:06o}\n", p.old_mode, p.new_mode).as_bytes(),
        );
    }

    if renamed {
        let verb = if kind == b'C' { "copy" } else { "rename" };
        out.extend_from_slice(format!("similarity index {}%\n", p.score()).as_bytes());
        out.extend_from_slice(format!("{verb} from ").as_bytes());
        out.extend_from_slice(&p.old_path);
        out.push(b'\n');
        out.extend_from_slice(format!("{verb} to ").as_bytes());
        out.extend_from_slice(&p.new_path);
        out.push(b'\n');
    }

    if p.old_id != p.new_id {
        out.extend_from_slice(b"index ");
        out.extend_from_slice(oid_text(repo, &p.old_id, base_abbrev, opts.full_index).as_bytes());
        out.extend_from_slice(b"..");
        out.extend_from_slice(oid_text(repo, &p.new_id, base_abbrev, opts.full_index).as_bytes());
        if p.old_valid() && p.new_valid() && p.old_mode == p.new_mode {
            out.extend_from_slice(format!(" {:06o}", p.new_mode).as_bytes());
        }
        out.push(b'\n');
    }

    let old_label = if p.old_valid() {
        let mut s = opts.src_prefix.to_vec();
        s.extend_from_slice(&old_disp);
        s
    } else {
        b"/dev/null".to_vec()
    };
    let new_label = if p.new_valid() {
        let mut s = opts.dst_prefix.to_vec();
        s.extend_from_slice(&new_disp);
        s
    } else {
        b"/dev/null".to_vec()
    };

    if an.binary {
        out.extend_from_slice(b"Binary files ");
        out.extend_from_slice(&old_label);
        out.extend_from_slice(b" and ");
        out.extend_from_slice(&new_label);
        out.extend_from_slice(b" differ\n");
    } else if !an.hunks.is_empty() {
        out.extend_from_slice(b"--- ");
        out.extend_from_slice(&old_label);
        out.push(b'\n');
        out.extend_from_slice(b"+++ ");
        out.extend_from_slice(&new_label);
        out.push(b'\n');
        out.extend_from_slice(&an.hunks);
    }
}

fn mode_kind(mode: u32) -> EntryKind {
    match mode & IFMT {
        0o120000 => EntryKind::Link,
        0o160000 => EntryKind::Commit,
        _ if mode & 0o111 != 0 => EntryKind::BlobExecutable,
        _ => EntryKind::Blob,
    }
}

/// The object id as it appears on an `index` line.
fn oid_text(repo: &gix::Repository, id: &ObjectId, base: usize, full: bool) -> String {
    if full {
        id.to_hex().to_string()
    } else if id.is_null() {
        "0".repeat(base)
    } else {
        match id.attach(repo).shorten() {
            Ok(prefix) => prefix.to_string(),
            Err(_) => id.to_hex_with_len(base).to_string(),
        }
    }
}

/// git's default `index`-line width: `core.abbrev` when set, else derived from the
/// packed object count with a floor of 7.
fn base_abbrev(repo: &gix::Repository) -> usize {
    let hexsz = repo.object_hash().len_in_hex();
    let snapshot = repo.config_snapshot();
    match snapshot.string("core.abbrev") {
        Some(v) => {
            let v = v.to_str_lossy().into_owned();
            match v.as_str() {
                "auto" => auto_abbrev(repo),
                "no" | "false" | "off" => hexsz,
                _ => v
                    .parse::<usize>()
                    .map(|n| n.clamp(4, hexsz))
                    .unwrap_or_else(|_| auto_abbrev(repo)),
            }
        }
        None => auto_abbrev(repo),
    }
}

/// `calculate_auto_hex_len` from `gix::Id::shorten`.
fn auto_abbrev(repo: &gix::Repository) -> usize {
    let count = repo.objects.packed_object_count().unwrap_or(0);
    (64 - count.leading_zeros()).div_ceil(2).max(7) as usize
}

/// One side of a hunk header (`@@ -<here> +<here> @@`): the length is omitted when it is
/// 1, and a zero length reports the preceding line number, exactly like `git diff`.
fn fmt_range(start: u32, len: u32) -> String {
    match len {
        1 => format!("{start}"),
        0 => format!("{},0", start.saturating_sub(1)),
        _ => format!("{start},{len}"),
    }
}

/// git's built-in `def_ff` heuristic: a record qualifies as a function line when it
/// starts with a letter, `_` or `$`; the text is capped at 80 bytes and right-trimmed.
fn def_ff(rec: &[u8]) -> Option<&[u8]> {
    let first = *rec.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return None;
    }
    let mut len = rec.len().min(FUNCNAME_MAX);
    while len > 0 && rec[len - 1].is_ascii_whitespace() {
        len -= 1;
    }
    Some(&rec[..len])
}

/// A [`ConsumeHunk`] sink rendering unified-diff hunks with git's hunk-header function
/// context and `\ No newline at end of file` markers.
///
/// The tokens the differ compared may be whitespace-normalized, so line *content* comes
/// from the original `before`/`after` tables, tracked by the cursors the hunk header
/// establishes.
struct PatchSink<'a> {
    buf: Vec<u8>,
    before: &'a [&'a [u8]],
    after: &'a [&'a [u8]],
    /// `funclineprev` from `xdl_emit_diff`: the previous hunk's search origin, which
    /// bounds how far back the next hunk may look.
    func_prev: i64,
}

impl<'a> PatchSink<'a> {
    /// `get_func_line`: scan the pre-image backwards from the line above the hunk down to
    /// (but not including) the previous hunk's origin.
    fn func_name(&mut self, before_hunk_start: u32) -> Option<&'a [u8]> {
        let origin = i64::from(before_hunk_start) - 2; // 0-based line above the hunk
        let limit = self.func_prev;
        self.func_prev = origin;

        let mut l = origin;
        let mut found = None;
        while l != limit && l >= 0 && (l as usize) < self.before.len() {
            if def_ff(self.before[l as usize]).is_some() {
                found = Some(l as usize);
                break;
            }
            l -= 1;
        }
        found.and_then(|idx| def_ff(self.before[idx]))
    }
}

impl ConsumeHunk for PatchSink<'_> {
    type Out = Vec<u8>;

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        let mut head = Vec::new();
        head.extend_from_slice(b"@@ -");
        head.extend_from_slice(
            fmt_range(header.before_hunk_start, header.before_hunk_len).as_bytes(),
        );
        head.extend_from_slice(b" +");
        head.extend_from_slice(
            fmt_range(header.after_hunk_start, header.after_hunk_len).as_bytes(),
        );
        head.extend_from_slice(b" @@");
        if let Some(func) = self.func_name(header.before_hunk_start) {
            if !func.is_empty() {
                head.push(b' ');
                head.extend_from_slice(func);
            }
        }
        head.push(b'\n');
        self.buf.extend_from_slice(&head);

        let mut bi = header.before_hunk_start.saturating_sub(1) as usize;
        let mut ai = header.after_hunk_start.saturating_sub(1) as usize;
        for (kind, fallback) in lines {
            let (marker, content): (u8, &[u8]) = match kind {
                DiffLineKind::Context => {
                    let c = self.before.get(bi).copied().unwrap_or(*fallback);
                    bi += 1;
                    ai += 1;
                    (b' ', c)
                }
                DiffLineKind::Remove => {
                    let c = self.before.get(bi).copied().unwrap_or(*fallback);
                    bi += 1;
                    (b'-', c)
                }
                DiffLineKind::Add => {
                    let c = self.after.get(ai).copied().unwrap_or(*fallback);
                    ai += 1;
                    (b'+', c)
                }
            };
            self.buf.push(marker);
            self.buf.extend_from_slice(content);
            // Tokens keep their line terminator; a token without one is the last line of
            // a file that lacks a trailing newline.
            if content.last() != Some(&b'\n') {
                self.buf.push(b'\n');
                self.buf
                    .extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}
