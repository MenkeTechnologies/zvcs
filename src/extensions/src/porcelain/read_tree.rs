//! `git read-tree` — read tree information into the index.
//!
//! Served natively through the vendored gitoxide crates, so tools on `PATH` observe
//! the same index. Every index/worktree mutation is serialized through
//! [`crate::lock::RepoLock`].
//!
//! `read-tree` prints nothing on success, so fidelity here is entirely about the
//! resulting index (entry set, stage, object id, mode, and the stat cache) and the
//! exit code / stderr text on the failure paths.
//!
//! ## Supported forms
//!
//! * `read-tree <tree-ish>...` — plain read: the index is replaced by the union of
//!   the named trees, later trees winning per path. Entries land at stage 0 with a
//!   zeroed stat cache, matching stock git.
//! * `read-tree` (no trees) — emits git's deprecation warning and empties the index.
//! * `read-tree --empty` — empty the index.
//! * `read-tree -m <tree-ish>` — one-way merge: like the plain read, except an index
//!   entry whose id and mode already match the tree keeps its stat cache and its
//!   sticky flags (skip-worktree, assume-valid, intent-to-add), because
//!   `oneway_merge()` re-adds the existing entry rather than the tree's. Refuses to
//!   run on an unmerged index, and refuses to clobber a modified tracked file.
//! * `read-tree --reset <tree-ish>` — as `-m`, but unmerged entries are discarded and
//!   the safety checks are skipped.
//! * `read-tree --prefix=<p>/ <tree-ish>` — keep the index and bind the tree under
//!   `<p>/`, refusing paths that already exist. `--prefix` is the *traversal base*,
//!   not a mode: it selects `bind_merge` only at one tree, and with two or three it
//!   runs the same `twoway_merge`/`threeway_merge` those counts always run, reading
//!   every tree path under `<p>/`. A prefix with no trailing `/` gains one, an empty
//!   prefix adds no separator at all (so `--prefix=` overlaps the index immediately),
//!   and one starting with `/` is refused before the repository is opened.
//! * `-u` (with `-m`/`--reset`/`--prefix`) — update the worktree for the paths whose
//!   index entry actually changed, and delete files the read drops. `--reset -u`
//!   additionally restores tracked files that are dirty or missing.
//! * `-i`, `-n`/`--dry-run`, `-q`/`--quiet`, `-v`/`--verbose`,
//!   `--index-output=<file>`, `--exclude-per-directory=<file>`,
//!   `--[no-]sparse-checkout`, `--[no-]recurse-submodules`, `--[no-]debug-unpack`.
//! * `--no-super-prefix` — the hidden `OPT__SUPER_PREFIX` in its unset sense,
//!   which restores the `NULL` this port never leaves, so it is an exact no-op.
//!   The *set* sense (`--super-prefix=<p>`) is refused: git threads the value
//!   through `super_prefixed()` (unpack-trees.c:85) to prefix the path in every
//!   `ERRORMSG()` the read can emit, and those messages are rendered here from
//!   the bare path. `--super-prefix` with no value is stock's own
//!   `` error: option `super-prefix' requires a value `` (129, no usage block).
//!
//! Up-to-dateness is decided the way `ie_match_stat()` decides it: from the entry's
//! cached `stat` data, not from content. A file whose mtime, size, inode or owner
//! moved is "not uptodate" even when its bytes are unchanged — that is what makes
//! `read-tree -m` refuse and what makes `--reset -u` rewrite the file. Content is
//! only consulted for a racily-clean entry, exactly as `ce_modified_check_fs` does.
//! `core.trustCTime` and `core.checkStat` select the fields compared, and
//! `core.fileMode` whether the executable bit counts.
//!
//! ## Configuration honoured
//!
//! * `core.maxTreeDepth` — the tree-recursion fail-safe. A tree deeper than the
//!   limit stops the read with `error: exceeded maximum allowed tree depth` and
//!   exit 128, leaving the index untouched; the limit defaults to 2048, is read
//!   through `git_config_int` (so an unparseable value is fatal and a negative
//!   one rejects every tree), and is not charged for a `--prefix`.
//! * `core.fsync` / `core.fsyncMethod` — the index write is hardened when the
//!   `index` component is in the set.
//!
//! Options are parsed the way `parse-options` does: short switches cluster (`-mu`),
//! value-taking options accept both `--name=<v>` and `--name <v>`, every boolean has
//! its `--no-` form, `--` ends the option scan, and a usage error prints git's usage
//! block and exits 129.
//!
//! * `read-tree -m <old> <new>` — two-tree merge (`twoway_merge`), the "carry
//!   forward" table of `git-read-tree(1)`.
//! * `read-tree -m <base> <ours> <theirs>` — three-tree merge (`threeway_merge`),
//!   leaving unresolvable paths at stages 1/2/3. `--aggressive` resolves the
//!   trivial delete/delete and identical-add cases; `--trivial` refuses the whole
//!   merge (`Merge requires file-level merging`) if any path needed file-level
//!   merging. Four or more trees also run `threeway_merge`, with the last two trees
//!   as head and remote and the rest as ancestors; the ninth tree is refused
//!   (`I cannot read more than 8 trees`).
//!
//! ## Known deviations
//!
//! * The multi-tree merges decide per path over the union of the index's and the
//!   trees' paths rather than through `traverse_trees()`'s simultaneous walk. The
//!   merge functions themselves are ported verbatim; what the walk adds on top is
//!   directory/file-conflict detection — git substitutes `o->df_conflict_entry`
//!   for a slot whose path is a directory in that tree — and sparse-directory
//!   handling. A path that is a file in one tree and a directory in another is
//!   therefore merged as if the directory side were absent. Because no sparse
//!   directory entry is ever built here, the index this verb writes is never
//!   sparse, so `prime_cache_tree()`'s sparse-directory branch
//!   (cache-tree.c:872-891) cannot be reached through `read-tree`.
//! * The `-u` untracked-collision check rejects any existing file at a path the read
//!   adds; git additionally permits it when the file is `.gitignore`d.
//! * `--sparse-checkout` is accepted but never applies a sparse filter, and
//!   `--recurse-submodules` never descends into submodules — both need substrate
//!   this port does not have, so they behave as their `--no-` counterparts.
//! * `--exclude-per-directory` reproduces git's "meaningless unless -u" and
//!   "must be .gitignore" gates, but the ignore file it names is not consulted,
//!   since `-u` here never overwrites an existing untracked file in the first place.
//! * `--debug-unpack` is accepted and silent; there is no `unpack-trees` to trace.
//! * `read-tree --help` renders a man page under stock git and is not reproduced.

// `print!`/`println!` here go through git's stdout buffer. `merge` reaches this
// module in-process and arms that buffer (see `crate::cstdio`), so both halves of
// its output have to be buffered or they interleave against each other; run as
// its own command nothing arms it and these are unbuffered writes as before.
use crate::cstdio::print;
use anyhow::Result;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::stat::Options;
use gix::index::entry::{Flags, Mode, Stat};

use super::{Arg, LongOpt};

/// `cmd_read_tree()`'s `struct option read_tree_options[]`
/// (builtin/read-tree.c:130-170), in table order, as [`super::resolve_long`]
/// reads it. `--index-output`, `--prefix` and `--exclude-per-directory` carry
/// `PARSE_OPT_NONEG`; `--no-sparse-checkout` is an `OPT_BOOL` whose name already
/// carries the `no-`, so `--sparse-checkout` is that same entry unset.
pub(super) const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "super-prefix", neg: true, arg: Arg::Required },
    LongOpt { name: "index-output", neg: false, arg: Arg::Required },
    LongOpt { name: "empty", neg: true, arg: Arg::None },
    LongOpt { name: "verbose", neg: true, arg: Arg::None },
    LongOpt { name: "trivial", neg: true, arg: Arg::None },
    LongOpt { name: "aggressive", neg: true, arg: Arg::None },
    LongOpt { name: "reset", neg: true, arg: Arg::None },
    LongOpt { name: "prefix", neg: false, arg: Arg::Required },
    LongOpt { name: "exclude-per-directory", neg: false, arg: Arg::Required },
    LongOpt { name: "dry-run", neg: true, arg: Arg::None },
    LongOpt { name: "no-sparse-checkout", neg: true, arg: Arg::None },
    LongOpt { name: "debug-unpack", neg: true, arg: Arg::None },
    LongOpt { name: "recurse-submodules", neg: true, arg: Arg::Optional },
    LongOpt { name: "quiet", neg: true, arg: Arg::None },
];

