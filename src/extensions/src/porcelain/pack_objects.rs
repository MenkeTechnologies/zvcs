//! `git pack-objects` — create a packed archive of objects. **Not ported: this
//! module bails once the arguments are accepted.**
//!
//! What *is* covered is the complete argument surface, and only because those
//! paths are byte-verifiable without producing a pack:
//!   * `-h` → git's 4170-byte usage block on stdout, exit 129
//!   * git's parse-options behaviour for every option in the table, including
//!     unambiguous long-option abbreviation (`--stdi` → `--stdin-packs`),
//!     `--no-` negations, `=value` vs. separate-argv values, and `-q`/`-h`
//!   * the parse-options diagnostics, each byte-for-byte: `unknown option`,
//!     `unknown switch`, `ambiguous option`, `takes no value`, `requires a
//!     value`, and the integer/magnitude value-type messages
//!   * the value-callback `fatal:`s git raises *during* parsing, in argv order:
//!     `--index-version` (git's `strtoul` grammar, including the `,<offset>`
//!     tail and the `off32_limit` sign check), `--missing`, `--stdin-packs=<mode>`
//!   * the usage-on-no-output rule (`pack_to_stdout != !base_name`, plus a
//!     second positional) and every post-parse `fatal:` git emits before it
//!     touches the object database, in git's own order: bad compression level,
//!     `--thin` without `--stdout`, the `--keep-unreachable`/`--unpack-unreachable`
//!     conflict, the two `cannot use internal rev list with ...` diagnostics,
//!     the `--stdin-packs`/`--cruft` conflict, and `--name-hash-version`
//! (all checked against git 2.55.0.)
//!
//! Everything else — i.e. `pack-objects` actually writing a pack — bails,
//! naming the substrate that is missing. It is deliberately *not* approximated.
//! With `--stdout` the pack **is** the stdout the differential harness compares,
//! so an approximation is not "slightly different output": it is a byte stream
//! that differs from the first entry onward and carries a different trailing
//! checksum. Without `--stdout` the same bytes land in
//! `<base-name>-<hash>.{pack,idx}` and the hash is echoed on stdout, so the
//! divergence shows up in post-command state as well.
//!
//! The missing substrate, concretely, in the vendored crates under `src/ported`:
//!
//!   1. **Delta compression.** git's pack writer sorts objects into a delta
//!      window (`--window`, default 10) and emits `OFS_DELTA`/`REF_DELTA`
//!      entries for whatever it finds; `--depth`, `--window-memory`,
//!      `--no-reuse-delta`, `--delta-base-offset` and `--delta-islands` all
//!      exist purely to steer that search. `gix-pack` cannot compute a delta at
//!      all: its output iterator has exactly one mode, documented as "Copy base
//!      objects and deltas from packs, while non-packed objects will be treated
//!      as base objects (i.e. without trying to delta compress them)"
//!      (`gix-pack/src/data/output/entry/iter_from_counts.rs:362-366`). Any pack
//!      it writes over loose objects is fully undeltified, so it differs from
//!      git's in entry count, entry bytes, and total size.
//!   2. **git's object order.** git orders pack entries by type, then name-hash,
//!      then size descending (`--name-hash-version` selects the hash function),
//!      because that ordering is what makes the delta window productive.
//!      `gix-pack`'s iterator emits objects in counting order. Even for a pack
//!      with no deltas at all, a different order is a different byte stream and
//!      a different pack checksum.
//!   3. **Reachability bitmaps.** `--write-bitmap-index` needs an EWAH bitmap
//!      writer, and `--use-bitmap-index` needs the counting path to consume one.
//!      The string `bitmap` does not occur anywhere under `gix-pack/src`;
//!      `gix-bitmap` is a read-only EWAH decoder.
//!   4. **Cruft packs.** `--cruft`/`--cruft-expiration` require writing a
//!      `.mtimes` file alongside the pack. No `.mtimes` reader or writer exists
//!      in `gix-pack`.
//!   5. **Thin packs.** `--thin` emits entries whose delta bases are deliberately
//!      absent from the pack — a special case of (1), and unreachable for the
//!      same reason.
//!   6. **Object filtering.** `--filter=<filter-spec>` needs the list-objects
//!      filter machinery (`blob:none`, `blob:limit=`, `tree:`, `sparse:oid=`,
//!      `object:type=`, `combine:`); nothing in the vendored crates implements
//!      that filter grammar over a reachability walk.
//!
//! Two deliberate gaps in the covered part, so this doc claims no more than the
//! code does: `pack.compression`/`core.compression` and `pack.indexVersion` are
//! not read from config, so the compression and index-version diagnostics fire
//! only for values given on the command line; and `--missing=allow-promisor`
//! does not additionally imply `--exclude-promisor-objects` handling, since the
//! command bails before either could matter.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// Stock git's `pack-objects` usage block, byte-for-byte (4170 bytes, git
/// 2.55.0), including the trailing blank line. Printed on `-h` (stdout), after
/// the `unknown option` / `unknown switch` diagnostics (stderr), on stdout after
/// the `ambiguous option` diagnostic, and on stderr on its own when neither an
/// output file nor `--stdout` was given.
const USAGE: &str = r#"usage: git pack-objects [-q | --progress | --all-progress] [--all-progress-implied]
                        [--no-reuse-delta] [--delta-base-offset] [--non-empty]
                        [--local] [--incremental] [--window=<n>] [--depth=<n>]
                        [--revs [--unpacked | --all]] [--keep-pack=<pack-name>]
                        [--cruft] [--cruft-expiration=<time>]
                        [--stdout [--filter=<filter-spec>] | <base-name>]
                        [--shallow] [--keep-true-parents] [--[no-]sparse]
                        [--name-hash-version=<n>] [--path-walk] < <object-list>

    -q, --[no-]quiet      do not show progress meter
    --[no-]progress       show progress meter
    --[no-]all-progress   show progress meter during object writing phase
    --[no-]all-progress-implied
                          similar to --all-progress when progress meter is shown
    --index-version <version>[,<offset>]
                          write the pack index file in the specified idx format version
    --max-pack-size <n>   maximum size of each output pack file
    --[no-]local          ignore borrowed objects from alternate object store
    --[no-]incremental    ignore packed objects
    --[no-]window <n>     limit pack window by objects
    --window-memory <n>   limit pack window by memory in addition to object limit
    --[no-]depth <n>      maximum length of delta chain allowed in the resulting pack
    --[no-]reuse-delta    reuse existing deltas
    --[no-]reuse-object   reuse existing objects
    --[no-]delta-base-offset
                          use OFS_DELTA objects
    --[no-]threads <n>    use threads when searching for best delta matches
    --[no-]non-empty      do not create an empty pack output
    --[no-]revs           read revision arguments from standard input
    --unpacked            limit the objects to those that are not yet packed
    --all                 include objects reachable from any reference
    --reflog              include objects referred by reflog entries
    --indexed-objects     include objects referred to by the index
    --[no-]stdin-packs[=<mode>]
                          read packs from stdin
    --[no-]stdout         output pack to stdout
    --[no-]include-tag    include tag objects that refer to objects to be packed
    --[no-]keep-unreachable
                          keep unreachable objects
    --[no-]pack-loose-unreachable
                          pack loose unreachable objects
    --[no-]unpack-unreachable[=<time>]
                          unpack unreachable objects newer than <time>
    --[no-]cruft          create a cruft pack
    --[no-]cruft-expiration[=<time>]
                          expire cruft objects older than <time>
    --[no-]sparse         use the sparse reachability algorithm
    --[no-]thin           create thin packs
    --[no-]path-walk      use the path-walk API to walk objects when possible
    --[no-]shallow        create packs suitable for shallow fetches
    --[no-]honor-pack-keep
                          ignore packs that have companion .keep file
    --[no-]keep-pack <name>
                          ignore this pack
    --[no-]compression <n>
                          pack compression level
    --[no-]keep-true-parents
                          do not hide commits by grafts
    --[no-]use-bitmap-index
                          use a bitmap index if available to speed up counting objects
    --[no-]write-bitmap-index
                          write a bitmap index together with the pack index
    --[no-]filter <args>  object filtering
    --missing <action>    handling for missing objects
    --[no-]exclude-promisor-objects
                          do not pack objects in promisor packfiles
    --[no-]exclude-promisor-objects-best-effort
                          implies --missing=allow-any
    --[no-]delta-islands  respect islands during delta compression
    --[no-]uri-protocol <protocol>
                          exclude any configured uploadpack.blobpackfileuri with this protocol
    --[no-]name-hash-version <n>
                          use the specified name-hash function to group similar objects

