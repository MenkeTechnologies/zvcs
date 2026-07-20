//! `git repack` — pack unpacked objects into a pack. **Not ported: this module
//! bails once the arguments are accepted.**
//!
//! What *is* covered is the complete argument surface, and only because those
//! paths are byte-verifiable without touching the object database:
//!   * `-h` → git's 2699-byte usage block on stdout, exit 129
//!   * git's parse-options behaviour for every option in the table, including
//!     unambiguous long-option abbreviation (`--qui` → `--quiet`), `--no-`
//!     negations, `=value` vs. separate-argv values, clustered short switches,
//!     and `-g<n>` / `-g <n>`
//!   * the five distinct parse-options diagnostics, each byte-for-byte:
//!     `unknown option`, `unknown switch`, `ambiguous option`, `takes no value`,
//!     `requires a value`, plus the integer/magnitude value-type messages
//!   * the pre-flight option-conflict `fatal:`s that stock git emits before it
//!     does any work at all (exit 128): the `-A`/`-k`/`--cruft` triad, geometric
//!     vs. `-a`/`-A`, incremental-with-bitmaps, and `--filter-to` without
//!     `--filter`
//! (all checked against git 2.55.0.)
//!
//! Everything else — i.e. `repack` actually repacking — bails, naming the
//! substrate that is missing. It is deliberately *not* approximated: stock
//! `git repack` writes nothing to stdout and exits 0 on success, so a partial
//! implementation is indistinguishable from success at the stdout/exit-code
//! level while leaving `.git/objects` in a state stock git would never produce.
//! For a harness that also diffs post-command repository state, that is the
//! worst possible failure mode.
//!
//! The missing substrate, concretely, in the vendored crates under `src/ported`:
//!
//!   1. **Delta compression.** The entire point of `repack` is a
//!      delta-compressed pack. `gix-pack`'s writer cannot compute deltas; its
//!      only mode is documented as "Copy base objects and deltas from packs,
//!      while non-packed objects will be treated as base objects (i.e. without
//!      trying to delta compress them)"
//!      (`gix-pack/src/data/output/entry/iter_from_counts.rs:362`). A pack
//!      written through it differs from git's in size and in every byte, and the
//!      loose objects `repack` exists to fold in would all be stored undeltified
//!      — so `-f` and `-F`, which exist purely to control delta reuse, have
//!      nothing to control.
//!   2. **Reverse indexes.** git 2.55 writes a `pack-*.rev` file next to every
//!      pack it creates. `gix-pack` has no `.rev` writer (grep for `reverse`
//!      under `gix-pack/src` hits only pack-entry decoding and the multi-index).
//!      Its absence is directly observable in the post-command state.
//!   3. **Reachability bitmaps.** `-b`/`--write-bitmap-index` needs an EWAH
//!      bitmap index writer. The string `bitmap` does not occur anywhere under
//!      `gix-pack/src`; `gix-bitmap` is a read-only EWAH decoder.
//!   4. **Cruft packs.** `--cruft` requires writing a `.mtimes` file alongside
//!      the pack. No `.mtimes` reader or writer exists in `gix-pack`.
//!   5. **Redundant-pack removal and `prune-packed`.** `-d` deletes now-redundant
//!      packs and then removes the loose objects they cover. `gix-odb`'s loose
//!      store exposes no removal API at all.
//!   6. **`update-server-info`.** Absent `-n`, `repack` refreshes
//!      `objects/info/packs`; nothing in the vendored crates writes that file.
//!
//! Two smaller, deliberate gaps in the covered part, so this doc claims no more
//! than the code does: the `warning: minimum pack size limit is 1 MiB` that git
//! prints for `--max-pack-size` below 1 MiB is not emitted, and the
//! `repack.writeBitmaps` / `pack.writeBitmaps` config keys are not read, so the
//! incremental-with-bitmaps `fatal:` fires only when `-b` is given explicitly.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// Stock git's `repack` usage block, byte-for-byte (2699 bytes, git 2.55.0),
/// including the trailing blank line. Printed on `-h` (stdout) and after the
/// `unknown option` / `unknown switch` diagnostics (stderr).
const USAGE: &str = r#"usage: git repack [-a] [-A] [-d] [-f] [-F] [-l] [-n] [-q] [-b] [-m]
       [--window=<n>] [--depth=<n>] [--threads=<n>] [--keep-pack=<pack-name>]
       [--write-midx[=<mode>]] [--name-hash-version=<n>] [--path-walk]

    -a                    pack everything in a single pack
    -A                    same as -a, and turn unreachable objects loose
    --[no-]cruft          same as -a, pack unreachable cruft objects separately
    --[no-]cruft-expiration <approxidate>
                          with --cruft, expire objects older than this
    --combine-cruft-below-size <n>
                          with --cruft, only repack cruft packs smaller than this
    --max-cruft-size <n>  with --cruft, limit the size of new cruft packs
    -d                    remove redundant packs, and run git-prune-packed
    -f                    pass --no-reuse-delta to git-pack-objects
    -F                    pass --no-reuse-object to git-pack-objects
    --[no-]name-hash-version <n>
                          specify the name hash version to use for grouping similar objects by path
    --[no-]path-walk      pass --path-walk to git-pack-objects
    -n                    do not run git-update-server-info
    -q, --[no-]quiet      be quiet
    -l, --[no-]local      pass --local to git-pack-objects
    -b, --[no-]write-bitmap-index
                          write bitmap index
    -i, --[no-]delta-islands
                          pass --delta-islands to git-pack-objects
    --[no-]unpack-unreachable <approxidate>
                          with -A, do not loosen objects older than this
    -k, --[no-]keep-unreachable
                          with -a, repack unreachable objects
    --[no-]window <n>     size of the window used for delta compression
    --[no-]window-memory <bytes>
                          same as the above, but limit memory size instead of entries count
    --[no-]depth <n>      limits the maximum delta depth
    --[no-]threads <n>    limits the maximum number of threads
    --max-pack-size <n>   maximum size of each packfile
    --[no-]filter <args>  object filtering
    --[no-]pack-kept-objects
                          repack objects in packs marked with .keep
    --[no-]keep-pack <name>
                          do not repack this pack
    -g, --[no-]geometric <n>
                          find a geometric progression with factor <N>
    --[no-]write-midx[=<mode>]
                          write a multi-pack index of the resulting packs
    --[no-]expire-to <dir>
                          pack prefix to store a pack containing pruned objects
    --[no-]filter-to <dir>
                          pack prefix to store a pack containing filtered out objects

