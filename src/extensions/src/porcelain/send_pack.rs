//! `git send-pack` — push objects over the git protocol. **Not ported: this
//! module bails once the arguments are accepted.**
//!
//! What *is* covered is the complete argument surface, and only because those
//! paths are byte-verifiable without opening a connection:
//!   * `-h` → git's 1472-byte usage block on stdout, exit 129
//!   * git's parse-options behaviour for every option in the table, including
//!     unambiguous long-option abbreviation (`--sign` → `--signed`), `--no-`
//!     negations, `=value` vs. separate-argv values, and the `-v`/`-q`/`-n`/`-f`
//!     short switches (clustered as well as separate)
//!   * the parse-options diagnostics, each byte-for-byte: `unknown option`,
//!     `unknown switch`, `ambiguous option`, `takes no value`, and `requires a
//!     value`
//!   * the value callbacks git runs *during* parsing, in argv order:
//!     `--signed=<value>` (git's `git_parse_maybe_bool` grammar plus the
//!     `if-asked` special case, `die`ing with `bad signed argument: <value>`),
//!     and `--force-with-lease=<ref>:<expect>` (whose `<expect>` is resolved as
//!     a revision, reporting `cannot parse expected object name '<expect>'`)
//!   * the two post-parse usage checks: a missing `<directory>`, and git's
//!     rule that `--all` and `--mirror` are mutually exclusive and that neither
//!     may be combined with explicit `<ref>` arguments
//! (all checked against git 2.55.0.)
//!
//! Everything else — i.e. `send-pack` actually pushing — bails, naming the
//! substrate that is missing. It is deliberately *not* approximated. A partial
//! implementation would mutate refs in the *receiving* repository, which is
//! precisely the post-command state a differential harness inspects, and a push
//! that lands the wrong pack or the wrong ref update is indistinguishable from
//! success at the exit-code level.
//!
//! The missing substrate, concretely, in the vendored crates under `src/ported`:
//!
//!   1. **No receive-pack client at all.** `gix-protocol`'s v2 command
//!      abstraction knows exactly two commands, `ls-refs` and `fetch`
//!      (`gix-protocol/src/command/mod.rs`, `Command::as_str`), and the crate
//!      exports only the `handshake`, `ls_refs` and `fetch` modules
//!      (`gix-protocol/src/lib.rs`). `gix_transport::Service::ReceivePack`
//!      exists as a name (`gix-transport/src/lib.rs:43`), but nothing in the
//!      vendored tree ever requests it: the whole `git-receive-pack` side of the
//!      protocol — the `<old> <new> <ref>\0<capabilities>` command list, the
//!      flush, and the pack that follows — is unimplemented.
//!   2. **No report-status parsing.** The strings `report-status` and
//!      `report_status` do not occur anywhere under `gix-protocol/src`,
//!      `gix-transport/src` or `gix/src`. `send-pack`'s entire stdout and its
//!      exit code are derived from the server's `unpack ok` / `ng <ref> <why>`
//!      report, so without a parser there is nothing to print and no way to know
//!      whether the push succeeded.
//!   3. **No pack generation fit to send.** The pack `send-pack` streams is
//!      produced by `pack-objects`. `gix-pack` cannot compute a delta: its
//!      output iterator has one mode, documented as "Copy base objects and
//!      deltas from packs, while non-packed objects will be treated as base
//!      objects (i.e. without trying to delta compress them)"
//!      (`gix-pack/src/data/output/entry/iter_from_counts.rs`). `--thin`, which
//!      is what `send-pack` uses by default under `push`, is a stronger form of
//!      the same requirement — entries whose delta bases are deliberately
//!      absent — and is unreachable for the same reason.
//!   4. **No push certificates.** `--signed` requires generating and signing a
//!      push cert with the `push-cert` capability. There is no push-cert writer
//!      in the vendored crates and no signing path that produces one.
//!   5. **No `--atomic` / `--push-option` transport support.** Both are
//!      capabilities negotiated on the receive-pack side, which (1) rules out.
//!      `gix-protocol`'s crate status lists `push` and, under it, "report-status,
//!      sideband, delete-refs, push-options and atomic pushes" as unimplemented
//!      (`src/ported/crate-status.md`).
//!
//! Two deliberate gaps in the covered part, so this doc claims no more than the
//! code does. `--force-with-lease=<ref>:<expect>` resolves `<expect>` through
//! gitoxide's `rev_parse_single` rather than git's `repo_get_oid`; the two
//! accept the same everyday spellings but are not proven byte-identical on
//! exotic ones. And the `fatal: '<dest>' does not appear to be a git
//! repository` diagnostic git emits for an unreachable destination is not
//! reproduced — that message comes from the connection attempt, which is the
//! part that does not exist.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// Stock git's `send-pack` usage block, byte-for-byte (1472 bytes, git 2.55.0),
/// including the trailing blank line. Printed on `-h` (stdout), after the
/// `unknown option` / `unknown switch` diagnostics (stderr), on stdout after the
/// `ambiguous option` diagnostic, and on stderr on its own for the two
/// post-parse usage checks.
const USAGE: &str = r#"usage: git send-pack [--mirror] [--dry-run] [--force]
                     [--receive-pack=<git-receive-pack>]
                     [--verbose] [--thin] [--atomic]
                     [--[no-]signed | --signed=(true|false|if-asked)]
                     [<host>:]<directory> (--all | <ref>...)

    -v, --[no-]verbose    be more verbose
    -q, --[no-]quiet      be more quiet
    --[no-]receive-pack <receive-pack>
                          receive pack program
    --[no-]exec <receive-pack>
                          receive pack program
    --[no-]remote <remote>
                          remote name
    --[no-]all            push all refs
    -n, --[no-]dry-run    dry run
    --[no-]mirror         mirror all refs
    -f, --[no-]force      force updates
    --[no-]signed[=(yes|no|if-asked)]
                          GPG sign the push
    --[no-]push-option <server-specific>
                          option to transmit
    --[no-]progress       force progress reporting
    --[no-]thin           use thin pack
    --[no-]atomic         request atomic transaction on remote side
    --[no-]stateless-rpc  use stateless RPC protocol
    --[no-]stdin          read refs from stdin
    --[no-]helper-status  print status from remote helper
    --[no-]force-with-lease[=<refname>:<expect>]
                          require old value of ref to be at this value
    --[no-]force-if-includes
                          require remote updates to be integrated locally

