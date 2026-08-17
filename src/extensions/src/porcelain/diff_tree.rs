//! `git diff-tree` — compare the content and mode of blobs found via two tree objects.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). The tree-vs-tree walk is
//! implemented here rather than through `gix::Repository::diff_tree_to_tree` because
//! that helper always recurses breadth-first and always descends, while `diff-tree`
//! needs git's depth-first emission order plus `-r`/`-t` control over which tree
//! entries are reported. The entry comparison used below is the same
//! `tree-entry-comparison` gitoxide implements in
//! `src/ported/gix-diff/src/tree/function.rs` (names compared with an implicit `/`
//! appended to trees).
//!
//! ### Covered (byte-identical stdout and exit code against stock git)
//!
//! * `diff-tree <tree-ish> <tree-ish> [<path>...]` — raw output, no commit-id line
//! * `diff-tree <commit> [<path>...]` — commit vs. its first parent, prefixed by the
//!   commit id; a root commit prints nothing unless `--root` is given, and a merge
//!   prints nothing unless `-m` is given
//! * `-r`, `-t` (implies `-r`), `--root`, `-m`
//! * `-R` — swap every file pair (git's reverse diff). Applied before
//!   `--diff-filter`, so a reversed addition filters as a deletion; it swaps the raw
//!   mode/oid columns and status letter, the `--numstat` add/delete counts, the
//!   `--shortstat` totals, and turns a `--summary` create line into a delete (and
//!   vice versa). Paths keep their walk order, matching git.
//! * `--raw` (the default), `--name-only`, `--name-status`, `-s`/`--no-patch`
//! * `--numstat`, `--shortstat`, `--summary` — the file-granular stat family, forced
//!   recursive like git; numstat line counts come from the vendored imara-diff (git's
//!   default Myers algorithm) and binary blobs print `-`
//! * path quoting: every raw and name record goes through the shared `quote_c_style()`
//!   port in [`super::diff_files`], so `core.quotePath` is honoured and `"`/`\`/control
//!   bytes are escaped whatever it says. `-z` sets `DIFF_FORMAT_NO_QUOTE` and emits the
//!   path raw, as in git
//! * `--merge-base <a> <b>` — diff the single merge base of the two commits against the
//!   second commit's tree; zero/multiple bases or a non-commit operand reproduce git's
//!   fatal messages (exit 128)
//! * `-z`, `--abbrev=<n>` (parsed with git's `strtoul`: leading base-10 digits
//!   only, no error on garbage, clamped to 4..=hash length)
//! * `--no-commit-id`, `--always`
//! * `--diff-filter=<letters>`, `--exit-code`, `--quiet`
//! * literal `<path>` filters (exact entry, directory prefix, or a tree that a filter
//!   points below), before or after `--`
//! * git's argument classification: a positional is a `<tree-ish>` when it resolves as
//!   a revision, otherwise the first one and every argument after it must name a path
//!   that exists in the working tree. The three fatal paths git takes here — `bad
//!   object`, `ambiguous argument`, `no such path` — are reproduced verbatim on
//!   stderr with exit 128, and `option '<x>' must come before non-option arguments`
//!   likewise.
//! * git's parse-time value validation, which runs before revision resolution: an
//!   invalid `--color=<x>` value (anything but always/auto/never, case-blind) is a
//!   usage error (`error: option `color' expects …`, exit 129), and an invalid
//!   `--pretty`/`--format` name is fatal (`fatal: invalid --pretty format: <x>`,
//!   exit 128). A valid `--pretty`/`--format` is still a format this port cannot
//!   render and is recorded like any other unsupported option. An invalid
//!   `--expand-tabs=<n>` (not a base-10 non-negative integer) and an invalid
//!   `--ignore-submodules=<v>` (outside none/untracked/dirty/all) are both fatal
//!   (`fatal: '<n>': not a non-negative integer` / `fatal: bad --ignore-submodules
//!   argument: <v>`, exit 128). A valid `--expand-tabs` is recorded as unsupported;
//!   of the `--ignore-submodules` values only `all` is, the other three being
//!   no-ops for a tree-to-tree diff (see the option arm for the check against
//!   stock).
//! * `--merge-base` requires exactly two commits; git enforces this after resolving
//!   revisions but before the missing-`<tree-ish>` check, so any other count — zero
//!   included — is `fatal: --merge-base only works with two commits` (exit 128). The
//!   valid two-commit case is implemented via the vendored merge-base computation
//!   (`gix::Repository::merge_bases_many`).
//! * `-h` — git's usage text on stdout, exit 129; no `<tree-ish>` — the same text on
//!   stderr, exit 129
//!
//! * `--no-abbrev` — `cmd_diff_tree` starts from `opt->abbrev = 0`, so this restores
//!   the full object names the command already defaults to
//! * `--stdin` — object names read from stdin, one per line: a commit is diffed like
//!   the single-`<commit>` form (with any further ids on the line replacing its parent
//!   list), a tree needs a second tree id and is announced by a `<t1> <t2>` line, and a
//!   line that is not an object name is copied to stdout unchanged. `--stdin` together
//!   with `--merge-base` is git's fatal "cannot be used together"
//! * `-c`/`--cc` — the combined merge diff. A path reaches it only when it differs from
//!   *every* parent, and the commit-id line is printed whether or not any does
//!   (`diff_tree_combined`'s `show_log_first`). `-c` renders the combined raw format
//!   (`::<modes…> <oids…> <statuses>\t<path>`), which is also what `--name-status` and
//!   `--name-only` narrow down to
//!
//! ### Formats rendered by `diff-pairs`
//!
//! Every patch, diffstat, dirstat, whitespace and rename/rewrite option is handled by
//! handing this module's walk output to [`super::diff_pairs`] in `diff-tree -z -r --raw`
//! form — the pipeline `git diff-pairs` documents, run in-process. [`needs_pairs`] lists
//! what switches a run over; [`raw_pair_stream`] writes the stream, and `-R`,
//! `--diff-filter` and the format flags travel with it rather than being applied twice.
//! That covers `-p`/`-u`/`--patch`, `-U<n>`, `--stat`/`--compact-summary`/`--dirstat`,
//! `-w`/`-b`/`--ignore-*`, `-W`, `--inter-hunk-context=<n>`, `--line-prefix=<s>`,
//! `-M`/`-C`/`-B`/`--no-renames`, the pickaxe and `--check`.
//!
//! ### Options accepted but deliberately without effect
//!
//! [`is_ignorable`] lists options that only steer rendering this module still owns; each
//! entry there was checked against stock git in the raw, `-t`, `--name-status` and
//! commit-id-line forms before being listed.
//!
//! `--combined-all-paths` is accepted and changes nothing, which is what stock git 2.55.0
//! does for `diff-tree`: `diff-tree --combined-all-paths -c -r <merge>` and
//! `diff-tree -c -r <merge>` emit the same bytes, so the per-parent names
//! `show_raw_diff()` would print are never reached from this command.
//!
//! ### Honest limitations
//!
//! Every other option stock git accepts is recognised by [`is_known_unsupported`] and
//! recorded, not applied. Recognition is load-bearing: git validates and resolves its
//! arguments *before* it renders anything, so `diff-tree --numstat <bad-rev>` has to
//! fail on the revision exactly as git does. The recorded option is turned into a
//! terse bail at the point output would be produced, so an invocation that would print
//! the wrong bytes fails loudly instead. When git itself produces no output for the
//! invocation (an unborn root commit without `--root`, a `<tree-ish>` that is not a
//! commit, a merge without `-m` or `-c`), there are no bytes to get wrong and the exit
//! status is git's.
//!
//! Not implemented, and bailed on whenever they would matter:
//!
//! * bare `--abbrev` (no `=<n>`), whose width is git's *auto* abbreviation derived from
//!   the repository's approximate object count; the vendored crates expose no equivalent.
//! * `--cc`, the *dense* combined patch. The path set is computed exactly as for `-c`,
//!   so a merge with no combined change prints the same bytes as git; a non-empty one
//!   bails, because there is no `xdl_diff3`-style combined hunk emitter here.
//! * `-v`, `--pretty`/`--format` — these need commit-message formatting, which belongs
//!   to the `log`/`show` machinery, not the tree diff.
//! * `--relative`, `--submodule`/`--ignore-submodules`.
//! * magic (`:(...)`) and glob pathspecs.
//! * `-z` alongside a routed format: `diff-pairs` terminates its raw records the way the
//!   flag it was given asks, so the NUL form is carried through, but the stat and patch
//!   emitters it owns are line-terminated as in git.

use anyhow::{bail, Result};
use std::cmp::Ordering;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::blob::{sources, Algorithm, Diff, InternedInput};
use gix::hash::ObjectId;
use gix::objs::tree::EntryMode;

/// Stock git's `diff-tree` usage block, byte-for-byte (1755 bytes), including the
/// trailing blank line. Printed on `-h` (stdout) and when no `<tree-ish>` is given
/// (stderr); both exit 129.
const USAGE: &str = r#"usage: git diff-tree [--stdin] [-m] [-s] [-v] [--no-commit-id] [--pretty]
              [-t] [-r] [-c | --cc] [--combined-all-paths] [--root] [--merge-base]
              [<common-diff-options>] <tree-ish> [<tree-ish>] [<path>...]

  -r            diff recursively
  -c            show combined diff for merge commits
  --cc          show combined diff for merge commits removing uninteresting hunks
  --combined-all-paths
                show name of file in all parents for combined diffs
  --root        include the initial commit as diff against /dev/null

common diff options:
  -z            output diff-raw with lines terminated with NUL.
  -p            output patch format.
  -u            synonym for -p.
  --patch-with-raw
                output both a patch and the diff-raw format.
  --stat        show diffstat instead of patch.
  --numstat     show numeric diffstat instead of patch.
  --patch-with-stat
                output a patch and prepend its diffstat.
  --name-only   show only names of changed files.
  --name-status show names and status of changed files.
  --full-index  show full object name on index lines.
  --abbrev=<n>  abbreviate object names in diff-tree header and diff-raw.
  -R            swap input file pairs.
  -B            detect complete rewrites.
  -M            detect renames.
  -C            detect copies.
  --find-copies-harder
                try unchanged files as candidate for copy detection.
  -l<n>         limit rename attempts up to <n> paths.
  -O<file>      reorder diffs according to the <file>.
  -S<string>    find filepair whose only one side contains the string.
  --pickaxe-all
                show all files diff when -S is used and hit is found.
  -a  --text    treat all files as text.

"#;