"#;

/// How an option consumes (and validates) its value.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `OPT_BOOL`/`OPT_SET_INT`: no value; `--opt=x` is an error.
    Bool,
    /// `OPT_INTEGER`: signed, optional single `k`/`m`/`g` suffix.
    Int,
    /// `OPT_MAGNITUDE`: as `Int` but non-negative.
    Magnitude,
    /// `OPT_STRING`/`OPT_CALLBACK`: any value, from `=` or the next argv entry.
    Str,
    /// `PARSE_OPT_OPTARG`: value only ever comes from `=`.
    OptStr,
}

/// One entry of git's `pack-objects` option table.
struct OptDef {
    long: &'static str,
    kind: Kind,
    /// Whether `--no-<long>` is accepted (`--[no-]` in the usage block).
    negatable: bool,
}

/// The long-option table **in git's declaration order**, which is the order the
/// usage block lists them in. The order is load-bearing: parse-options resolves
/// an ambiguous abbreviation by reporting the last two matches it walked past,
/// so reordering this array changes the text of `ambiguous option` diagnostics.
const OPTS: &[OptDef] = &[
    OptDef { long: "quiet", kind: Kind::Bool, negatable: true },
    OptDef { long: "progress", kind: Kind::Bool, negatable: true },
    OptDef { long: "all-progress", kind: Kind::Bool, negatable: true },
    OptDef { long: "all-progress-implied", kind: Kind::Bool, negatable: true },
    OptDef { long: "index-version", kind: Kind::Str, negatable: false },
    OptDef { long: "max-pack-size", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "local", kind: Kind::Bool, negatable: true },
    OptDef { long: "incremental", kind: Kind::Bool, negatable: true },
    OptDef { long: "window", kind: Kind::Int, negatable: true },
    OptDef { long: "window-memory", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "depth", kind: Kind::Int, negatable: true },
    OptDef { long: "reuse-delta", kind: Kind::Bool, negatable: true },
    OptDef { long: "reuse-object", kind: Kind::Bool, negatable: true },
    OptDef { long: "delta-base-offset", kind: Kind::Bool, negatable: true },
    OptDef { long: "threads", kind: Kind::Int, negatable: true },
    OptDef { long: "non-empty", kind: Kind::Bool, negatable: true },
    OptDef { long: "revs", kind: Kind::Bool, negatable: true },
    OptDef { long: "unpacked", kind: Kind::Bool, negatable: false },
    OptDef { long: "all", kind: Kind::Bool, negatable: false },
    OptDef { long: "reflog", kind: Kind::Bool, negatable: false },
    OptDef { long: "indexed-objects", kind: Kind::Bool, negatable: false },
    OptDef { long: "stdin-packs", kind: Kind::OptStr, negatable: true },
    OptDef { long: "stdout", kind: Kind::Bool, negatable: true },
    OptDef { long: "include-tag", kind: Kind::Bool, negatable: true },
    OptDef { long: "keep-unreachable", kind: Kind::Bool, negatable: true },
    OptDef { long: "pack-loose-unreachable", kind: Kind::Bool, negatable: true },
    OptDef { long: "unpack-unreachable", kind: Kind::OptStr, negatable: true },
    OptDef { long: "cruft", kind: Kind::Bool, negatable: true },
    OptDef { long: "cruft-expiration", kind: Kind::OptStr, negatable: true },
    OptDef { long: "sparse", kind: Kind::Bool, negatable: true },
    OptDef { long: "thin", kind: Kind::Bool, negatable: true },
    OptDef { long: "path-walk", kind: Kind::Bool, negatable: true },
    OptDef { long: "shallow", kind: Kind::Bool, negatable: true },
    OptDef { long: "honor-pack-keep", kind: Kind::Bool, negatable: true },
    OptDef { long: "keep-pack", kind: Kind::Str, negatable: true },
    OptDef { long: "compression", kind: Kind::Int, negatable: true },
    OptDef { long: "keep-true-parents", kind: Kind::Bool, negatable: true },
    OptDef { long: "use-bitmap-index", kind: Kind::Bool, negatable: true },
    OptDef { long: "write-bitmap-index", kind: Kind::Bool, negatable: true },
    OptDef { long: "filter", kind: Kind::Str, negatable: true },
    OptDef { long: "missing", kind: Kind::Str, negatable: false },
    OptDef { long: "exclude-promisor-objects", kind: Kind::Bool, negatable: true },
    OptDef { long: "exclude-promisor-objects-best-effort", kind: Kind::Bool, negatable: true },
    OptDef { long: "delta-islands", kind: Kind::Bool, negatable: true },
    OptDef { long: "uri-protocol", kind: Kind::Str, negatable: true },
    OptDef { long: "name-hash-version", kind: Kind::Int, negatable: true },
];