/// Parsed command line for a single `read-tree` invocation.
#[derive(Default)]
pub(super) struct Opts {
    merge: bool,               // -m
    reset: bool,               // --reset
    update: bool,              // -u
    index_only: bool,          // -i
    dry_run: bool,             // -n/--dry-run
    read_empty: bool,          // --empty
    prefix: Option<String>,    // --prefix=<p>
    aggressive: bool,          // --aggressive
    trivial: bool,             // --trivial
    index_output: Option<PathBuf>, // --index-output=<file>
    trees: Vec<String>,
}

impl Opts {
    /// Whether git would set `opts.merge` internally: `--reset` and `--prefix` both
    /// imply it, which is what gates `-u`/`-i` and the "at least one tree" check.
    fn merge_like(&self) -> bool {
        self.merge || self.reset || self.prefix.is_some()
    }

    /// Whether the merge safety checks apply. `--reset` explicitly opts out of them.
    fn checked(&self) -> bool {
        (self.merge || self.prefix.is_some()) && !self.reset && !self.index_only
    }
}

/// git's own `read-tree` usage block, byte for byte (`git read-tree -h`).
///
/// Reproduced verbatim because `parse-options` prints it on every usage error, and
/// parity compares stderr bytes. The trailing blank line is part of git's output.
const USAGE: &str = "\
usage: git read-tree [(-m [--trivial] [--aggressive] | --reset | --prefix=<prefix>)
                     [-u | -i]] [--index-output=<file>] [--no-sparse-checkout]
                     (--empty | <tree-ish1> [<tree-ish2> [<tree-ish3>]])

    --index-output <file> write resulting index to <file>
    --[no-]empty          only empty the index
    -v, --[no-]verbose    be verbose

Merging
    -m                    perform a merge in addition to a read
    --[no-]trivial        3-way merge if no file level merging required
    --[no-]aggressive     3-way merge in presence of adds and removes
    --[no-]reset          same as -m, but discard unmerged entries
    --prefix <subdirectory>/
                          read the tree into the index under <subdirectory>/
    -u                    update working tree with merge result
    --exclude-per-directory <gitignore>
                          allow explicitly ignored files to be overwritten
    -i                    don't check the working tree after merging
    -n, --[no-]dry-run    don't update the index or the work tree
    --no-sparse-checkout  skip applying sparse checkout filter
    --sparse-checkout     opposite of --no-sparse-checkout
    --[no-]debug-unpack   debug unpack-trees
    --[no-]recurse-submodules[=<checkout>]
                          control recursive updating of submodules
    -q, --[no-]quiet      suppress feedback messages

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]super-prefix`.
/// Captured byte-for-byte from stock git 2.55.0's `git read-tree --help-all`.
const USAGE_ALL: &str = r#"usage: git read-tree [(-m [--trivial] [--aggressive] | --reset | --prefix=<prefix>)
                     [-u | -i]] [--index-output=<file>] [--no-sparse-checkout]
                     (--empty | <tree-ish1> [<tree-ish2> [<tree-ish3>]])

    --[no-]super-prefix <prefix>
                          prefixed path to initial superproject
    --index-output <file> write resulting index to <file>
    --[no-]empty          only empty the index
    -v, --[no-]verbose    be verbose

Merging
    -m                    perform a merge in addition to a read
    --[no-]trivial        3-way merge if no file level merging required
    --[no-]aggressive     3-way merge in presence of adds and removes
    --[no-]reset          same as -m, but discard unmerged entries
    --prefix <subdirectory>/
                          read the tree into the index under <subdirectory>/
    -u                    update working tree with merge result
    --exclude-per-directory <gitignore>
                          allow explicitly ignored files to be overwritten
    -i                    don't check the working tree after merging
    -n, --[no-]dry-run    don't update the index or the work tree
    --no-sparse-checkout  skip applying sparse checkout filter
    --sparse-checkout     opposite of --no-sparse-checkout
    --[no-]debug-unpack   debug unpack-trees
    --[no-]recurse-submodules[=<checkout>]
                          control recursive updating of submodules
    -q, --[no-]quiet      suppress feedback messages

"#;

/// Report a git-style fatal and return git's exit code for it.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// An unrecognised option: an `error:` line, the usage block, and 129.
fn usage_err(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// A malformed value for a *known* option. Still 129, but `parse-options` prints only
/// the `error:` line here — the usage block is reserved for unrecognised options.
fn opt_err(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(129))
}

/// git's `git_parse_maybe_bool` spelling set, used by `--recurse-submodules=<v>`.
/// The empty string is false, matching `git_parse_maybe_bool_text`.
fn parse_maybe_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" | "1" => Some(true),
        "no" | "off" | "false" | "0" | "" => Some(false),
        _ => None,
    }
}

/// Report a git-style `error:` line and return git's exit code for it.
fn rejected(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(128))
}

/// git's `core.maxTreeDepth`: the recursion fail-safe `read_tree_at()` checks on
/// entry, defaulting to 2048 off Windows (`git-config(1)`, git 2.55.0).
///
/// Read through `git_config_int`, so a value it cannot parse is fatal and a
/// *negative* one is a perfectly good number that simply rejects every tree —
/// both reproduced here (`Err` carries git's `die_bad_number` line).
fn max_tree_depth(repo: &gix::Repository) -> std::result::Result<i64, String> {
    Ok(crate::config::config_int(repo, "core.maxTreeDepth")?.unwrap_or(2048))
}

/// git's `error("exceeded maximum allowed tree depth")` — an `error:` line, but
/// exit 128, because `read_tree_at()`'s failure propagates out of `read_tree()`
/// as a die rather than as a warning.
const TOO_DEEP: &str = "exceeded maximum allowed tree depth";

/// Whether every path `index` took from a tree stays inside `max` levels of tree
/// recursion.
///
/// `read_tree_at()` enters the root tree at depth 0 and checks `depth >
/// max_allowed_tree_depth` on each recursive call, so a blob at `a/b/c/d/f.txt`
/// is reached from a tree at depth 4 and needs `core.maxTreeDepth >= 4` — which
/// is exactly the number of `/` in its path, and is how the depth is counted
/// here. Measuring the paths rather than re-walking the trees keeps the check
/// free: the entries are already in memory, and reading them again would double
/// the cost of every `read-tree` for a fail-safe that essentially never fires.
///
/// The one divergence: a tree whose deepest branch ends in an *empty* tree
/// contributes no index entry, so git counts one level that this does not.
fn within_tree_depth(index: &gix::index::File, max: i64) -> bool {
    let backing = index.path_backing();
    index.entries().iter().all(|e| {
        let depth = e.path_in(backing).iter().filter(|b| **b == b'/').count();
        i64::try_from(depth).is_ok_and(|d| d <= max)
    })
}

pub fn read_tree(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 (dispatch strips it today).
    let argv: &[String] = match args.first() {
        Some(a) if a == "read-tree" => &args[1..],
        _ => args,
    };

    let o = match parse_args(argv)? {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };
    finish(o)
}