"#;

/// How an option consumes (and validates) its value.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `OPT_BOOL`/`OPT_BIT`: no value; `--opt=x` is an error.
    Bool,
    /// `OPT_INTEGER`: signed, optional single `k`/`m`/`g` suffix.
    Int,
    /// `OPT_MAGNITUDE`: as `Int` but non-negative.
    Magnitude,
    /// `OPT_STRING`: any value, taken from `=` or the next argv entry.
    Str,
    /// `PARSE_OPT_OPTARG`: value only ever comes from `=`.
    OptStr,
}

/// One entry of git's `repack` option table.
struct OptDef {
    long: &'static str,
    kind: Kind,
    /// Whether `--no-<long>` is accepted (`--[no-]` in the usage block).
    negatable: bool,
}

/// The long-option table **in git's declaration order**. The order is
/// load-bearing: parse-options resolves an ambiguous abbreviation by reporting
/// the last two matches it walked past, so reordering this array changes the
/// text of `ambiguous option` diagnostics.
const OPTS: &[OptDef] = &[
    OptDef { long: "cruft", kind: Kind::Bool, negatable: true },
    OptDef { long: "cruft-expiration", kind: Kind::Str, negatable: true },
    OptDef { long: "combine-cruft-below-size", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "max-cruft-size", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "name-hash-version", kind: Kind::Int, negatable: true },
    OptDef { long: "path-walk", kind: Kind::Bool, negatable: true },
    OptDef { long: "quiet", kind: Kind::Bool, negatable: true },
    OptDef { long: "local", kind: Kind::Bool, negatable: true },
    OptDef { long: "write-bitmap-index", kind: Kind::Bool, negatable: true },
    OptDef { long: "delta-islands", kind: Kind::Bool, negatable: true },
    OptDef { long: "unpack-unreachable", kind: Kind::Str, negatable: true },
    OptDef { long: "keep-unreachable", kind: Kind::Bool, negatable: true },
    OptDef { long: "window", kind: Kind::Int, negatable: true },
    OptDef { long: "window-memory", kind: Kind::Magnitude, negatable: true },
    OptDef { long: "depth", kind: Kind::Int, negatable: true },
    OptDef { long: "threads", kind: Kind::Int, negatable: true },
    OptDef { long: "max-pack-size", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "filter", kind: Kind::Str, negatable: true },
    OptDef { long: "pack-kept-objects", kind: Kind::Bool, negatable: true },
    OptDef { long: "keep-pack", kind: Kind::Str, negatable: true },
    OptDef { long: "geometric", kind: Kind::Int, negatable: true },
    OptDef { long: "write-midx", kind: Kind::OptStr, negatable: true },
    OptDef { long: "expire-to", kind: Kind::Str, negatable: true },
    OptDef { long: "filter-to", kind: Kind::Str, negatable: true },
];