/// The only `--missing=<action>` values git accepts.
const MISSING_ACTIONS: [&str; 3] = ["error", "allow-any", "allow-promisor"];

/// The only `--stdin-packs=<mode>` values; a bare `--stdin-packs` is the empty mode.
const STDIN_PACKS_MODES: [&str; 2] = ["", "follow"];

/// The flag state git derives while parsing, i.e. everything the post-parse
/// checks look at. Options that no check consults are accepted and dropped,
/// since the command bails before they could matter.
#[derive(Default)]
struct State {
    stdout: bool,
    thin: bool,
    cruft: bool,
    stdin_packs: bool,
    unpacked: bool,
    keep_unreachable: bool,
    unpack_unreachable: bool,
    /// `--revs` and the other options that turn on git's internal rev list
    /// without `--unpacked`'s stdin-packs exemption.
    internal_rev_list: bool,
    /// `--compression=<n>`, as the integer git parsed.
    compression: Option<i64>,
    /// `--name-hash-version=<n>`, as the integer git parsed.
    name_hash_version: Option<i64>,
    /// Non-option arguments; at most one (the base name) is legal.
    positionals: usize,
}

/// The outcome of parsing: either a fully-formed request, or a diagnostic that
/// has already decided the exit code.
enum Parsed {
    Ok(State),
    Exit(ExitCode),
}