/// `parse_options()` over git's `read-tree` option table.
///
/// Split out from [`read_tree`] because `git-merge-resolve.sh` runs
/// `git read-tree -u -m --aggressive $bases $head $remotes` with the operand
/// list interpolated *unquoted*, so an operand that looks like an option is one:
/// `git cherry-pick -Xtheirs --strategy=resolve` reaches here as `--theirs` and
/// dies as an unknown option. `super::merge_resolve` runs this scan for exactly
/// that reason, so the refusal is read-tree's own rather than a second opinion
/// about which operands are acceptable.
///
/// `Err(code)` means git already printed the diagnosis.
pub(super) fn parse_args(argv: &[String]) -> Result<std::result::Result<Opts, ExitCode>> {
    /// The scan's early exits go through `read_tree`'s own helpers, which return
    /// `Result<ExitCode>`; lift them into this function's shape.
    macro_rules! reject {
        ($e:expr) => {
            return Ok(Err($e?))
        };
    }

    let mut o = Opts::default();
    let mut only_positionals = false;
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;

        if only_positionals || a == "-" || !a.starts_with('-') {
            o.trees.push(a.to_string());
            continue;
        }
        if a == "--" {
            only_positionals = true;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): an exact match, tested after the `--` break
        // above and before any table lookup, so it never abbreviates and never
        // takes an `=<value>`. It renders `USAGE_FULL`, which for `read-tree`
        // is `USAGE` plus the hidden `--super-prefix`.
        if a == "--help-all" {
            print!("{USAGE_ALL}");
            return Ok(Err(ExitCode::from(129)));
        }

        // ---- Short option clusters (`-mu` is accepted by parse-options). ----
        if !a.starts_with("--") {
            for c in a[1..].chars() {
                match c {
                    'm' => o.merge = true,
                    'u' => o.update = true,
                    'i' => o.index_only = true,
                    'n' => o.dry_run = true,
                    // Progress and feedback: this port emits neither, so both no-op.
                    'v' | 'q' => {}
                    'h' => {
                        print!("{USAGE}");
                        return Ok(Err(ExitCode::from(129)));
                    }
                    _ => reject!(usage_err(format!("unknown switch `{c}'"))),
                }
            }
            continue;
        }

        // ---- Long options: `--name` or `--name=<value>`. ----
        // The name is resolved the way `parse_long_opt()` resolves it first, so a
        // unique abbreviation reaches the arm its full spelling reaches.
        let resolved = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(Err(super::ambiguous_option(a, &first, &second, USAGE)))
            }
        };
        let a = resolved.as_ref();
        let body = &a[2..];
        let (name, inline) = match body.find('=') {
            Some(eq) => (&body[..eq], Some(&body[eq + 1..])),
            None => (body, None),
        };

        // Every arm below that takes no argument rejects an inline `=value`, which is
        // what parse-options does for OPTION_SET_INT / OPTION_BOOL.
        macro_rules! no_value {
            () => {
                if inline.is_some() {
                    reject!(opt_err(format!("option `{name}' takes no value")));
                }
            };
        }
        // A value-taking option accepts `--name=<v>` or `--name <v>`.
        macro_rules! value {
            () => {
                match inline {
                    Some(v) => v.to_string(),
                    None => match argv.get(i) {
                        Some(next) => {
                            i += 1;
                            next.clone()
                        }
                        None => reject!(opt_err(format!("option `{name}' requires a value"))),
                    },
                }
            };
        }

        match name {
            "reset" => {
                no_value!();
                o.reset = true;
            }
            "no-reset" => {
                no_value!();
                o.reset = false;
            }
            "dry-run" => {
                no_value!();
                o.dry_run = true;
            }
            "no-dry-run" => {
                no_value!();
                o.dry_run = false;
            }
            "empty" => {
                no_value!();
                o.read_empty = true;
            }
            "no-empty" => {
                no_value!();
                o.read_empty = false;
            }
            // Feedback-only switches: this port is silent either way.
            "verbose" | "no-verbose" | "quiet" | "no-quiet" => no_value!(),
            // `--trivial`/`--aggressive` only tune the three-tree merge.
            "trivial" => {
                no_value!();
                o.trivial = true;
            }
            "no-trivial" => {
                no_value!();
                o.trivial = false;
            }
            "aggressive" => {
                no_value!();
                o.aggressive = true;
            }
            "no-aggressive" => {
                no_value!();
                o.aggressive = false;
            }
            // Sparse checkout is never applied by this port, so both directions no-op.
            "sparse-checkout" | "no-sparse-checkout" => no_value!(),
            // `unpack-trees` tracing has no analogue here.
            "debug-unpack" | "no-debug-unpack" => no_value!(),
            "no-recurse-submodules" => no_value!(),
            "recurse-submodules" => {
                // The optional value is a boolean; anything else is fatal in git.
                if let Some(v) = inline {
                    if parse_maybe_bool(v).is_none() {
                        reject!(fatal(format!("bad recurse-submodules argument: {v}")));
                    }
                }
            }
            // `OPT__SUPER_PREFIX` (parse-options.h:573-575) is an
            // `OPT_STRING_F(..., PARSE_OPT_HIDDEN)`, so it is negatable and its
            // set sense demands a value. The unset sense restores the default
            // `NULL`, which is the only state this port has, so it is an exact
            // no-op rather than an unimplemented flag.
            "no-super-prefix" => no_value!(),
            "super-prefix" => {
                // The set sense demands a value, and that refusal is stock's own
                // — it outranks this port's gap, so it has to come first. Spelled
                // out rather than via `value!()` because the value is never used:
                // the macro's `i += 1` would be a dead advance.
                if inline.is_none() && argv.get(i).is_none() {
                    reject!(opt_err("option `super-prefix' requires a value"));
                }
                // git threads this through `super_prefixed()` (unpack-trees.c:85)
                // to prefix the path in every `ERRORMSG()` this read can emit.
                // Those messages are rendered here from the bare path, so
                // honouring the flag would mean carrying it into each of them;
                // accepting and ignoring it would silently print unprefixed
                // paths where git prints prefixed ones.
                anyhow::bail!(
                    "unsupported --super-prefix (unpack-trees messages here are not super-prefixed)"
                );
            }
            "prefix" => o.prefix = Some(value!()),
            "index-output" => o.index_output = Some(PathBuf::from(value!())),
            "exclude-per-directory" => {
                let ignore_file = value!();
                // git checks both of these inside the option callback
                // (`exclude_per_directory_cb`, builtin/read-tree.c:57-71), so they
                // fire during the scan — before the tree-ishes are resolved, and
                // the `-u` test only sees the `-u` seen so far.
                if !o.update {
                    reject!(fatal("--exclude-per-directory is meaningless unless -u"));
                }
                if ignore_file != ".gitignore" {
                    reject!(fatal("--exclude-per-directory argument must be .gitignore"));
                }
            }
            _ => reject!(usage_err(format!("unknown option `{body}'"))),
        }
    }

    Ok(Ok(o))
}

/// The one check `cmd_read_tree` makes before it touches the repository, and so
/// the only one a caller can run without also performing the read.
///
/// Shared with `super::merge_resolve` for the same reason [`parse_args`] is: the
/// mode conflict is reachable from an operand that looks like an option
/// (`merge-resolve --prefix <base> -- …`), and the refusal has to be read-tree's.
pub(super) fn check_options(o: &Opts) -> Result<std::result::Result<(), ExitCode>> {
    if 1 < usize::from(o.merge) + usize::from(o.reset) + usize::from(o.prefix.is_some()) {
        return Ok(Err(fatal("Which one? -m, --reset, or --prefix?")?));
    }
    // "Prefix should not start with a directory separator" (builtin/read-tree.c:181-183).
    // It sits between the mode conflict and the lock, so it outranks every later
    // diagnosis: `--prefix=/x does-not-exist` reports the prefix, not the tree-ish.
    if o.prefix.as_deref().is_some_and(|p| p.starts_with('/')) {
        return Ok(Err(fatal("Invalid prefix, prefix cannot start with '/'")?));
    }
    Ok(Ok(()))
}

/// git's cap on how many trees one `read-tree` may name (`MAX_UNPACK_TREES`,
/// unpack-trees.h:10). `list_tree()` refuses the ninth (builtin/read-tree.c:33-34).
const MAX_UNPACK_TREES: usize = 8;

