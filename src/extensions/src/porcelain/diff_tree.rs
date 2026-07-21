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
//! * `--raw` (the default), `--name-only`, `--name-status`, `-s`/`--no-patch`
//! * `--numstat`, `--shortstat`, `--summary` — the file-granular stat family, forced
//!   recursive like git; numstat line counts come from the vendored imara-diff (git's
//!   default Myers algorithm) and binary blobs print `-`; paths are C-quoted (or raw
//!   under `-z` for numstat) exactly as git's `quote_c_style`
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
//!   argument: <v>`, exit 128); valid values are recorded as unsupported.
//! * `--merge-base` requires exactly two commits; git enforces this after resolving
//!   revisions but before the missing-`<tree-ish>` check, so any other count — zero
//!   included — is `fatal: --merge-base only works with two commits` (exit 128). The
//!   valid two-commit case is implemented via the vendored merge-base computation
//!   (`gix::Repository::merge_bases_many`).
//! * `-h` — git's usage text on stdout, exit 129; no `<tree-ish>` — the same text on
//!   stderr, exit 129
//!
//! ### Options accepted but deliberately without effect
//!
//! [`is_ignorable`] lists options that only steer *patch* and *stat* rendering. Since
//! this module bails rather than emit those formats, the options provably cannot
//! change the bytes it does emit; each entry there was checked against stock git in
//! the raw, `-t`, `--name-status` and commit-id-line forms before being listed.
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
//! commit, a merge without `-m`), there are no bytes to get wrong and the exit status
//! is git's.
//!
//! Not implemented, and bailed on whenever they would matter:
//!
//! * `-p`/`-u`/`--patch` and the `--stat`/`--dirstat` family. Patch output abbreviates
//!   the `index` line to git's *auto* abbreviation length, which is derived from the
//!   repository's approximate object count (`core.abbrev` when set); the vendored
//!   crates expose no equivalent. `--stat`'s graph column depends on terminal-width
//!   scaling this port does not reproduce. (`--numstat`/`--shortstat`/`--summary` are
//!   implemented — see above.)
//! * bare `--abbrev` (no `=<n>`), for the same reason.
//! * `-c`/`--cc`/`--combined-all-paths` — combined merge diffs have no substrate in
//!   the vendored `gix-diff`.
//! * `--stdin`, `-v`, `--pretty`/`--format` — these need commit-message formatting,
//!   which belongs to the `log`/`show` machinery, not the tree diff.
//! * whitespace-insensitive comparison (`-w`, `-b`, `--ignore-*`). These are not
//!   patch-only: git re-compares blob *content* and drops a pair whose only
//!   difference is whitespace, so the raw output changes too.
//! * rename/copy detection (`-M`/`-C`/`-B`), pickaxe (`-S`/`-G`), `-R`, `-O`,
//!   `--find-copies-harder`, `--line-prefix`, `--relative`,
//!   `--submodule`/`--ignore-submodules`.
//! * magic (`:(...)`) and glob pathspecs.

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

/// git's `die_verify_filename()` text for the remaining arguments, which are already
/// known to be paths.
const NO_SUCH_PATH_TAIL: &str = "no such path in the working tree.\n\
                                 Use 'git <command> -- <path>...' to specify paths \
                                 that do not exist locally.";

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
struct Opts {
    recurse: bool,       // -r (also implied by -t)
    show_trees: bool,    // -t: report tree entries themselves while recursing
    nul: bool,           // -z: NUL instead of TAB/LF
    root: bool,          // --root: show a parentless commit as a full creation
    merges: bool,        // -m: diff a merge against every parent
    no_commit_id: bool,  // --no-commit-id
    always: bool,        // --always: print the commit id even with no changes
    exit_code: bool,     // --exit-code/--quiet: exit 1 when anything differs
    abbrev: usize,       // object-id width in the raw output
    filter: u32,         // --diff-filter mask, see `filter_bit`
    format: Format,
    paths: Vec<BString>, // literal path filters (empty = whole tree)
}