/// The only accepted `--write-midx=<mode>` values; a bare `--write-midx` and
/// `--write-midx=` are equivalent to the empty mode.
const WRITE_MIDX_MODES: [&str; 2] = ["", "incremental"];

/// The flag state git derives while parsing, i.e. everything the pre-flight
/// conflict checks look at.
#[derive(Default)]
struct State {
    /// `ALL_INTO_ONE`, set by `-a`, `-A` and `--cruft`.
    all_into_one: bool,
    /// `LOOSEN_UNREACHABLE`, set by `-A` and by `--unpack-unreachable`.
    loosen_unreachable: bool,
    keep_unreachable: bool,
    cruft: bool,
    write_bitmap: bool,
    write_midx: bool,
    geometric: bool,
    filter: bool,
    filter_to: bool,
}

/// The outcome of parsing: either a fully-formed request, or a diagnostic that
/// has already decided the exit code.
enum Parsed {
    Ok(State),
    Exit(ExitCode),
}

/// `git repack` — argument validation and pre-flight conflict checks only; the
/// repacking itself is not ported.
///
/// Returns 129 with git's own output for `-h` and for every malformed
/// invocation, and 128 for the option conflicts git rejects before doing any
/// work. Any invocation that survives both bails, naming the substrate that is
/// missing; see the module documentation for the full list.
pub fn repack(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `repack` has no positional of its
    // own (stray positionals are silently ignored by git), so dropping a leading
    // copy of the verb cannot change the result.
    let args = match args.first().map(String::as_str) {
        Some("repack") => &args[1..],
        _ => args,
    };

    let state = match parse(args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Ok(state) => state,
    };

    if let Some(code) = preflight(&state) {
        return Ok(code);
    }

    bail!(
        "repack is not ported: gix-pack writes base objects only (no delta compression), \
         has no .rev reverse-index writer, no reachability-bitmap writer for -b, and no \
         .mtimes writer for --cruft, while -d needs loose-object removal that gix-odb does \
         not expose and update-server-info has no counterpart in the vendored crates \
         (ported: -h, argument validation, and the pre-flight option-conflict checks only)"
    )
}

/// Walk `args` exactly the way git's parse-options walks them, emitting git's
/// diagnostics verbatim on the first malformed entry.
fn parse(args: &[String]) -> Parsed {
    let mut st = State::default();
    let mut end_of_opts = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();

        if end_of_opts || !a.starts_with('-') || a == "-" {
            // Positionals are accepted and ignored, as git does.
            i += 1;
            continue;
        }

        if a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }

        if let Some(body) = a.strip_prefix("--") {
            match long_opt(body, args, &mut i, &mut st) {
                Some(code) => return Parsed::Exit(code),
                None => continue,
            }
        }

        // Clustered short switches, e.g. `-adq` or `-g2`.
        match short_opts(&a[1..], args, &mut i, &mut st) {
            Some(code) => return Parsed::Exit(code),
            None => continue,
        }
    }

    Parsed::Ok(st)
}