/// The base every tree path is read under, as `setup_traverse_info()` derives it
/// from `o->prefix` (tree-walk.c:192-205).
///
/// git drops one trailing `/` there and `make_traverse_path()` puts one back
/// between the base and each path, so `sub` and `sub/` both name `sub/` while
/// `sub//` names `sub//`. An empty prefix leaves `info->pathlen` at 0 and no
/// separator is inserted at all — which is why `--prefix=` binds the tree over
/// the index at the tree's own paths and immediately overlaps with it.
///
/// The prefix costs no tree-recursion levels: it is the traversal's starting
/// base, not a level descended into (verified against git 2.55.0 — a one-level
/// tree binds under a three-segment prefix even at `core.maxTreeDepth=0`).
fn traverse_base(prefix: &str) -> String {
    if prefix.is_empty() || prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

/// Everything `cmd_read_tree` does once `parse_options` has returned: the
/// argument validation, then the read itself.
fn finish(o: Opts) -> Result<ExitCode> {
    // ---- Argument validation, in git's own order (builtin/read-tree.c). ----
    if let Err(code) = check_options(&o)? {
        return Ok(code);
    }

    let repo = gix::discover(".")?;

    // Resolve every tree-ish before any other check, exactly like git's read loop.
    let mut tree_ids: Vec<ObjectId> = Vec::with_capacity(o.trees.len());
    for spec in &o.trees {
        // `repo_get_oid()` only has to *name* an object: a full-length hex
        // string is its own answer and the odb is never asked (see
        // [`crate::objname::full_hex`]), so an absent-but-well-formed id gets
        // past this check and fails in `list_tree()` below instead.
        let Some(id) = crate::objname::resolve(&repo, spec.as_str()) else {
            return fatal(format!("Not a valid object name {spec}"));
        };
        // `list_tree()` counts before it parses, so the ninth tree-ish is refused
        // for being the ninth even when it would not have peeled to a tree — but
        // only after its name resolved, which is `cmd_read_tree`'s own order.
        if tree_ids.len() >= MAX_UNPACK_TREES {
            return fatal(format!("I cannot read more than {MAX_UNPACK_TREES} trees"));
        }
        // `list_tree()`'s `repo_parse_tree_indirect()` returns NULL both for an
        // object the repository does not have and for one that will not peel to
        // a tree; either way `cmd_read_tree` reports the same message.
        let peeled = repo
            .find_object(id)
            .ok()
            .and_then(|obj| obj.peel_to_tree().ok());
        let Some(tree) = peeled else {
            return fatal(format!("failed to unpack tree object {spec}"));
        };
        tree_ids.push(tree.id);
    }

    if tree_ids.is_empty() && !o.read_empty && !o.merge_like() {
        eprintln!("warning: read-tree: emptying the index with no arguments is deprecated; use --empty");
    } else if !tree_ids.is_empty() && o.read_empty {
        return fatal("passing trees as arguments contradicts --empty");
    }

    if o.index_only && o.update {
        return fatal("-u and -i at the same time makes no sense");
    }
    if (o.update || o.index_only) && !o.merge_like() {
        let flag = if o.update { "-u" } else { "-i" };
        return fatal(format!("{flag} is meaningless without -m, --reset, or --prefix"));
    }
    if o.merge_like() && tree_ids.is_empty() {
        return fatal("you must specify at least one tree to merge");
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Bound first: `index_or_empty` yields an Arc<FileSnapshot<File>>, and the
    // deref chain to &File only resolves once it is a named binding.
    let index = repo.index_or_empty()?;
    let old = gix::index::File::clone(&index);

    if o.merge_like() && !o.reset && old.entries().iter().any(|e| e.stage_raw() != 0) {
        return fatal("You need to resolve your current index first");
    }

    // `core.maxTreeDepth` bounds the tree recursion every form below performs, so
    // it is resolved once here; an unreadable value is fatal exactly as
    // `git_config_int` makes it.
    let depth_limit = match max_tree_depth(&repo) {
        Ok(limit) => limit,
        Err(message) => return fatal(message),
    };

    // Resolved before the write so `core.fsync`'s diagnostics land where git
    // prints them — while reading config, ahead of the work — rather than after.
    let fsync = match crate::config::FsyncPolicy::load(&repo) {
        Ok(policy) => policy,
        Err(message) => return fatal(message),
    };

    // The tree count alone picks the unpack function (builtin/read-tree.c:237-259):
    // two trees select `twoway_merge` and three or more `threeway_merge`, whether
    // the merge was asked for with `-m`, `--reset` or `--prefix`. Only the one-tree
    // case reads `opts.prefix`, and only to choose `bind_merge` over `oneway_merge`
    // — with more trees the prefix stays what it always is, the traversal base.
    if o.merge_like() && tree_ids.len() > 1 {
        return multi_tree_read(&repo, &o, &old, &tree_ids, depth_limit, &fsync);
    }

    // ---- Build the index this invocation wants to end up with. ----
    let mut new_index = if o.read_empty || tree_ids.is_empty() {
        gix::index::File::from_state(
            gix::index::State::new(repo.object_hash()),
            repo.index_path(),
        )
    } else if let Some(prefix) = &o.prefix {
        match bind_prefix(&repo, &old, tree_ids[0], prefix, depth_limit)? {
            Ok(index) => index,
            Err(code) => return Ok(code),
        }
    } else {
        match union_of_trees(&repo, &tree_ids, depth_limit)? {
            Ok(index) => index,
            Err(code) => return Ok(code),
        }
    };

    // `-m`/`--reset` carry the stat cache of entries the tree leaves untouched, so a
    // later `git status` does not see the whole tree as freshly modified.
    //
    // They carry the entry's sticky flags for the same reason: `oneway_merge()` reacts to
    // `same(old, a)` with `add_entry(o, old, update, CE_STAGEMASK)`, re-adding the *existing*
    // cache entry rather than the one built from the tree, so every `ce_flags` bit but the
    // stage survives the read. Dropping them turns a sparse-checkout index inside out —
    // `read-tree -m -u HEAD` cleared `CE_SKIP_WORKTREE` on the cone-excluded paths and left
    // `git status` reporting them as deleted. `CE_EXTENDED` rides along because
    // `Entry::write_to` only serialises the extended flag word (which holds skip-worktree and
    // intent-to-add) when it is set.
    const STICKY: gix::index::entry::Flags = gix::index::entry::Flags::ASSUME_VALID
        .union(gix::index::entry::Flags::EXTENDED)
        .union(gix::index::entry::Flags::INTENT_TO_ADD)
        .union(gix::index::entry::Flags::SKIP_WORKTREE);
    let old_map = stage0_map(&old);
    if o.merge || o.reset {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            if let Some((id, mode, stat, flags)) = old_map.get(&e.path_in(&backing).to_owned()) {
                if *id == e.id && *mode == e.mode {
                    e.stat = *stat;
                    e.flags |= *flags & STICKY;
                }
            }
        }
    }

    // ---- Classify the paths this read touches. ----
    let new_map = stage0_map(&new_index);
    let new_paths: BTreeSet<BString> = new_map.keys().cloned().collect();
    let old_paths: BTreeSet<BString> = old_map.keys().cloned().collect();

    // Changed = added by this read, or present before with a different id/mode.
    let changed: BTreeSet<BString> = new_paths
        .iter()
        .filter(|p| match (old_map.get(*p), new_map.get(*p)) {
            (Some((oid, omode, _, _)), Some((nid, nmode, _, _))) => oid != nid || omode != nmode,
            _ => true,
        })
        .cloned()
        .collect();
    // `--prefix` never drops anything; every other form replaces the index wholesale.
    let removed: BTreeSet<BString> = if o.prefix.is_some() {
        BTreeSet::new()
    } else {
        old_paths.difference(&new_paths).cloned().collect()
    };

    // ---- Merge safety checks, before touching anything. ----
    // Needed for the checks, and for `--reset -u` to know which files to restore.
    // Both are decided from `stat` data against the OLD index, which is what
    // `verify_uptodate_1`/`oneway_merge` compare against (`o->src_index`).
    let (dirty, absent) = if o.checked() || (o.reset && o.update) {
        let stat_ctx = StatCtx::new(&repo, &old)?;
        let dirty = worktree_dirty(&repo, &old, &stat_ctx);
        let absent = if o.reset && o.update {
            worktree_absent(&repo, &old, &stat_ctx)
        } else {
            HashSet::new()
        };
        (dirty, absent)
    } else {
        (HashSet::new(), HashSet::new())
    };

    if o.checked() {
        for path in old_paths.union(&new_paths) {
            let loses_content = removed.contains(path)
                || (changed.contains(path) && old_map.contains_key(path));
            if loses_content && dirty.contains(path) {
                return rejected(format!(
                    "Entry '{}' not uptodate. Cannot merge.",
                    path.to_str_lossy()
                ));
            }
            // A file we are about to create must not already exist untracked, but
            // only when we would actually write it (`-u`).
            if o.update && changed.contains(path) && !old_map.contains_key(path) {
                let exists = repo
                    .workdir_path(path.as_bstr())
                    .is_some_and(|p| p.symlink_metadata().is_ok());
                if exists {
                    return rejected(format!(
                        "Untracked working tree file '{}' would be overwritten by merge.",
                        path.to_str_lossy()
                    ));
                }
            }
        }
    }

    if o.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // ---- Worktree update. ----
    if o.update {
        // `--reset` also restores tracked files that drifted from the index.
        let mut wanted = changed.clone();
        if o.reset {
            // `oneway_merge`: for an entry the tree leaves alone, `--reset -u`
            // still writes the file when `lstat()` fails *or* `ie_match_stat()`
            // reports a change — so a deleted worktree file comes back even
            // though its index entry never moved.
            wanted.extend(
                new_paths
                    .iter()
                    .filter(|p| dirty.contains(*p) || absent.contains(*p))
                    .cloned(),
            );
        }
        checkout_subset(&repo, &mut new_index, &wanted)?;
        for path in &removed {
            if let Some(full) = repo.workdir_path(path.as_bstr()) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    // ---- Persist. ----
    if let Some(out) = &o.index_output {
        new_index.set_path(out.clone());
    }
    // "When reading only one tree (either the most basic form, `-m ent` or
    // `--reset ent` form), we can obtain a fully valid cache-tree because the index
    // must match exactly what came from the tree" (builtin/read-tree.c:281-290).
    // That precondition holds here for the same reason it holds in git: every entry
    // above took its id and mode from `tree_ids[0]`, and what `-m`/`--reset` carry
    // forward from the old index is stat data and sticky flags, neither of which a
    // tree records. A `--prefix` read is excluded because the index then holds the
    // old entries *plus* the bound tree, which is not any single tree.
    //
    // Every other shape goes through `unpack_trees()`'s parting
    // `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)` instead, which
    // validates a node only when the tree it would serialise is already in the odb
    // and writes nothing.
    match (tree_ids.len(), o.prefix.as_ref()) {
        (1, None) => {
            new_index.prime_cache_tree(&repo.objects, &tree_ids[0])?;
            super::write_tree::prepare_offset_table(&repo, &mut new_index);
        }
        _ => super::write_tree::rebuild_cache_tree(&repo, &mut new_index),
    }
    new_index.write(crate::config::index_write_options(&repo))?;
    // `core.fsync=index` (or an aggregate that contains it) hardens the index git
    // has just rewritten; the default set does not, so this is normally a no-op.
    fsync.harden_path(crate::config::FsyncComponent::Index, new_index.path());

    Ok(ExitCode::SUCCESS)
}

/// The index built by reading `tree_ids` in order: the union of their entries, with
/// a later tree replacing an earlier one at the same path. All entries are stage 0
/// with a zeroed stat cache, which is what stock git produces for a plain read.
///
/// Each tree is checked against `depth_limit` (`core.maxTreeDepth`) as it is read,
/// so a too-deep tree stops the read before the index is touched — git leaves the
/// old index in place on this failure.
fn union_of_trees(
    repo: &gix::Repository,
    tree_ids: &[ObjectId],
    depth_limit: i64,
) -> Result<std::result::Result<gix::index::File, ExitCode>> {
    let mut index = repo.index_from_tree(&tree_ids[0])?;
    if !within_tree_depth(&index, depth_limit) {
        eprintln!("error: {TOO_DEEP}");
        return Ok(Err(ExitCode::from(128)));
    }
    for id in &tree_ids[1..] {
        let extra = repo.index_from_tree(id)?;
        if !within_tree_depth(&extra, depth_limit) {
            eprintln!("error: {TOO_DEEP}");
            return Ok(Err(ExitCode::from(128)));
        }
        let extra_paths: HashSet<BString> = {
            let backing = extra.path_backing();
            extra.entries().iter().map(|e| e.path_in(backing).to_owned()).collect()
        };
        index.remove_entries(|_, path, _| extra_paths.contains(&path.to_owned()));
        let backing = extra.path_backing().to_owned();
        for e in extra.entries() {
            index.dangerously_push_entry(e.stat, e.id, e.flags, e.mode, e.path_in(&backing));
        }
        index.sort_entries();
    }
    Ok(Ok(index))
}

/// `--prefix=<p>`: keep `old` and add every entry of `tree` under `<p>/`.
///
/// Reached only for the one-tree case: `bind_merge` is the single unpack function
/// git guards with `if (o->merge_size != 1) BUG(...)`, and `cmd_read_tree` only
/// selects it when exactly one tree was named. With more trees `--prefix` is still
/// honoured, but through [`multi_tree_read`]'s two- and three-tree merges.
///
/// Returns `Err(ExitCode)` for git's bind-overlap rejection and for the
/// `core.maxTreeDepth` rejection, so the caller can exit with git's code and
/// message instead of an `anyhow` error. The depth is measured on the tree as
/// read, *before* the prefix is applied — see [`traverse_base`].
fn bind_prefix(
    repo: &gix::Repository,
    old: &gix::index::File,
    tree: ObjectId,
    prefix: &str,
    depth_limit: i64,
) -> Result<std::result::Result<gix::index::File, ExitCode>> {
    let prefix = traverse_base(prefix);

    let existing: HashSet<BString> = {
        let backing = old.path_backing();
        old.entries().iter().map(|e| e.path_in(backing).to_owned()).collect()
    };

    let mut index = gix::index::File::clone(old);
    let from_tree = repo.index_from_tree(&tree)?;
    if !within_tree_depth(&from_tree, depth_limit) {
        eprintln!("error: {TOO_DEEP}");
        return Ok(Err(ExitCode::from(128)));
    }
    let backing = from_tree.path_backing().to_owned();
    for e in from_tree.entries() {
        let mut path = BString::from(prefix.as_bytes());
        path.extend_from_slice(e.path_in(&backing).as_ref());
        if existing.contains(&path) {
            let shown = path.to_str_lossy();
            eprintln!("error: Entry '{shown}' overlaps with '{shown}'.  Cannot bind.");
            return Ok(Err(ExitCode::from(128)));
        }
        index.dangerously_push_entry(e.stat, e.id, e.flags, e.mode, path.as_bstr());
    }
    index.sort_entries();
    Ok(Ok(index))
}

/// `read-tree -m`, `--reset` or `--prefix` with two or more trees: the two- and
/// three-tree merges of `unpack-trees.c`.
///
/// Decisions are made per path over the union of the index's and the trees'
/// paths, rather than by `traverse_trees()`'s simultaneous walk. Every merge
/// function is a faithful port; what the walk adds beyond them is
/// directory/file-conflict detection (git substitutes `o->df_conflict_entry` for
/// a slot whose path is a directory in that tree) and sparse-directory handling.
/// Neither is reproduced here, so a path that is a file in one tree and a
/// directory in another is merged as if the directory side were simply absent.
///
/// `--prefix=<p>` is the traversal base ([`traverse_base`]), so every tree path
/// is read at `<p>/<path>` and nothing else changes: index entries outside the
/// prefix reach the merge function with all tree slots absent, which is exactly
/// what git's pre-traversal and left-over `unpack_index_entry()` loops
/// (unpack-trees.c:1992-2027) hand it, and `twoway_merge`'s "4 and 5" arm is what
/// carries them through unchanged.
#[allow(clippy::too_many_arguments)]
fn multi_tree_read(
    repo: &gix::Repository,
    o: &Opts,
    old: &gix::index::File,
    tree_ids: &[ObjectId],
    depth_limit: i64,
    fsync: &crate::config::FsyncPolicy,
) -> Result<ExitCode> {
    // Each tree, flattened to path → entry under the traversal base, with
    // `core.maxTreeDepth` charged against the tree as read — the prefix is the
    // base the walk starts from, not a level it descends into.
    let base = o.prefix.as_deref().map(traverse_base).unwrap_or_default();
    let mut trees: Vec<HashMap<BString, Ce>> = Vec::with_capacity(tree_ids.len());
    for id in tree_ids {
        let from_tree = repo.index_from_tree(id)?;
        if !within_tree_depth(&from_tree, depth_limit) {
            eprintln!("error: {TOO_DEEP}");
            return Ok(ExitCode::from(128));
        }
        let backing = from_tree.path_backing();
        trees.push(
            from_tree
                .entries()
                .iter()
                .map(|e| {
                    let mut path = BString::from(base.as_bytes());
                    path.extend_from_slice(e.path_in(backing).as_ref());
                    (path, Ce { id: e.id, mode: e.mode, conflicted: false })
                })
                .collect(),
        );
    }

    // `read_index_unmerged()` collapses a conflicted path into one `CE_CONFLICTED`
    // existence marker; `-m` has already refused such an index, so this only
    // matters under `--reset`.
    let mut index_slots: HashMap<BString, Ce> = HashMap::new();
    let mut old_stats: HashMap<BString, Stat> = HashMap::new();
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing).to_owned();
            let conflicted = e.stage_raw() != 0;
            let slot = Ce { id: e.id, mode: e.mode, conflicted };
            match index_slots.get(&path) {
                // Keep the first (lowest) stage, as `read_index_unmerged` does.
                Some(existing) if existing.conflicted => {}
                _ => {
                    index_slots.insert(path.clone(), slot);
                    old_stats.insert(path, e.stat);
                }
            }
        }
    }

    let mut paths: BTreeSet<BString> = index_slots.keys().cloned().collect();
    for tree in &trees {
        paths.extend(tree.keys().cloned());
    }

    let stat_ctx = StatCtx::new(repo, old)?;
    let ctx = MergeCtx {
        repo,
        old,
        stat: &stat_ctx,
        update: o.update,
        index_only: o.index_only,
        reset: o.reset,
        aggressive: o.aggressive,
    };
    // `is_index_unborn()`: an index that was never written has nothing to carry.
    let initial_checkout = old.entries().is_empty();
    let head_idx = if trees.len() == 2 { 1 } else { trees.len() - 1 };

    let mut result: Vec<(BString, Ce, u8, bool)> = Vec::new();
    let mut removed: BTreeSet<BString> = BTreeSet::new();
    let mut nontrivial = false;

    for path in &paths {
        let mut stages: Vec<Option<Ce>> = Vec::with_capacity(trees.len() + 1);
        stages.push(index_slots.get(path).copied());
        for tree in &trees {
            stages.push(tree.get(path).copied());
        }

        let verdict = if trees.len() == 2 {
            twoway_merge(
                &ctx,
                [stages[0], stages[1], stages[2]],
                path.as_bstr(),
                initial_checkout,
            )
        } else {
            threeway_merge(&ctx, &stages, head_idx, path.as_bstr()).map(|(v, nt)| {
                nontrivial |= nt;
                v
            })
        };
        match verdict {
            Ok(Verdict::Keep(kept)) => {
                for (ce, stage) in kept {
                    result.push((path.clone(), ce, stage, false));
                }
            }
            Ok(Verdict::Merge { ce, update }) => result.push((path.clone(), ce, 0, update)),
            Ok(Verdict::Delete) => {
                removed.insert(path.clone());
            }
            Ok(Verdict::Nothing) => {}
            Err(message) => return rejected(message),
        }
    }

    // `--trivial`: refuse a merge any path could not resolve without file-level
    // merging, after the whole traversal (unpack_trees.c).
    if o.trivial && nontrivial {
        return rejected("Merge requires file-level merging");
    }

    // A path that stays in the index but was not in the result set is gone.
    let kept_paths: HashSet<&BString> = result.iter().map(|(p, ..)| p).collect();
    for path in index_slots.keys() {
        if !kept_paths.contains(path) {
            removed.insert(path.clone());
        }
    }

    if o.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // ---- Assemble the result index. ----
    let mut new_index =
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path());
    let mut wanted: BTreeSet<BString> = BTreeSet::new();
    for (path, ce, stage, update) in &result {
        // "Take the stat information from stage0": an entry carried through
        // unchanged keeps the stat cache it already had, so a later `git status`
        // does not see the whole index as freshly touched.
        let stat = match old_stats.get(path) {
            Some(stat) if !*update => *stat,
            _ => Stat::default(),
        };
        let flags = gix::index::entry::Flags::from(stage_enum(u32::from(*stage)));
        new_index.dangerously_push_entry(stat, ce.id, flags, ce.mode, path.as_bstr());
        if *update && *stage == 0 {
            wanted.insert(path.clone());
        }
    }
    new_index.sort_entries();

    if o.update {
        checkout_subset(repo, &mut new_index, &wanted)?;
        for path in &removed {
            if let Some(full) = repo.workdir_path(path.as_bstr()) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    if let Some(out) = &o.index_output {
        new_index.set_path(out.clone());
    }
    // Two or more trees never reach `prime_cache_tree()` (builtin/read-tree.c:287),
    // so all this index gets is `unpack_trees()`'s repair pass: nodes whose tree the
    // repository already has keep an id, everything else — including every node above
    // an unmerged path — comes out invalid.
    super::write_tree::rebuild_cache_tree(repo, &mut new_index);
    new_index.write(crate::config::index_write_options(repo))?;
    fsync.harden_path(crate::config::FsyncComponent::Index, new_index.path());
    Ok(ExitCode::SUCCESS)
}

/// Map a raw stage number (0..=3) to gitoxide's `Stage`.
fn stage_enum(stage: u32) -> gix::index::entry::Stage {
    use gix::index::entry::Stage;
    match stage {
        0 => Stage::Unconflicted,
        1 => Stage::Base,
        2 => Stage::Ours,
        _ => Stage::Theirs,
    }
}

// ---------------------------------------------------------------------------
// The two- and three-tree merges (`unpack-trees.c`)
// ---------------------------------------------------------------------------

/// One `src[]` slot of `unpack_trees`' merge functions: an index or tree entry.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Ce {
    id: ObjectId,
    mode: Mode,
    /// `CE_CONFLICTED`, which `same()` treats as "never equal to anything".
    conflicted: bool,
}

