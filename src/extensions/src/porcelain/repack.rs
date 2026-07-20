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
//!     `requires a value`, plus the integer/magnitude value-type messages and
//!     the `not in range [-2147483648,2147483647]` message for an integer that
//!     overflows a C `int` once its `k`/`m`/`g` suffix is applied
//!   * `--filter` spec validation, which is a parse-options callback and so
//!     dies (exit 128) at its own position in argv: `invalid filter-spec`,
//!     `expected 'tree:<depth>'`, `expected something after combine:`,
//!     `sparse:path filters support has been dropped`, and the
//!     `object:type=<type>` message
//!   * the pre-flight option-conflict `fatal:`s that stock git emits before it
//!     does any work at all (exit 128): the `-A`/`-k`/`--cruft` triad, geometric
//!     vs. `-a`/`-A`, incremental-with-bitmaps, `--filter-to` without
//!     `--filter`, and — last of the five — `invalid --name-hash-version
//!     option: <n>` for any version above 2
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
//! Smaller, deliberate gaps in the covered part, so this doc claims no more
//! than the code does:
//!   * the `warning: minimum pack size limit is 1 MiB` that git prints for
//!     `--max-pack-size` below 1 MiB is not emitted;
//!   * the `repack.writeBitmaps` / `pack.writeBitmaps` config keys are not read,
//!     so the incremental-with-bitmaps `fatal:` fires only when `-b` is given
//!     explicitly;
//!   * `--filter=sparse:oid=<rev>` is accepted on syntax alone — git's rejection
//!     of it depends on resolving and parsing the named blob;
//!   * `combine:` sub-specs are not percent-decoded;
//!   * with an invalid *integer* value earlier in argv than an invalid filter
//!     spec, git reports the filter (`--window=x --filter=bogus:spec` → exit
//!     128) while this reports the integer (exit 129). The mechanism behind
//!     that inversion was not identified, and the ordering is otherwise
//!     positional, so the positional behaviour is what is implemented.

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
    /// The scaled value of the last `--name-hash-version`; 0 when unset or
    /// cleared by `--no-name-hash-version`, which is the default git accepts.
    name_hash_version: i64,
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
        set_long(idx, true, None, st);
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
        // `--filter` is an `OPT_PARSE_LIST_OBJECTS_FILTER` callback, so a bad
        // spec dies where it sits in argv rather than at the end of parsing:
        // `--filter=bogus:spec --zzz` reports the filter, `--zzz
        // --filter=bogus:spec` reports the unknown option (git 2.55.0).
        if def.long == "filter" {
            if let Some(msg) = filter_error(v) {
                return Some(fatal(&msg));
            }
        }
    }

    set_long(idx, false, value, st);
    *i += 1;
    None
}