/// git's `die_verify_filename()` text when the argument was the first non-revision
/// one, i.e. the one that could plausibly have been a misspelt revision.
const AMBIGUOUS_TAIL: &str = "unknown revision or path not in the working tree.\n\
                             Use '--' to separate paths from revisions, like this:\n\
                             'git <command> [<revision>...] -- [<file>...]'";

/// The `S_IFMT` mask git uses to decide whether a pair is a *type* change (`T`) or a
/// plain modification (`M`); `100644` and `100755` share a type, `120000` and
/// `160000` do not.
const IFMT: u16 = 0o170000;

/// git's `MINIMUM_ABBREV`: `--abbrev=<n>` below this is raised to it.
const MINIMUM_ABBREV: usize = 4;

/// Exit code git uses for a fatal error.
const FATAL: u8 = 128;

/// Exit code git uses for a usage error.
const USAGE_ERROR: u8 = 129;

/// How the change list should be rendered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    /// `:<omode> <nmode> <ooid> <noid> <status>\t<path>` — git's default.
    Raw,
    NameOnly,
    NameStatus,
    /// `--numstat`: `<added>\t<deleted>\t<path>` per changed blob (binary: `-\t-`).
    NumStat,
    /// `--shortstat`: the single ` N files changed, …` summary line.
    ShortStat,
    /// `--summary`: create/delete/mode-change lines (nothing for plain modifies).
    Summary,
    /// `-s`/`--no-patch`: the commit-id line only.
    NoOutput,
}

impl Format {
    /// The stat family (`--numstat`/`--shortstat`/`--summary`) always operates at file
    /// granularity: git forces recursion and never lists tree entries themselves,
    /// regardless of `-r`/`-t`.
    fn is_stat(self) -> bool {
        matches!(self, Format::NumStat | Format::ShortStat | Format::Summary)
    }
}

/// Parsed command-line options for a single `diff-tree` invocation.
#[derive(Clone)]
struct Opts {
    recurse: bool,       // -r (also implied by -t)
    show_trees: bool,    // -t: report tree entries themselves while recursing
    nul: bool,           // -z: NUL instead of TAB/LF
    root: bool,          // --root: show a parentless commit as a full creation
    merges: bool,        // -m: diff a merge against every parent
    no_commit_id: bool,  // --no-commit-id
    always: bool,        // --always: print the commit id even with no changes
    reverse: bool,       // -R: swap the two sides of every file pair
    exit_code: bool,     // --exit-code/--quiet: exit 1 when anything differs
    abbrev: usize,       // object-id width in the raw output
    filter: u32,         // --diff-filter mask, see `filter_bit`
    format: Format,
    paths: Vec<BString>, // the raw path filters (empty = whole tree)
    // The parsed pathspec set matching `paths`; `None` when there is no filter.
    // Behind an `Rc` so `Opts` stays `Clone`; the matcher itself is never mutated.
    specs: Option<std::rc::Rc<super::log::PathspecMatcher>>,
    /// `-c`/`--cc`: `rev->combine_merges`. A merge is rendered as one combined diff
    /// against every parent at once instead of being skipped.
    combine: bool,
    /// `--cc`: `rev->dense_combined_merges`, which also selects the combined *patch*
    /// format instead of the combined raw format.
    dense_combined: bool,
    /// `--combined-all-paths`: name the file in every parent, not only in the result.
    combined_all_paths: bool,
    /// When set, the file pairs are handed to `diff-pairs` with these options instead
    /// of being rendered by this module. See [`needs_pairs`].
    route: Option<Vec<String>>,
    /// `--line-prefix=<s>`, which git puts in front of the commit-id line too.
    line_prefix: Vec<u8>,
    /// `-v` / `--pretty[=<fmt>]` / `--format[=<fmt>]`: `revs->verbose_header`, which
    /// makes `show_log()` print a formatted commit header where the bare object name
    /// would otherwise go. Behind an `Rc` so `Opts` stays `Clone`, like `specs`.
    pretty: Option<std::rc::Rc<super::log::Pretty>>,
}

/// One file-level change, in the form the raw/name output needs.
///
/// `None` on a side means the entry is absent there (an addition or a deletion).
#[derive(Clone, Copy)]
struct Side {
    mode: EntryMode,
    id: ObjectId,
}

#[derive(Clone)]
struct Change {
    old: Option<Side>,
    new: Option<Side>,
    path: BString,
}