/// git's `same()`: two slots agree when both are absent, or both are present,
/// unconflicted, and carry the same mode and object id.
fn same(a: Option<Ce>, b: Option<Ce>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => !(a.conflicted || b.conflicted) && a.mode == b.mode && a.id == b.id,
        _ => false,
    }
}

/// What the merge decided for one path.
enum Verdict {
    /// `keep_entry()`: carry the slot through at the given stage, untouched.
    Keep(Vec<(Ce, u8)>),
    /// `merged_entry()`: the result is this slot at stage 0. `update` is git's
    /// `CE_UPDATE` — cleared when the old index entry already matched, which is
    /// what stops a matching worktree file from being rewritten.
    Merge { ce: Ce, update: bool },
    /// `deleted_entry()`: the path leaves the index (and the worktree under `-u`).
    Delete,
    /// The path contributes nothing (git's `return 0` with no `add_entry`).
    Nothing,
}

/// The safety checks and option state the merge functions consult.
struct MergeCtx<'a> {
    repo: &'a gix::Repository,
    old: &'a gix::index::File,
    stat: &'a StatCtx,
    update: bool,
    index_only: bool,
    reset: bool,
    aggressive: bool,
}

impl MergeCtx<'_> {
    /// git's `verify_uptodate()`: refuse to overwrite a worktree file that has
    /// drifted from the index entry naming it.
    fn verify_uptodate(&self, path: &gix::bstr::BStr) -> std::result::Result<(), String> {
        if self.index_only || self.reset {
            return Ok(());
        }
        let Some(entry) = self.old.entry_by_path(path) else {
            return Ok(());
        };
        match self.stat.probe(self.repo, entry, path) {
            // `errno == ENOENT` is up to date: there is nothing to lose.
            Probe::Absent | Probe::Uptodate => Ok(()),
            Probe::Changed => Err(format!(
                "Entry '{}' not uptodate. Cannot merge.",
                path.to_str_lossy()
            )),
        }
    }

    /// git's `verify_absent()`: refuse to write over an untracked worktree file.
    ///
    /// `overwritten` picks between git's two messages — a path being created
    /// clobbers a file, a path being dropped removes one.
    fn verify_absent(
        &self,
        path: &gix::bstr::BStr,
        overwritten: bool,
    ) -> std::result::Result<(), String> {
        if self.index_only || !self.update || self.reset {
            return Ok(());
        }
        let exists = self
            .repo
            .workdir_path(path)
            .is_some_and(|p| p.symlink_metadata().is_ok());
        if !exists {
            return Ok(());
        }
        let shown = path.to_str_lossy();
        Err(if overwritten {
            format!("Untracked working tree file '{shown}' would be overwritten by merge.")
        } else {
            format!("Untracked working tree file '{shown}' would be removed by merge.")
        })
    }

    /// git's `merged_entry()`, minus the sparse-checkout and submodule arms.
    fn merged_entry(
        &self,
        ce: Ce,
        old: Option<Ce>,
        path: &gix::bstr::BStr,
    ) -> std::result::Result<Verdict, String> {
        match old {
            None => {
                self.verify_absent(path, true)?;
                Ok(Verdict::Merge { ce, update: true })
            }
            // "See if we can re-use the old CE directly? That way we get the
            // uptodate stat info. This also removes the UPDATE flag on a match;
            // otherwise we will end up overwriting local changes in the work tree."
            Some(old) if !old.conflicted => {
                if same(Some(old), Some(ce)) {
                    Ok(Verdict::Merge { ce, update: false })
                } else {
                    self.verify_uptodate(path)?;
                    Ok(Verdict::Merge { ce, update: true })
                }
            }
            // "Previously unmerged entry left as an existence marker."
            Some(_) => Ok(Verdict::Merge { ce, update: true }),
        }
    }

    /// git's `deleted_entry()`.
    fn deleted_entry(
        &self,
        old: Option<Ce>,
        path: &gix::bstr::BStr,
    ) -> std::result::Result<Verdict, String> {
        let Some(old) = old else {
            self.verify_absent(path, false)?;
            return Ok(Verdict::Nothing);
        };
        if !old.conflicted {
            self.verify_uptodate(path)?;
        }
        Ok(Verdict::Delete)
    }
}

