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
//!   `--ignore-cr-at-eol`, `--ignore-blank-lines`, `-I<re>`/`--ignore-matching-lines` —
//!   the last two mark a change ignorable exactly as `xdl_mark_ignorable_lines` and
//!   `xdl_mark_ignorable_regex` do, so an isolated one drops out of the patch as well as
//!   out of the counts
//! * `-W`/`--function-context` (`XDL_EMIT_FUNCCONTEXT`) and `--inter-hunk-context=<n>`,
//!   both reproduced by the in-tree port of `xdl_emit_diff`/`xdl_get_hunk`
//! * `--indent-heuristic` (the default) / `--no-indent-heuristic`
//! * `--line-prefix=<s>`, `--output=<file>`, `-D`/`--irreversible-delete`,
//!   `-O<file>` (`diffcore_order`)
//! * the pickaxe: `-S<string>` (with `--pickaxe-regex`), `-G<regex>`, `--find-object=<id>`,
//!   `--pickaxe-all`
//! * `--relative[=<prefix>]` / `--no-relative`, `--rotate-to=<p>` / `--skip-to=<p>`
//! * `--output-indicator-new`/`-old`/`-context=<char>` — remap the `+`/`-`/` ` markers on
//!   body lines (a value longer than one byte errors with exit 129, like `diff_opt_char`)
//! * `-a`/`--text` — force a textual diff on content gitoxide flags as binary. The patch
//!   shows hunks instead of `Binary files ... differ`; `--stat`/`--numstat` still report
//!   the file as binary, matching git's diffstat which ignores the TEXT option.
//! * `--minimal`, `--histogram`, `--diff-algorithm=<myers|minimal|histogram>` — choose the
//!   internal blob-diff algorithm (an unknown name errors with exit 129, like git's
//!   `parse_algorithm`)
//! * rename/copy/break detection — `-M`/`--find-renames[=<n>]`, `-C`/`--find-copies[=<n>]`,
//!   `-C -C`/`--find-copies-harder`, `--no-renames`, `--rename-empty`/`--no-rename-empty`,
//!   `-B`/`--break-rewrites[=<n>[/<m>]]` and the `-l<n>` rename limit — through the port of
//!   `diffcore-delta.c`/`diffcore-rename.c`/`diffcore-break.c` in [`super::diffcore_rename`].
//!   Detection is off unless asked for: `diff.renames` is a porcelain default and
//!   `diff-pairs` is plumbing, so the status letters read off stdin survive untouched
//!   (`builtin/diff-pairs.c` sets `skip_resolving_statuses` for exactly that reason).
//!   That flag is specific to reading pairs from stdin; [`render_raw_stream`] clears it
//!   for an in-process caller, which queues raw pairs the way `diff_tree_oid()` does and
//!   needs `diff_resolve_rename_copy()` to assign the letters.
//!
//! ### In-process use
//!
//! [`render_raw_stream`] is this command with the pair stream supplied directly and the
//! bytes appended to a caller-owned buffer. `git diff-tree` is `diff-tree -z -r --raw`
//! piped into this renderer, so [`super::diff_tree`] routes its patch, diffstat, dirstat,
//! whitespace and rename formats here instead of growing a second implementation. The
//! `-z` usage error only applies to the stdin path; for an in-process caller the flag
//! decides nothing but how the raw and name records are terminated.
//! * `--color[=<when>]` / `--no-color` and `--ws-error-highlight=<kind>`, with the
//!   `color.diff.*` slots and `core.whitespace` rules the emit layer paints from.
//!   `--ws-error-highlight`, `--color-moved-ws` and `--word-diff-regex` all accept
//!   their value as the next argument as well as glued on with `=`, and report
//!   ``error: option `<name>' requires a value`` (exit 129) when it is missing
//! * `--textconv` / `--no-textconv` (`DIFF_OPT_ALLOW_TEXTCONV`, off by default for
//!   plumbing): each side's `diff.<driver>.textconv` program is resolved through the
//!   `userdiff_find_by_path()` port in [`super::cat_file`] and its stdout is what the
//!   patch body diffs. The substitution is patch-only, matching git: `builtin_diffstat()`
//!   fills its buffers with `fill_mmfile()` rather than `fill_textconv()`, so
//!   `--stat`/`--numstat` keep counting the raw blobs and a raw-binary path still
//!   reports `Bin <a> -> <b> bytes` while its patch shows hunks; `--check` and the
//!   pickaxe read the raw blobs for the same reason
//! * `--ext-diff` / `--no-ext-diff` (`o->flags.allow_external`, off by default for
//!   plumbing). With the flag on, a pair whose path selects a `diff.<driver>.command`
//!   — or, failing that, `GIT_EXTERNAL_DIFF` — is handed to that program instead of
//!   being diffed internally, following `run_external_diff()`'s protocol exactly:
//!   `<cmd> <path> <old-temp> <old-hex> <old-mode> <new-temp> <new-hex> <new-mode>`
//!   with `<new-path>` and the uncoloured `fill_metainfo()` block appended for a
//!   rename or copy, `GIT_DIFF_PATH_COUNTER`/`GIT_DIFF_PATH_TOTAL` in the child's
//!   environment, and `use_shell` argument handling. Each existing side is inflated
//!   into its own `git-blob-XXXXXX` directory under its basename and passed through
//!   the checkout filters, as `prep_temp_blob()` does; the worktree file is never
//!   borrowed, because `builtin/diff-pairs.c` never reads the index and
//!   `reuse_worktree_file()` therefore always declines. The driver's stdout becomes
//!   the patch for that pair verbatim — never re-coloured, never given
//!   `--line-prefix`. `GIT_EXTERNAL_DIFF_TRUST_EXIT_CODE` and
//!   `diff.<driver>.trustExitCode` select which exit statuses mean "no change";
//!   without them any non-zero status is `external diff died, stopping at <path>`
//!   (exit 128). `diff.external` is deliberately *not* read: it is parsed by
//!   `git_diff_ui_config()` and `builtin/diff-pairs.c` registers
//!   `git_diff_basic_config()`, so stock git ignores it here too.
//! * `--ext-diff --exit-code` turns on `diff_from_contents`, so the status reports
//!   what the rendering pass found rather than what was queued — a trusted driver
//!   that answers "equal" for every pair brings the exit code back to 0. That also
//!   brings in `diff_flush_patch_quietly()`: the raw/name loop drops a pair the probe
//!   calls unchanged, and `-s`/`--quiet` render each pair with the output nulled and
//!   stop at the first change
//! * `--submodule[=<format>]`: `short` (the `Subproject commit <oid>` line diff a
//!   gitlink pair renders as by default) and `log` — the bare `--submodule` — which
//!   opens the submodule's own repository and prints
//!   `Submodule <path> <a><..|...><b>[ (rewind)]:` followed by the
//!   `--left-right --first-parent` commit list, `  < <subject>` for the left side and
//!   `  > <subject>` for the right. The `..`/`...` choice, the `(rewind)` suffix and
//!   the `(new submodule)` / `(submodule deleted)` / `(commits not present)` /
//!   `(corrupt repository)` messages all follow `show_submodule_header()`, including
//!   its quirk that a pair whose two commits are *both* unreadable still prints `..`
//!   (`merge_bases_many()`'s `one == twos[i]` short-circuit compares two NULL
//!   pointers). An unchanged gitlink prints nothing at all.
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
//! * `--binary`: the `GIT binary patch` payload is a base85-armoured *deflate*
//!   stream, and parity means byte-identical output. This bails today, but the
//!   blocker named here for a long time — "no deflate in this tree matches the zlib
//!   git links against" — is **no longer true**, and the measurement that
//!   established it was of the wrong coder. `gix-zlib` (zlib-rs) does not match:
//!   writing one 1184922-byte blob with `hash-object --no-filters -w` gives 128323
//!   bytes here against stock's 93551, which is C zlib's level-1 output exactly.
//!   But `archive.rs`'s `gzip` module is a port of zlib's own `deflate.c` +
//!   `trees.c`, and it *does* match: `git archive --format=tgz -1/-6/-9` over a
//!   ~1 MB payload is byte-identical to stock git 2.50.1 at every level. Wiring
//!   that coder up with a zlib wrapper (rather than gzip's) is what `--binary`
//!   needs; the base85 armour and the `literal <size>` framing are the rest.
//! * `--anchored=<text>`: git's own documentation states it "uses the "patience diff"
//!   algorithm internally", and the vendored `gix-imara-diff` ships only `myers.rs` and
//!   `histogram.rs`. Same floor as `--patience` below.
//! * `--patience` / `--diff-algorithm=patience`: imara-diff has no patience variant, so
//!   these bail rather than silently substituting Myers (the same floor `git diff` hits).
//! * `--submodule=diff`: `show_submodule_inline_diff()` starts a whole second
//!   `git diff --submodule=diff --color=<when> --src-prefix=… --dst-prefix=… <a> <b>`
//!   inside the submodule and pipes its stdout through, which this port does not do.
//!   `--submodule=short` and `--submodule=log` are both covered above.
//! * `--find-copies-harder` is parsed and fed to `diffcore_rename`, but a `diff-pairs`
//!   batch only ever contains the pairs stdin listed: git supplies the unmodified pairs
//!   the option needs from a tree walk, and there is none here, so it behaves as plain
//!   `-C` on any input that does not itself list unmodified pairs.
//! * `--ita-invisible-in-index` / `--ita-visible-in-index` are accepted and inert: they
//!   only steer a diff computed against the index, which this command never reads.
//! * `--follow` is fatal (`--follow requires exactly one pathspec`), matching git — the
//!   command takes no pathspec, so the option can never be satisfied.
//! * `gitattributes` diff drivers: a driver's `textconv` is honoured under `--textconv`
//!   and its `command` under `--ext-diff`, but its custom `funcname` pattern is not, so
//!   hunk headers use git's built-in `def_ff` heuristic only.
//! * `--stat`/`--summary` file names are not C-quoted, so a path containing a byte that
//!   git would escape is emitted verbatim.

use anyhow::{bail, Result};
use std::cmp::Ordering;
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::{InternedInput, ResourceKind};
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;
use gix::prelude::ObjectIdExt;
use regex::bytes::Regex;

use super::diff_color;
use super::diff_files;
use super::diffcore_rename;

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
    /// `DIFF_FORMAT_DIRSTAT` — `--dirstat`/`-X`/`--cumulative`/`--dirstat-by-file`.
    dirstat: bool,
    /// `DIFF_FORMAT_CHECKDIFF` — `--check`.
    check: bool,
    no_output: bool,
    /// `opt->line_termination == 0`: the raw/name records are NUL-separated and
    /// NUL-terminated instead of TAB/LF. `git diff-pairs` refuses to run without `-z`,
    /// so this is always set on that path; an in-process caller may want the ordinary
    /// line-terminated form.
    nul: bool,
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
            || self.dirstat
            || self.check
            || self.no_output
    }

    /// Whether one of the per-pair "name" formats runs before the stat block.
    fn name_group(&self) -> bool {
        self.raw || self.name_only || self.name_status
    }

    /// The clear half of the `OPT_BITOP(..., DIFF_FORMAT_NO_OUTPUT)` entries: every
    /// format bit git ORs in also drops `-s`. The three `OPT_BIT_F` options
    /// (`--check`, `--name-only`, `--name-status`) deliberately do not.
    fn or_patch(&mut self) {
        self.patch = true;
        self.no_output = false;
    }
    fn or_raw(&mut self) {
        self.raw = true;
        self.no_output = false;
    }
    fn or_numstat(&mut self) {
        self.numstat = true;
        self.no_output = false;
    }
    fn or_shortstat(&mut self) {
        self.shortstat = true;
        self.no_output = false;
    }
    fn or_summary(&mut self) {
        self.summary = true;
        self.no_output = false;
    }
    /// `diff_opt_stat()` / `diff_opt_compact_summary()` both clear `-s` as well.
    fn or_stat(&mut self) {
        self.stat = true;
        self.no_output = false;
    }
    /// `parse_dirstat_opt()` clears `-s` and sets `DIFF_FORMAT_DIRSTAT`.
    fn or_dirstat(&mut self) {
        self.dirstat = true;
        self.no_output = false;
    }

    /// `OPT_SET_INT('s', "no-patch", &options->output_format, DIFF_FORMAT_NO_OUTPUT)`:
    /// `-s` *assigns* the format word, so it wipes every bit set before it.
    fn set_no_output(&mut self) {
        *self = Formats {
            no_output: true,
            ..Formats::default()
        };
    }

    /// `diff_setup_done()`'s `check_mask`: the four formats that cannot be combined.
    fn check_mask_bits(&self) -> u32 {
        u32::from(self.name_only)
            + u32::from(self.name_status)
            + u32::from(self.check)
            + u32::from(self.no_output)
    }

    /// `diff_setup_done()`: `--name-only`, `--name-status`, `--check` and `-s` turn
    /// every other output format off.
    fn clear_others(&mut self) {
        self.raw = false;
        self.numstat = false;
        self.stat = false;
        self.shortstat = false;
        self.dirstat = false;
        self.summary = false;
        self.patch = false;
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
    // --output-indicator-new / -old / -context: the character printed at the start of an
    // added / removed / context body line, replacing '+' / '-' / ' '.
    ind_new: u8,
    ind_old: u8,
    ind_context: u8,
    // --diff-algorithm / --minimal / --histogram: overrides the algorithm gitoxide would
    // otherwise pick for the internal blob diff. `None` keeps gitoxide's own default.
    algo: Option<gix::diff::blob::Algorithm>,
    // -a / --text: force a textual diff even for content flagged binary.
    text: bool,
    /// `--line-prefix=<s>`: written at the start of every record git emits.
    line_prefix: Vec<u8>,
    /// `-D`/`--irreversible-delete`: a deletion prints its header and stops, omitting the
    /// pre-image body so the patch cannot be reversed.
    irreversible_delete: bool,
    /// `--indent-heuristic` (git's default) shifts hunk boundaries to indentation-friendly
    /// positions; `--no-indent-heuristic` runs the plain post-processing pass instead.
    indent_heuristic: bool,
    /// `--ignore-blank-lines`: `XDF_IGNORE_BLANK_LINES`, marking an all-blank change
    /// ignorable so an isolated one is dropped from the hunk stream entirely.
    ignore_blank_lines: bool,
    /// `-W`/`--function-context`: `XDL_EMIT_FUNCCONTEXT`, growing each hunk out to the
    /// enclosing function's boundaries.
    func_context: bool,
    /// `--inter-hunk-context=<n>`: `xecfg.interhunkctxlen`, the extra gap two changes may
    /// span and still share one hunk.
    inter_hunk_ctx: usize,
    /// `-O<file>`: `diffcore_order`'s glob list, read lazily so an unreadable file is only
    /// fatal once there is a non-empty queue to sort — exactly `prepare_order`'s timing.
    orderfile: Option<String>,
    /// The `diffcore_std()` rename/copy/break knobs. `diff-pairs` is plumbing, so
    /// detection is off unless `-M`/`-C`/`-B` asks for it (`diff.renames` is a
    /// porcelain-only default and is never read here).
    rename: diffcore_rename::Options,
    /// `--color[=<when>]` / `--no-color`; `None` defers to `color.diff` /
    /// `diff.color` / `color.ui` and the terminal test.
    color_when: Option<diff_color::ColorWhen>,
    /// `--ws-error-highlight=<kind>`, seeded from `diff.wsErrorHighlight`.
    ws_error_highlight: u32,
    /// `--dirstat[=<p>]`/`-X[<p>]`/`--cumulative`/`--dirstat-by-file[=<p>]`. Only
    /// consulted when `formats.dirstat` is on. `diff.dirstat` deliberately does not
    /// seed it: `builtin/diff-pairs.c` calls `repo_init_revisions()` (which copies
    /// `default_diff_options`) *before* `repo_config()`, so the config never reaches
    /// this command's options — it is read only to report its own parse errors.
    dirstat: diff_files::DirStat,
    /// `--ignore-submodules[=<when>]`: `all` (and the bare form) drops gitlink pairs
    /// outright. `none`/`untracked`/`dirty` only concern a worktree comparison, which
    /// `diff-pairs` never performs, so they leave the pair in place — as in git.
    ignore_submodules: bool,
    /// `o->flags.override_submodule_config`, raised by *any* form of
    /// `--ignore-submodules`. It stops `is_submodule_ignored()` from consulting
    /// `.gitmodules`, which is the only thing that ever reads the index here.
    ignore_submodules_set: bool,
    /// `--submodule[=<format>]` (`parse_submodule_params()`): how a `160000` pair is
    /// rendered in the patch.
    submodule_format: SubmoduleFormat,
    /// `--ext-diff` / `--no-ext-diff` (`o->flags.allow_external`). Off by default:
    /// `diff_setup()` only turns external drivers on for the porcelain commands.
    allow_external: bool,
    /// The inverse of `builtin/diff-pairs.c`s `skip_resolving_statuses`. That flag is
    /// specific to reading pairs off stdin, where the status letters are given; every
    /// other caller of `diffcore_std()` — `diff-tree` routed through here included —
    /// runs `diff_resolve_rename_copy()` and expects it to assign them.
    resolve_statuses: bool,
}

/// git's `enum diff_submodule_format`, restricted to the two this port renders.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SubmoduleFormat {
    /// `DIFF_SUBMODULE_SHORT` — the `Subproject commit <oid>` line diff, which is
    /// what a gitlink pair already renders as.
    Short,
    /// `DIFF_SUBMODULE_LOG` — the `Submodule <path> <a>..<b>:` header and the
    /// `--left-right --first-parent` commit list beneath it. This is what the bare
    /// `--submodule` selects. `DIFF_SUBMODULE_INLINE_DIFF` is refused at parse time.
    Log,
}

/// `--textconv`'s resolved driver machinery, shared by every pair in a batch, or
/// `None` when the flag was never given — plumbing has `DIFF_OPT_ALLOW_TEXTCONV`
/// off by default. The `RefCell` is what lets the `&`-borrowed diff pipeline drive
/// the attribute stack's mutable cursor, which `userdiff_find_by_path()` needs to
/// answer one path at a time.
type TextconvRef<'a, 'repo> = Option<&'a std::cell::RefCell<super::cat_file::Textconv<'repo>>>;

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
    /// git's `check_pair_status()` case `0`: `diffcore_break()` produced this pair and
    /// no `diff_resolve_rename_copy()` ran to give it a status letter, which is fatal
    /// in every format that consults the status (all of them except `--summary`).
    unresolved: bool,
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
    /// The buffers `builtin_diff()` actually fed to xdiff when `--textconv` replaced a
    /// side: `fill_textconv()`'s output. `None` whenever the raw blobs were diffed.
    /// Only the hunk stream and `check_blank_at_eof()` read these — the pickaxe,
    /// `--check`, `--dirstat` and the `Bin <a> -> <b>` stat sizes all read the raw
    /// blobs, since none of those paths calls `fill_textconv()` in git either.
    converted: Option<(Vec<u8>, Vec<u8>)>,
    hunks: Vec<u8>,
}

/// `git diff-pairs` — see the module documentation for the covered surface.
pub fn diff_pairs(args: &[String]) -> Result<ExitCode> {
    render_raw_stream(args, None, None)
}