"#;

/// How an option consumes (and validates) its value.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `OPT_BOOL`/`OPT_SET_INT`: no value; `--opt=x` is an error.
    Bool,
    /// `OPT_STRING`/`OPT_CALLBACK`: any value, from `=` or the next argv entry.
    Str,
    /// `PARSE_OPT_OPTARG`: value only ever comes from `=`, and may be absent.
    OptStr,
}

/// One entry of git's `send-pack` option table. Every option in this table is
/// negatable — the usage block spells all nineteen with `--[no-]`.
struct OptDef {
    long: &'static str,
    kind: Kind,
}

/// The long-option table **in git's declaration order**, which is the order the
/// usage block lists them in. The order is load-bearing: parse-options resolves
/// an ambiguous abbreviation by reporting the last two matches it walked past,
/// so reordering this array changes the text of `ambiguous option` diagnostics
/// (`--s` names `--stateless-rpc` and `--stdin`, not `--signed`).
const OPTS: &[OptDef] = &[
    OptDef { long: "verbose", kind: Kind::Bool },
    OptDef { long: "quiet", kind: Kind::Bool },
    OptDef { long: "receive-pack", kind: Kind::Str },
    OptDef { long: "exec", kind: Kind::Str },
    OptDef { long: "remote", kind: Kind::Str },
    OptDef { long: "all", kind: Kind::Bool },
    OptDef { long: "dry-run", kind: Kind::Bool },
    OptDef { long: "mirror", kind: Kind::Bool },
    OptDef { long: "force", kind: Kind::Bool },
    OptDef { long: "signed", kind: Kind::OptStr },
    OptDef { long: "push-option", kind: Kind::Str },
    OptDef { long: "progress", kind: Kind::Bool },
    OptDef { long: "thin", kind: Kind::Bool },
    OptDef { long: "atomic", kind: Kind::Bool },
    OptDef { long: "stateless-rpc", kind: Kind::Bool },
    OptDef { long: "stdin", kind: Kind::Bool },
    OptDef { long: "helper-status", kind: Kind::Bool },
    OptDef { long: "force-with-lease", kind: Kind::OptStr },
    OptDef { long: "force-if-includes", kind: Kind::Bool },
];