/// git's `reject_merge()` message.
fn reject_merge(path: &gix::bstr::BStr) -> String {
    format!(
        "Entry '{}' would be overwritten by merge. Cannot merge.",
        path.to_str_lossy()
    )
}

/// git's `twoway_merge()`: the "carry forward" table of `git-read-tree(1)`.
///
/// `src` is `[index, oldtree, newtree]`. `initial_checkout` is
/// `is_index_unborn()`, which lets a two-tree read populate an empty index
/// instead of refusing the staged-deletion case.
fn twoway_merge(
    ctx: &MergeCtx<'_>,
    src: [Option<Ce>; 3],
    path: &gix::bstr::BStr,
    initial_checkout: bool,
) -> std::result::Result<Verdict, String> {
    let [current, oldtree, newtree] = src;

    if let Some(cur) = current {
        if cur.conflicted {
            if same(oldtree, newtree) || ctx.reset {
                return match newtree {
                    None => ctx.deleted_entry(current, path),
                    Some(new) => ctx.merged_entry(new, current, path),
                };
            }
            return Err(reject_merge(path));
        }
        // 4 and 5; 6 and 7; 14 and 15; 18 and 19.
        if (oldtree.is_none() && newtree.is_none())
            || (oldtree.is_none() && newtree.is_some() && same(current, newtree))
            || (oldtree.is_some() && newtree.is_some() && same(oldtree, newtree))
            || (oldtree.is_some()
                && newtree.is_some()
                && !same(oldtree, newtree)
                && same(current, newtree))
        {
            return Ok(Verdict::Keep(vec![(cur, 0)]));
        }
        // 10 or 11.
        if oldtree.is_some() && newtree.is_none() && same(current, oldtree) {
            return ctx.deleted_entry(current, path);
        }
        // 20 or 21.
        if let (Some(_), Some(new)) = (oldtree, newtree) {
            if same(current, oldtree) && !same(current, newtree) {
                return ctx.merged_entry(new, current, path);
            }
        }
        return Err(reject_merge(path));
    }

    if let Some(new) = newtree {
        if oldtree.is_some() && !initial_checkout {
            // "deletion of the path was staged"
            if same(oldtree, newtree) {
                return Ok(Verdict::Nothing);
            }
            return Err(reject_merge(path));
        }
        return ctx.merged_entry(new, current, path);
    }
    ctx.deleted_entry(current, path)
}