/// `git diff-tree` — see the module documentation for the covered surface.
pub fn diff_tree(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("diff-tree") => &args[1..],
        _ => args,
    };

    // `show_usage_if_asked(argc, argv, diff_tree_usage)` (builtin/diff-tree.c:125)
    // answers a LONE `-h` or `--help-all` on stdout, before the repository is
    // discovered. Either spelling alongside anything else is not a help request:
    // it falls through to the ordinary scan, where `usage()` reports it on
    // stderr. The option table has no `PARSE_OPT_HIDDEN` entry, so the two
    // spellings render the same block.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    super::diff_files::init_quote_path(&repo);
    let hash = repo.object_hash();

    let mut opts = Opts {
        recurse: false,
        show_trees: false,
        nul: false,
        root: false,
        merges: false,
        no_commit_id: false,
        always: false,
        reverse: false,
        exit_code: false,
        abbrev: hash.len_in_hex(),
        filter: ALL_STATUSES,
        format: Format::Raw,
        paths: Vec::new(),
        specs: None,
        combine: false,
        dense_combined: false,
        combined_all_paths: false,
        route: None,
        line_prefix: Vec::new(),
        pretty: None,
    };

    // The first option git accepts but this port cannot honour. Kept until we know
    // whether the invocation produces output at all; see the module documentation.
    let mut unsupported: Option<String> = None;
    // `--merge-base` is validated after revision resolution: git requires exactly two
    // commits and dies fatally (not a usage error) otherwise, even with zero revs.
    let mut merge_base = false;
    let mut revs: Vec<String> = Vec::new();
    let mut raw_paths: Vec<String> = Vec::new();
    // `--stdin`: read `<oid>` lines and diff each one, instead of (or as well as) the
    // revisions on the command line.
    let mut read_stdin = false;
    // Every option that belongs to `diff_opt_parse` rather than to `diff-tree` itself,
    // in command-line order. These are what a routed run hands to `diff-pairs`.
    let mut diff_args: Vec<String> = Vec::new();
    // git's `diff_tree_tweak_rev`: the raw format is only the default while
    // `setup_revisions` left `diffopt.output_format` at zero.
    let mut format_explicit = false;

    // git scans the whole argument list for a literal `--` up front; when one is
    // present every argument before it must be a revision.
    let seen_dashdash = args.iter().any(|a| a == "--");

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            raw_paths.extend(args[i + 1..].iter().cloned());
            break;
        }
        // `--ws-error-highlight <kind>`, `--color-moved-ws <modes>` and
        // `--word-diff-regex <re>` take their value as the next argument when it is
        // not glued on with `=`. All three only steer *patch* rendering, which this
        // command never produces, so the accepted value has no effect here — but
        // parse-options still validates it, and it still has to be consumed so it is
        // not left behind to be misread as a revision.
        //
        // `--diff-algorithm <value>` is the same shape — an `OPT_CALLBACK_F` with a
        // required argument (diff.c:6289) — and unlike the three above it *does*
        // change what is rendered. Left unconsumed, its value was read as the next
        // positional and every separated spelling died with `fatal: ambiguous
        // argument 'myers'` where stock rendered the patch. It is folded into the
        // `=` spelling on the way into `diff_args` because parse-options makes the
        // two forms reach `parse_algorithm_value()` identically, and because
        // everything downstream — [`needs_pairs`] included — keys off that one
        // spelling.
        // `-S <string>`, `-G <regex>` and `-I <regex>` are the same shape again, as
        // short options: `OPT_PICKAXE_S`/`OPT_PICKAXE_G` (diff.c:6270-6275) and
        // `OPT_CALLBACK_F('I', …)` all take a required argument, so a bare one takes
        // the next argv entry. Their value re-glues without an `=`.
        if a == "--ws-error-highlight"
            || a == "--diff-algorithm"
            || SHORT_GLUED_WHEN_SEPARATED.contains(&a)
            || super::diff_color::needs_separate_value(a)
        {
            let Some(v) = args.get(i + 1).map(String::as_str) else {
                eprintln!("error: {}", super::diff_color::missing_value(a));
                return Ok(ExitCode::from(USAGE_ERROR));
            };
            if let Some(code) = validate_render_value(a, v) {
                return Ok(code);
            }
            if a == "--diff-algorithm" {
                diff_args.push(format!("{a}={v}"));
            } else if SHORT_GLUED_WHEN_SEPARATED.contains(&a) {
                diff_args.push(format!("{a}{v}"));
            } else {
                diff_args.push(a.to_string());
                diff_args.push(v.to_string());
            }
            i += 2;
            continue;
        }
        // The same three spelled with `=`, validated by the same callback.
        if let Some((flag, v)) = a.split_once('=') {
            if flag == "--ws-error-highlight" || super::diff_color::needs_separate_value(flag) {
                if let Some(code) = validate_render_value(flag, v) {
                    return Ok(code);
                }
                diff_args.push(a.to_string());
                i += 1;
                continue;
            }
        }
        if a.starts_with('-') && a != "-" {
            // The value checks `diff_opt_parse`'s callbacks run as each option is
            // seen — before any revision is resolved, and before this module gets
            // to decide whether it can render the option at all.
            if let Some(line) = super::diff_optval::reject(a) {
                eprintln!("{line}");
                return Ok(ExitCode::from(USAGE_ERROR));
            }
            // `--dirstat`'s callback dies rather than erroring, so it exits 128 —
            // and it runs here, ahead of the synopsis this command prints when no
            // tree-ish was named.
            if let Some(text) = super::diff_optval::dirstat_reject(a) {
                eprint!("{text}");
                return Ok(ExitCode::from(FATAL));
            }
            // Everything `diff_opt_parse` owns is remembered verbatim: a routed run
            // replays exactly these onto `diff-pairs`, in order.
            if !is_diff_tree_option(a) {
                diff_args.push(a.to_string());
            }
            if sets_output_format(a) {
                format_explicit = true;
            }
            match a {
                "-r" => opts.recurse = true,
                "-t" => {
                    opts.recurse = true; // -t implies -r
                    opts.show_trees = true;
                }
                "-z" => opts.nul = true,
                "--root" => opts.root = true,
                "-m" => opts.merges = true,
                "--no-commit-id" => opts.no_commit_id = true,
                "--always" => opts.always = true,
                // `-v` is `revs->verbose_header` on its own, and a bare
                // `--pretty`/`--format` is that plus `CMIT_FMT_MEDIUM` — git's default
                // when the option carries no value.
                "-v" | "--pretty" | "--format" => {
                    opts.pretty = Some(std::rc::Rc::new(super::log::Pretty::Medium));
                }
                // `-R` (git's `DIFF_OPT_REVERSE_DIFF`): swap the two sides of every
                // file pair. git applies this in diffcore before `--diff-filter`
                // classification, so a reversed addition is filtered as a deletion;
                // the swap therefore runs in [`collect`] before [`apply_filter`].
                "-R" => opts.reverse = true,
                // git validates the operand count for `--merge-base` after it has
                // resolved the revisions (exactly two commits), so the flag is only
                // recorded here; the count check runs once parsing is complete. The
                // valid two-commit case diffs the merge base's tree against the second
                // commit's tree (see [`merge_base_diff`]).
                "--merge-base" => merge_base = true,
                // `--stdin`: `cmd_diff_tree` reads `<oid>` lines after it has handled
                // whatever the command line asked for, and a bare `--stdin` with no
                // revision is *not* the usage error a bare `diff-tree` is.
                "--stdin" => read_stdin = true,
                // `-c`/`--cc`: `rev->combine_merges` / `rev->dense_combined_merges`.
                // `--cc` implies `-c` and additionally selects the combined *patch*
                // format, which is why it also counts as an explicit output format
                // for `diff_tree_tweak_rev`.
                "-c" => opts.combine = true,
                "--cc" => {
                    opts.combine = true;
                    opts.dense_combined = true;
                }
                "--combined-all-paths" => opts.combined_all_paths = true,
                "--raw" => opts.format = Format::Raw,
                "--name-only" => opts.format = Format::NameOnly,
                "--name-status" => opts.format = Format::NameStatus,
                "--numstat" => opts.format = Format::NumStat,
                "--shortstat" => opts.format = Format::ShortStat,
                "--summary" => opts.format = Format::Summary,
                "-s" | "--no-patch" => opts.format = Format::NoOutput,
                "--exit-code" => opts.exit_code = true,
                // `--quiet` is `-s` plus `--exit-code`: git still prints the
                // commit-id line, only the diff body is suppressed.
                "--quiet" => {
                    opts.format = Format::NoOutput;
                    opts.exit_code = true;
                }
                // The lone-`-h` help is answered before discovery. Reaching
                // here means `-h` had company, which `cmd_diff_tree` treats as
                // any other unhandled argument: `usage(diff_tree_usage)`, so the
                // same block but on stderr.
                "-h" => {
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(USAGE_ERROR));
                }
                // `cmd_diff_tree` starts from `opt->abbrev = 0` (full object names) and
                // `--no-abbrev` puts it back there, so it is the standing default here.
                "--no-abbrev" => opts.abbrev = hash.len_in_hex(),
                // `--line-prefix=<s>` prefixes every emitted line, the commit-id line
                // included; the diff body is prefixed by whoever renders it.
                _ if a.starts_with("--line-prefix=") => {
                    opts.line_prefix = a["--line-prefix=".len()..].as_bytes().to_vec();
                }
                _ if a.starts_with("--abbrev=") => {
                    // git parses the value with strtoul(arg, NULL, 10): leading
                    // base-10 digits only, no error on garbage (yields 0), then
                    // clamps to [MINIMUM_ABBREV, hash length]. A value above the hash
                    // length — including one that wrapped from a leading `-` — clamps
                    // down to it, so `--abbrev=true`, `--abbrev=0x10`, `--abbrev=` and
                    // `--abbrev=-5` are all accepted exactly as git accepts them.
                    let n = git_strtoul(&a["--abbrev=".len()..]);
                    opts.abbrev =
                        n.clamp(MINIMUM_ABBREV as u64, hash.len_in_hex() as u64) as usize;
                }
                _ if a.starts_with("--diff-filter=") => {
                    match parse_diff_filter(&a["--diff-filter=".len()..]) {
                        Some(mask) => opts.filter = mask,
                        None => return Ok(ExitCode::from(USAGE_ERROR)),
                    }
                }
                // git validates `--color`'s value while parsing options (before it
                // resolves revisions), accepting only always/auto/never
                // case-insensitively and rejecting everything else — including
                // `true`/`false`/`0`/`1`/empty — with a usage error.
                _ if a.starts_with("--color=") => {
                    let v = &a["--color=".len()..];
                    if !matches!(
                        v.to_ascii_lowercase().as_str(),
                        "always" | "never" | "auto"
                    ) {
                        eprintln!(
                            "error: option `color' expects \"always\", \"auto\", or \"never\""
                        );
                        return Ok(ExitCode::from(USAGE_ERROR));
                    }
                    // Accepted; has no effect on the raw output this port emits.
                }
                // git validates the `--pretty`/`--format` argument through
                // `get_commit_format` at parse time and dies fatally on a format name
                // it does not recognise, before it ever checks for a missing
                // <tree-ish>. A valid format is still one this port cannot render, so
                // it is recorded like any other unsupported option.
                _ if a.starts_with("--format=") || a.starts_with("--pretty=") => {
                    let v = a.split_once('=').map(|(_, r)| r).unwrap_or("");
                    if !valid_pretty_format(v) {
                        eprintln!("fatal: invalid --pretty format: {v}");
                        return Ok(ExitCode::from(FATAL));
                    }
                    // A name this port can render becomes the verbose header; one it
                    // only *validates* stays recorded as unsupported, so the run still
                    // refuses rather than printing the wrong bytes.
                    match super::log::get_commit_format(v) {
                        Ok(Some((fmt, _))) => opts.pretty = Some(std::rc::Rc::new(fmt)),
                        _ => {
                            unsupported.get_or_insert_with(|| a.to_string());
                        }
                    }
                }
                // git parses `--expand-tabs=<n>` at option time as a base-10 integer
                // (leading whitespace and an optional sign allowed, the whole value
                // consumed, no overflow) and dies fatally on anything that is not a
                // non-negative integer — before any revision is resolved. A valid
                // value only affects patch rendering, which this port never emits.
                _ if a.starts_with("--expand-tabs=") => {
                    let v = &a["--expand-tabs=".len()..];
                    if parse_nonneg_int(v).is_none() {
                        eprintln!("fatal: '{v}': not a non-negative integer");
                        return Ok(ExitCode::from(FATAL));
                    }
                }
                // git validates `--ignore-submodules=<value>` at option time against a
                // fixed, case-sensitive set and dies fatally on anything else (the
                // empty string included), before revision resolution.
                //
                // Only `all` changes a tree-to-tree diff. `dirty` and `untracked` name
                // states a worktree has and two trees do not, and `none` is the
                // default, so all three leave the output alone; `all` drops gitlink
                // pairs entirely and stays recorded as unimplemented. Verified against
                // git 2.55.0 over a pair of commits whose only submodule change is a
                // gitlink oid: `diff-tree -r A B` is byte-identical with `none`,
                // `untracked` and `dirty` (raw and `-p`), and loses the `:160000` row
                // only with `all`.
                _ if a.starts_with("--ignore-submodules=") => {
                    let v = &a["--ignore-submodules=".len()..];
                    if !matches!(v, "none" | "untracked" | "dirty" | "all") {
                        eprintln!("fatal: bad --ignore-submodules argument: {v}");
                        return Ok(ExitCode::from(FATAL));
                    }
                    if v == "all" {
                        unsupported.get_or_insert_with(|| a.to_string());
                    }
                }
                // Rendered by `diff-pairs` further down; see [`needs_pairs`].
                _ if needs_pairs(a) => {}
                _ if is_ignorable(a) => {}
                _ if is_known_unsupported(a) => {
                    unsupported.get_or_insert_with(|| a.to_string());
                }
                // Not one of git's diff-tree options as far as this port knows; git
                // would answer with its usage text and 129, but guessing that here
                // would hide a genuinely missing option, so fail loudly instead.
                _ => crate::git_fatal!("unrecognized option {a:?}"),
            }
            i += 1;
            continue;
        }

        // A positional. It is a `<tree-ish>` exactly when it resolves as a revision.
        // `handle_revision_arg()` gets it first either way, so `get_oid_basic()`'s
        // ambiguity warning belongs here — once per positional, ahead of the
        // several times this spec is resolved again further down.
        crate::objname::warn_ambiguous_refname(&repo, a);
        if repo.rev_parse_single(a).is_ok() {
            revs.push(a.to_string());
            i += 1;
            continue;
        }
        // A full-length object name always parses as one, so git gets as far as
        // looking the object up and reports its absence rather than guessing that a
        // path was meant.
        if a.len() == hash.len_in_hex() && a.bytes().all(|b| b.is_ascii_hexdigit()) {
            eprintln!("fatal: bad object {a}");
            return Ok(ExitCode::from(FATAL));
        }
        if seen_dashdash {
            eprintln!("fatal: bad revision '{a}'");
            return Ok(ExitCode::from(FATAL));
        }
        // git stops parsing options here and requires this argument and every one
        // after it to name an existing path.
        for (n, rest) in args[i..].iter().enumerate() {
            if let Some(code) = verify_filename(rest, n == 0) {
                return Ok(ExitCode::from(code));
            }
            raw_paths.push(rest.clone());
        }
        break;
    }

    // `diff_setup_done()`'s pickaxe check. It is a `die()`, not a usage error, and
    // it runs after every revision has been resolved — a bad revision anywhere in
    // argv still wins, which is why this waits until the scan is over.
    if super::diff_optval::pickaxe_conflict(args) {
        eprintln!("{}", super::diff_optval::PICKAXE_CONFLICT);
        return Ok(ExitCode::from(FATAL));
    }
    // `diff_setup_done()`'s second pickaxe `die()`, immediately after the first:
    // `--pickaxe-all` has no meaning for the objfind kind, so the combination is
    // refused rather than given one.
    if args.iter().take_while(|a| *a != "--").any(|a| a == "--pickaxe-all")
        && args
            .iter()
            .take_while(|a| *a != "--")
            .any(|a| a == "--find-object" || a.starts_with("--find-object="))
    {
        eprintln!("{}", super::diff_optval::PICKAXE_ALL_OBJFIND_CONFLICT);
        return Ok(ExitCode::from(FATAL));
    }

    for p in &raw_paths {
        opts.paths.push(BString::from(p.as_bytes()));
    }
    // The pathspec set, parsed once by the shared engine — magic and wildcards
    // included, the same as every other verb.
    if !opts.paths.is_empty() {
        opts.specs =
            Some(std::rc::Rc::new(super::log::PathspecMatcher::new(&repo, &raw_paths)?));
    }

    // The stat family is always file-granular in git: recursion is forced on and tree
    // entries are never reported, overriding whatever `-r`/`-t` asked for.
    if opts.format.is_stat() {
        opts.recurse = true;
        opts.show_trees = false;
    }

    // An option only `diff-pairs` can render switches the whole rendering pass over to
    // it. The pair list handed over is then the untouched walk output: `-R` and
    // `--diff-filter` travel with the other diff options and are applied there, so
    // applying them here as well would double them up.
    if diff_args.iter().any(|a| needs_pairs(a)) {
        let mut route = diff_args.clone();
        // `diff_tree_tweak_rev`: with no output format on the command line, `diff-tree`
        // defaults to the raw format where `diff-pairs` would default to a patch.
        if !format_explicit {
            route.insert(0, if opts.dense_combined { "-p" } else { "--raw" }.to_string());
        }
        // `diff_setup_done` turns recursion on for the file-granular formats; `-M`
        // alone leaves it off, so `diff-tree -M` still reports a changed tree entry.
        if diff_args.iter().any(|a| format_forces_recursion(a)) {
            opts.recurse = true;
            opts.show_trees = false;
        }
        opts.route = Some(route);
        opts.reverse = false;
        opts.filter = ALL_STATUSES;
    }

    // git checks the `--merge-base` operand count after resolving revisions but before
    // the missing-<tree-ish> usage error, so zero revs here is the fatal merge-base
    // message, not the usage text.
    if merge_base && revs.len() != 2 {
        eprintln!("fatal: --merge-base only works with two commits");
        return Ok(ExitCode::from(FATAL));
    }

    // `cmd_diff_tree` dies on this combination before it looks at the operand count.
    if read_stdin && merge_base {
        eprintln!("fatal: options '--stdin' and '--merge-base' cannot be used together");
        return Ok(ExitCode::from(FATAL));
    }

    // With `--stdin` the revision list may legitimately be empty: the objects to diff
    // arrive on stdin instead.
    if revs.is_empty() && !read_stdin {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(USAGE_ERROR));
    }

    let mut out: Vec<u8> = Vec::new();
    let mut differed = false;
    let code = if revs.is_empty() {
        0
    } else if merge_base {
        // `--merge-base <a> <b>`: diff the merge base of the two commits against the
        // second commit's tree. No commit-id line, like the two-tree form.
        merge_base_diff(&repo, &revs[0], &revs[1], &opts, &mut out, &mut differed)?
    } else if revs.len() > 2 {
        // git accepts more than two tree-ishes and then prints nothing at all.
        0
    } else if revs.len() == 2 {
        // Two tree-ishes: a plain tree-vs-tree diff with no commit-id line. git dies
        // on the first argument it cannot use, so the second is only looked at once
        // the first resolved.
        match resolve_tree(&repo, &revs[0])? {
            None => FATAL,
            Some(old) => match resolve_tree(&repo, &revs[1])? {
                None => FATAL,
                Some(new) => {
                    let changes = collect(&repo, Some(old), Some(new), &opts)?;
                    differed = !changes.is_empty();
                    render_all(&repo, &mut out, &changes, &opts)?;
                    0
                }
            },
        }
    } else {
        single_commit(&repo, &revs[0], &opts, &mut out, &mut differed)?
    };

    // `cmd_diff_tree`'s `--stdin` loop: one object name per line, each diffed like the
    // single-`<commit>` form. A line that is not an object name is echoed verbatim, and
    // a commit line may carry replacement ("grafted") parents after the id.
    let mut code = code;
    if read_stdin && code == 0 {
        code = diff_tree_stdin(&repo, &opts, &mut out, &mut differed)?;
    }

    // A recognised-but-unimplemented option can only produce wrong bytes when there
    // are bytes to produce. `differed` is checked alongside the buffer because an
    // option such as `--numstat` renders from the change list even in the forms that
    // leave the raw buffer empty (`-s --no-commit-id`).
    if code == 0 && (differed || !out.is_empty()) {
        if let Some(flag) = &unsupported {
            bail!("unsupported flag {flag:?}");
        }
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;

    if code == 0 && opts.exit_code && differed {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::from(code))
}