/// The flag state git derives while parsing, i.e. everything the post-parse
/// checks look at. Options no check consults are accepted and dropped, since the
/// command bails before they could matter.
#[derive(Default)]
struct State {
    send_all: bool,
    send_mirror: bool,
    /// Non-option arguments: the first is `<directory>`, the rest are `<ref>`s.
    positionals: usize,
}

/// The outcome of parsing: either a fully-formed request, or a diagnostic that
/// has already decided the exit code.
enum Parsed {
    Ok(State),
    Exit(ExitCode),
}

/// `git send-pack` — argument validation and pre-flight checks only; the push
/// itself is not ported.
///
/// Returns 129 with git's own output for `-h`, for every malformed invocation,
/// for a missing `<directory>`, and for the `--all`/`--mirror`/`<ref>` conflict;
/// 128 for the `--signed` value git rejects during parsing. Any invocation that
/// survives both bails, naming the substrate that is missing; see the module
/// documentation for the full list.
pub fn send_pack(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `send-pack` does take positionals
    // (the destination and the refs), so the leading verb must be dropped rather
    // than counted as one.
    let args = match args.first().map(String::as_str) {
        Some("send-pack") => &args[1..],
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
        "send-pack is not ported: gix-protocol has no receive-pack client (its command \
         abstraction knows only ls-refs and fetch, so the `<old> <new> <ref>` command list and \
         the pack that follows are never sent), no report-status parser (which is where \
         send-pack's entire stdout and exit code come from), no push-certificate writer for \
         --signed, and no atomic/push-option capability negotiation; gix-pack additionally has \
         no delta compression and so cannot build the thin pack send-pack streams (ported: -h, \
         argument validation, the --signed and --force-with-lease value callbacks, and the \
         post-parse usage checks only)"
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
        set_long(def.long, false, st);
        *i += 1;
        return None;
    }

    let value = match def.kind {
        Kind::Bool => None,
        // `PARSE_OPT_OPTARG` only ever reads a value glued on with `=`; a bare
        // `--signed` / `--force-with-lease` passes NULL to its callback.
        Kind::OptStr => inline,
        Kind::Str => match inline {
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

    if let Some(code) = check_value(def, value) {
        return Some(code);
    }

    set_long(def.long, true, st);
    *i += 1;
    None
}

/// Run the value callback for the two options that declare one. Both fire during
/// the parse walk, so they are reported in argv order and before the post-parse
/// usage checks.
fn check_value(def: &OptDef, value: Option<&str>) -> Option<ExitCode> {
    match def.long {
        // `option_parse_push_signed`: a boolean, or `if-asked`, or a `die()`.
        // A bare `--signed` (value `None`) means "always" and is accepted.
        "signed" => match value {
            Some(v) if parse_maybe_bool(v).is_none() && !v.eq_ignore_ascii_case("if-asked") => {
                eprintln!("fatal: bad signed argument: {v}");
                Some(ExitCode::from(128))
            }
            _ => None,
        },
        // `parse_push_cas_option`: `<refname>[:<expect>]`. A bare option, or one
        // whose `<expect>` is empty or absent, means "use the tracking ref" and
        // resolves nothing; otherwise `<expect>` must name an object.
        "force-with-lease" => {
            let expect = value?.split_once(':')?.1;
            if expect.is_empty() || resolve_rev(expect) {
                return None;
            }
            eprintln!("error: cannot parse expected object name '{expect}'");
            Some(ExitCode::from(129))
        }
        _ => None,
    }
}

/// git's `git_parse_maybe_bool`: the boolean words (case-insensitive), the empty
/// string as false, or any integer [`parse_int`] accepts (non-zero being true).
/// `None` for anything else, which is what makes `--signed=<value>` `die`.
fn parse_maybe_bool(v: &str) -> Option<bool> {
    if v.is_empty() {
        return Some(false);
    }
    for word in ["true", "yes", "on"] {
        if v.eq_ignore_ascii_case(word) {
            return Some(true);
        }
    }
    for word in ["false", "no", "off"] {
        if v.eq_ignore_ascii_case(word) {
            return Some(false);
        }
    }
    parse_int(v).map(|n| n != 0)
}

/// git's `git_parse_int`, i.e. C `strtoimax(value, &end, 0)` followed by
/// `get_unit_factor(end)`: optional leading whitespace and sign, then digits in
/// a base the prefix selects (`0x` hex, a leading `0` octal, otherwise decimal),
/// then an optional single `k`/`m`/`g` suffix and nothing else. No digits, a
/// trailing suffix that is not a unit, or a result outside `int` range is a
/// failure — the bound really is 32-bit, since git passes
/// `maximum_signed_value_of_type(int)` as the maximum (`--signed=2147483647` is
/// accepted, `--signed=2147483648` `die`s).
fn parse_int(v: &str) -> Option<i32> {
    let s = v.trim_start();
    let (negative, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };

    let (digits, radix) = if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (rest, 16)
    } else if s.len() > 1 && s.starts_with('0') {
        (&s[1..], 8)
    } else {
        (s, 10)
    };

    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }

    let factor: i64 = match &digits[end..] {
        "" => 1,
        "k" | "K" => 1024,
        "m" | "M" => 1024 * 1024,
        "g" | "G" => 1024 * 1024 * 1024,
        _ => return None,
    };
    let magnitude = i64::from_str_radix(&digits[..end], radix)
        .ok()?
        .checked_mul(factor)?;
    // The sign is applied before the range check, so `-2g` (exactly `INT_MIN`)
    // is accepted while `2g` is not.
    i32::try_from(if negative { -magnitude } else { magnitude }).ok()
}