/// git's `threeway_merge()`. `stages` is `[index, ancestors…, head, remote]`,
/// indexed exactly as git indexes it, with `head_idx` naming the head slot.
///
/// Returns the verdict plus whether the path took the "no merge" path, which is
/// git's `o->internal.nontrivial_merge` and what `--trivial` refuses.
fn threeway_merge(
    ctx: &MergeCtx<'_>,
    stages: &[Option<Ce>],
    head_idx: usize,
    path: &gix::bstr::BStr,
) -> std::result::Result<(Verdict, bool), String> {
    let remote = stages[head_idx + 1];
    let index = stages[0];
    let head = stages[head_idx];

    let mut any_anc_missing = false;
    let mut no_anc_exists = true;
    for stage in &stages[1..head_idx] {
        if stage.is_none() {
            any_anc_missing = true;
        } else {
            no_anc_exists = false;
        }
    }

    // "First, if there's a #16 situation, note that to prevent #13 and #14."
    let mut head_match = 0usize;
    let mut remote_match = 0usize;
    if !same(remote, head) {
        for (i, stage) in stages.iter().enumerate().take(head_idx).skip(1) {
            if same(*stage, head) {
                head_match = i;
            }
            if same(*stage, remote) {
                remote_match = i;
            }
        }
    }

    // #14, #14ALT, #2ALT — the index is allowed to match the result, not head.
    if let Some(rem) = remote {
        if head_match != 0 && remote_match == 0 {
            if index.is_some() && !same(index, remote) && !same(index, head) {
                return Err(reject_merge(path));
            }
            return Ok((ctx.merged_entry(rem, index, path)?, false));
        }
    }
    // Otherwise an index entry has to match head.
    if index.is_some() && !same(index, head) {
        return Err(reject_merge(path));
    }

    if let Some(h) = head {
        // #5ALT, #15.
        if same(head, remote) {
            return Ok((ctx.merged_entry(h, index, path)?, false));
        }
        // #13, #3ALT.
        if remote_match != 0 && head_match == 0 {
            return Ok((ctx.merged_entry(h, index, path)?, false));
        }
    }

    // #1.
    if head.is_none() && remote.is_none() && any_anc_missing {
        return Ok((Verdict::Nothing, false));
    }

    // "Under the 'aggressive' rule, we resolve mostly trivial cases that we
    // historically had git-merge-one-file resolve."
    if ctx.aggressive {
        let head_deleted = head.is_none();
        let remote_deleted = remote.is_none();
        let ce = index
            .or(head)
            .or(remote)
            .or_else(|| stages[1..head_idx].iter().copied().flatten().next());

        // Deleted in both; deleted in one and unchanged in the other.
        if (head_deleted && remote_deleted)
            || (head_deleted && remote.is_some() && remote_match != 0)
            || (remote_deleted && head.is_some() && head_match != 0)
        {
            if index.is_some() {
                return Ok((ctx.deleted_entry(index, path)?, false));
            }
            if ce.is_some() && !head_deleted {
                ctx.verify_absent(path, false)?;
            }
            return Ok((Verdict::Nothing, false));
        }
        // Added in both, identically.
        if no_anc_exists && head.is_some() && remote.is_some() && same(head, remote) {
            return Ok((ctx.merged_entry(head.expect("checked"), index, path)?, false));
        }
    }

    // "Handle 'no merge' cases": the path stays conflicted, so make sure the
    // worktree copy is not about to be lost.
    if index.is_some() {
        ctx.verify_uptodate(path)?;
    }

    // #2, #3, #4, #6, #7, #9, #10, #11.
    //
    // The stage each kept entry carries is the one `unpack_single_entry()` gave
    // it when the tree was read, not its slot number: for tree slot `i`
    // (0-based) it is 1 while `i + 1 < head_idx`, 2 at `i + 1 == head_idx` and 3
    // beyond (unpack-trees.c:1211-1226). Only a three-tree merge makes those
    // coincide — there `head_idx` is 2 — so a four-tree `--aggressive` merge
    // (two merge bases, as `git merge -s resolve` over a criss-cross history
    // runs) filed the head under stage 3 and left no stage 2 at all.
    let mut kept: Vec<(Ce, u8)> = Vec::new();
    if head_match == 0 || remote_match == 0 {
        for stage in stages.iter().take(head_idx).skip(1) {
            if let Some(ce) = stage {
                kept.push((*ce, 1));
                break;
            }
        }
    }
    if let Some(h) = head {
        kept.push((h, 2));
    }
    if let Some(r) = remote {
        kept.push((r, 3));
    }
    Ok((Verdict::Keep(kept), true))
}

/// Path → (id, mode, stat) for the stage-0 entries of `index`.
fn stage0_map(index: &gix::index::File) -> HashMap<BString, (ObjectId, Mode, Stat, Flags)> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .filter(|e| e.stage_raw() == 0)
        .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode, e.stat, e.flags)))
        .collect()
}

/// What one `lstat()` of the worktree file behind an index entry says about it.
///
/// The three outcomes are what git's callers actually branch on: `verify_uptodate_1`
/// treats `ENOENT` as up to date and only a *stat mismatch* as a reason to refuse,
/// while `oneway_merge`'s `--reset -u` arm restores the file for **either**
/// (`if (lstat(...) || ie_match_stat(...)) update |= CE_UPDATE`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Probe {
    /// `lstat()` failed — the file is gone from the worktree.
    Absent,
    /// Present, and `ie_match_stat()` reports no change.
    Uptodate,
    /// Present, and `ie_match_stat()` reports a change.
    Changed,
}

/// The `stat`-comparison context `ie_match_stat()` needs, resolved once.
///
/// git decides up-to-dateness from `stat` data, not from content: an entry whose
/// mtime/size/inode moved is "not uptodate" even when the bytes are unchanged.
/// That distinction is load-bearing — `read-tree -m` refuses on it, and
/// `read-tree --reset -u` / `checkout -f` rewrite the file because of it.
pub(super) struct StatCtx {
    workdir: Option<PathBuf>,
    /// `core.trustCtime` / `core.checkStat`, as `Stat::matches` consumes them.
    opts: gix::index::entry::stat::Options,
    /// `istate->timestamp`: the index file's own mtime, for the racy-git check.
    timestamp: gix::index::entry::stat::Time,
    /// `trust_executable_bit` (`core.fileMode`).
    trust_executable_bit: bool,
}

impl StatCtx {
    pub(super) fn new(repo: &gix::Repository, index: &gix::index::File) -> Result<Self> {
        Ok(Self {
            workdir: repo.workdir().map(Path::to_owned),
            opts: repo.stat_options()?,
            timestamp: index.timestamp().into(),
            trust_executable_bit: repo
                .config_snapshot()
                .boolean("core.fileMode")
                .unwrap_or(true),
        })
    }