/// git's `diff_tree_stdin()` loop: read object names from stdin, one per line.
///
/// A line whose first field is not an object name is copied to stdout unchanged. A
/// commit id may be followed by further ids, which replace ("graft") its parent list for
/// this diff only. A tree id must be followed by exactly one more tree id; git prints
/// `<tree1> <tree2>` and then the tree-vs-tree diff.
fn diff_tree_stdin(
    repo: &gix::Repository,
    opts: &Opts,
    out: &mut Vec<u8>,
    differed: &mut bool,
) -> Result<u8> {
    let hexsz = repo.object_hash().len_in_hex();
    let mut line = String::new();
    loop {
        line.clear();
        if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)? == 0 {
            return Ok(0);
        }
        let body = line.strip_suffix('\n').unwrap_or(&line);
        let head = body.get(..hexsz).unwrap_or("");
        let parsed = (head.len() == hexsz && head.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| ObjectId::from_hex(head.as_bytes()).ok())
            .flatten();
        let Some(id) = parsed else {
            // `fputs(line, stdout)`: not an object name, echoed as it arrived.
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        let Ok(object) = repo.find_object(id) else {
            return Ok(0);
        };
        let rest: Vec<ObjectId> = body[hexsz..]
            .split_ascii_whitespace()
            .filter_map(|w| ObjectId::from_hex(w.as_bytes()).ok())
            .collect();
        match object.kind {
            gix::object::Kind::Commit => {
                let commit = object.try_into_commit()?;
                let commit_id = commit.id;
                let new_tree = commit.tree_id()?.detach();
                let parents: Vec<ObjectId> = if rest.is_empty() {
                    commit.parent_ids().map(|p| p.detach()).collect()
                } else {
                    rest
                };
                emit_commit_diff(repo, commit_id, &parents, new_tree, opts, out, differed)?;
            }
            gix::object::Kind::Tree => {
                if rest.len() != 1 {
                    eprintln!("error: Need exactly two trees, separated by a space");
                    continue;
                }
                let (a, b) = (object.id, rest[0]);
                out.extend_from_slice(format!("{a} {b}\n").as_bytes());
                let (Some(ta), Some(tb)) = (peel_tree(repo, a), peel_tree(repo, b)) else {
                    return Ok(0);
                };
                let changes = collect(repo, Some(ta), Some(tb), opts)?;
                *differed |= !changes.is_empty();
                render_all(repo, out, &changes, opts)?;
            }
            kind => {
                eprintln!("error: Object {id} is a {kind}, not a commit or tree");
            }
        }
    }
}

/// The tree of `id`, or `None` when it cannot be peeled to one.
fn peel_tree(repo: &gix::Repository, id: ObjectId) -> Option<ObjectId> {
    repo.find_object(id).ok()?.peel_to_tree().ok().map(|t| t.id)
}

/// The body of [`single_commit`] once the commit, its tree and its parent list are
/// known — shared with the `--stdin` loop, whose parents may have been grafted.
fn emit_commit_diff(
    repo: &gix::Repository,
    commit_id: ObjectId,
    parents: &[ObjectId],
    new_tree: ObjectId,
    opts: &Opts,
    out: &mut Vec<u8>,
    differed: &mut bool,
) -> Result<u8> {
    if parents.len() > 1 && opts.combine {
        return combined_commit(repo, commit_id, parents, new_tree, opts, out, differed);
    }
    let befores: Vec<Option<ObjectId>> = if parents.is_empty() {
        if opts.root {
            vec![None]
        } else {
            return Ok(0);
        }
    } else if parents.len() > 1 && !opts.merges {
        // A merge is silently skipped unless -m asks for per-parent diffs.
        return Ok(0);
    } else if opts.merges {
        let mut trees = Vec::with_capacity(parents.len());
        for p in parents {
            trees.push(Some(tree_of(repo, *p)?));
        }
        trees
    } else {
        vec![Some(tree_of(repo, parents[0])?)]
    };

    for before in befores {
        let changes = collect(repo, before, Some(new_tree), opts)?;
        *differed |= !changes.is_empty();
        if opts.always || (!opts.no_commit_id && !changes.is_empty()) {
            emit_commit_header(repo, out, commit_id, opts)?;
        }
        render_all(repo, out, &changes, opts)?;
    }
    Ok(0)
}

/// `show_log()` (log-tree.c:741) followed by the blank line `log_tree_diff_flush()`
/// (log-tree.c:939) puts between the header and the diff.
///
/// Without `revs->verbose_header` that is just the object name, which is what plumbing
/// callers parse. With it — `-v`, `--pretty`, `--format` — the name gains git's `commit `
/// prefix and the formatted message follows, except under `oneline`, where the subject
/// shares the name's line and no blank line separates it from the diff.
fn emit_commit_header(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    commit_id: ObjectId,
    opts: &Opts,
) -> Result<()> {
    let term = if opts.nul { b'\0' } else { b'\n' };
    out.extend_from_slice(&opts.line_prefix);
    let Some(pretty) = opts.pretty.as_deref() else {
        out.extend_from_slice(commit_id.to_hex().to_string().as_bytes());
        out.push(term);
        return Ok(());
    };
    let oneline = matches!(pretty, super::log::Pretty::Oneline);
    if !oneline {
        out.extend_from_slice(b"commit ");
    }
    out.extend_from_slice(commit_id.to_hex().to_string().as_bytes());
    // git separates the object name from a oneline body with a space and from every
    // other body with the line terminator.
    out.push(if oneline { b' ' } else { term });
    let commit = repo.find_object(commit_id)?.try_into_commit()?;
    let body = super::log::rev_list_pretty_body(repo, &commit, pretty)?;
    if !body.is_empty() {
        out.extend_from_slice(&body);
        // `pp_remainder()` already terminates every multi-line format's last message
        // line; `oneline` is the one that stops after the subject, so only it still
        // owes a terminator.
        if body.last() != Some(&term) {
            out.push(term);
        }
    }
    // log-tree.c:941 — "an extra newline between the end of log and the diff/diffstat
    // output for readability", suppressed for `oneline` and for `-s`, which selects
    // `DIFF_FORMAT_NO_OUTPUT` and so leaves nothing for the blank line to separate.
    if opts.format != Format::NoOutput && !oneline {
        out.extend_from_slice(&opts.line_prefix);
        // With both a diffstat and a patch requested the separator is a `---` line
        // rather than an empty one.
        if stat_and_patch(opts) {
            out.extend_from_slice(b"---");
        }
        out.push(b'\n');
    }
    Ok(())
}