/// Whether `spec` names an object in the current repository, i.e. whether git's
/// `repo_get_oid` would have succeeded. Failing to open a repository at all
/// counts as "unresolvable", matching git's own outcome there.
fn resolve_rev(spec: &str) -> bool {
    let Ok(repo) = gix::discover(".") else {
        return false;
    };
    repo.rev_parse_single(spec).is_ok()
}

/// Record the effect of long option `long`; `on` is false for the `--no-` form.
///
/// Only the two flags the post-parse checks consult are tracked.
fn set_long(long: &str, on: bool, st: &mut State) {
    match long {
        "all" => st.send_all = on,
        "mirror" => st.send_mirror = on,
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
/// does: an exact match wins outright, otherwise every prefix match is collected
/// and two or more of them is an ambiguity.
fn resolve_long(name: &str) -> Resolved {
    for (idx, o) in OPTS.iter().enumerate() {
        if o.long == name {
            return Resolved::Unique(idx, false);
        }
        if name.strip_prefix("no-") == Some(o.long) {
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
        if format!("no-{}", o.long).starts_with(name) {
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
/// `send-pack` declares `-v`, `-q`, `-n` and `-f`; `-h` is parse-options' own.
fn short_opts(cluster: &str, i: &mut usize) -> Option<ExitCode> {
    for c in cluster.chars() {
        match c {
            'h' => {
                print!("{USAGE}");
                return Some(ExitCode::from(129));
            }
            // None of these feed a post-parse check, so they are accepted and
            // dropped; `-n` is `--dry-run` and `-f` is `--force`.
            'v' | 'q' | 'n' | 'f' => {}
            other => {
                eprint!("error: unknown switch `{other}'\n{USAGE}");
                return Some(ExitCode::from(129));
            }
        }
    }
    *i += 1;
    None
}

/// The two checks stock git makes after parsing and before it connects, in git's
/// own order. Both print the bare usage block on stderr and exit 129.
fn preflight(st: &State) -> Option<ExitCode> {
    // `if (!dest) usage_with_options(...)` — the destination is the first
    // positional, so no positionals at all means no destination.
    if st.positionals == 0 {
        eprint!("{USAGE}");
        return Some(ExitCode::from(129));
    }

    // "--all and --mirror are incompatible; neither makes sense with any
    // refspecs." Refspecs are every positional past the destination.
    let refspecs = st.positionals - 1;
    if (refspecs > 0 && (st.send_all || st.send_mirror)) || (st.send_all && st.send_mirror) {
        eprint!("{USAGE}");
        return Some(ExitCode::from(129));
    }

    None
}