/// Handle one `--...` entry. Advances `i` past everything it consumed, or
/// returns the exit code of a diagnostic.
fn long_opt(body: &str, args: &[String], i: &mut usize, st: &mut State) -> Option<ExitCode> {
    let (name, inline) = match body.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (body, None),
    };

    let (idx, negated) = match resolve_long(name) {
        Resolved::Unique(idx, negated) => (idx, negated),
        Resolved::Ambiguous(first, second) => {
            // Verified quirk: unlike every other diagnostic here, the ambiguity
            // message goes to stderr while its usage block goes to *stdout*.
            eprintln!("error: ambiguous option: {name} (could be --{first} or --{second})");
            print!("{USAGE}");
            return Some(ExitCode::from(129));
        }
        Resolved::Unknown => {
            // git echoes the argument as written, `=value` included.
            eprint!("error: unknown option `{body}'\n{USAGE}");
            return Some(ExitCode::from(129));
        }
    };

    let def = &OPTS[idx];
    // The diagnostics name the matched form, not the abbreviation the user typed.
    let shown = if negated {
        format!("no-{}", def.long)
    } else {
        def.long.to_string()
    };

    // A negation never takes a value, and neither does a boolean.
    if (negated || def.kind == Kind::Bool) && inline.is_some() {
        eprintln!("error: option `{shown}' takes no value");
        return Some(ExitCode::from(129));
    }

    if negated {
        set_long(idx, true, st);
        *i += 1;
        return None;
    }

    let value = match def.kind {
        Kind::Bool => None,
        // `PARSE_OPT_OPTARG` only ever reads a value glued on with `=`.
        Kind::OptStr => Some(inline.unwrap_or("")),
        _ => match inline {
            Some(v) => Some(v),
            None => match args.get(*i + 1) {
                Some(v) => {
                    *i += 1;
                    Some(v.as_str())
                }
                None => {
                    eprintln!("error: option `{shown}' requires a value");
                    return Some(ExitCode::from(129));
                }
            },
        },
    };

    if let Some(v) = value {
        if let Some(code) = check_value(def, &shown, v) {
            return Some(code);
        }
    }

    set_long(idx, false, st);
    *i += 1;
    None
}

/// Validate a value against the option's parse-options type, emitting git's
/// exact type diagnostic on failure.
fn check_value(def: &OptDef, shown: &str, v: &str) -> Option<ExitCode> {
    match def.kind {
        Kind::Int if !is_number(v, true) => {
            eprintln!(
                "error: option `{shown}' expects an integer value with an optional k/m/g suffix"
            );
            Some(ExitCode::from(129))
        }
        Kind::Magnitude if !is_number(v, false) => {
            eprintln!(
                "error: option `{shown}' expects a non-negative integer value with an optional k/m/g suffix"
            );
            Some(ExitCode::from(129))
        }
        Kind::OptStr if def.long == "write-midx" && !WRITE_MIDX_MODES.contains(&v) => {
            eprintln!("error: unknown value for write-midx: {v}");
            Some(ExitCode::from(129))
        }
        _ => None,
    }
}

/// Record the effect of long option `OPTS[idx]`; `negated` is true for the
/// `--no-<long>` form, which clears the flag instead of setting it.
///
/// Only the flags the pre-flight checks consult are tracked; the rest are
/// accepted and dropped, since the command bails before they could matter.
///
/// `--no-cruft` clears `cruft` but leaves `all_into_one` alone: git's `-a`/`-A`
/// and `--cruft` all set the same `ALL_INTO_ONE` bit, and the `--no-` form of a
/// bit option only clears its own bit once it has been set by that option.
fn set_long(idx: usize, negated: bool, st: &mut State) {
    let on = !negated;
    match OPTS[idx].long {
        "cruft" => {
            st.cruft = on;
            st.all_into_one |= on;
        }
        "keep-unreachable" => st.keep_unreachable = on,
        "write-bitmap-index" => st.write_bitmap = on,
        "unpack-unreachable" => st.loosen_unreachable = on,
        "write-midx" => st.write_midx = on,
        "geometric" => st.geometric = on,
        "filter" => st.filter = on,
        "filter-to" => st.filter_to = on,
        _ => {}
    }
}

/// The result of matching a long-option name against the table.
enum Resolved {
    /// `(table index, is a `--no-` negation)`.
    Unique(usize, bool),
    /// The last two candidates walked past, in table order — the pair git names.
    Ambiguous(String, String),
    Unknown,
}

/// Resolve `name` (the text between `--` and any `=`) the way parse-options
/// does: an exact match wins outright, otherwise every prefix match is
/// collected and two or more of them is an ambiguity.
fn resolve_long(name: &str) -> Resolved {
    for (idx, o) in OPTS.iter().enumerate() {
        if o.long == name {
            return Resolved::Unique(idx, false);
        }
        if o.negatable && name.strip_prefix("no-") == Some(o.long) {
            return Resolved::Unique(idx, true);
        }
    }

    // git keeps only the last two matches it walked past and names those.
    let mut last: Option<(usize, bool)> = None;
    let mut prev: Option<(usize, bool)> = None;
    for (idx, o) in OPTS.iter().enumerate() {
        if o.long.starts_with(name) {
            prev = last;
            last = Some((idx, false));
        }
        if o.negatable && format!("no-{}", o.long).starts_with(name) {
            prev = last;
            last = Some((idx, true));
        }
    }

    let display = |(idx, neg): (usize, bool)| {
        if neg {
            format!("no-{}", OPTS[idx].long)
        } else {
            OPTS[idx].long.to_string()
        }
    };
    match (prev, last) {
        (Some(p), Some(l)) => Resolved::Ambiguous(display(p), display(l)),
        (None, Some(l)) => Resolved::Unique(l.0, l.1),
        _ => Resolved::Unknown,
    }
}