/// git's `OPT_INTEGER` value, scaled by an optional single `k`/`m`/`g` factor of
/// 1024, or `None` when the text is not a number at all. Computed in `i128` so
/// that a value far outside `long` still yields a magnitude the range check can
/// reject, matching `strtol`'s `ERANGE` path.
fn scaled(v: &str) -> Option<i128> {
    let (negative, rest) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v),
    };
    let (digits, factor) = match rest.chars().last() {
        Some('k' | 'K') => (&rest[..rest.len() - 1], 1024i128),
        Some('m' | 'M') => (&rest[..rest.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&rest[..rest.len() - 1], 1024 * 1024 * 1024),
        _ => (rest, 1),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // A literal too long for `i128` is out of range by any measure; saturating
    // keeps it on the range-error path instead of the type-error one.
    let n = digits.parse::<i128>().unwrap_or(i128::MAX / factor);
    let n = n.saturating_mul(factor);
    Some(if negative { -n } else { n })
}

/// Validate a `--filter` spec the way `gently_parse_list_objects_filter` does,
/// returning the text git puts after `fatal: ` for the first rejection.
///
/// `sparse:oid=<rev>` is accepted on syntax alone: git's own rejection of it
/// depends on resolving the object, and this command bails before reaching the
/// object database either way.
///
/// Not covered: the percent-decoding git applies to each `combine:` sub-spec.
fn filter_error(spec: &str) -> Option<String> {
    let invalid = || Some(format!("invalid filter-spec '{spec}'"));

    if let Some(rest) = spec.strip_prefix("combine:") {
        if rest.is_empty() {
            return Some("expected something after combine:".to_string());
        }
        // Empty sub-specs are skipped, so `combine:+` is accepted.
        return rest.split('+').filter(|s| !s.is_empty()).find_map(filter_error);
    }

    match spec {
        "blob:none" => None,
        _ if spec.starts_with("blob:limit=") => {
            // `git_parse_ulong`: digits with an optional k/m/g, never signed.
            match scaled(&spec["blob:limit=".len()..]) {
                Some(n) if n >= 0 => None,
                _ => invalid(),
            }
        }
        _ if spec.starts_with("tree:") => match scaled(&spec["tree:".len()..]) {
            Some(n) if n >= 0 => None,
            _ => Some("expected 'tree:<depth>'".to_string()),
        },
        _ if spec.starts_with("sparse:path=") => {
            Some("sparse:path filters support has been dropped".to_string())
        }
        _ if spec.starts_with("sparse:oid=") => None,
        _ if spec.starts_with("object:type=") => {
            let ty = &spec["object:type=".len()..];
            if matches!(ty, "blob" | "tree" | "commit" | "tag") {
                None
            } else {
                Some(format!("'{ty}' for 'object:type=<type>' is not a valid object type"))
            }
        }
        _ => invalid(),
    }
}

/// Validate a value against the option's parse-options type, emitting git's
/// exact type diagnostic on failure.
fn check_value(def: &OptDef, shown: &str, v: &str) -> Option<ExitCode> {
    // parse-options names a long option as ``option `x'`` and a short one as
    // ``switch `x'``; only `-g` reaches the value checks by its short form.
    let label = format!("option `{shown}'");
    match def.kind {
        Kind::Int => int_value(&label, v).err(),
        Kind::Magnitude if v.is_empty() => Some(numerical_value(&label)),
        Kind::Magnitude if !is_number(v, false) => {
            eprintln!(
                "error: {label} expects a non-negative integer value with an optional k/m/g suffix"
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

/// git's diagnostic for an empty value, which every numeric option shares.
fn numerical_value(label: &str) -> ExitCode {
    eprintln!("error: {label} expects a numerical value");
    ExitCode::from(129)
}

/// Parse an `OPT_INTEGER` value for the already-formatted `label` (e.g.
/// ``option `geometric'`` or ``switch `g'``), emitting git's type diagnostic for
/// non-numbers and its range diagnostic for anything a C `int` cannot hold
/// (`--name-hash-version=3g` scales to 3 GiB and hits the latter). Every one of
/// these prints a single line and exits 129, with no usage block.
fn int_value(label: &str, v: &str) -> Result<i64, ExitCode> {
    if v.is_empty() {
        return Err(numerical_value(label));
    }
    let n = match scaled(v) {
        Some(n) => n,
        None => {
            eprintln!("error: {label} expects an integer value with an optional k/m/g suffix");
            return Err(ExitCode::from(129));
        }
    };
    if n < i32::MIN as i128 || n > i32::MAX as i128 {
        eprintln!(
            "error: value {v} for {label} not in range [{},{}]",
            i32::MIN,
            i32::MAX
        );
        return Err(ExitCode::from(129));
    }
    Ok(n as i64)
}

/// Record the effect of long option `OPTS[idx]`; `negated` is true for the
/// `--no-<long>` form, which clears the flag instead of setting it. `value` is
/// the option's argument, already validated, for the one option whose value the
/// pre-flight checks read.
///
/// Only the flags the pre-flight checks consult are tracked; the rest are
/// accepted and dropped, since the command bails before they could matter.
///
/// `--no-cruft` clears `cruft` but leaves `all_into_one` alone: git's `-a`/`-A`
/// and `--cruft` all set the same `ALL_INTO_ONE` bit, and the `--no-` form of a
/// bit option only clears its own bit once it has been set by that option.
fn set_long(idx: usize, negated: bool, value: Option<&str>, st: &mut State) {
    let on = !negated;
    match OPTS[idx].long {
        // `--no-name-hash-version` restores the default, which git accepts.
        "name-hash-version" => {
            st.name_hash_version = match value {
                Some(v) if on => scaled(v).unwrap_or(0) as i64,
                _ => 0,
            }
        }
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
                if let Err(code) = int_value("switch `g'", &value) {
                    return Some(code);
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

    // git only knows name hash versions 1 and 2; it leaves everything at or
    // below 0 alone, since 0 is the "unset" default and a negative value never
    // reaches the hashing code.
    if st.name_hash_version > 2 {
        return Some(fatal(&format!(
            "invalid --name-hash-version option: {}",
            st.name_hash_version
        )));
    }

    None
}

/// git's `die()` shape: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}