    /// `lstat()` + `ie_match_stat()` for one entry, named by its index path.
    pub(super) fn probe(
        &self,
        repo: &gix::Repository,
        entry: &gix::index::Entry,
        path: &gix::bstr::BStr,
    ) -> Probe {
        let Some(workdir) = &self.workdir else {
            return Probe::Absent;
        };
        let full = workdir.join(gix::path::from_bstr(path).as_ref());
        let Ok(meta) = gix::index::fs::Metadata::from_path_no_follow(&full) else {
            return Probe::Absent;
        };
        // "Historic default policy was to allow submodule to be out of sync wrt
        // the superproject index" — `verify_uptodate_1` returns 0 for a gitlink
        // whatever the stat says, and `oneway_merge` only moves one behind
        // `should_update_submodules()`, which this port does not do.
        if entry.mode.contains(Mode::COMMIT) {
            return Probe::Uptodate;
        }
        // `ce_intent_to_add()`: "the index entry by definition never matches
        // what is in the work tree until it actually gets added".
        if entry.flags.contains(gix::index::entry::Flags::INTENT_TO_ADD) {
            return Probe::Changed;
        }
        if self.basic_changed(entry, &meta) {
            return Probe::Changed;
        }
        // Racily clean: the entry's mtime is not older than the index's own, so
        // the stat data cannot prove the file is unchanged. `ie_match_stat` falls
        // back to `ce_modified_check_fs`, which compares the content.
        if self.is_racy(entry) && self.content_differs(repo, entry, &full, &meta) {
            return Probe::Changed;
        }
        Probe::Uptodate
    }

    /// git's `is_racy_stat()`: an entry whose mtime is not older than the index's
    /// own timestamp cannot be proven clean from `stat` data alone. A zero index
    /// timestamp (an index never written to disk) disables the check entirely.
    fn is_racy(&self, entry: &gix::index::Entry) -> bool {
        use std::cmp::Ordering;
        if self.timestamp.secs == 0 {
            return false;
        }
        match self.timestamp.secs.cmp(&entry.stat.mtime.secs) {
            Ordering::Less => true,
            Ordering::Equal if self.opts.use_nsec && self.opts.check_stat => {
                self.timestamp.nsecs <= entry.stat.mtime.nsecs
            }
            Ordering::Equal => true,
            Ordering::Greater => false,
        }
    }

    /// git's `ce_match_stat_basic()`: the type/mode check, then `match_stat_data`,
    /// then the racily-smudged zero-size special case.
    fn basic_changed(&self, entry: &gix::index::Entry, meta: &gix::index::fs::Metadata) -> bool {
        match entry.mode {
            Mode::FILE | Mode::FILE_EXECUTABLE => {
                if !meta.is_file() {
                    return true;
                }
                // "We consider only the owner x bit to be relevant for mode changes."
                if self.trust_executable_bit
                    && meta.is_executable() != (entry.mode == Mode::FILE_EXECUTABLE)
                {
                    return true;
                }
            }
            Mode::SYMLINK => {
                if !meta.is_symlink() {
                    return true;
                }
            }
            _ => return true,
        }
        let Ok(now) = gix::index::entry::Stat::from_fs(meta) else {
            return true;
        };
        if self.stat_data_changed(&entry.stat, &now) {
            return true;
        }
        // "Racily smudged entry?": a zero recorded size for a non-empty blob is
        // how `ce_smudge_racily_clean_entry` marks an entry as needing a real look.
        entry.stat.size == 0 && !entry.id.is_empty_blob()
    }

    /// git's `match_stat_data()` (statinfo.c), field for field.
    ///
    /// Not `gix_index::entry::Stat::matches`, which gates the `ctime` comparison
    /// on `trust_ctime` alone; git gates it on `trust_ctime && check_stat`, so
    /// under `core.checkStat=minimal` gix rejects a file whose ctime moved while
    /// git accepts it. That is the whole point of `minimal` — the setting exists
    /// for filesystems and copies that do not preserve ctime or inode — and
    /// getting it wrong turns every such entry into a spurious
    /// "not uptodate. Cannot merge."
    fn stat_data_changed(
        &self,
        sd: &gix::index::entry::Stat,
        now: &gix::index::entry::Stat,
    ) -> bool {
        let Options {
            trust_ctime,
            check_stat,
            use_nsec,
            use_stdev,
        } = self.opts;
        if sd.mtime.secs != now.mtime.secs {
            return true;
        }
        if trust_ctime && check_stat && sd.ctime.secs != now.ctime.secs {
            return true;
        }
        if use_nsec {
            if check_stat && sd.mtime.nsecs != now.mtime.nsecs {
                return true;
            }
            if trust_ctime && check_stat && sd.ctime.nsecs != now.ctime.nsecs {
                return true;
            }
        }
        if check_stat {
            if sd.uid != now.uid || sd.gid != now.gid {
                return true;
            }
            if sd.ino != now.ino {
                return true;
            }
            if use_stdev && sd.dev != now.dev {
                return true;
            }
        }
        sd.size != now.size
    }

    /// git's `ce_modified_check_fs()`, reached only for a racily-clean entry.
    ///
    /// git re-hashes the worktree file and compares object ids; comparing the
    /// stored blob's bytes is the same decision one step earlier, and matches this
    /// port's `git add`, which likewise hashes worktree bytes without filtering.
    fn content_differs(
        &self,
        repo: &gix::Repository,
        entry: &gix::index::Entry,
        full: &Path,
        meta: &gix::index::fs::Metadata,
    ) -> bool {
        let Ok(blob) = repo.find_object(entry.id) else {
            return true;
        };
        if meta.is_symlink() {
            return match std::fs::read_link(full) {
                Ok(target) => gix::path::into_bstr(target).as_ref() != blob.data.as_slice(),
                Err(_) => true,
            };
        }
        match std::fs::read(full) {
            Ok(bytes) => bytes != blob.data,
            Err(_) => true,
        }
    }
}

/// Tracked paths whose worktree file `ie_match_stat()` reports as changed.
///
/// A path missing from the worktree is deliberately *not* reported: git's
/// `verify_uptodate_1` treats `ENOENT` as up to date. `--reset -u` needs the
/// missing ones too and collects them separately via [`StatCtx::probe`].
fn worktree_dirty(
    repo: &gix::Repository,
    index: &gix::index::File,
    ctx: &StatCtx,
) -> HashSet<BString> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .filter(|e| e.stage_raw() == 0)
        .filter(|e| ctx.probe(repo, e, e.path_in(backing)) == Probe::Changed)
        .map(|e| e.path_in(backing).to_owned())
        .collect()
}

/// Tracked paths whose worktree file is gone (`lstat` → `ENOENT`).
///
/// `oneway_merge` restores these under `--reset -u`: the entry is unchanged, so
/// nothing else marks the path for a write, yet the file has to come back.
fn worktree_absent(
    repo: &gix::Repository,
    index: &gix::index::File,
    ctx: &StatCtx,
) -> HashSet<BString> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .filter(|e| e.stage_raw() == 0)
        .filter(|e| ctx.probe(repo, e, e.path_in(backing)) == Probe::Absent)
        .map(|e| e.path_in(backing).to_owned())
        .collect()
}

/// Check out exactly the entries of `index` named by `wanted`, then carry the stat
/// information gained back onto `index` so the written index is clean for them.
fn checkout_subset(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    wanted: &BTreeSet<BString>,
) -> Result<()> {
    if wanted.is_empty() {
        return Ok(());
    }
    let workdir = repo
        .workdir()
        .ok_or_else(|| crate::fatal::need_work_tree())?
        .to_owned();

    // Checking out a subset index writes only those paths, leaving the worktree
    // copies of carried-forward entries (which may be modified) untouched.
    let mut subset = gix::index::File::clone(index);
    subset.remove_entries(|_, path, _| !wanted.contains(&path.to_owned()));
    subset.remove_tree();

    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let should_interrupt = AtomicBool::new(false);
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        &should_interrupt,
        opts,
    )?;

    let fresh: HashMap<BString, Stat> = {
        let backing = subset.path_backing();
        subset
            .entries()
            .iter()
            .map(|e| (e.path_in(backing).to_owned(), e.stat))
            .collect()
    };
    let backing = index.path_backing().to_owned();
    for e in index.entries_mut() {
        if let Some(stat) = fresh.get(&e.path_in(&backing).to_owned()) {
            e.stat = *stat;
        }
    }
    Ok(())
}