/// Handle one clustered short-switch entry (`cluster` excludes the leading `-`).
fn short_opts(cluster: &str, args: &[String], i: &mut usize, st: &mut State) -> Option<ExitCode> {
    let chars: Vec<char> = cluster.chars().collect();
    let mut c = 0;
    while c < chars.len() {
        match chars[c] {
            'h' => {
                print!("{USAGE}");
                return Some(ExitCode::from(129));
            }
            'a' => st.all_into_one = true,
            'A' => {
                st.all_into_one = true;
                st.loosen_unreachable = true;
            }
            'k' => st.keep_unreachable = true,
            'b' => st.write_bitmap = true,
            // `-m` is git's undocumented short form of `--write-midx`.
            'm' => st.write_midx = true,
            'd' | 'f' | 'F' | 'n' | 'q' | 'l' | 'i' => {}
            'g' => {
                // The remainder of the cluster is the value, else the next argv.
                let rest: String = chars[c + 1..].iter().collect();
                let value = if rest.is_empty() {
                    match args.get(*i + 1) {
                        Some(v) => {
                            *i += 1;
                            v.clone()
                        }
                        None => {
                            eprintln!("error: switch `g' requires a value");
                            return Some(ExitCode::from(129));
                        }
                    }
                } else {
                    rest
                };
                if !is_number(&value, true) {
                    eprintln!(
                        "error: option `geometric' expects an integer value with an optional k/m/g suffix"
                    );
                    return Some(ExitCode::from(129));
                }
                st.geometric = true;
                *i += 1;
                return None;
            }
            other => {
                eprint!("error: unknown switch `{other}'\n{USAGE}");
                return Some(ExitCode::from(129));
            }
        }
        c += 1;
    }
    *i += 1;
    None
}

/// git's number grammar for `OPT_INTEGER` / `OPT_MAGNITUDE`: digits with an
/// optional single `k`/`m`/`g` suffix (either case), and a sign only when
/// `signed` (i.e. never for a magnitude).
fn is_number(v: &str, signed: bool) -> bool {
    let digits = match v.strip_prefix('-') {
        Some(rest) if signed => rest,
        Some(_) => return false,
        None => v,
    };
    let digits = match digits.chars().last() {
        Some('k' | 'K' | 'm' | 'M' | 'g' | 'G') => &digits[..digits.len() - 1],
        _ => digits,
    };
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// The option conflicts stock git rejects before it does any work, in git's own
/// order. Each prints `fatal: <msg>` on stderr and exits 128.
fn preflight(st: &State) -> Option<ExitCode> {
    // die_for_incompatible_opt3(-A, -k/--keep-unreachable, --cruft)
    let triad = [
        (st.loosen_unreachable, "-A"),
        (st.keep_unreachable, "-k/--keep-unreachable"),
        (st.cruft, "--cruft"),
    ];
    let set: Vec<&str> = triad.iter().filter(|(on, _)| *on).map(|(_, n)| *n).collect();
    match set.len() {
        2 => return Some(fatal(&format!(
            "options '{}' and '{}' cannot be used together",
            set[0], set[1]
        ))),
        3 => return Some(fatal(&format!(
            "options '{}', '{}', and '{}' cannot be used together",
            set[0], set[1], set[2]
        ))),
        _ => {}
    }

    if st.geometric && st.all_into_one {
        return Some(fatal("options '--geometric' and '-A/-a' cannot be used together"));
    }

    if st.write_bitmap && !st.all_into_one && !st.write_midx {
        return Some(fatal(
            "Incremental repacks are incompatible with bitmap indexes.  Use\n\
             --no-write-bitmap-index or disable the pack.writeBitmaps configuration.",
        ));
    }

    if st.filter_to && !st.filter {
        return Some(fatal(
            "option '--filter-to' can only be used along with '--filter'",
        ));
    }

    None
}

/// git's `die()` shape: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}