/// `(DIFF_FORMAT_DIFFSTAT | DIFF_FORMAT_PATCH)` both requested, which is the case
/// `log_tree_diff_flush()` marks with a `---` line. Both formats at once are always a
/// routed run — this module's own `Format` holds one at a time — so the answer comes
/// from the arguments being forwarded to `diff-pairs`.
fn stat_and_patch(opts: &Opts) -> bool {
    let Some(args) = &opts.route else {
        return false;
    };
    let stat = args
        .iter()
        .any(|a| a == "--stat" || a.starts_with("--stat=") || a == "--patch-with-stat");
    let patch = args.iter().any(|a| {
        matches!(a.as_str(), "-p" | "-u" | "--patch" | "--patch-with-raw" | "--patch-with-stat")
    });
    stat && patch
}

/// [`crate::setup::verify_filename`], reported and turned into git's exit code.
///
/// `first` is git's `diagnose_misspelt_rev`: the leading non-revision argument is
/// diagnosed as a possibly-misspelt revision, later ones simply as missing paths.
fn verify_filename(arg: &str, first: bool) -> Option<u8> {
    let msg = crate::setup::verify_filename(arg, first)?;
    eprintln!("fatal: {msg}");
    Some(FATAL)
}

/// `--diff-filter` status bits. Bit 0 is git's "all or none" marker (`*`).
const AON: u32 = 1;
/// Every real status bit, i.e. the mask that filters nothing out.
const ALL_STATUSES: u32 = !AON;

/// The bit git assigns to a `--diff-filter` change class, or `None` if the letter is
/// not one.
fn filter_bit(letter: u8) -> Option<u32> {
    let shift = match letter {
        b'*' => 0,
        b'A' => 1,
        b'B' => 2,
        b'C' => 3,
        b'D' => 4,
        b'M' => 5,
        b'R' => 6,
        b'T' => 7,
        b'U' => 8,
        b'X' => 9,
        _ => return None,
    };
    Some(1 << shift)
}

/// Parse `--diff-filter=<letters>` into a status mask.
///
/// Uppercase letters (and `*`) select, lowercase letters deselect. A string made only
/// of deselections starts from "everything" so that `--diff-filter=d` means "all but
/// deletions"; any selection present starts from nothing instead, which is why
/// `--diff-filter=Md` is just `M`. `None` means git rejected the string, after
/// printing its error; the caller exits 129.
fn parse_diff_filter(spec: &str) -> Option<u32> {
    let selects = spec.bytes().any(|b| b == b'*' || b.is_ascii_uppercase());
    let mut mask = if selects { 0 } else { ALL_STATUSES };
    for b in spec.bytes() {
        let negate = b.is_ascii_lowercase();
        let Some(bit) = filter_bit(b.to_ascii_uppercase()) else {
            eprintln!(
                "error: unknown change class '{}' in --diff-filter={spec}",
                b as char
            );
            return None;
        };
        if negate {
            mask &= !bit;
        } else {
            mask |= bit;
        }
    }
    Some(mask)
}

/// Apply a `--diff-filter` mask to a collected change list.
fn apply_filter(changes: &mut Vec<Change>, mask: u32) {
    if mask == ALL_STATUSES {
        return;
    }
    let wanted = mask & !AON;
    let matches = |c: &Change| filter_bit(status(c)).is_some_and(|b| b & wanted != 0);
    if mask & AON != 0 {
        // "all or none": one hit shows the whole list, no hit shows nothing.
        if !changes.iter().any(matches) {
            changes.clear();
        }
    } else {
        changes.retain(matches);
    }
}

/// git's `strtoul(s, NULL, 10)`, used for `--abbrev=<n>`: skip leading ASCII
/// whitespace and an optional sign, read base-10 digits until the first non-digit,
/// and never report an error — no digits yields 0. A leading `-` negates with the
/// same unsigned wraparound C's `strtoul` performs, which the caller then clamps to
/// the hash length.
fn git_strtoul(s: &str) -> u64 {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let neg = match b.get(i) {
        Some(b'+') => {
            i += 1;
            false
        }
        Some(b'-') => {
            i += 1;
            true
        }
        _ => false,
    };
    let mut val: u64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        val = val.wrapping_mul(10).wrapping_add((b[i] - b'0') as u64);
        i += 1;
    }
    if neg {
        val.wrapping_neg()
    } else {
        val
    }
}

/// git's option-time integer parse for `--expand-tabs=<n>`: base-10 `strtol` with the
/// whole value consumed, then a non-negative check. Leading ASCII whitespace and an
/// optional `+`/`-` sign are allowed, trailing characters are not, and a value that
/// overflows is rejected. `None` is what git turns into
/// `die("'%s': not a non-negative integer")`.
///
/// Confirmed against stock git 2.55: `0`, `5`, `+3`, `-0`, `08`, ` 5`, `\t5` accept;
/// `v1`, `-1`, ``(empty), `3x`, `5 `(trailing space), `0x5`, and an overflowing run of
/// digits reject.
fn parse_nonneg_int(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let neg = match b.get(i) {
        Some(b'+') => {
            i += 1;
            false
        }
        Some(b'-') => {
            i += 1;
            true
        }
        _ => false,
    };
    let start = i;
    let mut val: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        val = val.checked_mul(10)?.checked_add((b[i] - b'0') as i64)?;
        i += 1;
    }
    // At least one digit, and nothing after it (git's strtol skips leading whitespace
    // but never trailing).
    if i == start || i != b.len() {
        return None;
    }
    let val = if neg { -val } else { val };
    (val >= 0).then_some(val)
}

/// git's `get_commit_format` accept/reject decision for a `--pretty`/`--format`
/// value. Accepted: the empty string (the default format), any custom format (a
/// `format:`/`tformat:` prefix or a string containing `%`), and any case-insensitive
/// prefix of a built-in format name. Everything else is what git rejects with
/// `die("invalid --pretty format: <arg>")`; the caller reproduces that fatal.
///
/// The built-in names and the prefix matching were both confirmed against stock git
/// 2.55: `--format=med` resolves (`medium`), `--format=Full` resolves case-blind,
/// while `auto`, `default`, `onelineX` and a leading-space value are rejected.
fn valid_pretty_format(v: &str) -> bool {
    const PRESETS: &[&str] = &[
        "oneline",
        "short",
        "medium",
        "full",
        "fuller",
        "reference",
        "email",
        "raw",
        "mboxrd",
    ];
    if v.is_empty() || v.contains('%') {
        return true;
    }
    if v.starts_with("format:") || v.starts_with("tformat:") {
        return true;
    }
    let lower = v.to_ascii_lowercase();
    PRESETS.iter().any(|p| p.starts_with(&lower))
}

/// Options stock git's `diff-tree` accepts that only steer patch or stat rendering.
///
/// This module never emits either format — it bails first — so these provably cannot
/// change the bytes it does emit. Each entry was compared against stock git in the
/// raw, `-t`, `--name-status` and commit-id-line forms before being listed here.
/// Options that re-compare blob content (`-w`, `-b`, the `--ignore-*` family) are
/// deliberately absent: they drop pairs from the raw output too.
/// The value check parse-options runs for the patch-rendering options this command
/// accepts but cannot act on. `--ws-error-highlight` and `--color-moved-ws` both
/// validate their argument in the option callback, so a bad value is a 129 here even
/// though no patch is ever emitted; `--word-diff-regex` takes any string, since git
/// only compiles the pattern once a word diff actually runs.
///
/// `Some(code)` means the message was written and the command must exit with it.
/// Short options whose value is the next argv entry when nothing is glued on, and
/// which re-glue without an `=` (`-Sdd`, not `-S=dd`).
const SHORT_GLUED_WHEN_SEPARATED: &[&str] = &["-S", "-G", "-I"];

fn validate_render_value(flag: &str, value: &str) -> Option<ExitCode> {
    match flag {
        "--ws-error-highlight" => match super::diff_color::parse_ws_error_highlight(value) {
            Ok(_) => None,
            Err(accepted) => {
                eprintln!(
                    "error: unknown value after ws-error-highlight={}",
                    &value[..accepted]
                );
                Some(ExitCode::from(USAGE_ERROR))
            }
        },
        "--color-moved-ws" => {
            let mut probe = super::diff_color::MoveWordOpts::default();
            let mut when = None;
            match probe.parse_flag(&format!("{flag}={value}"), &mut when) {
                Some(Err(msg)) => {
                    eprintln!("{msg}");
                    Some(ExitCode::from(USAGE_ERROR))
                }
                _ => None,
            }
        }
        // `diff_opt_diff_algorithm()`'s `error()`, which the `=` spelling already
        // reaches through [`super::diff_optval::reject`] — the separated form
        // bypasses that scan, so it is checked here instead.
        "--diff-algorithm" => match super::diff_optval::parse_algorithm_value(value) {
            Some(_) => None,
            None => {
                eprintln!("{}", super::diff_optval::DIFF_ALGORITHM_ERR);
                Some(ExitCode::from(USAGE_ERROR))
            }
        },
        // `diff_opt_pickaxe_string()` / `diff_opt_pickaxe_regex()`'s refusal of an
        // empty pattern, from the same callback the glued spelling reaches.
        "-S" | "-G" if value.is_empty() => {
            eprintln!("{}", super::diff_optval::pickaxe_empty(flag.as_bytes()[1]));
            Some(ExitCode::from(USAGE_ERROR))
        }
        _ => None,
    }
}

/// Options `diff-tree` owns rather than passing to `diff_opt_parse`; everything else on
/// the command line is a diff option and is replayed verbatim onto `diff-pairs` when a
/// run is routed there.
fn is_diff_tree_option(a: &str) -> bool {
    const EXACT: &[&str] = &[
        "-r",
        "-t",
        "--root",
        "-m",
        "-c",
        "--cc",
        "--combined-all-paths",
        "--no-commit-id",
        "--always",
        "--merge-base",
        "--stdin",
        "-v",
        "--pretty",
        "--format",
        "-h",
    ];
    EXACT.contains(&a) || a.starts_with("--pretty=") || a.starts_with("--format=")
}

/// Options that set `diffopt.output_format`, which is what `diff_tree_tweak_rev` checks
/// before defaulting to the raw format.
fn sets_output_format(a: &str) -> bool {
    const EXACT: &[&str] = &[
        "-p",
        "-u",
        "--patch",
        "--patch-with-raw",
        "--patch-with-stat",
        "--raw",
        "--name-only",
        "--name-status",
        "--numstat",
        "--shortstat",
        "--summary",
        "--stat",
        "--compact-summary",
        "--dirstat",
        "--dirstat-by-file",
        "--cumulative",
        "-X",
        "--check",
        "-s",
        "--no-patch",
        "--quiet",
    ];
    const PREFIX: &[&str] = &["--stat=", "--stat-", "--dirstat=", "--dirstat-by-file=", "-X"];
    EXACT.contains(&a) || PREFIX.iter().any(|p| a.starts_with(p))
}