/// The body of [`diff_pairs`], with the raw-pair stream supplied instead of read from
/// stdin and the rendered bytes appended to `sink` instead of written to stdout.
/// `git diff-tree` is `diff-tree -z -r --raw` piped into this exact renderer, so
/// [`super::diff_tree`] hands its own walk output here rather than growing a second
/// patch/stat/rename implementation.
pub(crate) fn render_raw_stream(
    args: &[String],
    pairs: Option<Vec<u8>>,
    sink: Option<&mut Vec<u8>>,
) -> Result<ExitCode> {
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
        ind_new: b'+',
        ind_old: b'-',
        ind_context: b' ',
        algo: None,
        text: false,
        line_prefix: Vec::new(),
        irreversible_delete: false,
        indent_heuristic: true,
        ignore_blank_lines: false,
        func_context: false,
        inter_hunk_ctx: 0,
        orderfile: None,
        rename: diffcore_rename::Options::default(),
        color_when: None,
        // git's `ws_error_highlight_default`; `diff.wsErrorHighlight` replaces it
        // once the repository is open, unless a flag already set it.
        ws_error_highlight: diff_color::WSEH_NEW,
        dirstat: diff_files::DirStat::default(),
        ignore_submodules: false,
        ignore_submodules_set: false,
        submodule_format: SubmoduleFormat::Short,
        allow_external: false,
        resolve_statuses: false,
    };
    // Whether a `--ws-error-highlight` flag was seen, so the config default does not
    // overwrite it (git reads the config first and the command line last).
    let mut wseh_explicit = false;
    // `--quiet` sets `flags.quick`, which `diff_setup_done()` — not the option parser —
    // turns into `output_format = DIFF_FORMAT_NO_OUTPUT` plus `exit_with_status`.
    let mut quick = false;
    // `--color-moved*` / `--word-diff*` / `--color-words`, layered over
    // `diff.colorMoved` / `diff.colorMovedWS` / `diff.wordRegex` once the repository
    // is open.
    let mut move_word = diff_color::MoveWordOpts::default();
    let mut nul = false;
    // `--output=<file>`: git redirects the whole diff stream into this file.
    let mut output_file: Option<String> = None;
    // `--follow` is fatal for this command (see the `--follow` arm), but only after the
    // whole command line has been parsed, exactly like git's `diff_setup_done`.
    let mut follow = false;
    // Deferred until the whole line is read so `--pickaxe-regex`/`--pickaxe-all`, which
    // may follow the `-S`/`-G`, can fold in. `b'S'` counts occurrences, `b'G'` greps.
    // `--textconv` / `--no-textconv`: `o->flags.allow_textconv`. The driver machinery
    // itself is only built once the repository is open, since it needs the index.
    let mut allow_textconv = false;
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
                        None => {
                            // parse-options' `error: option `x' requires a value` for a
                            // long name, `switch `x'` for a short one; exit 129 either way.
                            eprintln!("error: {}", diff_color::missing_value(s));
                            return Ok(ExitCode::from(129));
                        }
                    }
                }
            }};
        }
        // `--color-moved-ws <modes>` and `--word-diff-regex <re>` also spell their
        // value as the next argument; parse-options consumes it and then hands the
        // pair to the same callback the `=` form uses.
        if diff_color::needs_separate_value(s) {
            let glued = format!("{s}={}", want_value!(s.len()));
            if let Some(Err(msg)) = move_word.parse_flag(&glued, &mut opts.color_when) {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            i += 1;
            continue;
        }
        // `--color-moved[=<mode>]`, `--color-moved-ws=<modes>`, `--word-diff[=<mode>]`,
        // `--word-diff-regex=<re>` and `--color-words[=<re>]`.
        if let Some(res) = move_word.parse_flag(s, &mut opts.color_when) {
            if let Err(msg) = res {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            i += 1;
            continue;
        }
        match s {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-z" => nul = true,
            "-p" | "-u" | "--patch" => opts.formats.or_patch(),
            "-s" | "--no-patch" => opts.formats.set_no_output(),
            "--raw" => opts.formats.or_raw(),
            "--name-only" => opts.formats.name_only = true,
            "--name-status" => opts.formats.name_status = true,
            "--numstat" => opts.formats.or_numstat(),
            "--shortstat" => opts.formats.or_shortstat(),
            "--summary" => opts.formats.or_summary(),
            "--stat" => opts.formats.or_stat(),
            // `--check` (`OPT_BIT_F`, so it does not clear `-s`).
            "--check" => opts.formats.check = true,
            // The dirstat family (`diff_opt_dirstat()` → `parse_dirstat_opt()`).
            // `-X` is a short `PARSE_OPT_OPTARG`, so only the attached `-Xlines` form
            // carries a value; `--dirstat lines` never consumes the next argument.
            "--dirstat" | "-X" => {
                if let Some(c) = apply_dirstat(&mut opts, "") {
                    return Ok(c);
                }
            }
            _ if s.starts_with("--dirstat=") || (s.starts_with("-X") && s.len() > 2) => {
                let v = s.strip_prefix("--dirstat=").unwrap_or(&s[2..]).to_string();
                if let Some(c) = apply_dirstat(&mut opts, &v) {
                    return Ok(c);
                }
            }
            "--cumulative" => {
                if let Some(c) = apply_dirstat(&mut opts, "cumulative") {
                    return Ok(c);
                }
            }
            // `--dirstat-by-file` folds in `files` first and then its own parameters,
            // so `--dirstat-by-file=lines` really does end up in `lines` mode.
            "--dirstat-by-file" => {
                if let Some(c) = apply_dirstat(&mut opts, "files") {
                    return Ok(c);
                }
                if let Some(c) = apply_dirstat(&mut opts, "") {
                    return Ok(c);
                }
            }
            _ if s.starts_with("--dirstat-by-file=") => {
                if let Some(c) = apply_dirstat(&mut opts, "files") {
                    return Ok(c);
                }
                let v = s["--dirstat-by-file=".len()..].to_string();
                if let Some(c) = apply_dirstat(&mut opts, &v) {
                    return Ok(c);
                }
            }
            _ if s.starts_with("--stat=") => {
                opts.formats.or_stat();
                parse_stat_geometry(&mut opts.stat, &s["--stat=".len()..]);
            }
            "--compact-summary" => {
                opts.formats.or_stat();
                opts.stat.with_summary = true;
            }
            "--no-compact-summary" => opts.stat.with_summary = false,
            // The four `--stat-*` geometry options are `OPT_INTEGER`s, so git's
            // parse-options accepts both the `--opt=<n>` and the `--opt <n>` spelling.
            "--stat-width" | "--stat-name-width" | "--stat-graph-width" | "--stat-count" => {
                let v = want_value!(s.len());
                opts.formats.or_stat();
                let slot = match s {
                    "--stat-width" => &mut opts.stat.width,
                    "--stat-name-width" => &mut opts.stat.name_width,
                    "--stat-graph-width" => &mut opts.stat.graph_width,
                    _ => &mut opts.stat.count,
                };
                *slot = parse_i64(&v);
            }
            _ if s.starts_with("--stat-width=") => {
                opts.formats.or_stat();
                opts.stat.width = parse_i64(&s["--stat-width=".len()..]);
            }
            _ if s.starts_with("--stat-name-width=") => {
                opts.formats.or_stat();
                opts.stat.name_width = parse_i64(&s["--stat-name-width=".len()..]);
            }
            _ if s.starts_with("--stat-graph-width=") => {
                opts.formats.or_stat();
                opts.stat.graph_width = parse_i64(&s["--stat-graph-width=".len()..]);
            }
            _ if s.starts_with("--stat-count=") => {
                opts.formats.or_stat();
                opts.stat.count = parse_i64(&s["--stat-count=".len()..]);
            }
            "--patch-with-raw" => {
                opts.formats.or_patch();
                opts.formats.or_raw();
            }
            "--patch-with-stat" => {
                opts.formats.or_patch();
                opts.formats.or_stat();
            }
            "--full-index" => opts.full_index = true,
            "--no-full-index" => opts.full_index = false,
            // `--color[=<when>]` / `--no-color` (`OPT_COLOR_FLAG`).
            "--color" => opts.color_when = Some(diff_color::ColorWhen::Always),
            "--no-color" => opts.color_when = Some(diff_color::ColorWhen::Never),
            _ if s.starts_with("--color=") => {
                match diff_color::parse_color_when(&s["--color=".len()..]) {
                    Some(w) => opts.color_when = Some(w),
                    None => {
                        eprintln!(
                            "error: option `color' expects \"always\", \"auto\", or \"never\""
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `--ws-error-highlight=<kind>` / `--ws-error-highlight <kind>`
            // (`diff_opt_ws_error_highlight()`), whose error tail is the prefix of
            // the value git had already accepted.
            _ if s == "--ws-error-highlight" || s.starts_with("--ws-error-highlight=") => {
                let v = &want_value!("--ws-error-highlight=".len());
                match diff_color::parse_ws_error_highlight(v) {
                    Ok(val) => {
                        opts.ws_error_highlight = val;
                        wseh_explicit = true;
                    }
                    Err(accepted) => {
                        eprintln!(
                            "error: unknown value after ws-error-highlight={}",
                            &v[..accepted]
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
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
            // `--quiet` only raises `flags.quick`; `diff_setup_done()` is what turns
            // that into `output_format = DIFF_FORMAT_NO_OUTPUT` and `--exit-code`.
            "--quiet" => quick = true,
            // -R swaps the two prefixes and, per pair, the two sides at render time.
            "-R" => opts.reverse = true,
            // Whitespace comparison flags.
            "-w" | "--ignore-all-space" => opts.ws = Whitespace::IgnoreAll,
            "-b" | "--ignore-space-change" => opts.ws = Whitespace::IgnoreChange,
            "--ignore-space-at-eol" => opts.ws = Whitespace::IgnoreAtEol,
            "--ignore-cr-at-eol" => opts.ws = Whitespace::IgnoreCrAtEol,
            "--ignore-blank-lines" => opts.ignore_blank_lines = true,
            // `-W` only shapes the unified output; unlike `-U<n>` it does not select the
            // patch format, so `-W --stat` prints a diffstat and nothing else.
            "-W" | "--function-context" => opts.func_context = true,
            "--no-function-context" => opts.func_context = false,
            "--inter-hunk-context" => {
                opts.inter_hunk_ctx = parse_ctx(&want_value!(s.len()))? as usize;
            }
            _ if s.starts_with("--inter-hunk-context=") => {
                opts.inter_hunk_ctx = parse_ctx(&s["--inter-hunk-context=".len()..])? as usize;
            }
            // -a / --text: force a textual diff for content git would flag as binary.
            "-a" | "--text" => opts.text = true,
            "--no-text" => opts.text = false,
            // Diff-algorithm selection. imara-diff has no `patience` variant, so
            // `--patience`/`--diff-algorithm=patience` bail rather than silently
            // substituting Myers, exactly like the sibling `git diff` port.
            "--minimal" => opts.algo = Some(gix::diff::blob::Algorithm::MyersMinimal),
            "--histogram" => opts.algo = Some(gix::diff::blob::Algorithm::Histogram),
            "--patience" => crate::git_fatal!("diff algorithm {:?} is not available", "patience"),
            "--diff-algorithm" => {
                let v = want_value!(s.len());
                match classify_algo(&v) {
                    AlgoChoice::Use(a) => opts.algo = Some(a),
                    AlgoChoice::Patience => {
                        crate::git_fatal!("diff algorithm {:?} is not available", "patience")
                    }
                    AlgoChoice::Unknown => {
                        eprintln!(
                            "error: option diff-algorithm accepts \"myers\", \"minimal\", \
                             \"patience\" and \"histogram\""
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if s.starts_with("--diff-algorithm=") => {
                match classify_algo(&s["--diff-algorithm=".len()..]) {
                    AlgoChoice::Use(a) => opts.algo = Some(a),
                    AlgoChoice::Patience => {
                        crate::git_fatal!("diff algorithm {:?} is not available", "patience")
                    }
                    AlgoChoice::Unknown => {
                        eprintln!(
                            "error: option diff-algorithm accepts \"myers\", \"minimal\", \
                             \"patience\" and \"histogram\""
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            "-I" | "--ignore-matching-lines" => {
                let v = want_value!(s.len());
                let re = compile_regex(v.as_bytes())
                    .map_err(|e| anyhow::anyhow!("invalid regex given to -I: {e}"))?;
                opts.ignore_lines.push(Needle::Regex(re));
            }
            _ if s.starts_with("-I") => {
                let re = compile_regex(&s.as_bytes()[2..])
                    .map_err(|e| anyhow::anyhow!("invalid regex given to -I: {e}"))?;
                opts.ignore_lines.push(Needle::Regex(re));
            }
            _ if s.starts_with("--ignore-matching-lines=") => {
                let re = compile_regex(&s.as_bytes()["--ignore-matching-lines=".len()..])
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
            _ if s.starts_with("-S") => pickaxe_pending = Some((b'S', s.as_bytes()[2..].to_vec())),
            "-G" => pickaxe_pending = Some((b'G', want_value!(s.len()).into_bytes())),
            _ if s.starts_with("-G") => pickaxe_pending = Some((b'G', s.as_bytes()[2..].to_vec())),
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
                opts.relative = Relative::Path(BString::from(&s.as_bytes()["--relative=".len()..]));
            }
            // --rotate-to / --skip-to
            "--rotate-to" => opts.anchor = Some(Anchor::Rotate(want_value!(s.len()).into())),
            _ if s.starts_with("--rotate-to=") => {
                opts.anchor = Some(Anchor::Rotate(BString::from(
                    &s.as_bytes()["--rotate-to=".len()..],
                )));
            }
            "--skip-to" => opts.anchor = Some(Anchor::Skip(want_value!(s.len()).into())),
            _ if s.starts_with("--skip-to=") => {
                opts.anchor = Some(Anchor::Skip(BString::from(
                    &s.as_bytes()["--skip-to=".len()..],
                )));
            }
            // Accepted and ignored, matching stock git: `--abbrev=<n>` has no effect on
            // this command's `index` lines (only `core.abbrev` does).
            "--abbrev" | "--no-abbrev" => {}
            _ if s.starts_with("--abbrev=") => {}
            "-U" => {
                opts.ctx = parse_ctx(&want_value!(s.len()))?;
                opts.formats.or_patch();
            }
            _ if s.starts_with("-U") => {
                opts.ctx = parse_ctx(&s[2..])?;
                opts.formats.or_patch();
            }
            _ if s.starts_with("--unified=") => {
                opts.ctx = parse_ctx(&s["--unified=".len()..])?;
                opts.formats.or_patch();
            }
            "--unified" => opts.formats.patch = true,
            // --output-indicator-new / -old / -context: each takes a single character.
            // git's `diff_opt_char` errors (exit 129) if the value is longer than one byte.
            "--output-indicator-new" => {
                let v = want_value!(s.len());
                if let Err(c) = set_indicator(&mut opts.ind_new, &v, "output-indicator-new") {
                    return Ok(c);
                }
            }
            _ if s.starts_with("--output-indicator-new=") => {
                let v = &s["--output-indicator-new=".len()..];
                if let Err(c) = set_indicator(&mut opts.ind_new, v, "output-indicator-new") {
                    return Ok(c);
                }
            }
            "--output-indicator-old" => {
                let v = want_value!(s.len());
                if let Err(c) = set_indicator(&mut opts.ind_old, &v, "output-indicator-old") {
                    return Ok(c);
                }
            }
            _ if s.starts_with("--output-indicator-old=") => {
                let v = &s["--output-indicator-old=".len()..];
                if let Err(c) = set_indicator(&mut opts.ind_old, v, "output-indicator-old") {
                    return Ok(c);
                }
            }
            "--output-indicator-context" => {
                let v = want_value!(s.len());
                if let Err(c) = set_indicator(&mut opts.ind_context, &v, "output-indicator-context")
                {
                    return Ok(c);
                }
            }
            _ if s.starts_with("--output-indicator-context=") => {
                let v = &s["--output-indicator-context=".len()..];
                if let Err(c) = set_indicator(&mut opts.ind_context, v, "output-indicator-context") {
                    return Ok(c);
                }
            }
            // The three prefix options are `OPT_STRING`s: `--opt=<v>` and `--opt <v>` both
            // parse, so each accepts a detached value as well as an attached one.
            "--src-prefix" => opts.src_prefix = BString::from(want_value!(s.len())),
            _ if s.starts_with("--src-prefix=") => {
                opts.src_prefix = BString::from(&s.as_bytes()["--src-prefix=".len()..]);
            }
            "--dst-prefix" => opts.dst_prefix = BString::from(want_value!(s.len())),
            _ if s.starts_with("--dst-prefix=") => {
                opts.dst_prefix = BString::from(&s.as_bytes()["--dst-prefix=".len()..]);
            }
            "--line-prefix" => opts.line_prefix = want_value!(s.len()).into_bytes(),
            _ if s.starts_with("--line-prefix=") => {
                opts.line_prefix = s.as_bytes()["--line-prefix=".len()..].to_vec();
            }
            // `--output=<file>`: `set_default_output_file` reopens the diff stream on the
            // named file, so nothing reaches stdout.
            "--output" => output_file = Some(want_value!(s.len())),
            _ if s.starts_with("--output=") => output_file = Some(s["--output=".len()..].to_string()),
            // `-O<file>`: `diffcore_order` reorders the queue by the first glob in the
            // file that matches the destination path or any of its directory prefixes.
            "-O" => opts.orderfile = Some(want_value!(s.len())),
            _ if s.starts_with("-O") => opts.orderfile = Some(s[2..].to_string()),
            // `-D`: `diff_opt_irreversible_delete` — a deletion emits only its header.
            "-D" | "--irreversible-delete" => opts.irreversible_delete = true,
            // `--indent-heuristic` is git's default (`diff.indentHeuristic` defaults to
            // true); the negation runs imara-diff's plain post-processing pass instead.
            "--indent-heuristic" => opts.indent_heuristic = true,
            "--no-indent-heuristic" => opts.indent_heuristic = false,
            // `--follow` is only meaningful for a revision walk restricted to one path.
            // `diff_setup_done` dies for every other caller, and `diff-pairs` accepts no
            // pathspec at all, so the flag is always fatal here.
            "--follow" => follow = true,
            "--no-follow" => follow = false,
            // Rename/copy/break detection. `diff-pairs` runs the same `diffcore_std()`
            // passes as any other diff command over the pairs it read from stdin
            // (`builtin/diff-pairs.c` calls `diffcore_std()` per batch), so these are
            // parsed exactly as `diff.c` does and handed to `diffcore_rename`.
            "--no-renames" => opts.rename.detect_rename = 0,
            "--rename-empty" => opts.rename.rename_empty = true,
            "--no-rename-empty" => opts.rename.rename_empty = false,
            "-M" | "--find-renames" => {
                opts.rename.rename_score = 0;
                opts.rename.detect_rename = diffcore_rename::DETECT_RENAME;
            }
            _ if s.starts_with("--find-renames=") || (s.starts_with("-M") && s.len() > 2) => {
                let raw = s.strip_prefix("--find-renames=").unwrap_or(&s[2..]);
                let (score, rest) = diffcore_rename::parse_rename_score(raw);
                if !rest.is_empty() {
                    eprintln!("error: invalid argument to find-renames");
                    return Ok(ExitCode::from(129));
                }
                opts.rename.rename_score = score;
                opts.rename.detect_rename = diffcore_rename::DETECT_RENAME;
            }
            "-C" | "--find-copies" => {
                opts.rename.rename_score = 0;
                if opts.rename.detect_rename == diffcore_rename::DETECT_COPY {
                    opts.rename.find_copies_harder = true;
                } else {
                    opts.rename.detect_rename = diffcore_rename::DETECT_COPY;
                }
            }
            _ if s.starts_with("--find-copies=") || (s.starts_with("-C") && s.len() > 2) => {
                let raw = s.strip_prefix("--find-copies=").unwrap_or(&s[2..]);
                let (score, rest) = diffcore_rename::parse_rename_score(raw);
                if !rest.is_empty() {
                    eprintln!("error: invalid argument to find-copies");
                    return Ok(ExitCode::from(129));
                }
                opts.rename.rename_score = score;
                if opts.rename.detect_rename == diffcore_rename::DETECT_COPY {
                    opts.rename.find_copies_harder = true;
                } else {
                    opts.rename.detect_rename = diffcore_rename::DETECT_COPY;
                }
            }
            "--find-copies-harder" => opts.rename.find_copies_harder = true,
            "--no-find-copies-harder" => opts.rename.find_copies_harder = false,
            "-B" | "--break-rewrites" => opts.rename.break_opt = 0,
            _ if s.starts_with("--break-rewrites=") || (s.starts_with("-B") && s.len() > 2) => {
                let raw = s.strip_prefix("--break-rewrites=").unwrap_or(&s[2..]);
                match diffcore_rename::parse_break_opt(raw) {
                    Ok(v) => opts.rename.break_opt = v,
                    Err(()) => {
                        eprintln!("error: break-rewrites expects <n>/<m> form");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            "-l" => {
                let v = want_value!(s.len());
                match v.parse::<i64>() {
                    Ok(n) => opts.rename.rename_limit = n,
                    Err(_) => {
                        eprintln!("error: option `l' expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if s.starts_with("-l") => match s[2..].parse::<i64>() {
                Ok(n) => opts.rename.rename_limit = n,
                Err(_) => {
                    eprintln!("error: option `l' expects a numerical value");
                    return Ok(ExitCode::from(129));
                }
            },
            // `ita_invisible_in_index` is only consulted when a diff is computed against
            // the index (`diff-lib.c`). `diff-pairs` reads its pairs from stdin and never
            // opens the index, so both spellings are inert here, in git as well.
            "--ita-invisible-in-index" | "--ita-visible-in-index" => {}
            // `--max-depth <n>` (`diff_opt_max_depth()`): a `git_parse_int()` whose only
            // consumers are `tree-diff.c`'s recursion guards. `diff-pairs` reads its
            // pairs from stdin and never walks a tree — it dies on a `040000` entry
            // rather than descending — so the depth itself can never be consulted, in
            // stock git as well. The parse *is* live: a non-integer errors with 129.
            "--max-depth" => {
                let v = want_value!(s.len());
                if parse_git_int(&v).is_none() {
                    eprintln!("error: invalid value for '--max-depth': '{v}'");
                    return Ok(ExitCode::from(129));
                }
            }
            _ if s.starts_with("--max-depth=") => {
                let v = &s["--max-depth=".len()..];
                if parse_git_int(v).is_none() {
                    eprintln!("error: invalid value for '--max-depth': '{v}'");
                    return Ok(ExitCode::from(129));
                }
            }
            // `--ignore-submodules[=<when>]` (`handle_ignore_submodules_arg()`).
            "--ignore-submodules" => {
                opts.ignore_submodules = true;
                opts.ignore_submodules_set = true;
            }
            _ if s.starts_with("--ignore-submodules=") => {
                opts.ignore_submodules_set = true;
                match &s["--ignore-submodules=".len()..] {
                    "all" => opts.ignore_submodules = true,
                    // `none`, `untracked` and `dirty` only relax what counts as a
                    // *modification* of a checked-out submodule, which needs a worktree
                    // comparison; `diff-pairs` is handed the pair already, so all three
                    // leave it in the queue.
                    "none" | "untracked" | "dirty" => opts.ignore_submodules = false,
                    v => {
                        eprintln!("fatal: bad --ignore-submodules argument: {v}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            // `--textconv` / `--no-textconv` (`DIFF_OPT_ALLOW_TEXTCONV`). Off by
            // default here: `diff_setup()` only turns textconv on for the porcelain
            // commands, so plumbing needs the flag before a `diff.<driver>.textconv`
            // program is ever consulted.
            "--textconv" => allow_textconv = true,
            "--no-textconv" => allow_textconv = false,
            // `--ext-diff` / `--no-ext-diff` (`o->flags.allow_external`). Off by
            // default: `diff_setup()` leaves external drivers off for plumbing, so
            // `--no-ext-diff` asks for the state this command starts in.
            "--ext-diff" => opts.allow_external = true,
            "--no-ext-diff" => opts.allow_external = false,
            // `--submodule[=<format>]` (`parse_submodule_params()`): the bare form is
            // `log`, matching `diff_opt_submodule()`'s `arg ? arg : "log"`. `diff`
            // runs a second `git diff` inside the submodule and is not ported.
            "--submodule" => opts.submodule_format = SubmoduleFormat::Log,
            _ if s.starts_with("--submodule=") => {
                match &s["--submodule=".len()..] {
                    "short" => opts.submodule_format = SubmoduleFormat::Short,
                    "log" => opts.submodule_format = SubmoduleFormat::Log,
                    "diff" => bail!(
                        "unsupported flag \"--submodule=diff\" (runs a second `git diff` \
                         inside the submodule; --submodule=short and --submodule=log are ported)"
                    ),
                    v => {
                        eprintln!("error: failed to parse --submodule option parameter: '{v}'");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ => bail!(
                "unsupported flag {s:?} (ported: -z, -p/-u/--patch, -s/--no-patch, --raw, \
                 --name-only, --name-status, --numstat, --stat[=<w>], --shortstat, --summary, \
                 --compact-summary, --patch-with-raw, --patch-with-stat, -U<n>/--unified[=<n>], \
                 --full-index, --no-prefix, --default-prefix, --src-prefix=, --dst-prefix=, \
                 -R, --diff-filter=<f>, -w/-b/--ignore-space-at-eol/--ignore-cr-at-eol, \
                 -I<re>, -S<s>/-G<re>/--find-object=<id>/--pickaxe-regex/--pickaxe-all, \
                 --relative[=<p>]/--no-relative, --rotate-to=<p>/--skip-to=<p>, \
                 --line-prefix=<s>, --output=<file>, -D/--irreversible-delete, -O<file>, \
                 -W/--function-context, --inter-hunk-context=<n>, --ignore-blank-lines, \
                 --no-renames, --indent-heuristic/--no-indent-heuristic, \
                 --output-indicator-new/-old/-context=<char>, \
                 -a/--text, --minimal/--histogram/--diff-algorithm=<myers|minimal|histogram>, \
                 --textconv/--no-textconv, --ext-diff/--no-ext-diff, \
                 --ignore-submodules[=<when>], --submodule[=short|log], \
                 --exit-code, --quiet, --abbrev[=<n>], -h)"
            ),
        }
        i += 1;
    }

    // ---- diff_setup_done() ----
    // `check_mask`: `--name-only`, `--name-status`, `--check` and `-s` are mutually
    // exclusive, and any one of them turns every other output format off. `--quiet`
    // is *not* part of the mask — it only raises `flags.quick`, which is applied
    // further down, so `--quiet --check` is legal where `-s --check` is not.
    if opts.formats.check_mask_bits() > 1 {
        return Ok(fatal(
            "options '--name-only', '--name-status', '--check', and '-s' cannot be used together",
        ));
    }
    if opts.formats.check_mask_bits() == 1 {
        opts.formats.clear_others();
    }
    // `diff_setup_done` runs before the `-z` requirement is checked, so `--follow` wins
    // over the usage error just as it does in stock git.
    if follow {
        return Ok(fatal("--follow requires exactly one pathspec"));
    }
    // `flags.quick`: showing the first hit found makes no sense, so the whole output is
    // dropped and the exit status carries the answer instead.
    if quick {
        opts.formats.set_no_output();
        opts.exit_code = true;
        opts.rename.detect_rename = 0;
        opts.rename.find_copies_harder = false;
    }
    // `builtin/diff-pairs.c` refuses to parse anything but the NUL-terminated stream.
    // An in-process caller has already produced that stream, so the flag it would have
    // had to pass only decides how the raw/name records come back out.
    if !nul && pairs.is_none() {
        eprintln!("usage: working without -z is not supported");
        return Ok(ExitCode::from(129));
    }
    opts.formats.nul = nul;
    // `skip_resolving_statuses` exists because `diff-pairs` is handed status letters on
    // stdin; an in-process caller queues raw pairs the way `diff_tree_oid()` does and
    // needs `diff_resolve_rename_copy()` to assign them.
    opts.resolve_statuses = pairs.is_some();
    if !opts.formats.requested() {
        opts.formats.or_patch();
    }

    let repo = match gix::discover(".") {
        Ok(r) => r,
        Err(_) => {
            eprintln!("fatal: not a git repository (or any of the parent directories): .git");
            return Ok(ExitCode::from(128));
        }
    };
    if !wseh_explicit {
        if let Ok(v) = diff_color::ws_error_highlight_default(&repo) {
            opts.ws_error_highlight = v;
        }
    }
    // The gitattributes stack and filter pipeline `userdiff_find_by_path()` and
    // `prep_temp_blob()` need, built once for the whole stream as git builds its
    // attribute check once and reuses it for every filespec. `--textconv` and
    // `--ext-diff` both drive it, so one instance serves both.
    let drivers = if allow_textconv || opts.allow_external {
        match super::cat_file::Textconv::new(&repo) {
            Ok(t) => Some(std::cell::RefCell::new(t)),
            Err(e) => return Ok(fatal(&e.to_string())),
        }
    } else {
        None
    };
    let textconv = if allow_textconv { drivers.as_ref() } else { None };
    // `git_diff_basic_config()`'s `diff.dirstat` arm parses the value and warns about
    // whatever it could not understand. It cannot change *this* command's behaviour:
    // `builtin/diff-pairs.c` runs `repo_init_revisions()` — which copies
    // `default_diff_options` — before `repo_config()`, so the parsed parameters land in
    // a struct the command has already finished copying from. The warning is the whole
    // of the observable effect, and it is emitted whether or not `--dirstat` was given.
    if let Some(v) = repo.config_snapshot().string("diff.dirstat") {
        let mut ignored = diff_files::DirStat::default();
        let errors = diff_files::parse_dirstat_params(&v.to_string(), &mut ignored);
        if !errors.is_empty() {
            eprint!("warning: Found errors in 'diff.dirstat' config variable:\n{errors}\n");
        }
    }
    // `--color[=<when>]` / `--no-color`, falling back to `color.diff` / `diff.color`
    // / `color.ui` and the terminal test.
    let colors =
        diff_color::DiffColors::resolve(&repo, diff_color::resolve_color(&repo, opts.color_when));
    let ws_rule = diff_color::whitespace_rule_cfg(&repo);
    let mut extra = match move_word.resolve(&repo) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(ExitCode::from(128));
        }
    };
    // `external_diff()` (diff.c:558) and `diff_setup_done()`'s two consequences of it:
    // a `GIT_EXTERNAL_DIFF` that is allowed to run turns `--color-moved` off, since
    // move detection needs the symbol stream the driver's output never enters.
    let ext_env = match external_diff_env() {
        Ok(e) => e,
        Err(code) => return Ok(code),
    };
    let ext = match (opts.allow_external, drivers.as_ref()) {
        (true, Some(d)) => {
            if ext_env.is_some() {
                extra.color_moved = None;
            }
            Some(ExtCtx {
                drivers: d,
                env: ext_env,
                counter: std::cell::Cell::new(0),
                index_read: std::cell::Cell::new(false),
                index: std::cell::OnceCell::new(),
            })
        }
        _ => None,
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

    let input = match pairs {
        Some(v) => v,
        None => {
            let mut v = Vec::new();
            std::io::stdin().read_to_end(&mut v)?;
            v
        }
    };

    let hexsz = repo.object_hash().len_in_hex();
    let base_abbrev = base_abbrev(&repo);
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;

    let stdout = std::io::stdout();
    // `--output=<file>` swaps the diff stream for a freshly truncated file; git's
    // `xfopen` reports the failure verbatim and exits 128. An in-process caller
    // (`diff-tree`) passes its own buffer instead, so its commit-id lines and this
    // renderer's output interleave in the right order.
    let mut out: Box<dyn Write + '_> = match sink {
        Some(buf) => Box::new(buf),
        None => match &output_file {
            Some(path) => match std::fs::File::create(path) {
                Ok(f) => Box::new(std::io::BufWriter::new(f)),
                Err(e) => {
                    return Ok(fatal(&format!(
                        "could not open '{path}' for writing: {}",
                        io_reason(&e)
                    )))
                }
            },
            None => Box::new(stdout.lock()),
        },
    };
    let mut any_pair = false;
    let mut batch: Vec<Pair> = Vec::new();
    // `diff_result_code()` prints the rename-limit warnings once, after the whole
    // stream has been written.
    let mut warnings = diffcore_rename::Warnings::default();
    // `o->flags.check_failed`: sticky across batches, read once by `diff_result_code()`.
    let mut check_failed = false;
    // `o->found_changes`: only consulted when `diff_from_contents` is on.
    let mut found_changes = false;
    let mut cursor = 0usize;

    // Records are NUL-terminated fields; a zero-length header field closes a batch.
    while cursor < input.len() {
        let Some(end) = input[cursor..].iter().position(|&b| b == 0) else {
            return Ok(fatal("invalid raw diff input"));
        };
        let header = &input[cursor..cursor + end];
        cursor += end + 1;

        if header.is_empty() {
            match flush(&mut out, &repo, &mut cache, &batch, &opts, base_abbrev, &colors, &extra, ws_rule, &mut warnings, &mut check_failed, textconv, ext.as_ref(), &mut found_changes)? {
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
        if let Some(ctx) = ext.as_ref() {
            if queues_read_index(&pair, opts.ignore_submodules_set) {
                ctx.index_read.set(true);
            }
        }
        batch.push(pair);
    }

    match flush(&mut out, &repo, &mut cache, &batch, &opts, base_abbrev, &colors, &extra, ws_rule, &mut warnings, &mut check_failed, textconv, ext.as_ref(), &mut found_changes)? {
        Ok(()) => {}
        Err(code) => return Ok(code),
    }
    out.flush()?;
    warnings.emit("diff.renameLimit");

    // `diff_result_code()`: bit 0 is `--exit-code` with changes, bit 1 is `--check`
    // having reported something. They are independent, so `--check --exit-code` on a
    // dirty tree exits 3.
    let mut code = 0u8;
    // `diff_flush()`'s tail: with `diff_from_contents` on, `has_changes` is replaced
    // by what the rendering pass found, so an external driver that reports "equal"
    // can bring the status back to 0. Without `--ext-diff` the queue-time
    // `has_changes` stands, exactly as `diff_queue_change()` set it.
    let changed = if opts.allow_external && opts.exit_code {
        found_changes
    } else {
        any_pair
    };
    if opts.exit_code && changed {
        code |= 0o1;
    }
    if opts.formats.check && check_failed {
        code |= 0o2;
    }
    Ok(ExitCode::from(code))
}

/// `parse_dirstat_opt()` (diff.c:5454): fold one parameter list into the accumulated
/// `--dirstat` state and turn the format on, or report git's `die()` and its exit code.
fn apply_dirstat(opts: &mut Opts, params: &str) -> Option<ExitCode> {
    let errors = diff_files::parse_dirstat_params(params, &mut opts.dirstat);
    if !errors.is_empty() {
        eprint!("fatal: Failed to parse --dirstat/-X option parameter:\n{errors}\n");
        return Some(ExitCode::from(128));
    }
    opts.formats.or_dirstat();
    None
}

/// `git_parse_int()` → `git_parse_signed()` with an `int` ceiling (config.c): optional
/// leading blanks, then `strtoimax` in base 0 — so `0x10` is hex and `010` is octal —
/// then an optional `k`/`m`/`g` unit suffix, then the end of the string. The value must
/// still fit an `int` after the suffix multiplies it.
fn parse_git_int(value: &str) -> Option<i32> {
    let b = value.as_bytes();
    let mut i = 0usize;
    // `strtoimax` skips leading whitespace, so `" 3"` parses but `"3 "` does not.
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = match b.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let (radix, start) = if b[i..].starts_with(b"0x") || b[i..].starts_with(b"0X") {
        (16u32, i + 2)
    } else if b.get(i) == Some(&b'0') {
        (8u32, i)
    } else {
        (10u32, i)
    };
    i = start;
    let digits_at = i;
    let mut magnitude: i64 = 0;
    while let Some(d) = b.get(i).and_then(|c| (*c as char).to_digit(radix)) {
        magnitude = magnitude.checked_mul(i64::from(radix))?.checked_add(i64::from(d))?;
        if magnitude > i64::from(u32::MAX) {
            return None;
        }
        i += 1;
    }
    if i == digits_at {
        return None;
    }
    let factor: i64 = match b.get(i) {
        Some(b'k') | Some(b'K') => {
            i += 1;
            1024
        }
        Some(b'm') | Some(b'M') => {
            i += 1;
            1024 * 1024
        }
        Some(b'g') | Some(b'G') => {
            i += 1;
            1024 * 1024 * 1024
        }
        _ => 1,
    };
    if i != b.len() {
        return None;
    }
    let val = magnitude.checked_mul(factor)?;
    let val = if negative { -val } else { val };
    i32::try_from(val).ok()
}

/// Report a git-style fatal error and yield its exit code.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// The bare `strerror` text of an I/O failure: Rust appends ` (os error <n>)` to the
/// system message, which git's `%s` of `strerror(errno)` never prints.
fn io_reason(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// `diff_line_prefix()`: `--line-prefix=<s>` is written once at the start of every record
/// git emits. `body` holds records terminated by newlines (the stat block, the summary and
/// the patch), so the prefix goes at the start and after every terminator but the last.
fn append_prefixed(out: &mut Vec<u8>, lp: &[u8], body: &[u8]) {
    if lp.is_empty() {
        out.extend_from_slice(body);
        return;
    }
    for line in byte_lines(body) {
        out.extend_from_slice(lp);
        out.extend_from_slice(line);
    }
}

fn parse_ctx(s: &str) -> Result<u32> {
    s.parse::<u32>()
        .map_err(|_| anyhow::anyhow!("invalid context line count {s:?}"))
}

/// Parse a bare integer for the `--stat-*` width options; git treats a bad value as unset.
fn parse_i64(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(-1)
}

/// `diff_opt_char`: the value for a `--output-indicator-*` option must be a single byte.
/// git errors with `error: <name> expects a character, got '<val>'` (exit 129) for any
/// value longer than one byte; an empty value leaves the marker as a NUL byte, matching
/// git's read past the empty string.
fn set_indicator(slot: &mut u8, val: &str, name: &str) -> std::result::Result<(), ExitCode> {
    if val.len() <= 1 {
        *slot = val.as_bytes().first().copied().unwrap_or(0);
        Ok(())
    } else {
        eprintln!("error: {name} expects a character, got '{val}'");
        Err(ExitCode::from(129))
    }
}

/// The outcome of resolving a `--diff-algorithm=<name>` value.
enum AlgoChoice {
    Use(gix::diff::blob::Algorithm),
    /// git-valid, but imara-diff has no patience variant.
    Patience,
    Unknown,
}

/// Map a `--diff-algorithm` value to an imara-diff algorithm, matching git's
/// case-insensitive `parse_algorithm` (which accepts `myers`/`default`, `minimal`,
/// `histogram` and `patience`).
fn classify_algo(name: &str) -> AlgoChoice {
    use gix::diff::blob::Algorithm::{Histogram, Myers, MyersMinimal};
    match name.to_ascii_lowercase().as_str() {
        "myers" | "default" => AlgoChoice::Use(Myers),
        "minimal" => AlgoChoice::Use(MyersMinimal),
        "histogram" => AlgoChoice::Use(Histogram),
        "patience" => AlgoChoice::Patience,
        _ => AlgoChoice::Unknown,
    }
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
            unresolved: false,
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

/// Reads a filespec's blob for [`diffcore_rename`]. Every side of a `diff-pairs`
/// record names an object in the database, so `diff_populate_filespec()` here is just
/// an odb lookup.
struct OdbContent<'a> {
    repo: &'a gix::Repository,
}

impl diffcore_rename::Content for OdbContent<'_> {
    fn size(&mut self, spec: &diffcore_rename::FileSpec) -> Option<u64> {
        // `check_size_only = 1`: the odb header answers without inflating the blob.
        let header = self.repo.find_header(spec.oid).ok()?;
        (header.kind() == gix::object::Kind::Blob).then(|| header.size())
    }

    fn data(&mut self, spec: &diffcore_rename::FileSpec) -> Option<Vec<u8>> {
        self.repo.find_object(spec.oid).ok().map(|o| o.detach().data)
    }
}

/// The `diffcore_std()` break / rename / merge-broken slice over one batch, rewriting
/// `pairs` in place with the detected renames, copies and rewrites plus the status
/// tokens `diff_resolve_rename_copy()` assigns.
fn run_rename_detection(
    repo: &gix::Repository,
    pairs: &mut Vec<Pair>,
    opts: &diffcore_rename::Options,
    resolve_statuses: bool,
) -> std::result::Result<diffcore_rename::Warnings, ExitCode> {
    let mut q = diffcore_rename::Queue::default();
    for p in pairs.iter() {
        // A mode of zero is git's `!DIFF_FILE_VALID`; the record's own (all-zero)
        // object id is kept so it round-trips into the rebuilt pair unchanged.
        let one = q.add_spec(diffcore_rename::FileSpec::new(
            p.old_path.clone(),
            p.old_mode,
            p.old_id,
            p.old_valid(),
        ));
        let two = q.add_spec(diffcore_rename::FileSpec::new(
            p.new_path.clone(),
            p.new_mode,
            p.new_id,
            p.new_valid(),
        ));
        // The record's own status and score come along: without `-M` git never calls
        // `diff_resolve_rename_copy()` (`skip_resolving_statuses`), so these are what
        // the output has to reproduce.
        let idx = q.add_pair(one, two);
        q.pairs[idx].status = p.kind();
        q.pairs[idx].score = p.score() * (diffcore_rename::MAX_SCORE as u32) / 100;
    }

    let mut opts = *opts;
    opts.hash_kind = repo.object_hash();
    let mut content = OdbContent { repo };
    let warnings = diffcore_rename::run(&mut q, &opts, &mut content);
    // `builtin/diff-pairs.c` sets `skip_resolving_statuses` when no rename detection
    // was asked for, leaving the statuses read off stdin alone.
    if resolve_statuses || opts.detect_rename != 0 {
        diffcore_rename::resolve_rename_copy(&mut q);
    }

    pairs.clear();
    for p in &q.pairs {
        let one = &q.specs[p.one];
        let two = &q.specs[p.two];
        // A pair that reached the flush with no status at all — which is what `-B`
        // without `-M` leaves behind for every pair it broke — is `check_pair_status()`'s
        // fatal case, raised by whichever render block reaches it first.
        let unresolved = p.status == 0;
        let mut status = BString::from(vec![if unresolved { b'M' } else { p.status }]);
        if p.score != 0 {
            status.extend_from_slice(
                format!("{:03}", diffcore_rename::similarity_index(p.score)).as_bytes(),
            );
        }
        pairs.push(Pair {
            old_mode: one.mode,
            new_mode: two.mode,
            old_id: one.oid,
            new_id: two.oid,
            status,
            old_path: one.path.clone(),
            new_path: two.path.clone(),
            unresolved,
        });
    }
    Ok(warnings)
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
    colors: &diff_color::DiffColors,
    extra: &diff_color::ExtraPaint,
    ws_rule: u32,
    warnings: &mut diffcore_rename::Warnings,
    // `o->flags.check_failed`, accumulated across every batch — `diff_result_code()`
    // reads it once, after the whole stream has been written.
    check_failed: &mut bool,
    tc: TextconvRef<'_, '_>,
    ext: Option<&ExtCtx<'_, '_>>,
    // `o->found_changes`, accumulated across every batch. Only consulted when
    // `diff_from_contents` is on, which for this command means `--ext-diff
    // --exit-code`.
    found_changes: &mut bool,
) -> Result<std::result::Result<(), ExitCode>> {
    // `diff_from_contents` (diff_setup_done, diff.c:5359): with an external diff
    // allowed, the driver may declare non-identical contents equal, so the exit
    // status has to come from the rendering pass instead of from the queue.
    let from_contents = opts.allow_external && opts.exit_code;
    if batch.is_empty() {
        return Ok(Ok(()));
    }
    // `diff_flush()` still walks the queue with no output format when it has to
    // answer `--exit-code` from the contents.
    if opts.formats.no_output && !from_contents {
        return Ok(Ok(()));
    }

    let mut pairs: Vec<Pair> = batch.to_vec();

    // `diff_queue_change()` refuses to queue a pair whose *both* sides are gitlinks
    // once the submodule is ignored, so `--ignore-submodules[=all]` drops it before
    // `diffcore_std()` ever sees it. A gitlink that was only added or only deleted
    // keeps one non-gitlink (zero) mode and survives.
    if opts.ignore_submodules {
        pairs.retain(|p| !(is_gitlink_mode(p.old_mode) && is_gitlink_mode(p.new_mode)));
    }

    // ---- diffcore_std order: break -> rename -> merge-broken -> pickaxe ----
    // `builtin/diff-pairs.c` calls `diffcore_std()` per batch, and only sets
    // `skip_resolving_statuses` when no detection was asked for — so the status
    // letters read off stdin survive untouched unless `-M`/`-C`/`-B` is present.
    if opts.rename.detect_rename != 0 || opts.rename.break_opt != -1 {
        match run_rename_detection(repo, &mut pairs, &opts.rename, opts.resolve_statuses) {
            // `needed_rename_limit` is reset by every `too_many_rename_candidates()`
            // call, so the last batch's value is the one `diff_result_code()` reports;
            // `degraded_cc_to_c` is only ever set, never cleared.
            Ok(w) => {
                warnings.needed_rename_limit = w.needed_rename_limit;
                warnings.degraded_cc_to_c |= w.degraded_cc_to_c;
            }
            Err(code) => return Ok(Err(code)),
        }
    }

    // ---- diffcore_std order: pickaxe -> rotate -> apply_filter ----
    if let Some(px) = &opts.pickaxe {
        let mut keep = Vec::with_capacity(pairs.len());
        for p in &pairs {
            match pickaxe_hit(repo, cache, px, p, opts, tc) {
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

    // `diffcore_order` sits between the pickaxe and the rotation in `diffcore_std`.
    if let Some(path) = &opts.orderfile {
        let patterns = match std::fs::read(path) {
            Ok(data) => parse_orderfile(&data),
            Err(e) => {
                return Ok(Err(fatal(&format!(
                    "failed to read orderfile '{path}': {}",
                    io_reason(&e)
                ))))
            }
        };
        pairs.sort_by_key(|p| match_order(&patterns, &p.new_path));
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
    let lp = opts.line_prefix.as_slice();
    // `check_pair_status()` is consulted by the raw/name loop, the diffstat loop and
    // the patch loop — in that order — but never by `--summary`. Raise its fatal at
    // whichever of those blocks runs first, after flushing what git already printed.
    let unresolved = pairs.iter().any(|p| p.unresolved);
    let unresolved_fatal = || fatal("internal error in diff-resolve-rename-copy");

    // `diff_flush()`'s raw/name/check loop consults `diff_flush_patch_quietly()` when
    // the exit status has to come from the contents, and drops every pair the probe
    // reports unchanged. The probe renders the pair a second time — with `--raw -p`
    // git runs an external driver once per loop, and so does this.
    let probed: Option<Vec<Pair>> =
        if from_contents && (opts.formats.name_group() || opts.formats.check) {
            let mut keep = Vec::with_capacity(pairs.len());
            for p in &pairs {
                match pair_found_changes(
                    repo,
                    cache,
                    p,
                    opts,
                    base_abbrev,
                    tc,
                    ext,
                    pairs.len(),
                ) {
                    Ok(hit) => {
                        *found_changes |= hit;
                        if hit {
                            keep.push(p.clone());
                        }
                    }
                    Err(code) => return Ok(Err(code)),
                }
            }
            Some(keep)
        } else {
            None
        };
    let shown: &[Pair] = probed.as_deref().unwrap_or(&pairs);

    // ---- name/raw group ----
    // These records are NUL-terminated and carry a NUL between their own fields, so the
    // line prefix is written once per record rather than once per NUL.
    if opts.formats.name_group() {
        if unresolved {
            return Ok(Err(unresolved_fatal()));
        }
        for p in shown {
            buf.extend_from_slice(lp);
            render_name(&mut buf, p, &opts.formats);
        }
    }

    // ---- --check ----
    // `DIFF_FORMAT_CHECKDIFF` shares the name/raw loop's slot in `diff_flush()`, and
    // `diff_setup_done()` guarantees it is the only format left standing.
    if opts.formats.check {
        match render_check(&mut buf, repo, cache, shown, opts, colors, ws_rule, tc) {
            Ok(failed) => *check_failed |= failed,
            Err(code) => {
                out.write_all(&buf)?;
                return Ok(Err(code));
            }
        }
    }

    // ---- content analyses (numstat/stat/shortstat share these) ----
    // `show_dirstat_by_line()` reads the diffstat, so `--dirstat=lines` pulls the whole
    // stat computation in even when no stat format was asked for.
    let dirstat_by_line = opts.formats.dirstat && opts.dirstat.by_line;
    let stats_wanted =
        opts.formats.numstat || opts.formats.stat || opts.formats.shortstat || dirstat_by_line;
    if stats_wanted && unresolved {
        out.write_all(&buf)?;
        return Ok(Err(unresolved_fatal()));
    }
    let files: Vec<StatFile> = if stats_wanted {
        let mut analyses = Vec::with_capacity(pairs.len());
        for p in &pairs {
            match analyze(repo, cache, p, opts, tc) {
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
    // `--numstat` records are NUL-terminated with an embedded NUL like the raw ones, so
    // they take the per-record prefix; `--stat`/`--shortstat` are newline records.
    if opts.formats.numstat {
        render_numstat(&mut buf, &files, lp);
    }
    if opts.formats.stat {
        let mut sub = Vec::new();
        render_stat(&mut sub, &files, &opts.stat, colors);
        append_prefixed(&mut buf, lp, &sub);
    }
    if opts.formats.shortstat {
        let mut sub = Vec::new();
        render_shortstat(&mut sub, &files);
        append_prefixed(&mut buf, lp, &sub);
    }
    // `show_dirstat_by_line()` closes the stat block; it counts *lines*, normalising a
    // binary file's byte counts at 64 bytes to the line.
    if dirstat_by_line {
        let damage: Vec<(BString, u64)> = files
            .iter()
            .map(|f| {
                let d = u64::from(f.added) + u64::from(f.deleted);
                (
                    f.new_path.clone(),
                    if f.binary { d.div_ceil(64) } else { d },
                )
            })
            .collect();
        let mut sub = Vec::new();
        diff_files::render_dirstat(&mut sub, damage, &opts.dirstat);
        append_prefixed(&mut buf, lp, &sub);
    }

    // ---- --dirstat (the byte-damage modes) ----
    // `show_dirstat()` sits outside the stat block and, unlike every other format,
    // never bumps `separator` — so `--dirstat -p` runs the two straight together.
    if opts.formats.dirstat && !dirstat_by_line {
        let damage = match dirstat_damage(repo, cache, &pairs, opts, tc) {
            Ok(d) => d,
            Err(code) => {
                out.write_all(&buf)?;
                return Ok(Err(code));
            }
        };
        let mut sub = Vec::new();
        diff_files::render_dirstat(&mut sub, damage, &opts.dirstat);
        append_prefixed(&mut buf, lp, &sub);
    }

    // ---- summary ----
    let summary_shown = opts.formats.summary && !summary_is_empty(&pairs);
    if summary_shown {
        let mut sub = Vec::new();
        for p in &pairs {
            render_summary(&mut sub, p);
        }
        append_prefixed(&mut buf, lp, &sub);
    }

    // ---- patch ----
    // git sets `separator` whenever any non-patch format is requested (even if it
    // produced no bytes), so `--stat -p` with an empty stat still emits the NUL.
    if opts.formats.patch && unresolved {
        out.write_all(&buf)?;
        return Ok(Err(unresolved_fatal()));
    }
    if opts.formats.patch {
        let separator = opts.formats.name_group()
            || opts.formats.numstat
            || opts.formats.stat
            || opts.formats.shortstat
            || dirstat_by_line
            || summary_shown;
        if separator {
            // `DIFF_SYMBOL_SEPARATOR` prints the line prefix ahead of the terminator.
            buf.extend_from_slice(lp);
            buf.push(b'\0');
        }
        // The whole patch is assembled uncolored first, then re-emitted in one pass
        // through git's `fn_out_consume()` chain — the ordering
        // `diff_flush_patch_all_file_pairs()` uses so that move detection and the
        // word diff both see every file pair.
        let paint_opts = diff_color::PaintOptions {
            ws_error_highlight: opts.ws_error_highlight,
            indicators: (opts.ind_new, opts.ind_old, opts.ind_context),
            // `diff.suppressBlankEmpty` is not read by this module, so the sign of an
            // empty context line is always kept, as git's default does.
            suppress_blank_empty: false,
        };
        let mut sink = PatchSink {
            out: Vec::new(),
            plain: Vec::new(),
            files: Vec::new(),
            colors,
            paint: paint_opts,
            extra,
            ws_rule,
            lp,
        };
        let mut failed = None;
        for p in &pairs {
            if let Err(code) = render_patch(
                &mut sink,
                repo,
                cache,
                p,
                opts,
                base_abbrev,
                ws_rule,
                tc,
                ext,
                pairs.len(),
                found_changes,
            ) {
                failed = Some(code);
                break;
            }
        }
        buf.extend_from_slice(&sink.finish());
        if let Some(code) = failed {
            out.write_all(&buf)?;
            return Ok(Err(code));
        }
    }

    // ---- the `DIFF_FORMAT_NO_OUTPUT` probe ----
    // `diff_flush()`'s last block: with nothing to print but a contents-derived exit
    // status to produce, git renders each pair quietly and stops at the first change.
    if opts.formats.no_output && from_contents {
        for p in &pairs {
            match pair_found_changes(
                repo,
                cache,
                p,
                opts,
                base_abbrev,
                tc,
                ext,
                pairs.len(),
            ) {
                Ok(hit) => *found_changes |= hit,
                Err(code) => {
                    out.write_all(&buf)?;
                    return Ok(Err(code));
                }
            }
            // The break is tested after the probe, so the first pair of a batch is
            // always rendered even when an earlier batch already found a change.
            if *found_changes {
                break;
            }
        }
    }

    out.write_all(&buf)?;
    Ok(Ok(()))
}

/// `prepare_order`: one glob per line, skipping empty lines and `#` comments.
fn parse_orderfile(data: &[u8]) -> Vec<BString> {
    data.split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] != b'#')
        .map(BString::from)
        .collect()
}

/// `match_order`: the index of the first glob matching `path` or any of its directory
/// prefixes, or `patterns.len()` for a path no glob claims (which sorts last).
fn match_order(patterns: &[BString], path: &BString) -> usize {
    for (i, pat) in patterns.iter().enumerate() {
        let mut p: &[u8] = path.as_slice();
        while !p.is_empty() {
            if gix::glob::wildmatch(
                pat.as_bstr(),
                p.as_bstr(),
                gix::glob::wildmatch::Mode::empty(),
            ) {
                return i;
            }
            match p.rfind_byte(b'/') {
                Some(at) => p = &p[..at],
                None => break,
            }
        }
    }
    patterns.len()
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
        unresolved: false,
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
        unresolved: false,
    }
}

/// `--raw` / `--name-only` / `--name-status` for one pair (`--name-status` wins when
/// several are set, matching `flush_one_pair`'s precedence).
fn render_name(out: &mut Vec<u8>, p: &Pair, f: &Formats) {
    let two_paths = matches!(p.kind(), b'R' | b'C');
    // `opt->line_termination`: NUL for both fields under `-z`, otherwise git's
    // `inter_name_termination` TAB between the status and the path and LF at the end.
    let sep = if f.nul { b'\0' } else { b'\t' };
    let term = if f.nul { b'\0' } else { b'\n' };
    let name = |bytes: &BString| -> Vec<u8> {
        if f.nul {
            bytes.to_vec()
        } else {
            diff_files::quoted_name(bytes)
        }
    };
    if f.name_status {
        out.extend_from_slice(&p.status);
        out.push(sep);
    } else if f.name_only {
        out.extend_from_slice(&name(&p.new_path));
        out.push(term);
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
        out.push(sep);
    }
    out.extend_from_slice(&name(&p.old_path));
    out.push(if two_paths { sep } else { term });
    if two_paths {
        out.extend_from_slice(&name(&p.new_path));
        out.push(term);
    }
}

// ---------------------------------------------------------------------------
// --ext-diff (diff.c `external_diff`, `prepare_temp_file`, `run_external_diff`)
// ---------------------------------------------------------------------------

/// git's `struct external_diff`: the command line to run, plus whether its exit
/// status carries the "were there changes" answer.
#[derive(Clone)]
struct ExternalDiff {
    cmd: String,
    trust_exit_code: bool,
}

/// The gitattributes stack + filter pipeline `userdiff_find_by_path()` and
/// `prep_temp_blob()` share. One instance serves both `--textconv` and `--ext-diff`.
type Drivers<'a, 'repo> = &'a std::cell::RefCell<super::cat_file::Textconv<'repo>>;

/// The `--ext-diff` state the whole stream shares. Present only when the flag was
/// given; `env` may still be `None`, in which case only a path whose `diff`
/// attribute names a driver with a `command` reaches an external program.
struct ExtCtx<'a, 'repo> {
    drivers: Drivers<'a, 'repo>,
    /// `external_diff()`. Its *presence* — not just its use on some path — is what
    /// suppresses `run_diff()`'s file/symlink split and `--color-moved`.
    env: Option<ExternalDiff>,
    /// `o->diff_path_counter`: set to zero by `diff_setup_done()` and never reset,
    /// so it counts external invocations across every batch.
    counter: std::cell::Cell<u32>,
    /// Whether `r->index` has been populated, which decides whether
    /// `reuse_worktree_file()` can borrow a worktree file at all. See
    /// [`queues_read_index`].
    index_read: std::cell::Cell<bool>,
    /// The worktree index, opened the first time it is needed.
    index: std::cell::OnceCell<Option<WorktreeIndex>>,
}

/// The worktree index plus the `core.trustCtime` / `core.checkStat` knobs
/// `ie_match_stat()` obeys.
struct WorktreeIndex {
    state: gix::worktree::IndexPersistedOrInMemory,
    stat: gix::index::entry::stat::Options,
    workdir: std::path::PathBuf,
}

impl ExtCtx<'_, '_> {
    /// `reuse_worktree_file()` (diff.c:4388): whether the file checked out at `path`
    /// is known to hold exactly `oid`, so `prepare_temp_file()` can hand the driver
    /// the worktree path instead of inflating a temporary copy.
    ///
    /// The `if (!istate->cache) return 0` guard comes first: with no index read
    /// nothing is ever reused. `want_file` is 1 for this caller, which skips both
    /// the pack and the `would_convert_to_git()` shortcuts.
    fn reuse_worktree_file(&self, repo: &gix::Repository, path: &BString, oid: &ObjectId) -> bool {
        if !self.index_read.get() {
            return false;
        }
        let idx = self
            .index
            .get_or_init(|| {
                let state = repo.index_or_load_from_head().ok()?;
                Some(WorktreeIndex {
                    state,
                    stat: repo.stat_options().ok()?,
                    workdir: repo.workdir()?.to_path_buf(),
                })
            })
            .as_ref();
        let Some(idx) = idx else {
            return false;
        };
        let Some(entry) = idx.state.entry_by_path(path.as_bstr()) else {
            return false;
        };
        // "This is not the sha1 we are looking for, or unreusable because it is not
        // a regular file."
        if entry.id != *oid
            || !matches!(
                entry.mode,
                gix::index::entry::Mode::FILE | gix::index::entry::Mode::FILE_EXECUTABLE
            )
        {
            return false;
        }
        // `CE_VALID` ("assume unchanged") and skip-worktree both mean the worktree
        // is not guaranteed to hold the entry's content.
        if entry
            .flags
            .intersects(gix::index::entry::Flags::ASSUME_VALID | gix::index::entry::Flags::SKIP_WORKTREE)
        {
            return false;
        }
        // `ce_uptodate()` is an in-core marker that a fresh `repo_read_index()` never
        // sets, so the answer always comes from `lstat()` + `ie_match_stat()`.
        let full = idx.workdir.join(gix::path::from_bstr(path.as_bstr()).as_ref());
        let Ok(meta) = gix::index::fs::Metadata::from_path_no_follow(&full) else {
            return false;
        };
        let Ok(stat) = gix::index::entry::Stat::from_fs(&meta) else {
            return false;
        };
        entry.stat.matches(&stat, idx.stat)
    }
}

/// Whether queueing this pair makes git read the index.
///
/// `diff_queue_change()` and `diff_queue_addremove()` run `is_submodule_ignored()`
/// on a gitlink, which reaches `submodule_from_path()` → `repo_read_gitmodules()` →
/// `repo_read_index()`. Nothing else in `builtin/diff-pairs.c` reads the index, so a
/// batch without a gitlink leaves `istate->cache` NULL and `reuse_worktree_file()`
/// declines every path. `--ignore-submodules[=<when>]` sets
/// `override_submodule_config`, which skips the lookup and therefore the read.
/// A rename or copy is queued by `diff_queue()` directly and never consults it.
fn queues_read_index(p: &Pair, overridden: bool) -> bool {
    if overridden {
        return false;
    }
    match p.kind() {
        b'A' => is_gitlink_mode(p.new_mode),
        b'D' => is_gitlink_mode(p.old_mode),
        b'M' | b'T' => is_gitlink_mode(p.old_mode) && is_gitlink_mode(p.new_mode),
        _ => false,
    }
}

/// The patch stream as it is assembled.
///
/// Internally produced sections are collected uncoloured and re-emitted in one pass
/// through `fn_out_consume()`'s colour chain, then given `--line-prefix`. An external
/// driver's stdout goes in exactly as the child wrote it: git hands the child its own
/// output descriptor, so neither the colouriser nor the line prefix ever touches it.
struct PatchSink<'a> {
    /// Finished bytes: coloured and prefixed sections interleaved with spliced ones.
    out: Vec<u8>,
    /// The uncoloured patch text accumulated since the last splice.
    plain: Vec<u8>,
    /// Per-file-pair paint state for the pending `plain` section.
    files: Vec<diff_color::FilePaint>,
    colors: &'a diff_color::DiffColors,
    paint: diff_color::PaintOptions,
    extra: &'a diff_color::ExtraPaint,
    ws_rule: u32,
    lp: &'a [u8],
}

impl PatchSink<'_> {
    /// Close the pending internal section, colouring and prefixing it.
    fn flush_plain(&mut self) {
        if self.plain.is_empty() {
            return;
        }
        let sub = diff_color::colorize_patch_ex(
            &self.plain,
            self.colors,
            &self.paint,
            &self.files,
            diff_color::FilePaint::new(self.ws_rule),
            self.extra,
        );
        append_prefixed(&mut self.out, self.lp, &sub);
        self.plain.clear();
        self.files.clear();
    }

    /// Splice an external driver's stdout in verbatim.
    fn splice(&mut self, bytes: &[u8]) {
        self.flush_plain();
        self.out.extend_from_slice(bytes);
    }

    /// Append lines that carry their own colouring — `--submodule=log`'s summary,
    /// whose `emit_line()` calls paint each line themselves — but still take
    /// `--line-prefix`.
    fn prefixed(&mut self, bytes: &[u8]) {
        self.flush_plain();
        append_prefixed(&mut self.out, self.lp, bytes);
    }

    fn finish(mut self) -> Vec<u8> {
        self.flush_plain();
        self.out
    }
}

/// `external_diff()` (diff.c:558), restricted to what this command can observe.
///
/// The configuration half — `diff.external` and `diff.trustExitCode` — is parsed by
/// `git_diff_ui_config()`, and `builtin/diff-pairs.c` registers
/// `git_diff_basic_config()` instead, so `diff-pairs` never reads it. Verified
/// against stock git 2.55.0: `git -c diff.external=<prog> diff-pairs -z --ext-diff`
/// still prints the internal patch. Only the environment reaches here.
fn external_diff_env() -> std::result::Result<Option<ExternalDiff>, ExitCode> {
    let Some(cmd) = std::env::var_os("GIT_EXTERNAL_DIFF") else {
        return Ok(None);
    };
    // `xstrdup_or_null()` keeps an empty value, so an empty variable still selects
    // an (unrunnable) external diff rather than falling back to the built-in one.
    Ok(Some(ExternalDiff {
        cmd: cmd.to_string_lossy().into_owned(),
        trust_exit_code: git_env_bool("GIT_EXTERNAL_DIFF_TRUST_EXIT_CODE", false)?,
    }))
}

/// `git_env_bool()` + `git_parse_maybe_bool()`: an unset variable takes the default,
/// `true`/`yes`/`on`/a non-zero integer are true, `false`/`no`/`off`/`0`/empty are
/// false, and anything else is fatal.
fn git_env_bool(key: &str, def: bool) -> std::result::Result<bool, ExitCode> {
    let Some(raw) = std::env::var_os(key) else {
        return Ok(def);
    };
    let v = raw.to_string_lossy().into_owned();
    if v.is_empty() {
        return Ok(false);
    }
    if v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes") || v.eq_ignore_ascii_case("on")
    {
        return Ok(true);
    }
    if v.eq_ignore_ascii_case("false")
        || v.eq_ignore_ascii_case("no")
        || v.eq_ignore_ascii_case("off")
    {
        return Ok(false);
    }
    match v.parse::<i64>() {
        Ok(n) => Ok(n != 0),
        Err(_) => Err(fatal(&format!(
            "bad boolean environment value '{v}' for '{key}'"
        ))),
    }
}

/// `run_diff_cmd()`'s driver override: with `allow_external` on, a path whose `diff`
/// gitattribute names a driver that configures `diff.<name>.command` uses that
/// driver in preference to `GIT_EXTERNAL_DIFF`.
fn external_for_path(
    repo: &gix::Repository,
    drivers: Drivers<'_, '_>,
    path: &gix::bstr::BStr,
    env: Option<&ExternalDiff>,
) -> std::result::Result<Option<ExternalDiff>, ExitCode> {
    let name = match drivers.borrow_mut().driver_name(path) {
        Ok(n) => n,
        Err(e) => return Err(fatal(&e.to_string())),
    };
    if let Some(name) = name {
        if let Some(cmd) = super::cat_file::diff_driver_config(repo, &name, "command") {
            let trust = super::cat_file::diff_driver_config(repo, &name, "trustexitcode")
                .map(|v| {
                    let v = v.trim();
                    !(v.is_empty()
                        || v.eq_ignore_ascii_case("false")
                        || v.eq_ignore_ascii_case("no")
                        || v.eq_ignore_ascii_case("off")
                        || v == "0")
                })
                .unwrap_or(false);
            return Ok(Some(ExternalDiff {
                cmd,
                trust_exit_code: trust,
            }));
        }
    }
    Ok(env.cloned())
}

/// One side of the argument triple `run_external_diff()` hands the driver.
struct TempSide {
    /// The private directory holding the temporary file, removed once the driver
    /// has run. `None` for the `/dev/null` placeholder, which owns nothing.
    dir: Option<std::path::PathBuf>,
    name: std::ffi::OsString,
    hex: String,
    mode: String,
}

/// `prepare_temp_file()` (diff.c:4698).
///
/// A missing side becomes the `/dev/null` `.`/`.` triple. An existing one is
/// normally inflated into a temporary file of its own, except when
/// `reuse_worktree_file()` confirms the checked-out file already holds exactly that
/// object — then the driver is handed the worktree path itself, with the pair's own
/// mode rather than the index entry's. Gitlinks never take that branch.
fn prepare_temp_file(
    repo: &gix::Repository,
    drivers: Drivers<'_, '_>,
    ctx: &ExtCtx<'_, '_>,
    path: &BString,
    id: &ObjectId,
    mode: u32,
    valid: bool,
) -> std::result::Result<TempSide, ExitCode> {
    use std::os::unix::ffi::OsStrExt;

    if !valid {
        return Ok(TempSide {
            dir: None,
            name: std::ffi::OsString::from("/dev/null"),
            hex: ".".to_string(),
            mode: ".".to_string(),
        });
    }
    if !is_gitlink_mode(mode) && ctx.reuse_worktree_file(repo, path, id) {
        return Ok(TempSide {
            dir: None,
            name: std::ffi::OsStr::from_bytes(path).to_os_string(),
            hex: id.to_hex().to_string(),
            mode: format!("{mode:06o}"),
        });
    }
    // `diff_populate_filespec()` synthesises a gitlink's content rather than looking
    // it up, which is why a submodule pair reaches the driver as one text line.
    let data = if is_gitlink_mode(mode) {
        format!("Subproject commit {}\n", id.to_hex()).into_bytes()
    } else {
        read_blob(repo, *id, true)?
    };
    let dir = match super::cat_file::temp_blob_dir() {
        Ok(d) => d,
        Err(e) => return Err(fatal(&e.to_string())),
    };
    let file = match drivers
        .borrow_mut()
        .prep_temp_blob(&dir, path.as_bstr(), &data)
    {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(fatal(&format!("unable to write temp-file: {e}")));
        }
    };
    Ok(TempSide {
        dir: Some(dir),
        name: file.into_os_string(),
        hex: id.to_hex().to_string(),
        mode: format!("{mode:06o}"),
    })
}

/// `fill_metainfo()` (diff.c:4858) as an external driver receives it: git always
/// builds this copy with `GIT_COLOR_NEVER`, and the mode lines (`new file mode`,
/// `deleted file mode`, `old mode`/`new mode`) are *not* part of it — `builtin_diff()`
/// writes those itself — so an addition or a deletion carries nothing but the
/// `index` line.
fn external_xfrm_msg(
    repo: &gix::Repository,
    p: &Pair,
    opts: &Opts,
    base_abbrev: usize,
) -> Vec<u8> {
    let mut msg = Vec::new();
    match p.kind() {
        k @ (b'C' | b'R') => {
            let verb = if k == b'C' { "copy" } else { "rename" };
            msg.extend_from_slice(format!("similarity index {}%\n", p.score()).as_bytes());
            msg.extend_from_slice(format!("{verb} from ").as_bytes());
            msg.extend_from_slice(&p.old_path);
            msg.extend_from_slice(format!("\n{verb} to ").as_bytes());
            msg.extend_from_slice(&p.new_path);
            msg.push(b'\n');
        }
        b'M' if p.score() != 0 => {
            msg.extend_from_slice(format!("dissimilarity index {}%\n", p.score()).as_bytes());
        }
        _ => {}
    }
    if p.old_id != p.new_id {
        msg.extend_from_slice(b"index ");
        msg.extend_from_slice(oid_text(repo, &p.old_id, base_abbrev, opts.full_index).as_bytes());
        msg.extend_from_slice(b"..");
        msg.extend_from_slice(oid_text(repo, &p.new_id, base_abbrev, opts.full_index).as_bytes());
        if p.old_valid() && p.new_valid() && p.old_mode == p.new_mode {
            msg.extend_from_slice(format!(" {:06o}", p.new_mode).as_bytes());
        }
        msg.push(b'\n');
    }
    msg
}

/// Whatever the driver wrote, and whether git counted the pair as changed.
struct ExtRun {
    stdout: Vec<u8>,
    found_changes: bool,
    /// `die(_("external diff died, stopping at %s"))`. Raised by the caller only
    /// *after* `stdout` has been passed on: git's child writes straight to the
    /// output descriptor, so everything it printed before failing is already out.
    died: Option<String>,
}

/// `run_external_diff()` (diff.c:4777).
///
/// The driver's stdout *is* the patch for this pair: git hands the child its own
/// output descriptor, so the bytes are never re-coloured and never carry
/// `--line-prefix`. Capturing them through a pipe and splicing them into the stream
/// puts the same bytes in the same order — git's own `fflush(NULL)` before forking
/// exists to guarantee exactly that ordering.
#[allow(clippy::too_many_arguments)]
fn run_external_diff(
    pgm: &ExternalDiff,
    repo: &gix::Repository,
    ctx: &ExtCtx<'_, '_>,
    p: &Pair,
    opts: &Opts,
    base_abbrev: usize,
    total: usize,
    // `o->file`: `false` is git's quiet probe (`diff_flush_patch_quietly()`), which
    // nulls the file pointer so nothing is written.
    want_output: bool,
) -> std::result::Result<ExtRun, ExitCode> {
    use std::os::unix::ffi::OsStrExt;

    // "If we don't need to show the diff and the external diff program lacks the
    // ability to tell us whether it's empty then we consider it non-empty without
    // even asking" — the driver is not run at all.
    if !pgm.trust_exit_code && !want_output {
        return Ok(ExtRun {
            stdout: Vec::new(),
            found_changes: true,
            died: None,
        });
    }

    let name = p.old_path.clone();
    let other = (p.old_path != p.new_path).then(|| p.new_path.clone());

    let one = prepare_temp_file(
        repo,
        ctx.drivers,
        ctx,
        &p.old_path,
        &p.old_id,
        p.old_mode,
        p.old_valid(),
    )?;
    let two = match prepare_temp_file(
        repo,
        ctx.drivers,
        ctx,
        &p.new_path,
        &p.new_id,
        p.new_mode,
        p.new_valid(),
    ) {
        Ok(t) => t,
        Err(code) => {
            if let Some(d) = &one.dir {
                let _ = std::fs::remove_dir_all(d);
            }
            return Err(code);
        }
    };

    let mut cmd = super::cat_file::shell_command(&pgm.cmd);
    cmd.arg(std::ffi::OsStr::from_bytes(&name));
    cmd.arg(&one.name).arg(&one.hex).arg(&one.mode);
    cmd.arg(&two.name).arg(&two.hex).arg(&two.mode);
    if let Some(other) = &other {
        cmd.arg(std::ffi::OsStr::from_bytes(other));
        let xfrm = external_xfrm_msg(repo, p, opts, base_abbrev);
        if !xfrm.is_empty() {
            cmd.arg(std::ffi::OsStr::from_bytes(&xfrm));
        }
    }
    ctx.counter.set(ctx.counter.get() + 1);
    cmd.env("GIT_DIFF_PATH_COUNTER", ctx.counter.get().to_string());
    cmd.env("GIT_DIFF_PATH_TOTAL", total.to_string());
    cmd.stdout(if want_output {
        std::process::Stdio::piped()
    } else {
        // `cmd.no_stdout = 1` opens /dev/null for the child.
        std::process::Stdio::null()
    });

    let spawned = cmd.spawn().and_then(|child| child.wait_with_output());
    for dir in [one.dir, two.dir].into_iter().flatten() {
        let _ = std::fs::remove_dir_all(dir);
    }

    let died = format!("external diff died, stopping at {}", name.to_str_lossy());
    let (rc, stdout) = match spawned {
        Ok(o) => (o.status.code().unwrap_or(-1), o.stdout),
        Err(e) => {
            // `prepare_cmd()` resolves a name with no directory separator through
            // `$PATH` and reports its own failure; a path that reaches `execve()`
            // fails inside the child instead, which `child_err_spew()` reports with
            // the die routine.
            if pgm.cmd.contains('/') {
                eprintln!("fatal: cannot exec '{}': {}", pgm.cmd, io_reason(&e));
            } else {
                eprintln!("error: cannot run {}: {}", pgm.cmd, io_reason(&e));
            }
            return Ok(ExtRun {
                stdout: Vec::new(),
                found_changes: false,
                died: Some(died),
            });
        }
    };

    let (found_changes, died) = match (pgm.trust_exit_code, rc) {
        (false, 0) => (true, None),
        (true, 0) => (false, None),
        (true, 1) => (true, None),
        _ => (false, Some(died)),
    };
    Ok(ExtRun {
        stdout,
        found_changes,
        died,
    })
}

// ---------------------------------------------------------------------------
// --submodule=log (submodule.c `show_submodule_diff_summary`)
// ---------------------------------------------------------------------------

/// `open_submodule()` (submodule.c:508) + `submodule_to_gitdir()`: `<path>/.git`
/// first — resolving a gitfile — and the superproject's `.git/modules/<name>` for an
/// absorbed submodule. `None` when the submodule is not present at all.
fn open_submodule(repo: &gix::Repository, path: &BString) -> Option<gix::Repository> {
    if let Some(workdir) = repo.workdir() {
        let dot_git = workdir
            .join(gix::path::from_bstr(path.as_bstr()).as_ref())
            .join(".git");
        if dot_git.exists() {
            if let Ok(sub) = gix::open(&dot_git) {
                return Some(sub);
            }
        }
    }
    let name = repo
        .submodules()
        .ok()
        .flatten()?
        .find(|m| m.path().map(|p| p == *path).unwrap_or(false))?
        .name()
        .to_owned();
    let dir = repo
        .common_dir()
        .join("modules")
        .join(gix::path::from_bstr(name.as_bstr()).as_ref());
    gix::open(dir).ok()
}

/// `lookup_commit_reference()`: peel `id` to a commit inside `sub`, or `None` when it
/// is the null id or simply absent from that repository.
fn lookup_commit_reference(sub: &gix::Repository, id: &ObjectId) -> Option<ObjectId> {
    if id.is_null() {
        return None;
    }
    let object = sub.find_object(*id).ok()?;
    Some(object.peel_to_kind(gix::object::Kind::Commit).ok()?.id)
}

/// One commit in the symmetric-difference walk, with the side it came from.
#[derive(Clone, Copy)]
struct SubCommit {
    id: ObjectId,
    /// `SYMMETRIC_LEFT`: `get_revision_mark()` prints `<` for it and `>` otherwise.
    left: bool,
    seconds: i64,
}

/// `commit_list_insert_by_date()`: newest first, an equal date landing after the
/// entries already holding it.
fn insert_by_date(list: &mut Vec<SubCommit>, item: SubCommit) {
    let at = list
        .iter()
        .position(|e| e.seconds < item.seconds)
        .unwrap_or(list.len());
    list.insert(at, item);
}

/// `prepare_submodule_diff_summary()` (submodule.c:451) and the `limit_list()` pass
/// behind it: a `--left-right --first-parent` walk between the two commits with the
/// merge bases marked uninteresting.
///
/// `mark_parents_uninteresting()` marks each merge base's whole ancestry over *every*
/// parent — `exclude_first_parent_only` is not set here — and `limit_list()` keeps
/// none of it. `first_parent_only` then makes each side linear, so what is left is a
/// date-ordered merge of two chains: pop the newest entry, print it, and queue its
/// first parent with the same `SYMMETRIC_LEFT` flag.
fn submodule_walk(
    sub: &gix::Repository,
    left: ObjectId,
    right: ObjectId,
    bases: &[ObjectId],
) -> Vec<SubCommit> {
    let mut uninteresting: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();
    let mut stack: Vec<ObjectId> = bases.to_vec();
    while let Some(id) = stack.pop() {
        if !uninteresting.insert(id) {
            continue;
        }
        if let Ok(commit) = sub.find_commit(id) {
            stack.extend(commit.parent_ids().map(|p| p.detach()));
        }
    }

    let seconds = |id: &ObjectId| -> i64 {
        sub.find_commit(*id)
            .ok()
            .and_then(|c| c.time().ok().map(|t| t.seconds))
            .unwrap_or(0)
    };
    let mut list: Vec<SubCommit> = Vec::new();
    // The `SEEN` flag: a commit enters the list exactly once, keeping the side it was
    // first reached from.
    let mut seen: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();
    let push = |list: &mut Vec<SubCommit>,
                seen: &mut std::collections::HashSet<ObjectId>,
                id: ObjectId,
                left: bool| {
        if !seen.insert(id) {
            return;
        }
        insert_by_date(
            list,
            SubCommit {
                id,
                left,
                seconds: seconds(&id),
            },
        );
    };
    push(&mut list, &mut seen, left, true);
    push(&mut list, &mut seen, right, false);

    let mut out = Vec::new();
    while !list.is_empty() {
        let e = list.remove(0);
        if uninteresting.contains(&e.id) {
            // Everything above it is uninteresting too, so the branch ends here.
            continue;
        }
        out.push(e);
        // `add_parents_to_list()` stops after the first parent under
        // `first_parent_only`.
        if let Some(parent) = sub
            .find_commit(e.id)
            .ok()
            .and_then(|c| c.parent_ids().next())
        {
            push(&mut list, &mut seen, parent.detach(), e.left);
        }
    }
    out
}

/// `DIRTY_SUBMODULE_UNTRACKED` (submodule.h): the submodule worktree holds
/// untracked files. `git diff` clears this bit by default
/// (`ignore_untracked_in_submodules` in `diff_setup_done()`, diff.c:5169).
pub(crate) const DIRTY_SUBMODULE_UNTRACKED: u8 = 1;
/// `DIRTY_SUBMODULE_MODIFIED`: tracked content inside the submodule worktree
/// differs from its `HEAD`.
pub(crate) const DIRTY_SUBMODULE_MODIFIED: u8 = 2;

/// `show_submodule_diff_summary()` (submodule.c:614), preceded by the
/// `show_submodule_header()` it shares with the inline-diff format.
///
/// The lines are written with their colours already applied, since `emit_line()`
/// paints the header with nothing and the two commit markers with `DIFF_FILE_OLD`
/// (the left side) and `DIFF_FILE_NEW` (the right). The caller supplies
/// `--line-prefix`.
///
/// `dirty` is git's `two->dirty_submodule` bitmask, which only a worktree pair
/// ever carries: its two bits print their own line ahead of the header, and an
/// otherwise-unchanged gitlink then stops there.
pub(crate) fn show_submodule_diff_summary(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    path: &BString,
    one: &ObjectId,
    two: &ObjectId,
    dirty: u8,
    base_abbrev: usize,
    colors: &diff_color::DiffColors,
) {
    let hdr = show_submodule_header(out, repo, path, one, two, dirty, base_abbrev);

    // "If we don't have both a left and a right pointer, there is no reason to try
    // and display a summary."
    let (Some(sub), Some(l), Some(r)) = (&hdr.sub, hdr.left, hdr.right) else {
        return;
    };
    for c in submodule_walk(sub, l, r, &hdr.bases) {
        let mut line = b"  ".to_vec();
        line.push(if c.left { b'<' } else { b'>' });
        line.push(b' ');
        line.extend_from_slice(&super::cherry::subject_of(sub, c.id).unwrap_or_default());
        let slot = if c.left {
            diff_color::DiffSlot::Old
        } else {
            diff_color::DiffSlot::New
        };
        diff_color::paint(out, colors, slot, &line);
        out.push(b'\n');
    }
}

/// What `show_submodule_header()` resolves for the caller that follows it: git
/// passes these back through the `sub` / `left` / `right` / `merge_bases`
/// out-parameters.
pub(crate) struct SubmoduleHeader {
    /// The submodule's own repository, when it could be opened at all.
    pub(crate) sub: Option<gix::Repository>,
    /// The pre-image commit, peeled inside `sub`.
    pub(crate) left: Option<ObjectId>,
    /// The post-image commit, peeled inside `sub`.
    pub(crate) right: Option<ObjectId>,
    /// Their merge bases, which decide `..` versus `...` and the `(rewind)` suffix.
    pub(crate) bases: Vec<ObjectId>,
}

/// `show_submodule_header()` (submodule.c:538): the `Submodule <path> <a>..<b>`
/// line both `--submodule=log` and `--submodule=diff` open with, plus the commits
/// each of them then needs.
pub(crate) fn show_submodule_header(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    path: &BString,
    one: &ObjectId,
    two: &ObjectId,
    dirty: u8,
    base_abbrev: usize,
) -> SubmoduleHeader {
    // `show_submodule_header()`'s first two statements (submodule.c:550): both
    // land before the `oideq(one, two)` early return, so a submodule that only
    // has local damage prints these and nothing else.
    if dirty & DIRTY_SUBMODULE_UNTRACKED != 0 {
        out.extend_from_slice(b"Submodule ");
        out.extend_from_slice(path);
        out.extend_from_slice(b" contains untracked content\n");
    }
    if dirty & DIRTY_SUBMODULE_MODIFIED != 0 {
        out.extend_from_slice(b"Submodule ");
        out.extend_from_slice(path);
        out.extend_from_slice(b" contains modified content\n");
    }
    let sub = open_submodule(repo, path);

    let mut message: Option<&str> = if one.is_null() {
        Some("(new submodule)")
    } else if two.is_null() {
        Some("(submodule deleted)")
    } else {
        None
    };
    let mut left = None;
    let mut right = None;
    let mut bases: Vec<ObjectId> = Vec::new();
    let mut fast_forward = false;
    let mut fast_backward = false;

    if let Some(subr) = &sub {
        left = lookup_commit_reference(subr, one);
        right = lookup_commit_reference(subr, two);
        // "Warn about missing commits in the submodule project, but only if they
        // aren't null."
        if (!one.is_null() && left.is_none()) || (!two.is_null() && right.is_none()) {
            message = Some("(commits not present)");
        }
        // `merge_bases_many()` answers with an empty list, not an error, whenever
        // one side is absent.
        match (left, right) {
            // Its `one == twos[i]` short-circuit compares the two commit *pointers*,
            // so with both sides unreadable the NULLs match: the base list becomes
            // `{NULL}`, its head equals `left`, and the header prints `..` rather
            // than `...`.
            (None, None) => fast_forward = true,
            (Some(l), Some(r)) if l == r => bases.push(l),
            (Some(l), Some(r)) => match repo_merge_bases(subr, l, r) {
                Ok(b) => bases = b,
                Err(()) => {
                    message = Some("(corrupt repository)");
                    emit_submodule_header(out, repo, path, one, two, base_abbrev, message, false, false);
                    // The header is the whole report for an object store that
                    // cannot answer a merge-base query: with no bases to mark
                    // uninteresting, a walk would print both histories in full.
                    return SubmoduleHeader { sub, left: None, right: None, bases };
                }
            },
            _ => {}
        }
        if let Some(first) = bases.first() {
            if Some(*first) == left {
                fast_forward = true;
            } else if Some(*first) == right {
                fast_backward = true;
            }
        }
        // An unchanged gitlink prints no header at all — the `-dirty` lines above
        // are the whole report.
        if one == two {
            return SubmoduleHeader { sub, left, right, bases };
        }
    } else if message.is_none() {
        message = Some("(commits not present)");
    }

    emit_submodule_header(
        out,
        repo,
        path,
        one,
        two,
        base_abbrev,
        message,
        fast_forward,
        fast_backward,
    );
    SubmoduleHeader { sub, left, right, bases }
}

/// `repo_get_merge_bases()`, reduced to the shape this caller needs: every merge
/// base, or `Err` for the "(corrupt repository)" report.
fn repo_merge_bases(
    sub: &gix::Repository,
    left: ObjectId,
    right: ObjectId,
) -> std::result::Result<Vec<ObjectId>, ()> {
    match sub.merge_bases_many(left, &[right]) {
        Ok(ids) => Ok(ids.into_iter().map(|id| id.detach()).collect()),
        Err(_) => Err(()),
    }
}

/// `show_submodule_header()`'s `output_header:` block (submodule.c:598).
#[allow(clippy::too_many_arguments)]
fn emit_submodule_header(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    path: &BString,
    one: &ObjectId,
    two: &ObjectId,
    base_abbrev: usize,
    message: Option<&str>,
    fast_forward: bool,
    fast_backward: bool,
) {
    out.extend_from_slice(b"Submodule ");
    out.extend_from_slice(path);
    out.push(b' ');
    // `strbuf_add_unique_abbrev()` shortens against the *superproject's* object
    // store, where a submodule commit is normally absent — so this is the plain
    // `DEFAULT_ABBREV`-wide prefix.
    out.extend_from_slice(oid_text(repo, one, base_abbrev, false).as_bytes());
    out.extend_from_slice(if fast_backward || fast_forward {
        &b".."[..]
    } else {
        &b"..."[..]
    });
    out.extend_from_slice(oid_text(repo, two, base_abbrev, false).as_bytes());
    match message {
        Some(m) => out.extend_from_slice(format!(" {m}\n").as_bytes()),
        None if fast_backward => out.extend_from_slice(b" (rewind):\n"),
        None => out.extend_from_slice(b":\n"),
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

/// `S_ISGITLINK()`.
fn is_gitlink_mode(mode: u32) -> bool {
    mode & IFMT == 0o160000
}

// ---------------------------------------------------------------------------
// --dirstat (diff.c `show_dirstat`)
// ---------------------------------------------------------------------------

/// `show_dirstat()` (diff.c:3366): how much damage each path contributes, in the
/// "changes" (byte-level) or "files" (one unit apiece) mode. `--dirstat=lines` does
/// not come through here — it reads the diffstat instead.
fn dirstat_damage(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    pairs: &[Pair],
    opts: &Opts,
    tc: TextconvRef<'_, '_>,
) -> std::result::Result<Vec<(BString, u64)>, ExitCode> {
    let mut out = Vec::with_capacity(pairs.len());
    for p in pairs {
        // Equal object ids mean identical content, so no blob has to be read at all.
        if p.old_valid() && p.new_valid() && p.old_id == p.new_id {
            out.push((p.new_path.clone(), 0));
            continue;
        }
        // In `--dirstat-by-file` mode the content is never examined: that the id
        // changed is the whole signal, and every file contributes equal damage.
        if opts.dirstat.by_file {
            out.push((p.new_path.clone(), 1));
            continue;
        }
        let an = analyze(repo, cache, p, opts, tc)?;
        let damage = if p.old_valid() && p.new_valid() {
            // `hash_chars()` asks `diff_filespec_is_binary()` about each side on its
            // own, so the two `is_text` flags are derived separately.
            let (copied, added) = diff_files::count_changes_sides(
                &an.old_data,
                !diffcore_rename::buffer_is_binary(&an.old_data),
                &an.new_data,
                !diffcore_rename::buffer_is_binary(&an.new_data),
            );
            // Original minus copied is the removed material and `added` is the new
            // material; both are damage done to the preimage.
            (an.old_data.len() as u64).saturating_sub(copied) + added
        } else if p.old_valid() {
            an.old_data.len() as u64
        } else if p.new_valid() {
            an.new_data.len() as u64
        } else {
            continue;
        };
        // The ids differ, so *something* changed; a zero score is forced to one.
        out.push((p.new_path.clone(), if damage == 0 { 1 } else { damage }));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// --check (diff.c `builtin_checkdiff` / `checkdiff_consume`, ws.c `ws_check`)
// ---------------------------------------------------------------------------

/// `whitespace_rule()` (ws.c:82) and `ll_merge_marker_size()`: the per-path
/// `whitespace` and `conflict-marker-size` gitattributes, over `core.whitespace`.
struct WsRules<'repo> {
    /// `whitespace_rule_cfg`, from `core.whitespace`.
    cfg: u32,
    /// `None` when no attribute stack could be built (a bare repository), in which
    /// case every path falls back to `cfg` — which is what an empty attribute set
    /// yields anyway.
    stack: Option<gix::AttributeStack<'repo>>,
    outcome: gix::attrs::search::Outcome,
}

impl<'repo> WsRules<'repo> {
    fn new(repo: &'repo gix::Repository, cfg: u32) -> Self {
        let stack = repo.index_or_empty().ok().and_then(|index| {
            repo.attributes_only(
                &index,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
            )
            .ok()
        });
        WsRules {
            cfg,
            stack,
            outcome: gix::attrs::search::Outcome::default(),
        }
    }

    /// The `(whitespace_rule, conflict_marker_size)` pair for `path`.
    fn at(&mut self, path: &BString) -> (u32, usize) {
        let Some(stack) = self.stack.as_mut() else {
            return (self.cfg, diff_files::DEFAULT_CONFLICT_MARKER_SIZE);
        };
        let mode = Some(gix::index::entry::Mode::FILE);
        // The stack only knows an attribute's name once a file declaring it has been
        // parsed, so descend first, then size the outcome, then match.
        if stack.at_entry(path.as_bstr(), mode).is_err() {
            return (self.cfg, diff_files::DEFAULT_CONFLICT_MARKER_SIZE);
        }
        self.outcome.initialize_with_selection(
            stack.attributes_collection(),
            ["whitespace", "conflict-marker-size"],
        );
        let Ok(platform) = stack.at_entry(path.as_bstr(), mode) else {
            return (self.cfg, diff_files::DEFAULT_CONFLICT_MARKER_SIZE);
        };
        platform.matching_attributes(&mut self.outcome);

        let mut rule = self.cfg;
        let mut marker = diff_files::DEFAULT_CONFLICT_MARKER_SIZE;
        for m in self.outcome.iter_selected() {
            match m.assignment.name.as_str() {
                "whitespace" => {
                    rule = match m.assignment.state {
                        // `true` (`whitespace`): every rule that neither loosens an
                        // error (`cr-at-eol`) nor is excluded by default
                        // (`tab-in-indent`), keeping the configured tab width.
                        gix::attrs::StateRef::Set => {
                            diff_color::ws_tab_width(self.cfg) as u32
                                | diff_color::WS_TRAILING_SPACE
                                | diff_color::WS_SPACE_BEFORE_TAB
                                | diff_color::WS_INDENT_WITH_NON_TAB
                                | diff_color::WS_BLANK_AT_EOL
                                | diff_color::WS_BLANK_AT_EOF
                                | diff_color::WS_INCOMPLETE_LINE
                        }
                        // `false` (`-whitespace`): nothing but the tab width.
                        gix::attrs::StateRef::Unset => diff_color::ws_tab_width(self.cfg) as u32,
                        gix::attrs::StateRef::Value(v) => {
                            diff_color::parse_whitespace_rule(&v.as_bstr().to_str_lossy())
                        }
                        // Unspecified and `!whitespace` both reset to the config.
                        gix::attrs::StateRef::Unspecified => self.cfg,
                    };
                }
                "conflict-marker-size" => {
                    if let gix::attrs::StateRef::Value(v) = m.assignment.state {
                        if let Ok(n) = v.as_bstr().to_str_lossy().parse::<usize>() {
                            if n > 0 {
                                marker = n;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        (rule, marker)
    }
}

/// `builtin_checkdiff()` (diff.c:4281) driving `checkdiff_consume()` (diff.c:3555).
///
/// Only the *new* side is examined — `--check` reports what the change introduces —
/// and the diff it walks is its own: `xecfg.ctxlen = 1` with `xpp.flags = 0`, so no
/// `-w`/`-b`/`-I`/`--ignore-blank-lines`, no indent heuristic and no
/// `--diff-algorithm` reach it. Returns `o->flags.check_failed`.
fn render_check(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    pairs: &[Pair],
    opts: &Opts,
    colors: &diff_color::DiffColors,
    ws_cfg: u32,
    tc: TextconvRef<'_, '_>,
) -> std::result::Result<bool, ExitCode> {
    let mut failed = false;
    let mut rules = WsRules::new(repo, ws_cfg);
    let lp = opts.line_prefix.as_slice();
    let set = colors.get(diff_color::DiffSlot::New);
    let ws_color = colors.get(diff_color::DiffSlot::Whitespace);
    let reset = colors.reset();

    for p in pairs {
        // `diff_flush_checkdiff()`: an unmodified pair and a tree entry are both skipped.
        if diff_unmodified_pair(p) {
            continue;
        }
        if (p.old_valid() && p.old_mode & IFMT == 0o040000)
            || (p.new_valid() && p.new_mode & IFMT == 0o040000)
        {
            continue;
        }

        // `run_checkdiff()`: `other` is the destination path when it differs, and both
        // the reported name and the attribute path come from it when it exists.
        let name = &p.new_path;
        let (mut ws_rule, marker_size) = rules.at(name);
        // A symlink being an incomplete line is not news.
        if p.new_valid() && p.new_mode & IFMT == 0o120000 {
            ws_rule &= !diff_color::WS_INCOMPLETE_LINE;
        }

        let an = analyze(repo, cache, p, opts, tc)?;
        // Deliberately only the new side is tested for binaryness.
        if p.new_valid() && diffcore_rename::buffer_is_binary(&an.new_data) {
            continue;
        }

        let before = byte_lines(&an.old_data);
        let after = byte_lines(&an.new_data);
        let mut input: InternedInput<Vec<u8>> = InternedInput::default();
        input.update_before(before.iter().map(|l| l.to_vec()));
        input.update_after(after.iter().map(|l| l.to_vec()));
        let mut diff =
            gix::diff::blob::Diff::compute(gix::diff::blob::Algorithm::Myers, &input);
        diff.postprocess_no_heuristic(&input);

        // xdiff hands `checkdiff_consume()` whole lines, and the record for a final
        // line without a terminator arrives with the newline `xdl_emit_diff()` writes
        // before the `\ No newline at end of file` marker — so `ws_check()` never sees
        // the missing terminator, and `WS_INCOMPLETE_LINE` is reported by the marker's
        // own branch instead.
        let mut last_added_is_final = false;
        for h in diff.hunks() {
            let start = h.after.start as usize;
            for (k, line) in after[start..start + h.after.len()].iter().enumerate() {
                let lineno = start + k + 1;
                let mut body: Vec<u8> = (*line).to_vec();
                if body.last() != Some(&b'\n') {
                    body.push(b'\n');
                    last_added_is_final = true;
                }
                if diff_files::is_conflict_marker_sized(&body, marker_size) {
                    failed = true;
                    out.extend_from_slice(lp);
                    out.extend_from_slice(name.as_slice());
                    out.extend_from_slice(format!(":{lineno}: leftover conflict marker\n").as_bytes());
                }
                let bad = diff_files::ws_check(&body, ws_rule);
                if bad == 0 {
                    continue;
                }
                failed = true;
                out.extend_from_slice(lp);
                out.extend_from_slice(name.as_slice());
                out.extend_from_slice(
                    format!(":{lineno}: {}.\n", diff_files::whitespace_error_string(bad)).as_bytes(),
                );
                // `emit_line(o, set, reset, line, 1)` prints the marker, then
                // `ws_check_emit()` repaints the body around its offending runs.
                out.extend_from_slice(lp);
                out.extend_from_slice(set.as_bytes());
                out.push(b'+');
                out.extend_from_slice(reset.as_bytes());
                diff_color::ws_check_emit(out, &body, ws_rule, set, reset, ws_color);
            }
        }

        // The `\ No newline at end of file` branch: reported only when the record it
        // follows was an added one.
        if ws_rule & diff_color::WS_INCOMPLETE_LINE != 0 && last_added_is_final {
            failed = true;
            out.extend_from_slice(lp);
            out.extend_from_slice(name.as_slice());
            out.extend_from_slice(
                format!(
                    ":{}: {}.\n",
                    after.len(),
                    diff_files::whitespace_error_string(diff_color::WS_INCOMPLETE_LINE)
                )
                .as_bytes(),
            );
        }

        // `check_blank_at_eof()` runs over the whole file rather than the hunk stream,
        // and — unlike every other line here — git prints it without the line prefix.
        if ws_rule & diff_color::WS_BLANK_AT_EOF != 0 {
            let (_, post) = diff_color::check_blank_at_eof(&an.old_data, &an.new_data);
            if post != 0 {
                failed = true;
                out.extend_from_slice(name.as_slice());
                out.extend_from_slice(
                    format!(
                        ":{post}: {}.\n",
                        diff_files::whitespace_error_string(diff_color::WS_BLANK_AT_EOF)
                    )
                    .as_bytes(),
                );
            }
        }
    }
    Ok(failed)
}

/// `diff_unmodified_pair()` (diff.c:6505).
fn diff_unmodified_pair(p: &Pair) -> bool {
    if p.old_valid() != p.new_valid() {
        return false;
    }
    if !p.old_valid() && !p.new_valid() {
        return true;
    }
    if p.old_mode != p.new_mode || p.old_path != p.new_path {
        return false;
    }
    p.old_id == p.new_id
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
    tc: TextconvRef<'_, '_>,
) -> std::result::Result<Analysis, ExitCode> {
    if is_gitlink(p) {
        let (add, del) = gitlink_counts(p);
        return Ok(Analysis {
            add,
            del,
            binary: false,
            old_data: Vec::new(),
            new_data: Vec::new(),
            converted: None,
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
    // gitoxide hands back only the *size* of content it classified as binary, so those
    // buffers are read from the object database instead. That is what
    // `diff_populate_filespec()` does for an oid-valid filespec in any case: the
    // working-tree conversion it can apply never runs on a path that came from a tree.
    // Without them `--stat` cannot print `Bin <a> -> <b> bytes`, `--dirstat` cannot
    // score the damage, and `-a` has nothing to diff.
    let old_data = match prep.old.data.as_slice() {
        Some(d) => d.to_vec(),
        None => read_blob(repo, p.old_id, p.old_valid())?,
    };
    let new_data = match prep.new.data.as_slice() {
        Some(d) => d.to_vec(),
        None => read_blob(repo, p.new_id, p.new_valid())?,
    };
    // Whether gitoxide classified each side on its own as binary. `as_slice()` is `None`
    // exactly for the content it refused to hand back, so this is `diff_filespec_is_binary()`
    // per side — which is what the `--textconv` binary test needs, since a converted side
    // is never binary no matter what the raw blob looked like.
    let old_is_binary = p.old_valid() && prep.old.data.as_slice().is_none();
    let new_is_binary = p.new_valid() && prep.new.data.as_slice().is_none();

    // `builtin_diff()`'s `--textconv` path: resolve each side's `diff.<driver>.textconv`
    // program and diff its stdout instead of the blob. A side that converted is never
    // treated as binary here — git's test is
    // `(!textconv_one && is_binary(one)) || (!textconv_two && is_binary(two))` — and a
    // program that fails is `die("unable to read files to diff")`.
    //
    // This is a *patch-only* substitution. `builtin_diffstat()` resolves the drivers
    // too but then fills its buffers with `fill_mmfile()`, not `fill_textconv()`, so
    // `--stat`/`--numstat`/`--shortstat` keep counting the raw blobs and a raw-binary
    // path still reports `Bin <a> -> <b> bytes` even while its patch shows hunks.
    // `builtin_checkdiff()` and `diffcore_pickaxe()` never call `fill_textconv()` at
    // all, so they read the raw blobs as well.
    let converted = match tc {
        Some(cell) => {
            let mut conv = cell.borrow_mut();
            let one = convert_side(&mut conv, p.old_path.as_bstr(), &old_data, p.old_valid())?;
            let two = convert_side(&mut conv, p.new_path.as_bstr(), &new_data, p.new_valid())?;
            match (&one, &two) {
                // Neither path names a driver, so this is an ordinary diff.
                (None, None) => None,
                // Only a side git left unconverted can still veto with binary, and
                // `-a`/`--text` skips that test outright as it does in `builtin_diff()`.
                _ if !opts.text
                    && ((one.is_none() && old_is_binary)
                        || (two.is_none() && new_is_binary)) =>
                {
                    None
                }
                _ => Some((
                    one.unwrap_or_else(|| old_data.clone()),
                    two.unwrap_or_else(|| new_data.clone()),
                )),
            }
        }
        None => None,
    };

    let raw = match prep.operation {
        Operation::SourceOrDestinationIsBinary => {
            // `-a`/`--text` forces a textual diff even for content gitoxide flags as
            // binary. The `binary` flag itself stays set so `--stat`/`--numstat` still
            // report the file as binary — git's diffstat ignores the TEXT option, so
            // stock `git diff -a --numstat` prints `-\t-` even though `-a` makes the
            // patch textual.
            let hunks = if opts.text {
                text_analysis(
                    &old_data,
                    &new_data,
                    opts,
                    opts.algo.unwrap_or(gix::diff::blob::Algorithm::Myers),
                )?
                .2
            } else {
                Vec::new()
            };
            Ok(Analysis {
                add: 0,
                del: 0,
                binary: true,
                old_data,
                new_data,
                converted: None,
                hunks,
            })
        }
        Operation::ExternalCommand { .. } => Err(fatal("external diff drivers are not supported")),
        Operation::InternalDiff { algorithm } => {
            // `builtin_diff()`: a `-B` rewrite that stayed a modification never runs
            // xdiff. `emit_rewrite_diff()` replaces the whole file in one hunk, and the
            // diffstat counts the same way (`count_lines()` on each side).
            if p.kind() == b'M' && p.score() != 0 {
                let hunks = emit_rewrite_diff(&old_data, &new_data, opts);
                return Ok(Analysis {
                    add: count_lines(&new_data),
                    del: count_lines(&old_data),
                    binary: false,
                    old_data,
                    new_data,
                    converted: None,
                    hunks,
                });
            }
            // `--diff-algorithm`/`--minimal`/`--histogram` override gitoxide's pick.
            let (add, del, hunks) =
                text_analysis(&old_data, &new_data, opts, opts.algo.unwrap_or(algorithm))?;
            Ok(Analysis {
                add,
                del,
                binary: false,
                old_data,
                new_data,
                converted: None,
                hunks,
            })
        }
    };
    let mut raw = raw?;

    // Re-render the patch body from the textconv output. Everything else the analysis
    // carries — the counts, the binary verdict, the raw buffers the stat, pickaxe,
    // `--check` and `--dirstat` paths read — stays exactly as the unconverted diff
    // produced it, because none of those code paths calls `fill_textconv()` in git.
    if let Some((old_txt, new_txt)) = converted {
        let algorithm = opts.algo.unwrap_or(gix::diff::blob::Algorithm::Myers);
        raw.hunks = if p.kind() == b'M' && p.score() != 0 {
            emit_rewrite_diff(&old_txt, &new_txt, opts)
        } else {
            text_analysis(&old_txt, &new_txt, opts, algorithm)?.2
        };
        raw.converted = Some((old_txt, new_txt));
    }
    Ok(raw)
}

/// `get_textconv()` + `fill_textconv()` for one side of a pair: `Some(text)` when the
/// path names a driver that configures `diff.<name>.textconv` and its program ran,
/// `None` when no driver applies — which is `fill_textconv()`'s "hand back the blob
/// unchanged" case. A side that does not exist has nothing to convert.
///
/// A program that could not be started or exited non-zero is `run_textconv()`'s NULL
/// return, which `fill_textconv()` turns into `die(_("unable to read files to diff"))`.
fn convert_side(
    conv: &mut super::cat_file::Textconv<'_>,
    path: &gix::bstr::BStr,
    data: &[u8],
    valid: bool,
) -> std::result::Result<Option<Vec<u8>>, ExitCode> {
    if !valid {
        return Ok(None);
    }
    match conv.convert(path, data) {
        Ok(super::cat_file::Converted::Text(t)) => Ok(Some(t)),
        Ok(super::cat_file::Converted::NoDriver) => Ok(None),
        Ok(super::cat_file::Converted::Failed) | Err(_) => {
            Err(fatal("unable to read files to diff"))
        }
    }
}

/// `diff_populate_filespec()` for a filespec whose object id is known: the blob's
/// bytes verbatim, or nothing at all when the side does not exist.
fn read_blob(
    repo: &gix::Repository,
    id: ObjectId,
    valid: bool,
) -> std::result::Result<Vec<u8>, ExitCode> {
    if !valid || id.is_null() {
        return Ok(Vec::new());
    }
    match repo.find_object(id) {
        Ok(o) => Ok(o.data.clone()),
        Err(_) => Err(fatal(&format!("unable to read {}", id.to_hex()))),
    }
}

/// `count_lines()`: lines in a buffer, counting an unterminated final line.
fn count_lines(data: &[u8]) -> u32 {
    if data.is_empty() {
        return 0;
    }
    let mut count = data.iter().filter(|&&b| b == b'\n').count() as u32;
    if data[data.len() - 1] != b'\n' {
        count += 1; // no trailing newline
    }
    count
}

/// `add_line_count()`: the range half of a rewrite's `@@` line.
fn rewrite_line_count(count: u32) -> String {
    match count {
        0 => "0,0".to_string(),
        1 => "1".to_string(),
        n => format!("1,{n}"),
    }
}

/// `emit_rewrite_diff()`: a `-B` rewrite's body — one hunk spanning both whole files,
/// every old line removed and every new line added, with no context. `-D` prints the
/// pre-image range as `?,?` and drops the removed lines entirely.
fn emit_rewrite_diff(old_data: &[u8], new_data: &[u8], opts: &Opts) -> Vec<u8> {
    let lc_a = count_lines(old_data);
    let lc_b = count_lines(new_data);
    let mut out = Vec::new();
    out.extend_from_slice(b"@@ -");
    if opts.irreversible_delete {
        out.extend_from_slice(b"?,?");
    } else {
        out.extend_from_slice(rewrite_line_count(lc_a).as_bytes());
    }
    out.extend_from_slice(b" +");
    out.extend_from_slice(rewrite_line_count(lc_b).as_bytes());
    out.extend_from_slice(b" @@\n");
    if lc_a != 0 && !opts.irreversible_delete {
        emit_rewrite_lines(&mut out, opts.ind_old, old_data);
    }
    if lc_b != 0 {
        emit_rewrite_lines(&mut out, opts.ind_new, new_data);
    }
    out
}

/// `emit_rewrite_lines()`: every line of `data` prefixed by `prefix`, with git's
/// incomplete-last-line marker when the buffer does not end in a newline.
fn emit_rewrite_lines(out: &mut Vec<u8>, prefix: u8, data: &[u8]) {
    let mut rest = data;
    let mut ended_with_newline = false;
    while !rest.is_empty() {
        let (line, tail) = match rest.iter().position(|&b| b == b'\n') {
            Some(i) => {
                ended_with_newline = true;
                (&rest[..=i], &rest[i + 1..])
            }
            None => {
                ended_with_newline = false;
                (rest, &rest[rest.len()..])
            }
        };
        out.push(prefix);
        out.extend_from_slice(line);
        if !ended_with_newline {
            out.push(b'\n');
        }
        rest = tail;
    }
    if !ended_with_newline {
        out.extend_from_slice(b"\\ No newline at end of file\n");
    }
}

/// `diff.indentHeuristic`, git's `diff_indent_heuristic` (default on).
///
/// `git_diff_heuristic_config()` is called from `git_diff_basic_config()`, not from the
/// UI config, so the plumbing commands read it as well as `git diff`.
pub(crate) fn indent_heuristic_default(repo: &gix::Repository) -> bool {
    repo.config_snapshot()
        .boolean("diff.indentHeuristic")
        .unwrap_or(true)
}

/// `xdl_do_diff()` followed by `xdl_change_compact()`: build the change script from the
/// interned (and, under `-w`/`-b`/`--ignore-space-at-eol`/`--ignore-cr-at-eol`,
/// whitespace-normalized) tokens, then slide each group with the indent heuristic
/// scoring the **original** records.
///
/// git keeps the two apart. `xdl_recmatch()` honours `XDF_IGNORE_WHITESPACE*` when it
/// decides whether two records are equal, but `xdl_change_compact()`'s `get_indent()`
/// reads `xdf->recs[i]->ptr` — the unmodified line as it appears in the file. Handing
/// `postprocess_lines()` the normalized interner instead measures stripped lines, so
/// under `-w` every record scores as unindented, `group_slide_down()` finds no reason
/// to prefer one landing spot over another, and the hunk stops where the raw edit
/// script left it. That is the whole `-w` divergence.
///
/// `postprocess_with` runs the pre-image pass with `tokens = input.before` and the
/// post-image pass with `tokens = input.after`, handing the slice straight through to
/// the heuristic, so the slice's address says which side is being scored. The scorer
/// itself only ever reaches `tokens[i]` through the indent closure, so a synthetic
/// identity sequence lets the crate's own `IndentHeuristic` score *positions* while
/// the real tokens keep driving the equality tests in `slide_up`/`slide_down`.
pub(crate) fn compute_compacted(
    algorithm: gix::diff::blob::Algorithm,
    input: &InternedInput<Vec<u8>>,
    before: &[&[u8]],
    after: &[&[u8]],
    indent_heuristic: bool,
) -> gix::diff::blob::Diff {
    use gix::diff::blob::{IndentHeuristic, IndentLevel, SliderHeuristic, Token};

    let mut diff = gix::diff::blob::Diff::compute(algorithm, input);
    if !indent_heuristic {
        diff.postprocess_no_heuristic(input);
        return diff;
    }
    // `get_indent()`: spaces count one, tabs round up to the next multiple of eight,
    // an all-whitespace line is blank.
    let indents = |lines: &[&[u8]]| -> Vec<IndentLevel> {
        lines
            .iter()
            .map(|l| IndentLevel::for_ascii_line(l.iter().copied(), 8))
            .collect()
    };
    let (indent_before, indent_after) = (indents(before), indents(after));
    let before_ptr = input.before.as_ptr();
    let synth: Vec<Token> = (0..input.before.len().max(input.after.len()) as u32)
        .map(Token)
        .collect();
    diff.postprocess_with(
        &input.before,
        &input.after,
        |tokens: &[Token], hunk: std::ops::Range<u32>, earliest_end: u32| {
            let side = if std::ptr::eq(tokens.as_ptr(), before_ptr) {
                &indent_before
            } else {
                &indent_after
            };
            IndentHeuristic::new(|t: Token| side[t.0 as usize]).best_slider_end(
                &synth[..tokens.len()],
                hunk,
                earliest_end,
            )
        },
    );
    diff
}

/// The text half of [`analyze`]: intern the two blobs (under the active whitespace
/// rules), diff them with `algorithm`, and return `(additions, removals, rendered
/// hunks)`. Shared by the normal `InternalDiff` path and the `-a`/`--text` path that
/// forces a textual diff on content gitoxide classifies as binary.
fn text_analysis(
    old_data: &[u8],
    new_data: &[u8],
    opts: &Opts,
    algorithm: gix::diff::blob::Algorithm,
) -> std::result::Result<(u32, u32, Vec<u8>), ExitCode> {
    let before: Vec<&[u8]> = byte_lines(old_data);
    let after: Vec<&[u8]> = byte_lines(new_data);
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| normalize(l, opts.ws)));
    input.update_after(after.iter().map(|l| normalize(l, opts.ws)));

    // `--indent-heuristic` (git's default) runs the slider post-processing over the
    // original records; `--no-indent-heuristic` runs the plain one, matching
    // `XDF_INDENT_HEURISTIC` off.
    let diff = compute_compacted(algorithm, &input, &before, &after, opts.indent_heuristic);
    // The change script, in `xdchange_t` shape, with the `ignore` bit that
    // `xdl_mark_ignorable_lines` (`--ignore-blank-lines`) and `xdl_mark_ignorable_regex`
    // (`-I<re>`) set on a change whose every pre- and post-image record is ignorable.
    // Both markers assign (rather than or into) `xch->ignore`, and the regex pass runs
    // second, so `-I` has the final say whenever it is present.
    let changes: Vec<Change> = diff
        .hunks()
        .map(|h| {
            let (i1, chg1) = (h.before.start as usize, h.before.len());
            let (i2, chg2) = (h.after.start as usize, h.after.len());
            let all = |pred: &dyn Fn(&[u8]) -> bool| {
                before[i1..i1 + chg1].iter().all(|l| pred(l))
                    && after[i2..i2 + chg2].iter().all(|l| pred(l))
            };
            let ignore = if !opts.ignore_lines.is_empty() {
                all(&|l| matches_any(&opts.ignore_lines, l))
            } else if opts.ignore_blank_lines {
                all(&|l| is_blank_rec(l, opts.ws))
            } else {
                false
            };
            Change { i1, chg1, i2, chg2, ignore }
        })
        .collect();

    // Hunks render the *original* line bytes: the tokens the differ compared may be
    // whitespace-normalized, so the emitter indexes `before`/`after` directly.
    let (add, del, hunks) = emit_unified(
        &before,
        &after,
        &changes,
        &EmitGeometry {
            ctx: opts.ctx as usize,
            inter_hunk_ctx: opts.inter_hunk_ctx,
            func_context: opts.func_context,
        },
    );
    Ok((add, del, hunks))
}

/// One `xdchange_t`: `chg1` pre-image records at `i1` replaced by `chg2` post-image
/// records at `i2`, plus xdiff's `ignore` bit.
#[derive(Clone, Copy)]
pub(crate) struct Change {
    pub(crate) i1: usize,
    pub(crate) chg1: usize,
    pub(crate) i2: usize,
    pub(crate) chg2: usize,
    /// `xdchange_t::ignore`: a change `-I`/`--ignore-blank-lines` marked as not
    /// worth a hunk of its own, which still prints when a real change pulls it in.
    pub(crate) ignore: bool,
}

/// `xdl_blankline`: with no whitespace option in force a record is blank only when it is
/// empty or a bare terminator; once any `XDF_WHITESPACE_FLAGS` bit is set, any record made
/// entirely of whitespace counts.
fn is_blank_rec(line: &[u8], ws: Whitespace) -> bool {
    if ws == Whitespace::Keep {
        return line.len() <= 1;
    }
    line.iter().all(|b| b.is_ascii_whitespace())
}

/// `is_empty_rec`: the record is nothing but leading whitespace.
fn is_empty_rec(line: &[u8]) -> bool {
    line.iter().all(|b| b.is_ascii_whitespace())
}

/// `get_func_line`: walk the pre-image from `start` toward `limit` (exclusive, in either
/// direction) and return the first record `def_ff` accepts as a function line, or `-1`.
fn get_func_line(before: &[&[u8]], start: isize, limit: isize) -> isize {
    let step: isize = if start > limit { -1 } else { 1 };
    let mut l = start;
    while l != limit && l >= 0 && (l as usize) < before.len() {
        if def_ff(before[l as usize]).is_some() {
            return l;
        }
        l += step;
    }
    -1
}

/// `xdl_get_hunk`: starting at change `cursor`, drop the ignorable changes that stand too
/// far from a real one and return the `(first, last)` change indices of the hunk, or
/// `None` once only droppable changes remain.
fn get_hunk(changes: &[Change], cursor: usize, ctxlen: usize, interhunk: usize) -> Option<(usize, usize)> {
    let n = changes.len();
    let max_common = 2 * ctxlen + interhunk;
    let max_ignorable = ctxlen;
    let gap = |a: usize, b: usize| changes[b].i1.saturating_sub(changes[a].i1 + changes[a].chg1);

    // Remove ignorable changes that are too far before other changes.
    let mut scr = cursor;
    let mut chp = cursor;
    while chp < n && changes[chp].ignore {
        let ch = chp + 1;
        if ch >= n || gap(chp, ch) >= max_ignorable {
            scr = ch;
        }
        chp = ch;
    }
    if scr >= n {
        return None;
    }

    let mut lxch = scr;
    let mut ignored = 0usize;
    let mut chp = scr;
    let mut ch = scr + 1;
    while ch < n {
        let distance = gap(chp, ch);
        if distance > max_common {
            break;
        }
        if distance < max_ignorable && (!changes[ch].ignore || lxch == chp) {
            lxch = ch;
            ignored = 0;
        } else if distance < max_ignorable && changes[ch].ignore {
            ignored += changes[ch].chg2;
        } else if lxch != chp
            && changes[ch].i1 + ignored > changes[lxch].i1 + changes[lxch].chg1 + max_common
        {
            break;
        } else if !changes[ch].ignore {
            lxch = ch;
            ignored = 0;
        } else {
            ignored += changes[ch].chg2;
        }
        chp = ch;
        ch += 1;
    }
    Some((scr, lxch))
}

/// The three knobs `xdl_emit_diff` reads out of `xdemitconf_t` to decide hunk
/// geometry, split out so callers that keep their options elsewhere — `git diff`'s
/// own patch path, for one — can drive the same emitter.
pub(crate) struct EmitGeometry {
    /// `--unified=<n>`.
    pub(crate) ctx: usize,
    /// `--inter-hunk-context=<n>`.
    pub(crate) inter_hunk_ctx: usize,
    /// `-W` / `--function-context`, i.e. `XDL_EMIT_FUNCCONTEXT`.
    pub(crate) func_context: bool,
}

/// `xdl_emit_diff`: turn the change script into unified-diff text and count the emitted
/// `+`/`-` records, which is what `diffstat_consume` counts too.
///
/// Reproduces xdiff's hunk geometry: `--unified=<n>` context, `--inter-hunk-context=<n>`
/// merging (via [`get_hunk`]), `XDL_EMIT_FUNCNAMES` hunk-header function names and, under
/// `-W`, `XDL_EMIT_FUNCCONTEXT`'s expansion of both hunk ends to enclosing-function
/// boundaries.
pub(crate) fn emit_unified(
    before: &[&[u8]],
    after: &[&[u8]],
    changes: &[Change],
    geom: &EmitGeometry,
) -> (u32, u32, Vec<u8>) {
    let (nrec1, nrec2) = (before.len(), after.len());
    let ctxlen = geom.ctx;
    let mut buf: Vec<u8> = Vec::new();
    let (mut add, mut del) = (0u32, 0u32);
    let mut funclineprev: isize = -1;
    let mut func_name: Vec<u8> = Vec::new();
    let mut cursor = 0usize;

    // Append one record, tagging a final line that lacks its terminator.
    let emit = |buf: &mut Vec<u8>, marker: u8, content: &[u8]| {
        buf.push(marker);
        buf.extend_from_slice(content);
        if content.last() != Some(&b'\n') {
            buf.push(b'\n');
            buf.extend_from_slice(b"\\ No newline at end of file\n");
        }
    };

    while cursor < changes.len() {
        // `xchp` is the queue position *before* `xdl_get_hunk` skips ignorable changes;
        // the `-W` pre-context walk may have to reach back to it.
        let mut xchp = cursor;
        let Some((mut first, mut last)) = get_hunk(changes, cursor, ctxlen, geom.inter_hunk_ctx)
        else {
            break;
        };

        // `pre_context_calculation`, re-entered when growing the context upwards pulled an
        // ignored change back into view.
        let (s1, s2) = loop {
            let mut s1 = changes[first].i1.saturating_sub(ctxlen);
            let mut s2 = changes[first].i2.saturating_sub(ctxlen);
            if !geom.func_context {
                break (s1, s2);
            }
            // `XDL_EMIT_FUNCCONTEXT`: grow the pre-context back to the enclosing function.
            let mut i1 = changes[first].i1 as isize;
            if i1 >= nrec1 as isize {
                // An appended chunk needs no extra context if it added a whole function.
                let mut i2 = changes[first].i2;
                while i2 < nrec2 && def_ff(after[i2]).is_none() {
                    i2 += 1;
                }
                if i2 < nrec2 {
                    break (s1, s2); // goto post_context_calculation
                }
                i1 = nrec1 as isize - 1;
            }
            let mut fs1 = get_func_line(before, i1, -1);
            while fs1 > 0
                && !is_empty_rec(before[(fs1 - 1) as usize])
                && def_ff(before[(fs1 - 1) as usize]).is_none()
            {
                fs1 -= 1;
            }
            let fs1 = fs1.max(0) as usize;
            if fs1 < s1 {
                s2 = s2.saturating_sub(s1 - fs1);
                s1 = fs1;
                while xchp != first
                    && changes[xchp].i1 + changes[xchp].chg1 <= s1
                    && changes[xchp].i2 + changes[xchp].chg2 <= s2
                {
                    xchp += 1;
                }
                if xchp != first {
                    first = xchp;
                    continue;
                }
            }
            break (s1, s2);
        };

        // `post_context_calculation`, re-entered whenever `-W` swallows the next change.
        let (e1, e2) = loop {
            let end1 = changes[last].i1 + changes[last].chg1;
            let end2 = changes[last].i2 + changes[last].chg2;
            let lctx = ctxlen.min(nrec1 - end1).min(nrec2 - end2);
            let (mut e1, mut e2) = (end1 + lctx, end2 + lctx);

            if geom.func_context {
                let mut fe1 = get_func_line(before, end1 as isize, nrec1 as isize);
                while fe1 > 0 && is_empty_rec(before[(fe1 - 1) as usize]) {
                    fe1 -= 1;
                }
                let fe1 = if fe1 < 0 { nrec1 } else { fe1 as usize };
                if fe1 > e1 {
                    e2 = (e2 + (fe1 - e1)).min(nrec2);
                    e1 = fe1;
                }
                // Overlap with the next change? Then fold it into this hunk.
                if last + 1 < changes.len() {
                    let l = changes[last + 1].i1.min(nrec1.saturating_sub(1));
                    if l <= e1 + ctxlen || get_func_line(before, l as isize, e1 as isize) < 0 {
                        last += 1;
                        continue;
                    }
                }
            }
            break (e1, e2);
        };

        // Hunk header, with `XDL_EMIT_FUNCNAMES`' enclosing-function name.
        buf.extend_from_slice(b"@@ -");
        buf.extend_from_slice(fmt_range(s1 as u32 + 1, (e1 - s1) as u32).as_bytes());
        buf.extend_from_slice(b" +");
        buf.extend_from_slice(fmt_range(s2 as u32 + 1, (e2 - s2) as u32).as_bytes());
        buf.extend_from_slice(b" @@");
        // `func_line` lives across hunks in `xdl_emit_diff`: a failed search leaves the
        // previously found name in place, because the search only spans back to the last
        // hunk's origin and finding nothing means the enclosing function is unchanged.
        let fl = get_func_line(before, s1 as isize - 1, funclineprev);
        funclineprev = s1 as isize - 1;
        if fl >= 0 {
            func_name = def_ff(before[fl as usize]).unwrap_or_default().to_vec();
        }
        if !func_name.is_empty() {
            buf.push(b' ');
            buf.extend_from_slice(&func_name);
        }
        buf.push(b'\n');

        // Pre-context comes from the post-image, like `xdl_emit_diff`.
        let mut c2 = s2;
        while c2 < changes[first].i2 {
            emit(&mut buf, b' ', after[c2]);
            c2 += 1;
        }
        let mut c1 = changes[first].i1;
        for ch in &changes[first..=last] {
            while c1 < ch.i1 && c2 < ch.i2 {
                emit(&mut buf, b' ', after[c2]);
                c1 += 1;
                c2 += 1;
            }
            for l in ch.i1..ch.i1 + ch.chg1 {
                emit(&mut buf, b'-', before[l]);
                del += 1;
            }
            for l in ch.i2..ch.i2 + ch.chg2 {
                emit(&mut buf, b'+', after[l]);
                add += 1;
            }
            c1 = ch.i1 + ch.chg1;
            c2 = ch.i2 + ch.chg2;
        }
        while c2 < e2 {
            emit(&mut buf, b' ', after[c2]);
            c2 += 1;
        }

        cursor = last + 1;
    }
    (add, del, buf)
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
    tc: TextconvRef<'_, '_>,
) -> std::result::Result<bool, ExitCode> {
    if let PickaxeKind::ObjFind(ids) = &px.kind {
        return Ok((p.old_valid() && ids.contains(&p.old_id))
            || (p.new_valid() && ids.contains(&p.new_id)));
    }
    if !p.old_valid() && !p.new_valid() {
        return Ok(false);
    }
    let an = analyze(repo, cache, p, opts, tc)?;
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

/// `show_numstat()` with git's `-z` field layout. `lp` is the `--line-prefix` string,
/// written once per record because a rename record embeds NULs between its own fields.
fn render_numstat(out: &mut Vec<u8>, files: &[StatFile], lp: &[u8]) {
    for f in files {
        out.extend_from_slice(lp);
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
fn render_stat(
    out: &mut Vec<u8>,
    files: &[StatFile],
    sw: &StatWidths,
    colors: &diff_color::DiffColors,
) {
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
            // `show_stats()` paints the two byte counts with the old/new colors.
            out.push(b' ');
            diff_color::paint(out, colors, diff_color::DiffSlot::Old, deleted.to_string().as_bytes());
            out.extend_from_slice(b" -> ");
            diff_color::paint(out, colors, diff_color::DiffSlot::New, added.to_string().as_bytes());
            out.extend_from_slice(b" bytes\n");
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
        // `show_graph()`: each run carries its own color, and emits nothing when empty.
        if add > 0 {
            diff_color::paint(out, colors, diff_color::DiffSlot::New, &b"+".repeat(add as usize));
        }
        if del > 0 {
            diff_color::paint(out, colors, diff_color::DiffSlot::Old, &b"-".repeat(del as usize));
        }
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
                // A `-B` rewrite carries a score and prints its own summary line.
                if p.score() != 0 {
                    return false;
                }
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
        _ => {
            // `diff_summary()`'s default arm: a `-B` rewrite that stayed a modification
            // announces itself and suppresses the name on the mode-change line.
            let score = p.score();
            if score != 0 {
                out.extend_from_slice(b" rewrite ");
                out.extend_from_slice(&diff_files::quoted_name(&p.new_path));
                out.extend_from_slice(format!(" ({score}%)\n").as_bytes());
            }
            summary_mode_change(out, p, score == 0);
        }
    }
}

/// `show_file_mode_name()`.
fn summary_mode_name(out: &mut Vec<u8>, verb: &str, mode: u32, path: &BString) {
    if mode != 0 {
        out.extend_from_slice(format!(" {verb} mode {:06o} ", mode).as_bytes());
    } else {
        out.extend_from_slice(format!(" {verb} ").as_bytes());
    }
    out.extend_from_slice(&diff_files::quoted_name(path));
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
            out.extend_from_slice(&diff_files::quoted_name(&p.new_path));
        }
        out.push(b'\n');
    }
}

/// `pprint_rename()`: compress the common leading directory and trailing suffix of a
/// rename/copy into `pfx{old-mid => new-mid}sfx`.
pub(crate) fn pprint_rename(a: &[u8], b: &[u8]) -> Vec<u8> {
    // A path that needs C-quoting skips the factoring entirely — git cannot splice
    // braces into a quoted string, so it prints `"old" => "new"` whole.
    if diff_files::needs_c_quote(a) || diff_files::needs_c_quote(b) {
        let mut out = diff_files::quoted_name_bytes(a);
        out.extend_from_slice(b" => ");
        out.extend_from_slice(&diff_files::quoted_name_bytes(b));
        return out;
    }
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
///
/// The bytes land in `out` uncolored and each section's whitespace state is pushed to
/// `files`; the caller re-emits the whole patch at once, which is what
/// `diff_flush_patch_all_file_pairs()` does and what lets `--color-moved` see a block
/// that moved from one file to another.
#[allow(clippy::too_many_arguments)]
fn render_patch(
    sink: &mut PatchSink<'_>,
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    p: &Pair,
    opts: &Opts,
    base_abbrev: usize,
    ws_rule: u32,
    tc: TextconvRef<'_, '_>,
    ext: Option<&ExtCtx<'_, '_>>,
    total: usize,
    found_changes: &mut bool,
) -> std::result::Result<(), ExitCode> {
    // `diff_flush_patch()` drops a pair whose two sides are identical in every
    // respect before `run_diff()` ever sees it.
    if diff_unmodified_pair(p) {
        return Ok(());
    }
    // `run_diff()` splits a file/symlink type change into a deletion followed by a
    // creation, but only `if (!pgm)` — with a `GIT_EXTERNAL_DIFF` in play the pair
    // goes to the driver whole. A driver reached through the path's `diff`
    // attribute does not suppress the split: `run_diff()` tests the environment
    // program, and each half re-resolves the attribute afterwards.
    let split = p.type_changed() && ext.is_none_or(|e| e.env.is_none());
    let steps: Vec<Pair> = if split {
        vec![as_deletion(p), as_creation(p)]
    } else {
        vec![p.clone()]
    };
    for step in &steps {
        if let Some(ctx) = ext {
            let pgm =
                external_for_path(repo, ctx.drivers, step.old_path.as_bstr(), ctx.env.as_ref())?;
            if let Some(pgm) = pgm {
                let run = run_external_diff(
                    &pgm, repo, ctx, step, opts, base_abbrev, total, true,
                )?;
                *found_changes |= run.found_changes;
                sink.splice(&run.stdout);
                if let Some(msg) = run.died {
                    return Err(fatal(&msg));
                }
                continue;
            }
        }
        // `builtin_diff()`'s first branch: with `--submodule=log`, a pair whose two
        // sides are each either absent or a gitlink is rendered from the submodule's
        // own history instead of as a blob diff, and always counts as a change.
        if opts.submodule_format == SubmoduleFormat::Log
            && (step.old_mode == 0 || is_gitlink_mode(step.old_mode))
            && (step.new_mode == 0 || is_gitlink_mode(step.new_mode))
        {
            let mut lines = Vec::new();
            show_submodule_diff_summary(
                &mut lines,
                repo,
                &step.old_path,
                &step.old_id,
                &step.new_id,
                // A `diff-pairs` batch is read from stdin, so no side of it was
                // ever a worktree: `two->dirty_submodule` is always zero here.
                0,
                base_abbrev,
                sink.colors,
            );
            sink.prefixed(&lines);
            *found_changes = true;
            continue;
        }
        let an = analyze(repo, cache, step, opts, tc)?;
        let before = sink.plain.len();
        emit_patch(&mut sink.plain, repo, step, &an, opts, base_abbrev);
        // A pair that renders nothing at all contributes no `diff --git` header, so
        // it must not consume a slot in the per-file state either.
        if sink.plain.len() != before {
            // Every `builtin_diff()` arm that writes a header also sets
            // `o->found_changes`, so having written anything is the answer.
            *found_changes = true;
            // `builtin_diff()` runs `check_blank_at_eof()` on `mf1`/`mf2` — the buffers
            // it just filled — so under `--textconv` the converted text is what decides
            // whether a trailing blank line is newly added.
            let (bof_old, bof_new) = match &an.converted {
                Some((o, n)) => (o.as_slice(), n.as_slice()),
                None => (an.old_data.as_slice(), an.new_data.as_slice()),
            };
            sink.files.push(diff_color::FilePaint {
                ws_rule,
                blank_at_eof: diff_color::check_blank_at_eof(bof_old, bof_new),
            });
        }
    }
    // `run_diff_cmd()` counts a copy or a rename as a change whatever the bodies did.
    if matches!(p.kind(), b'C' | b'R') {
        *found_changes = true;
    }
    Ok(())
}

/// `diff_flush_patch_quietly()` (diff.c:6566): render the pair with `o->file` nulled
/// and report only whether anything was found. Nothing is written, so an external
/// driver either runs with its stdout on `/dev/null` (when its exit status is
/// trusted) or is skipped outright.
#[allow(clippy::too_many_arguments)]
fn pair_found_changes(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    p: &Pair,
    opts: &Opts,
    base_abbrev: usize,
    tc: TextconvRef<'_, '_>,
    ext: Option<&ExtCtx<'_, '_>>,
    total: usize,
) -> std::result::Result<bool, ExitCode> {
    if diff_unmodified_pair(p) {
        return Ok(false);
    }
    let split = p.type_changed() && ext.is_none_or(|e| e.env.is_none());
    let steps: Vec<Pair> = if split {
        vec![as_deletion(p), as_creation(p)]
    } else {
        vec![p.clone()]
    };
    let mut found = matches!(p.kind(), b'C' | b'R');
    for step in &steps {
        if let Some(ctx) = ext {
            let pgm =
                external_for_path(repo, ctx.drivers, step.old_path.as_bstr(), ctx.env.as_ref())?;
            if let Some(pgm) = pgm {
                let run = run_external_diff(
                    &pgm, repo, ctx, step, opts, base_abbrev, total, false,
                )?;
                found |= run.found_changes;
                if let Some(msg) = run.died {
                    return Err(fatal(&msg));
                }
                continue;
            }
        }
        if opts.submodule_format == SubmoduleFormat::Log
            && (step.old_mode == 0 || is_gitlink_mode(step.old_mode))
            && (step.new_mode == 0 || is_gitlink_mode(step.new_mode))
        {
            found = true;
            continue;
        }
        let an = analyze(repo, cache, step, opts, tc)?;
        let mut scratch = Vec::new();
        emit_patch(&mut scratch, repo, step, &an, opts, base_abbrev);
        found |= !scratch.is_empty();
    }
    Ok(found)
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
    } else if kind == b'M' && p.score() != 0 {
        // `fill_metainfo()`'s MODIFIED arm: a `-B` rewrite reports how *dis*similar
        // the two sides are instead of a rename header.
        out.extend_from_slice(format!("dissimilarity index {}%\n", p.score()).as_bytes());
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

    // `-D`: `builtin_diff` stops right after the header whenever the post-image label is
    // `/dev/null`, so a deletion carries no pre-image body — binary or textual.
    if opts.irreversible_delete && !p.new_valid() {
        return;
    }

    // A pair whose patch body came from `--textconv` is never rendered as binary, even
    // when the raw blobs are: `builtin_diff()` only reaches its `Binary files … differ`
    // arm for a side that has no textconv driver.
    if an.binary && !opts.text && an.converted.is_none() {
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
        write_hunks(out, &an.hunks, opts);
    }
}

/// Write the rendered hunk body, remapping each context/removed/added line's leading
/// marker to the configured `--output-indicator-*` character.
///
/// The stored `Analysis::hunks` always carry git's canonical `' '`/`'-'`/`'+'` markers
/// (so `-G` and the diffstat see the real diff); the display characters are substituted
/// only here, at emit time, exactly as `diff.c` applies `output_indicators` in
/// `emit_line_0`. Hunk-header (`@@`) and `\ No newline at end of file` lines start with
/// `'@'`/`'\'` and are left untouched. The common default triple is a straight copy.
fn write_hunks(out: &mut Vec<u8>, hunks: &[u8], opts: &Opts) {
    if opts.ind_context == b' ' && opts.ind_old == b'-' && opts.ind_new == b'+' {
        out.extend_from_slice(hunks);
        return;
    }
    for line in byte_lines(hunks) {
        let marker = match line.first() {
            Some(b' ') => Some(opts.ind_context),
            Some(b'-') => Some(opts.ind_old),
            Some(b'+') => Some(opts.ind_new),
            _ => None,
        };
        match marker {
            Some(m) => {
                out.push(m);
                out.extend_from_slice(&line[1..]);
            }
            None => out.extend_from_slice(line),
        }
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