/// One file-level change, in the form the raw/name output needs.
///
/// `None` on a side means the entry is absent there (an addition or a deletion).
#[derive(Clone, Copy)]
struct Side {
    mode: EntryMode,
    id: ObjectId,
}

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

    // `-h` must work outside a repository, so it is answered before discovery.
    if args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "-h")
    {
        print!("{USAGE}");
        return Ok(ExitCode::from(USAGE_ERROR));
    }

    let repo = gix::discover(".")?;
    let hash = repo.object_hash();

    let mut opts = Opts {
        recurse: false,
        show_trees: false,
        nul: false,
        root: false,
        merges: false,
        no_commit_id: false,
        always: false,
        exit_code: false,
        abbrev: hash.len_in_hex(),
        filter: ALL_STATUSES,
        format: Format::Raw,
        paths: Vec::new(),
    };

    // The first option git accepts but this port cannot honour. Kept until we know
    // whether the invocation produces output at all; see the module documentation.
    let mut unsupported: Option<String> = None;
    // `--merge-base` is validated after revision resolution: git requires exactly two
    // commits and dies fatally (not a usage error) otherwise, even with zero revs.
    let mut merge_base = false;
    let mut revs: Vec<String> = Vec::new();
    let mut raw_paths: Vec<String> = Vec::new();

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
        if a.starts_with('-') && a != "-" {
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
                // git validates the operand count for `--merge-base` after it has
                // resolved the revisions (exactly two commits), so the flag is only
                // recorded here; the count check runs once parsing is complete. The
                // valid two-commit case diffs the merge base's tree against the second
                // commit's tree (see [`merge_base_diff`]).
                "--merge-base" => merge_base = true,
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
                // Normally answered before discovery; kept so `-h` never falls
                // through to the unknown-option arm.
                "-h" => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(USAGE_ERROR));
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
                    unsupported.get_or_insert_with(|| a.to_string());
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
                // empty string included), before revision resolution. A valid value is
                // still unsupported: it changes which gitlink pairs are reported, so it
                // is recorded like any other unimplemented option.
                _ if a.starts_with("--ignore-submodules=") => {
                    let v = &a["--ignore-submodules=".len()..];
                    if !matches!(v, "none" | "untracked" | "dirty" | "all") {
                        eprintln!("fatal: bad --ignore-submodules argument: {v}");
                        return Ok(ExitCode::from(FATAL));
                    }
                    unsupported.get_or_insert_with(|| a.to_string());
                }
                _ if is_ignorable(a) => {}
                _ if is_known_unsupported(a) => {
                    unsupported.get_or_insert_with(|| a.to_string());
                }
                // Not one of git's diff-tree options as far as this port knows; git
                // would answer with its usage text and 129, but guessing that here
                // would hide a genuinely missing option, so fail loudly instead.
                _ => bail!("unrecognized option {a:?}"),
            }
            i += 1;
            continue;
        }

        // A positional. It is a `<tree-ish>` exactly when it resolves as a revision.
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

    for p in &raw_paths {
        if p.starts_with(':') || p.bytes().any(|b| matches!(b, b'*' | b'?' | b'[')) {
            bail!("magic/glob pathspecs are not supported, got {p:?}");
        }
        opts.paths.push(BString::from(p.trim_end_matches('/').as_bytes()));
    }

    // The stat family is always file-granular in git: recursion is forced on and tree
    // entries are never reported, overriding whatever `-r`/`-t` asked for.
    if opts.format.is_stat() {
        opts.recurse = true;
        opts.show_trees = false;
    }

    // git checks the `--merge-base` operand count after resolving revisions but before
    // the missing-<tree-ish> usage error, so zero revs here is the fatal merge-base
    // message, not the usage text.
    if merge_base && revs.len() != 2 {
        eprintln!("fatal: --merge-base only works with two commits");
        return Ok(ExitCode::from(FATAL));
    }

    if revs.is_empty() {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(USAGE_ERROR));
    }

    let mut out: Vec<u8> = Vec::new();
    let mut differed = false;
    let code = if merge_base {
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

/// git's `verify_filename()`: `Some(code)` when the argument cannot be a path, after
/// printing the message git prints for it.
///
/// `first` selects between the two texts git uses — the leading non-revision argument
/// is diagnosed as a possibly-misspelt revision, later ones simply as missing paths.
fn verify_filename(arg: &str, first: bool) -> Option<u8> {
    if arg.starts_with('-') {
        eprintln!("fatal: option '{arg}' must come before non-option arguments");
        return Some(FATAL);
    }
    if looks_like_pathspec(arg) || std::path::Path::new(arg).symlink_metadata().is_ok() {
        return None;
    }
    if first {
        eprintln!("fatal: ambiguous argument '{arg}': {AMBIGUOUS_TAIL}");
    } else {
        eprintln!("fatal: {arg}: {NO_SUCH_PATH_TAIL}");
    }
    Some(FATAL)
}

/// git's `looks_like_pathspec()`: a leading `:` marks magic, and an unescaped glob
/// metacharacter means the argument was never meant to name a file directly.
fn looks_like_pathspec(arg: &str) -> bool {
    if arg.starts_with(':') {
        return true;
    }
    let mut escaped = false;
    for b in arg.bytes() {
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if matches!(b, b'*' | b'?' | b'[') {
            return true;
        }
    }
    false
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
    ];
    const PREFIX: &[&str] = &[
        "--src-prefix=",
        "--dst-prefix=",
        "--diff-algorithm=",
        "--inter-hunk-context=",
        "--output-indicator-new=",
        "--output-indicator-old=",
        "--output-indicator-context=",
        "--ws-error-highlight=",
        "--word-diff=",
        "--word-diff-regex=",
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
        "-R",
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
        "-v",
        "--pretty",
        "--format",
        // `--oneline` is `--pretty=oneline --abbrev-commit`; it renders a
        // commit-message line this port cannot produce.
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

    // Which "before" trees to diff against: `None` stands for the empty tree.
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
        for p in &parents {
            trees.push(Some(tree_of(repo, *p)?));
        }
        trees
    } else {
        vec![Some(tree_of(repo, parents[0])?)]
    };

    let term = if opts.nul { b'\0' } else { b'\n' };
    for before in befores {
        let changes = collect(repo, before, Some(new_tree), opts)?;
        *differed |= !changes.is_empty();
        if opts.always || (!opts.no_commit_id && !changes.is_empty()) {
            out.extend_from_slice(commit_id.to_hex().to_string().as_bytes());
            out.push(term);
        }
        render_all(repo, out, &changes, opts)?;
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

/// `true` if `path` starts with `pat` followed by a `/`.
fn under(path: &[u8], pat: &[u8]) -> bool {
    path.len() > pat.len() && path.starts_with(pat) && path[pat.len()] == b'/'
}

/// Whether an entry is reported under the active path filters.
///
/// A filter selects the entry when it names it exactly, when the entry lives inside
/// the filtered directory, or — for a tree — when the filter points somewhere below
/// the tree (`-- d1/sub` still reports the top-level `d1` without `-r`).
fn selects(path: &BString, is_tree: bool, opts: &Opts) -> bool {
    opts.paths.is_empty()
        || opts.paths.iter().any(|p| {
            path == p || under(path, p) || (is_tree && under(p, path))
        })
}

/// Whether the sub-tree at `path` can contain a filtered entry and so must be entered.
fn descend(path: &BString, opts: &Opts) -> bool {
    opts.paths.is_empty()
        || opts
            .paths
            .iter()
            .any(|p| path == p || under(path, p) || under(p, path))
}

fn render_all(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    changes: &[Change],
    opts: &Opts,
) -> Result<()> {
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

/// Write a path the way git's stat formats do: raw when `nul` (git's `-z`, no quoting),
/// otherwise through [`quote_c_style`].
fn write_path(out: &mut Vec<u8>, path: &BString, nul: bool) {
    if nul {
        out.extend_from_slice(path);
    } else {
        out.extend_from_slice(&quote_c_style(path));
    }
}

/// git's `quote_c_style` with the default `core.quotePath=true`: if any byte is a
/// control character, a double quote, a backslash, or has the high bit set, the whole
/// name is wrapped in double quotes with the standard C escapes (`\a \b \t \n \v \f \r
/// \" \\`) and every other out-of-range byte written as a three-digit octal `\ooo`.
/// A name needing none of that is returned unchanged.
fn quote_c_style(name: &[u8]) -> Vec<u8> {
    let needs_quote = name
        .iter()
        .any(|&b| b < 0x20 || b >= 0x80 || b == b'"' || b == b'\\');
    if !needs_quote {
        return name.to_vec();
    }
    let mut out = Vec::with_capacity(name.len() + 2);
    out.push(b'"');
    for &b in name {
        match b {
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0x0b => out.extend_from_slice(b"\\v"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b if b < 0x20 || b >= 0x80 => {
                out.extend_from_slice(format!("\\{b:03o}").as_bytes());
            }
            b => out.push(b),
        }
    }
    out.push(b'"');
    out
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
            out.extend_from_slice(&c.path);
            out.push(term);
        }
        Format::NameStatus => {
            out.push(status(c));
            out.push(sep);
            out.extend_from_slice(&c.path);
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
            out.extend_from_slice(&c.path);
            out.push(term);
        }
    }
}