/// git's `diff_setup_done`: the file-granular output formats turn `flags.recursive` on
/// whatever `-r` said. The raw format and rename detection alone do not, which is why
/// `diff-tree -M` still reports a changed tree entry.
fn format_forces_recursion(a: &str) -> bool {
    const EXACT: &[&str] = &[
        "-p",
        "-u",
        "--patch",
        "--patch-with-raw",
        "--patch-with-stat",
        "--stat",
        "--compact-summary",
        "--numstat",
        "--shortstat",
        "--summary",
        "--dirstat",
        "--dirstat-by-file",
        "--cumulative",
        "-X",
        "--check",
        "--name-only",
        "--name-status",
    ];
    const PREFIX: &[&str] = &["--stat=", "--stat-", "--dirstat=", "--dirstat-by-file=", "-U", "-X"];
    EXACT.contains(&a) || PREFIX.iter().any(|p| a.starts_with(p))
}

/// Options `diff-pairs` renders and this module's raw/name/stat emitters cannot.
///
/// Seeing one switches the whole rendering pass over to
/// [`super::diff_pairs::render_raw_stream`] — which is the `diff-tree -z -r --raw |
/// diff-pairs` pipeline `git diff-pairs` documents, run in-process — instead of growing
/// a second patch, diffstat, dirstat, whitespace and rename implementation here.
fn needs_pairs(a: &str) -> bool {
    const EXACT: &[&str] = &[
        // patch and stat output
        "-p",
        "-u",
        "--patch",
        "--patch-with-raw",
        "--patch-with-stat",
        "--binary",
        "--stat",
        "--compact-summary",
        "--dirstat",
        "--dirstat-by-file",
        "--cumulative",
        "-X",
        "--check",
        "--unified",
        // rename, copy and rewrite detection
        "-B",
        "-C",
        "-D",
        "-M",
        "--break-rewrites",
        "--find-renames",
        "--find-copies",
        "--find-copies-harder",
        "--irreversible-delete",
        "--no-renames",
        "--rename-empty",
        "--no-rename-empty",
        // whitespace-insensitive comparison, which also drops pairs
        "-b",
        "-w",
        "--ignore-cr-at-eol",
        "--ignore-space-at-eol",
        "--ignore-space-change",
        "--ignore-all-space",
        // hunk shaping
        "-W",
        "--function-context",
        // pickaxe and ordering
        "--pickaxe-all",
        "--pickaxe-regex",
        // path rewriting
        "--relative",
        "--no-relative",
        "--skip-to",
        "--rotate-to",
    ];
    const PREFIX: &[&str] = &[
        "--stat=",
        "--stat-",
        "--dirstat=",
        "--dirstat-by-file=",
        "--inter-hunk-context=",
        "--line-prefix=",
        "--find-renames=",
        "--find-copies=",
        "--break-rewrites=",
        "--unified=",
        "--ignore-matching-lines=",
        "--find-object=",
        "--skip-to=",
        "--rotate-to=",
        "--relative=",
        // short options carrying an attached value
        "-U",
        "-S",
        "-G",
        "-O",
        "-l",
        "-B",
        "-C",
        "-M",
        "-I",
        "-X",
    ];
    EXACT.contains(&a) || PREFIX.iter().any(|p| a.starts_with(p))
}

fn is_ignorable(a: &str) -> bool {
    const EXACT: &[&str] = &[
        "--no-prefix",
        "--default-prefix",
        "--color",
        "--no-color",
        "--color-words",
        "--abbrev-commit",
        "--no-abbrev-commit",
        "--text",
        "-a",
        "--minimal",
        "--patience",
        "--histogram",
        "--indent-heuristic",
        "--no-indent-heuristic",
        "--expand-tabs",
        "--no-expand-tabs",
        "--function-context",
        "-W",
        "--full-index",
        "--textconv",
        "--no-textconv",
        "--ext-diff",
        "--no-ext-diff",
        "--ita-invisible-in-index",
        "--ita-visible-in-index",
        // `revision.c`'s `--no-notes` turns off a display that is off by default,
        // so it cannot change any output this command produces.
        "--no-notes",
    ];
    const PREFIX: &[&str] = &[
        "--src-prefix=",
        "--dst-prefix=",
        "--diff-algorithm=",
        "--inter-hunk-context=",
        "--output-indicator-new=",
        "--output-indicator-old=",
        "--output-indicator-context=",
        "--word-diff=",
    ];
    EXACT.contains(&a) || PREFIX.iter().any(|p| a.starts_with(p))
}

/// Options stock git's `diff-tree` accepts that this port cannot reproduce.
///
/// Recognising them is what lets argument validation and revision resolution run in
/// git's order; the caller turns a recognised option into a bail as soon as output
/// would actually be produced.
fn is_known_unsupported(a: &str) -> bool {
    const EXACT: &[&str] = &[
        // patch and stat output
        "-p",
        "-u",
        "--patch",
        "--patch-with-raw",
        "--patch-with-stat",
        "--binary",
        "--stat",
        "--dirstat",
        "--dirstat-by-file",
        "--cumulative",
        "--compact-summary",
        "--check",
        "--unified",
        // object-name width we cannot derive
        "--abbrev",
        // rename, copy and rewrite detection
        "-B",
        "-C",
        "-D",
        "-M",
        "--break-rewrites",
        "--find-renames",
        "--find-copies",
        "--find-copies-harder",
        "--irreversible-delete",
        "--no-renames",
        "--rename-empty",
        "--no-rename-empty",
        // pickaxe and ordering
        "--pickaxe-all",
        "--pickaxe-regex",
        // path rewriting
        "--relative",
        "--no-relative",
        "--line-prefix",
        "--skip-to",
        "--rotate-to",
        "--output",
        // submodule handling changes which gitlink pairs are reported
        "--submodule",
        "--ignore-submodules",
        // content comparison: these drop pairs from the raw output as well
        "-b",
        "-w",
        "--ignore-cr-at-eol",
        "--ignore-space-at-eol",
        "--ignore-space-change",
        "--ignore-all-space",
        "--ignore-blank-lines",
        // commit formatting, combined diffs and revision walking
        "--stdin",
        // `--oneline` is `--pretty=oneline --abbrev-commit`; the abbreviated commit
        // name is what this port does not produce here.
        "--oneline",
        "--no-oneline",
        "-c",
        "--cc",
        "--combined-all-paths",
        "--diff-merges",
        "--no-diff-merges",
        "--remerge-diff",
        "--first-parent",
        "--full-diff",
        "--max-depth",
        "--max-count",
        // colour and word diff variants that need a rendered body
        "--word-diff",
        "--color-moved",
        "--no-color-moved",
        // `--no-color-moved-ws` is `OPT_CALLBACK_F(..., PARSE_OPT_NONEG)`'s twin
        // in `diff_opt_color_moved_ws()`: git takes it and clears the mode, so it
        // is a recognised flag here rather than an unknown one.
        "--no-color-moved-ws",
        "--anchored",
    ];
    const PREFIX: &[&str] = &[
        "--stat=",
        "--stat-width=",
        "--stat-name-width=",
        "--stat-count=",
        "--stat-graph-width=",
        "--dirstat=",
        "--dirstat-by-file=",
        "--submodule=",
        "--ignore-submodules=",
        "--ignore-matching-lines=",
        "--color-moved=",
        "--color-moved-ws=",
        "--line-prefix=",
        "--anchored=",
        "--pretty=",
        "--format=",
        "--diff-merges=",
        "--encoding=",
        "--max-depth=",
        "--max-count=",
        "--skip-to=",
        "--rotate-to=",
        "--relative=",
        "--output=",
        "--find-renames=",
        "--find-copies=",
        "--break-rewrites=",
        "--unified=",
        // short options that carry an attached value
        "-U",
        "-S",
        "-G",
        "-O",
        "-l",
        "-B",
        "-C",
        "-M",
        // `-I<regex>` (`--ignore-matching-lines`): git recognises the attached form
        // and, like the other content-comparison options, it can drop pairs from the
        // raw output, so it is recorded rather than applied.
        "-I",
    ];
    EXACT.contains(&a) || PREFIX.iter().any(|p| a.starts_with(p))
}

/// Resolve a `<tree-ish>` to the id of the tree it names.
///
/// `Ok(None)` means git would have died here; the message is already on stderr and
/// the caller exits 128.
fn resolve_tree(repo: &gix::Repository, spec: &str) -> Result<Option<ObjectId>> {
    let Ok(id) = repo.rev_parse_single(spec) else {
        eprintln!("fatal: ambiguous argument '{spec}': {AMBIGUOUS_TAIL}");
        return Ok(None);
    };
    let Ok(object) = id.object() else {
        eprintln!("fatal: bad object {spec}");
        return Ok(None);
    };
    let oid = object.id;
    match object.peel_to_tree() {
        Ok(tree) => Ok(Some(tree.id)),
        Err(_) => {
            eprintln!("fatal: unable to read tree ({oid})");
            Ok(None)
        }
    }
}

/// The single-`<commit>` form: diff the commit against its parent(s), each diff
/// prefixed by the commit id unless suppressed.
///
/// Returns the exit code and sets `differed` when any change survived filtering, which
/// is what `--exit-code` reports on.
fn single_commit(
    repo: &gix::Repository,
    spec: &str,
    opts: &Opts,
    out: &mut Vec<u8>,
    differed: &mut bool,
) -> Result<u8> {
    let Ok(id) = repo.rev_parse_single(spec) else {
        eprintln!("fatal: ambiguous argument '{spec}': {AMBIGUOUS_TAIL}");
        return Ok(FATAL);
    };
    let Ok(object) = id.object() else {
        eprintln!("fatal: bad object {spec}");
        return Ok(FATAL);
    };
    let (found_id, found_kind) = (object.id, object.kind);
    let Ok(commit) = object.peel_to_commit() else {
        // git treats this as non-fatal: it complains and exits 0.
        eprintln!("error: object {found_id} is a {found_kind}, not a commit");
        return Ok(0);
    };

    let commit_id = commit.id;
    let new_tree = commit.tree_id()?.detach();
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();

    emit_commit_diff(repo, commit_id, &parents, new_tree, opts, out, differed)
}