/// `git pack-objects` — argument validation and pre-flight checks only; writing
/// the pack itself is not ported.
///
/// Returns 129 with git's own output for `-h`, for every malformed invocation,
/// and when neither `--stdout` nor exactly one base name was given; 128 for the
/// value and option conflicts git rejects before it opens the object database.
/// Any invocation that survives both bails, naming the substrate that is
/// missing; see the module documentation for the full list.
pub fn pack_objects(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `pack-objects` does take a
    // positional (the base name), so the leading verb must be dropped rather
    // than counted as one.
    let args = match args.first().map(String::as_str) {
        Some("pack-objects") => &args[1..],
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
        "pack-objects is not ported: gix-pack has no delta compression (its only output mode \
         copies existing pack entries and stores everything else undeltified), does not \
         reproduce git's type/name-hash/size entry ordering, has no EWAH bitmap writer for \
         --write-bitmap-index, no .mtimes writer for --cruft, no thin-pack support, and no \
         list-objects filter for --filter (ported: -h, argument validation, and the \
         pre-flight value and option-conflict checks only)"
    )
}

/// Walk `args` exactly the way git's parse-options walks them, emitting git's
/// diagnostics verbatim on the first entry it rejects.
fn parse(args: &[String]) -> Parsed {
    let mut st = State::default();
    let mut end_of_opts = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();

        if end_of_opts || !a.starts_with('-') || a == "-" {
            st.positionals += 1;
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

        // Clustered short switches; `pack-objects` declares only `-q` (plus the
        // implicit `-h`).
        match short_opts(&a[1..], &mut i) {
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
        set_long(def.long, None, false, st);
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

    set_long(def.long, value, true, st);
    *i += 1;
    None
}

/// Validate a value against the option's parse-options type and, for the three
/// options git validates in a callback, against that callback's own grammar.
///
/// The type diagnostics exit 129; the callback ones are `die()`s and exit 128.
/// Both fire during the parse walk, so they are reported in argv order and
/// before the no-output usage check.
fn check_value(def: &OptDef, shown: &str, v: &str) -> Option<ExitCode> {
    match def.kind {
        Kind::Int if !is_number(v, true) => {
            eprintln!(
                "error: option `{shown}' expects an integer value with an optional k/m/g suffix"
            );
            return Some(ExitCode::from(129));
        }
        Kind::Magnitude if !is_number(v, false) => {
            eprintln!(
                "error: option `{shown}' expects a non-negative integer value with an optional k/m/g suffix"
            );
            return Some(ExitCode::from(129));
        }
        _ => {}
    }

    match def.long {
        "index-version" => check_index_version(v),
        "missing" if !MISSING_ACTIONS.contains(&v) => {
            Some(fatal(&format!("invalid value for '--missing': '{v}'")))
        }
        "stdin-packs" if !STDIN_PACKS_MODES.contains(&v) => {
            Some(fatal(&format!("invalid value for 'stdin-packs': '{v}'")))
        }
        _ => None,
    }
}

/// git's `parse_index_version()` callback, which is `strtoul`-shaped rather than
/// parse-options-shaped: the number is read greedily, an optional `,<offset>`
/// tail follows, and anything left over is an error. Both diagnostics quote the
/// argument as written, which is why `--index-version=-1` reports "unsupported"
/// (the unsigned read wraps past 2) rather than "bad".
fn check_index_version(v: &str) -> Option<ExitCode> {
    let (version, rest) = strtoul(v);
    if version > 2 {
        return Some(fatal(&format!("unsupported index version {v}")));
    }

    // The `,<offset>` tail is only read when a digit could follow the comma; a
    // bare trailing comma is left in `rest` and reported as a bad version.
    let (off32_limit, rest) = match rest.strip_prefix(',').filter(|t| !t.is_empty()) {
        Some(tail) => strtoul(tail),
        None => (0, rest),
    };
    if !rest.is_empty() || off32_limit & 0x8000_0000 != 0 {
        return Some(fatal(&format!("bad index version '{v}'")));
    }
    None
}

/// C's `strtoul` over a base-10 prefix of `s`: an optional sign, then digits,
/// wrapping on overflow and on a negative sign. Returns the value and the
/// unconsumed remainder (which is all of `s` when there are no digits).
fn strtoul(s: &str) -> (u64, &str) {
    let (negative, digits_at) = match s.as_bytes().first() {
        Some(b'-') => (true, 1),
        Some(b'+') => (false, 1),
        _ => (false, 0),
    };
    let digits: String = s[digits_at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return (0, s);
    }

    let mut value: u64 = 0;
    for c in digits.chars() {
        value = value
            .wrapping_mul(10)
            .wrapping_add(u64::from(c as u8 - b'0'));
    }
    if negative {
        value = 0u64.wrapping_sub(value);
    }
    (value, &s[digits_at + digits.len()..])
}

/// Record the effect of long option `long`; `on` is false for the `--no-` form.
///
/// Only the flags the post-parse checks consult are tracked.
fn set_long(long: &str, value: Option<&str>, on: bool, st: &mut State) {
    match long {
        "stdout" => st.stdout = on,
        "thin" => st.thin = on,
        "cruft" => st.cruft = on,
        "stdin-packs" => st.stdin_packs = on,
        "unpacked" => st.unpacked = on,
        "keep-unreachable" => {
            st.keep_unreachable = on;
            st.internal_rev_list |= on;
        }
        "unpack-unreachable" => {
            st.unpack_unreachable = on;
            st.internal_rev_list |= on;
        }
        "revs" | "all" | "reflog" | "indexed-objects" | "pack-loose-unreachable" => {
            st.internal_rev_list |= on;
        }
        "compression" => st.compression = on.then(|| to_number(value.unwrap_or("0"))).flatten(),
        "name-hash-version" => {
            st.name_hash_version = on.then(|| to_number(value.unwrap_or("0"))).flatten();
        }
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
/// `-q` is the only declared switch; `-h` is parse-options' built-in.
fn short_opts(cluster: &str, i: &mut usize) -> Option<ExitCode> {
    for c in cluster.chars() {
        match c {
            'h' => {
                print!("{USAGE}");
                return Some(ExitCode::from(129));
            }
            'q' => {}
            other => {
                eprint!("error: unknown switch `{other}'\n{USAGE}");
                return Some(ExitCode::from(129));
            }
        }
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

/// The integer value of a string already accepted by [`is_number`], applying the
/// `k`/`m`/`g` multiplier. This is the number git's diagnostics print.
fn to_number(v: &str) -> Option<i64> {
    let (negative, body) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v),
    };
    let (body, scale) = match body.chars().last() {
        Some('k' | 'K') => (&body[..body.len() - 1], 1024),
        Some('m' | 'M') => (&body[..body.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&body[..body.len() - 1], 1024 * 1024 * 1024),
        _ => (body, 1),
    };
    let n = body.parse::<i64>().ok()?.checked_mul(scale)?;
    Some(if negative { -n } else { n })
}

/// Everything stock git checks after parsing and before it does any work, in
/// git's own order (each ordering below is pinned by an observed invocation).
///
/// The first check prints the bare usage block on stderr and exits 129; the rest
/// are `die()`s and exit 128.
fn preflight(st: &State) -> Option<ExitCode> {
    // `pack_to_stdout != !base_name`, plus git's rejection of a second
    // positional. Beats every `fatal:` below: `--compression=99` on its own
    // reports usage, not a bad compression level.
    if st.stdout == (st.positionals == 1) || st.positionals > 1 {
        eprint!("{USAGE}");
        return Some(ExitCode::from(129));
    }

    // Beats `--thin`: `pack-objects base --thin --compression=99` reports the
    // compression level.
    if let Some(level) = st.compression {
        if !(-1..=9).contains(&level) {
            return Some(fatal(&format!("bad pack compression level {level}")));
        }
    }

    // Beats the conflicts below: `pack-objects base --thin --cruft --revs`
    // reports the thin pack.
    if st.thin && !st.stdout {
        return Some(fatal("--thin cannot be used to build an indexable pack"));
    }

    // Beats the rev-list checks: adding `--cruft --revs` to this pair still
    // reports the pair.
    if st.keep_unreachable && st.unpack_unreachable {
        return Some(fatal(
            "options '--keep-unreachable' and '--unpack-unreachable' cannot be used together",
        ));
    }

    // `--unpacked` is deliberately absent from this condition: it is the one
    // rev-list-implying option documented as compatible with `--stdin-packs`,
    // and `--stdout --stdin-packs --unpacked` is accepted.
    if st.stdin_packs && st.internal_rev_list {
        return Some(fatal("cannot use internal rev list with --stdin-packs"));
    }

    if st.stdin_packs && st.cruft {
        return Some(fatal(
            "options '--stdin-packs' and '--cruft' cannot be used together",
        ));
    }

    // Here `--unpacked` does count: `--stdout --cruft --unpacked` is rejected.
    if st.cruft && (st.internal_rev_list || st.unpacked) {
        return Some(fatal("cannot use internal rev list with --cruft"));
    }

    // Last: `--stdout --name-hash-version=9 --cruft --revs` reports the cruft
    // conflict. A negative value selects git's default and is accepted.
    if let Some(version) = st.name_hash_version {
        if version >= 0 && !(1..=2).contains(&version) {
            return Some(fatal(&format!(
                "invalid --name-hash-version option: {version}"
            )));
        }
    }

    None
}

/// git's `die()` shape: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}