/// git's `diff_tree_combined_merge()`: a merge rendered against every parent at once.
///
/// A path is part of a combined diff only when it differs from *all* parents — that is
/// what makes an ordinary clean merge print nothing but its commit-id line. The walk is
/// always recursive (`diff_tree_combined` sets `diffopts.flags.recursive = 1`) and the
/// commit-id line is emitted before the paths, whether or not any survive, which is
/// `diff_tree_combined`'s `show_log_first`.
///
/// `--cc` (`dense_combined_merges`) selects the combined *patch* format instead. This
/// port has no `xdl_diff3`-style combined hunk emitter, so a non-empty `--cc` path set
/// bails rather than print the wrong bytes; an empty one has no body either way.
fn combined_commit(
    repo: &gix::Repository,
    commit_id: ObjectId,
    parents: &[ObjectId],
    new_tree: ObjectId,
    opts: &Opts,
    out: &mut Vec<u8>,
    differed: &mut bool,
) -> Result<u8> {
    let mut walk_opts = opts.clone();
    walk_opts.recurse = true;
    walk_opts.show_trees = false;
    walk_opts.reverse = false;
    walk_opts.filter = ALL_STATUSES;
    walk_opts.route = None;

    // One change list per parent, in walk order.
    let mut per_parent: Vec<Vec<Change>> = Vec::with_capacity(parents.len());
    for p in parents {
        let before = tree_of(repo, *p)?;
        per_parent.push(collect(repo, Some(before), Some(new_tree), &walk_opts)?);
    }

    // Intersect on path: keep the first parent's order, drop anything a later parent
    // agrees with. `p->parent[i]` for the surviving paths is that parent's old side.
    let mut rows: Vec<(BString, Vec<Change>)> = Vec::new();
    'outer: for c in &per_parent[0] {
        let mut sides = vec![c.clone()];
        for others in &per_parent[1..] {
            match others.iter().find(|o| o.path == c.path) {
                Some(o) => sides.push(o.clone()),
                None => continue 'outer,
            }
        }
        rows.push((c.path.clone(), sides));
    }

    let term = if opts.nul { b'\0' } else { b'\n' };
    let sep = if opts.nul { b'\0' } else { b'\t' };
    if !opts.no_commit_id {
        emit_commit_header(repo, out, commit_id, opts)?;
    }
    if rows.is_empty() {
        return Ok(0);
    }
    *differed = true;
    if opts.dense_combined {
        bail!(
            "combined patch output (--cc) is not ported; {} path(s) of commit {commit_id} differ \
             from every parent (re-run with -c for the combined raw format)",
            rows.len()
        );
    }

    for (path, sides) in &rows {
        out.extend_from_slice(&opts.line_prefix);
        match opts.format {
            Format::NameOnly => {}
            _ => {
                if opts.format == Format::Raw {
                    for _ in sides {
                        out.push(b':');
                    }
                    for s in sides {
                        let m = s.old.map_or(0, |o| o.mode.value());
                        out.extend_from_slice(format!("{m:06o} ").as_bytes());
                    }
                    let rmode = sides[0].new.map_or(0, |n| n.mode.value());
                    out.extend_from_slice(format!("{rmode:06o}").as_bytes());
                    for s in sides {
                        let id = s.old.map_or_else(|| ObjectId::null(commit_id.kind()), |o| o.id);
                        out.extend_from_slice(format!(" {}", id.to_hex()).as_bytes());
                    }
                    let rid = sides[0]
                        .new
                        .map_or_else(|| ObjectId::null(commit_id.kind()), |n| n.id);
                    out.extend_from_slice(format!(" {} ", rid.to_hex()).as_bytes());
                }
                // The status column is one letter per parent, for `--raw` and
                // `--name-status` alike.
                for s in sides {
                    out.push(status(s));
                }
                out.push(sep);
            }
        }
        write_path(out, path, opts.nul);
        out.push(term);
    }
    Ok(0)
}

/// `--merge-base <a> <b>`: resolve the two revisions to commits, compute their single
/// merge base, and diff that base's tree against the second commit's tree. Emits no
/// commit-id line, matching git's two-argument form.
///
/// git validates this in stages: a revision that resolves to a non-commit draws the
/// same `error: object … is a … , not a commit` git prints, then merge-base search
/// yields nothing and the run dies `fatal: no merge base found`. Zero or several merge
/// bases are the fatal `no merge base found` / `multiple merge bases found` git prints
/// (exit 128).
fn merge_base_diff(
    repo: &gix::Repository,
    spec_a: &str,
    spec_b: &str,
    opts: &Opts,
    out: &mut Vec<u8>,
    differed: &mut bool,
) -> Result<u8> {
    let mut commits: Vec<ObjectId> = Vec::with_capacity(2);
    let mut all_commits = true;
    for spec in [spec_a, spec_b] {
        // Both specs already rev-parsed during argument classification, so this cannot
        // fail; resolve again to reach the object.
        let id = repo.rev_parse_single(spec)?;
        let object = id.object()?;
        let (oid, kind) = (object.id, object.kind);
        match object.peel_to_commit() {
            Ok(commit) => commits.push(commit.id),
            Err(_) => {
                eprintln!("error: object {oid} is a {kind}, not a commit");
                all_commits = false;
            }
        }
    }
    if !all_commits {
        eprintln!("fatal: no merge base found");
        return Ok(FATAL);
    }
    let bases = repo.merge_bases_many(commits[0], &commits[1..])?;
    match bases.len() {
        0 => {
            eprintln!("fatal: no merge base found");
            return Ok(FATAL);
        }
        1 => {}
        _ => {
            eprintln!("fatal: multiple merge bases found");
            return Ok(FATAL);
        }
    }
    let base_tree = tree_of(repo, bases[0].detach())?;
    let new_tree = tree_of(repo, commits[1])?;
    let changes = collect(repo, Some(base_tree), Some(new_tree), opts)?;
    *differed = !changes.is_empty();
    render_all(repo, out, &changes, opts)?;
    Ok(0)
}

/// The tree a commit points at.
fn tree_of(repo: &gix::Repository, commit: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_object(commit)?.peel_to_tree()?.id)
}

/// A tree entry, materialised so the borrow on the tree's buffer ends before we
/// recurse into child trees.
struct Entry {
    mode: EntryMode,
    name: BString,
    id: ObjectId,
}

/// Read the entries of `id` in stored (git-sorted) order; `None` yields no entries,
/// which is how the empty tree is represented throughout this module.
fn read_entries(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<Entry>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let tree = repo.find_tree(id)?;
    Ok(tree
        .decode()?
        .entries
        .iter()
        .map(|e| Entry {
            mode: e.mode,
            name: BString::from(e.filename.to_vec()),
            id: e.oid.to_owned(),
        })
        .collect())
}

/// Collect every change turning `old` into `new`, in git's emission order, with
/// `--diff-filter` applied.
fn collect(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    opts: &Opts,
) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    walk(repo, old, new, BStr::new(""), opts, &mut out)?;
    // `-R` reverses each pair before `--diff-filter` runs, so a reversed addition is
    // classified (and filtered) as a deletion, its numstat counts swap, and a
    // `--summary` create becomes a delete. Paths keep their walk order — git's `-R`
    // swaps sides in place, it does not re-sort.
    if opts.reverse {
        for c in &mut out {
            std::mem::swap(&mut c.old, &mut c.new);
        }
    }
    apply_filter(&mut out, opts.filter);
    Ok(out)
}

/// git's `tree-entry-comparison`: names compare byte-wise, with an implicit `/`
/// appended to tree entries. Two entries with the same name but different
/// "treeness" therefore never compare `Equal`.
fn entry_cmp(a: &Entry, b: &Entry) -> Ordering {
    let common = a.name.len().min(b.name.len());
    match a.name[..common].cmp(&b.name[..common]) {
        Ordering::Equal => {
            let ac = a.name.get(common).copied().or(a.mode.is_tree().then_some(b'/'));
            let bc = b.name.get(common).copied().or(b.mode.is_tree().then_some(b'/'));
            ac.cmp(&bc)
        }
        other => other,
    }
}

/// Depth-first merge-walk of two trees rooted at `prefix`, appending changes to `out`.
fn walk(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    prefix: &BStr,
    opts: &Opts,
    out: &mut Vec<Change>,
) -> Result<()> {
    let lhs = read_entries(repo, old)?;
    let rhs = read_entries(repo, new)?;
    let (mut i, mut j) = (0usize, 0usize);

    while i < lhs.len() || j < rhs.len() {
        let order = match (lhs.get(i), rhs.get(j)) {
            (Some(a), Some(b)) => entry_cmp(a, b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => unreachable!("loop condition guarantees one side has an entry"),
        };
        match order {
            Ordering::Equal => {
                let (a, b) = (&lhs[i], &rhs[j]);
                i += 1;
                j += 1;
                if a.mode == b.mode && a.id == b.id {
                    continue;
                }
                let path = join(prefix, a.name.as_bstr());
                // `Equal` implies both sides are trees or neither is.
                if a.mode.is_tree() {
                    emit_tree(out, opts, &path, Some(side(a)), Some(side(b)));
                    if opts.recurse && descend(&path, opts) {
                        walk(repo, Some(a.id), Some(b.id), path.as_bstr(), opts, out)?;
                    }
                } else if selects(&path, false, opts) {
                    out.push(Change {
                        old: Some(side(a)),
                        new: Some(side(b)),
                        path,
                    });
                }
            }
            Ordering::Less => {
                let a = &lhs[i];
                i += 1;
                let path = join(prefix, a.name.as_bstr());
                if a.mode.is_tree() {
                    emit_tree(out, opts, &path, Some(side(a)), None);
                    if opts.recurse && descend(&path, opts) {
                        walk(repo, Some(a.id), None, path.as_bstr(), opts, out)?;
                    }
                } else if selects(&path, false, opts) {
                    out.push(Change {
                        old: Some(side(a)),
                        new: None,
                        path,
                    });
                }
            }
            Ordering::Greater => {
                let b = &rhs[j];
                j += 1;
                let path = join(prefix, b.name.as_bstr());
                if b.mode.is_tree() {
                    emit_tree(out, opts, &path, None, Some(side(b)));
                    if opts.recurse && descend(&path, opts) {
                        walk(repo, None, Some(b.id), path.as_bstr(), opts, out)?;
                    }
                } else if selects(&path, false, opts) {
                    out.push(Change {
                        old: None,
                        new: Some(side(b)),
                        path,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Record the line for a changed tree entry itself.
///
/// git reports the tree when it is the leaf of the walk (no `-r`) or when `-t` asks
/// for tree entries alongside their recursed contents; with plain `-r` only the
/// contents are reported.
fn emit_tree(out: &mut Vec<Change>, opts: &Opts, path: &BString, old: Option<Side>, new: Option<Side>) {
    if (!opts.recurse || opts.show_trees) && selects(path, true, opts) {
        out.push(Change {
            old,
            new,
            path: path.clone(),
        });
    }
}

fn side(e: &Entry) -> Side {
    Side {
        mode: e.mode,
        id: e.id,
    }
}

fn join(prefix: &BStr, name: &BStr) -> BString {
    let mut p = BString::from(prefix.to_vec());
    if !p.is_empty() {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// Whether an entry is reported under the active path filters.
///
/// A filter selects the entry when it names it exactly, when the entry lives inside
/// the filtered directory, or — for a tree — when the filter points somewhere below
/// the tree (`-- d1/sub` still reports the top-level `d1` without `-r`).
fn selects(path: &BString, is_tree: bool, opts: &Opts) -> bool {
    let Some(specs) = opts.specs.as_ref() else { return true };
    if is_tree {
        // A tree is reported when the set selects it outright and when a spec names
        // something literally below it — the old `(is_tree && under(p, path))` leg.
        specs.selects_dir(path)
    } else {
        specs.matches(path)
    }
}

/// Whether the sub-tree at `path` can contain a filtered entry and so must be entered.
fn descend(path: &BString, opts: &Opts) -> bool {
    let Some(specs) = opts.specs.as_ref() else { return true };
    specs.may_contain_match(path)
}

fn render_all(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    changes: &[Change],
    opts: &Opts,
) -> Result<()> {
    // A routed run hands the walk output straight to `diff-pairs`, which owns every
    // patch, diffstat, dirstat, whitespace and rename format.
    if let Some(args) = &opts.route {
        super::diff_pairs::render_raw_stream(args, Some(raw_pair_stream(changes)), Some(out))?;
        return Ok(());
    }
    match opts.format {
        Format::NumStat => {
            for c in changes {
                render_numstat(repo, out, c, opts)?;
            }
        }
        Format::ShortStat => render_shortstat(repo, out, changes)?,
        Format::Summary => {
            for c in changes {
                render_summary(out, c);
            }
        }
        _ => {
            for c in changes {
                render(out, c, opts);
            }
        }
    }
    Ok(())
}

/// Serialize the walk output in `diff-pairs`' input format — the NUL-terminated raw
/// diff `diff-tree -z -r --raw` writes: `:<omode> <nmode> <ooid> <noid> <status>\0<path>\0`
/// with full object ids. An absent side is an all-zero mode and an all-zero id.
fn raw_pair_stream(changes: &[Change]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in changes {
        let (omode, ooid) = match c.old {
            Some(s) => (s.mode.value(), s.id),
            None => (0, ObjectId::null(c.new.map_or(gix::hash::Kind::Sha1, |s| s.id.kind()))),
        };
        let (nmode, noid) = match c.new {
            Some(s) => (s.mode.value(), s.id),
            None => (0, ObjectId::null(ooid.kind())),
        };
        out.extend_from_slice(
            format!(
                ":{omode:06o} {nmode:06o} {} {} {}",
                ooid.to_hex(),
                noid.to_hex(),
                status(c) as char
            )
            .as_bytes(),
        );
        out.push(0);
        out.extend_from_slice(&c.path);
        out.push(0);
    }
    out
}

/// The raw blob bytes on one side of a change; an absent side is the empty content.
fn side_bytes(repo: &gix::Repository, side: Option<Side>) -> Result<Vec<u8>> {
    match side {
        Some(s) => Ok(repo.find_object(s.id)?.detach().data),
        None => Ok(Vec::new()),
    }
}

/// git's `buffer_is_binary`: a NUL byte within the first `FIRST_FEW_BYTES` (8000)
/// marks the blob binary, which is what makes numstat print `-` for both counts.
fn is_binary(data: &[u8]) -> bool {
    const FIRST_FEW_BYTES: usize = 8000;
    let n = data.len().min(FIRST_FEW_BYTES);
    data[..n].contains(&0)
}

/// Added/removed line counts for one change, or `None` when either side is binary.
///
/// Uses git's default diff algorithm (Myers, non-minimal) over whole lines with the
/// trailing newline kept in each token, so a line that only gains or loses its final
/// newline counts as one removal plus one addition exactly as git reports.
fn numstat_counts(repo: &gix::Repository, c: &Change) -> Result<Option<(u32, u32)>> {
    let old = side_bytes(repo, c.old)?;
    let new = side_bytes(repo, c.new)?;
    if is_binary(&old) || is_binary(&new) {
        return Ok(None);
    }
    let input = InternedInput::new(sources::byte_lines(&old), sources::byte_lines(&new));
    let diff = Diff::compute(Algorithm::Myers, &input);
    Ok(Some((diff.count_additions(), diff.count_removals())))
}

/// One `--numstat` line: `<added>\t<deleted>\t<path>` (or `-\t-\t<path>` for a binary
/// change). Counts are always TAB-separated; `-z` only swaps the line terminator to
/// NUL and leaves the path unquoted, otherwise the path is C-quoted like git's.
fn render_numstat(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    c: &Change,
    opts: &Opts,
) -> Result<()> {
    match numstat_counts(repo, c)? {
        Some((add, del)) => {
            out.extend_from_slice(format!("{add}\t{del}\t").as_bytes());
        }
        None => out.extend_from_slice(b"-\t-\t"),
    }
    write_path(out, &c.path, opts.nul);
    out.push(if opts.nul { b'\0' } else { b'\n' });
    Ok(())
}

/// The single `--shortstat` line, aggregated over every changed blob. Binary blobs
/// count toward the file total but contribute no line counts.
fn render_shortstat(repo: &gix::Repository, out: &mut Vec<u8>, changes: &[Change]) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let mut insertions: u64 = 0;
    let mut deletions: u64 = 0;
    for c in changes {
        if let Some((add, del)) = numstat_counts(repo, c)? {
            insertions += add as u64;
            deletions += del as u64;
        }
    }
    print_stat_summary(out, changes.len() as u64, insertions, deletions);
    Ok(())
}

/// git's `print_stat_summary`: ` N file[s] changed[, X insertion[s](+)][, Y
/// deletion[s](-)]`. The insertion clause also shows when there are zero deletions and
/// vice versa, so a binary-only change still prints both zero clauses.
fn print_stat_summary(out: &mut Vec<u8>, files: u64, insertions: u64, deletions: u64) {
    let mut line = format!(" {files} file{} changed", if files != 1 { "s" } else { "" });
    if insertions > 0 || deletions == 0 {
        line.push_str(&format!(
            ", {insertions} insertion{}(+)",
            if insertions != 1 { "s" } else { "" }
        ));
    }
    if deletions > 0 || insertions == 0 {
        line.push_str(&format!(
            ", {deletions} deletion{}(-)",
            if deletions != 1 { "s" } else { "" }
        ));
    }
    line.push('\n');
    out.extend_from_slice(line.as_bytes());
}

/// One `--summary` line, or nothing for a plain modification. git emits ` create mode`
/// / ` delete mode` for additions and deletions and ` mode change <old> => <new>` when
/// two present sides carry different modes (an executable-bit flip or a type change).
/// Summary ignores `-z` entirely: it is always newline-terminated with a C-quoted path.
fn render_summary(out: &mut Vec<u8>, c: &Change) {
    match (c.old, c.new) {
        (None, Some(n)) => {
            out.extend_from_slice(format!(" create mode {:06o} ", n.mode.value()).as_bytes());
            write_path(out, &c.path, false);
            out.push(b'\n');
        }
        (Some(o), None) => {
            out.extend_from_slice(format!(" delete mode {:06o} ", o.mode.value()).as_bytes());
            write_path(out, &c.path, false);
            out.push(b'\n');
        }
        (Some(o), Some(n)) if o.mode.value() != n.mode.value() => {
            out.extend_from_slice(
                format!(" mode change {:06o} => {:06o} ", o.mode.value(), n.mode.value())
                    .as_bytes(),
            );
            write_path(out, &c.path, false);
            out.push(b'\n');
        }
        _ => {}
    }
}

/// Write a path the way git's emitters do: raw when `nul` (git's `-z` sets
/// `DIFF_FORMAT_NO_QUOTE`), otherwise through the shared `quote_c_style()` port in
/// [`super::diff_files`], which honours `core.quotePath`.
fn write_path(out: &mut Vec<u8>, path: &BString, nul: bool) {
    if nul {
        out.extend_from_slice(path);
    } else {
        out.extend_from_slice(&super::diff_files::quoted_name(path));
    }
}

/// The status letter git prints for a change.
fn status(c: &Change) -> u8 {
    match (c.old, c.new) {
        (None, _) => b'A',
        (_, None) => b'D',
        (Some(o), Some(n)) => {
            if o.mode.value() & IFMT != n.mode.value() & IFMT {
                b'T'
            } else {
                b'M'
            }
        }
    }
}

fn render(out: &mut Vec<u8>, c: &Change, opts: &Opts) {
    let sep = if opts.nul { b'\0' } else { b'\t' };
    let term = if opts.nul { b'\0' } else { b'\n' };

    match opts.format {
        // The stat family is rendered by `render_all` before this per-change path is
        // reached, so it never arrives here.
        Format::NumStat | Format::ShortStat | Format::Summary => {
            unreachable!("stat formats are rendered by render_all")
        }
        Format::NoOutput => {}
        Format::NameOnly => {
            write_path(out, &c.path, opts.nul);
            out.push(term);
        }
        Format::NameStatus => {
            out.push(status(c));
            out.push(sep);
            write_path(out, &c.path, opts.nul);
            out.push(term);
        }
        Format::Raw => {
            // ":<omode> <nmode> <ooid> <noid> <status>" then the separator and path.
            // Absent sides render as an all-zero mode and an all-zero object id.
            let zeros = "0".repeat(opts.abbrev);
            let (omode, ooid) = match c.old {
                Some(s) => (s.mode.value(), s.id.to_hex_with_len(opts.abbrev).to_string()),
                None => (0, zeros.clone()),
            };
            let (nmode, noid) = match c.new {
                Some(s) => (s.mode.value(), s.id.to_hex_with_len(opts.abbrev).to_string()),
                None => (0, zeros),
            };
            out.extend_from_slice(format!(":{omode:06o} {nmode:06o} {ooid} {noid} ").as_bytes());
            out.push(status(c));
            out.push(sep);
            write_path(out, &c.path, opts.nul);
            out.push(term);
        }
    }
}
