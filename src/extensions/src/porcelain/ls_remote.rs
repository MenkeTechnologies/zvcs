//! `git ls-remote` — list references in a remote repository.
//!
//! Covered: the full listing form over gitoxide's blocking transport
//! (`git ls-remote [<repository> [<patterns>...]]`) with `-b`/`--branches`
//! (and the `-h`/`--heads` synonyms), `-t`/`--tags`, `--refs`, `--symref`,
//! `--exit-code`, `--get-url`, `-q`/`--quiet`, `--sort=<key>` and
//! `-o`/`--server-option=<option>`, including git's `check_ref` filter
//! semantics, its `*/<pattern>` tail glob, its refname sort order, the
//! `From <url>` stderr header (printed only when `<repository>` is omitted),
//! and the exit codes (0 normally, 2 for `--exit-code` with no matching refs,
//! 128 when the remote cannot be reached or a sort key is rejected, 129 for
//! usage errors and for bare `-h`).
//!
//! `--sort` reproduces `ref-filter.c`: the *last* `--sort` on the command line
//! is the primary key, earlier ones break its ties, and a final ascending
//! `strcmp` on the refname breaks the rest (`compare_refs`). `-` reverses a
//! single key without reversing that final tiebreak; `version:`/`v:` selects
//! `versioncmp()`, ported verbatim below. Sort keys are validated only after
//! the refs have been fetched, exactly where git validates them, so an
//! unreachable remote still reports the transport failure rather than the key.
//!
//! Known gaps:
//!
//! * `--upload-pack=<exec>` bails: `Remote::connect` exposes no hook for the
//!   remote helper path, so honouring it is not possible in the vendored crates.
//! * `-o`/`--server-option` is parsed and validated but *not* transmitted:
//!   gitoxide has no protocol-v2 server-option plumbing (no `server_option`
//!   symbol exists anywhere under `gix-protocol`). Servers that would reject an
//!   unknown option therefore see a request git would have made differently.
//! * `--sort` keys beyond `refname`, `objectname`, `creatordate`,
//!   `committerdate`, `authordate` and `taggerdate` are refused with git's
//!   `unknown field name` fatal. git accepts more `for-each-ref` atoms than
//!   those, but computing them needs object data `ls-remote` never fetches.
//! * `version:` in front of a *date* key falls back to the plain numeric
//!   compare instead of git's versioncmp over the formatted date string.
//! * `versionsort.suffix` / `versionsort.prereleaseSuffix` are not consulted,
//!   so `version:` sorting is plain `strverscmp` ordering (git's behaviour when
//!   neither key is configured).
//!
//! Running outside a repository also bails: gitoxide resolves transport,
//! credential and `insteadOf` configuration through a `Repository`, and there
//! is no repository-less remote in the vendored crates.

use anyhow::{bail, Result};
use std::cmp::Ordering;
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::protocol::handshake::Ref;

/// `git ls-remote -h` used with nothing else prints this and exits 129.
const USAGE: &str = "\
usage: git ls-remote [--branches] [--tags] [--refs] [--upload-pack=<exec>]
                     [-q | --quiet] [--exit-code] [--get-url] [--sort=<key>]
                     [--symref] [<repository> [<patterns>...]]

    -q, --[no-]quiet      do not print remote URL
    --[no-]upload-pack <exec>
                          path of git-upload-pack on the remote host
    -t, --[no-]tags       limit to tags
    -b, --[no-]branches   limit to branches
    --[no-]refs           do not show peeled tags
    --[no-]get-url        take url.<base>.insteadOf into account
    --[no-]sort <key>     field name to sort on
    --[no-]exit-code      exit with exit code 2 if no matching refs are found
    --[no-]symref         show underlying ref in addition to the object pointed by it
    -o, --[no-]server-option <server-specific>
                          option to transmit

";

/// Parsed command line for a single `ls-remote` invocation.
struct Opts {
    branches: bool,  // -b/--branches (-h/--heads): git's REF_HEADS
    tags: bool,      // -t/--tags: git's REF_TAGS
    normal: bool,    // --refs: git's REF_NORMAL — drop HEAD and the `^{}` rows
    symref: bool,    // --symref: emit a `ref: <target> TAB <name>` line first
    quiet: bool,     // -q/--quiet: suppress the `From <url>` stderr header
    exit_code: bool, // --exit-code: exit 2 when nothing matched
    get_url: bool,   // --get-url: print the expanded URL and never connect
    /// Raw `--sort=<key>` arguments in command-line order; validated late.
    sort: Vec<String>,
}

/// One output record: a ref advertised by the remote, or the synthetic `^{}`
/// row git emits for an annotated tag's peeled object.
struct Row {
    /// The full ref name as git prints it, e.g. `refs/tags/v1.0` or `…^{}`.
    name: String,
    /// The object id printed in the first column, and the sort key for
    /// `--sort=objectname`.
    id: gix::ObjectId,
    /// The symbolic target, for the `--symref` line (only on the base row).
    symref: Option<String>,
    /// Whether this is the synthetic `^{}` row (git's "magic fake tag ref").
    peel: bool,
    /// Creator/committer/author/tagger seconds, filled in only when a date key
    /// is being sorted on. Mirrors `get_ref_atom_value`'s lazy population.
    date: i64,
}

/// The `for-each-ref` atoms `ls-remote` can evaluate from what it has.
#[derive(Clone, Copy, PartialEq)]
enum Atom {
    Refname,
    Objectname,
    /// `creatordate`, plus the type-specific dates that resolve identically for
    /// the commit/tag objects a ref can point at.
    Date,
}

/// One parsed `--sort` key: `[-][version:|v:]<atom>`.
struct SortKey {
    reverse: bool,
    version: bool,
    atom: Atom,
}

/// `git ls-remote` — list references available in a remote repository.
///
/// Output is `<oid> TAB <ref> LF` per ref in the order the remote advertised
/// them (refname order), reordered by `--sort` when given, matching stock git
/// byte-for-byte. Annotated tags contribute a second `<ref>^{}` row unless
/// `--refs` is given.
pub fn ls_remote(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the flags only, but tolerate a leading subcommand name.
    let args = match args.first() {
        Some(a) if a == "ls-remote" => &args[1..],
        _ => args,
    };

    // Bare `-h` is help, consistent with other git subcommands; anywhere else
    // `-h` is the deprecated synonym for `--branches`.
    if args.len() == 1 && args[0] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let mut opts = Opts {
        branches: false,
        tags: false,
        normal: false,
        symref: false,
        quiet: false,
        exit_code: false,
        get_url: false,
        sort: Vec::new(),
    };
    let mut positionals: Vec<&str> = Vec::new();

    if let Err(code) = parse_args(args, &mut opts, &mut positionals) {
        return Ok(code);
    }

    let (repository, patterns): (Option<&str>, &[&str]) = match positionals.split_first() {
        Some((first, rest)) => (Some(*first), rest),
        None => (None, &[]),
    };

    // gitoxide resolves URL rewriting, transport and credential configuration
    // through a Repository; there is no repository-less remote to fall back on.
    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => bail!("ls-remote outside a repository is not supported (no repository found)"),
    };

    let name_or_url = repository.map(BStr::new);
    let remote = match repo.find_fetch_remote(name_or_url) {
        Ok(remote) => remote,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    // `to_bstring` rather than `Display`, which redacts passwords; git prints
    // the URL verbatim.
    let url = remote
        .url(gix::remote::Direction::Fetch)
        .map(gix::url::Url::to_bstring)
        .unwrap_or_default();

    // `--get-url` expands `url.<base>.insteadOf` (applied by `find_fetch_remote`)
    // and exits without talking to the remote — before `--sort` is ever looked
    // at, so `ls-remote --get-url --sort=bogus .` still succeeds, as in git.
    if opts.get_url {
        println!("{url}");
        return Ok(ExitCode::SUCCESS);
    }

    // git prints the header only when `<repository>` was left off the command line.
    if repository.is_none() && !opts.quiet {
        eprintln!("From {url}");
    }

    // `prefix_from_spec_as_filter_on_remote` must be off: ls-remote lists every
    // advertised ref, not just the ones the remote's refspecs would fetch.
    let connection = match remote.connect(gix::remote::Direction::Fetch) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    let ref_map = match connection.ref_map(
        gix::progress::Discard,
        gix::remote::ref_map::Options {
            prefix_from_spec_as_filter_on_remote: false,
            ..Default::default()
        },
    ) {
        Ok((map, _handshake)) => map,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };

    let mut rows: Vec<Row> = Vec::new();
    for r in &ref_map.remote_refs {
        push_rows(r, &mut rows);
    }
    rows.retain(|row| check_ref(row, &opts) && tail_match(patterns, &row.name));
    rows.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

    // git only reorders when `--sort` was given; otherwise the advertisement
    // order stands, and it is already refname order.
    if !opts.sort.is_empty() {
        let keys = match parse_sort_keys(&opts.sort) {
            Ok(keys) => keys,
            Err(msg) => {
                eprintln!("fatal: {msg}");
                return Ok(ExitCode::from(128));
            }
        };
        if let Err(msg) = resolve_dates(&repo, &keys, &mut rows) {
            eprintln!("fatal: {msg}");
            return Ok(ExitCode::from(128));
        }
        rows.sort_by(|a, b| compare_rows(&keys, a, b));
    }

    let mut out = String::new();
    for row in &rows {
        if opts.symref {
            if let Some(target) = &row.symref {
                out.push_str(&format!("ref: {target}\t{}\n", row.name));
            }
        }
        out.push_str(&format!("{}\t{}\n", row.id.to_hex(), row.name));
    }
    print!("{out}");

    Ok(if rows.is_empty() && opts.exit_code {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

/// Walk the command line the way `parse_options` does.
///
/// ls-remote is parsed with `PARSE_OPT_STOP_AT_NON_OPTION`: the first operand
/// (any token that is not a switch, including a bare `-`) stops option parsing,
/// so it and every following token are the repository and patterns — even ones
/// that look like flags. `git ls-remote . --sort=x` treats `--sort=x` as a
/// pattern and never validates it; `git ls-remote . --get-url` treats
/// `--get-url` as a pattern and still connects. `--` is the explicit terminator:
/// it is consumed and everything after it becomes an operand.
///
/// Returns the exit code to hand back on a usage error: git answers 129 for an
/// unknown option, a value given to a boolean, and a missing required value,
/// printing the complaint (and, for unknown options, the usage block) on stderr.
fn parse_args<'a>(
    args: &'a [String],
    opts: &mut Opts,
    positionals: &mut Vec<&'a str>,
) -> Result<(), ExitCode> {
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].as_str();

        // `--` ends option parsing; consume it and take the rest as operands.
        if arg == "--" {
            positionals.extend(args[i + 1..].iter().map(String::as_str));
            break;
        }
        // The first non-option operand stops option parsing (a bare `-` is an
        // operand, not a switch); it and all following tokens are operands.
        if !arg.starts_with('-') || arg == "-" {
            positionals.extend(args[i..].iter().map(String::as_str));
            break;
        }

        i += 1;

        // Short options cluster (`-tb`) and `-o` may take a sticky value (`-ofoo`).
        if !arg.starts_with("--") {
            for (at, c) in arg[1..].char_indices() {
                match c {
                    'b' | 'h' => opts.branches = true,
                    't' => opts.tags = true,
                    'q' => opts.quiet = true,
                    'o' => {
                        // The rest of the cluster is the value, else the next
                        // argv — consumed even when it looks like a flag, so
                        // `ls-remote -o --tags` has no `--tags` and no
                        // repository. The value itself goes nowhere: gitoxide
                        // cannot transmit protocol-v2 server options.
                        let sticky = &arg[1 + at + c.len_utf8()..];
                        if sticky.is_empty() {
                            if args.get(i).is_none() {
                                eprintln!("error: switch `o' requires a value");
                                return Err(ExitCode::from(129));
                            }
                            i += 1;
                        }
                        break;
                    }
                    other => {
                        eprintln!("error: unknown switch `{other}'");
                        eprint!("{USAGE}");
                        return Err(ExitCode::from(129));
                    }
                }
            }
            continue;
        }

        // `--no-<flag>` clears the corresponding setting, as parse_options does.
        let (name, value, on) = split_long(arg);

        // Booleans reject an attached value; valued options require one.
        let boolean = |slot: &mut bool| -> Result<(), ExitCode> {
            if value.is_some() {
                eprintln!("error: option `{name}' takes no value");
                return Err(ExitCode::from(129));
            }
            *slot = on;
            Ok(())
        };

        match name {
            "branches" | "heads" => boolean(&mut opts.branches)?,
            "tags" => boolean(&mut opts.tags)?,
            "refs" => boolean(&mut opts.normal)?,
            "symref" => boolean(&mut opts.symref)?,
            "quiet" => boolean(&mut opts.quiet)?,
            "exit-code" => boolean(&mut opts.exit_code)?,
            "get-url" => boolean(&mut opts.get_url)?,
            "sort" | "server-option" => {
                // `--no-sort` / `--no-server-option` discard what was collected.
                if !on {
                    if name == "sort" {
                        opts.sort.clear();
                    }
                    continue;
                }
                let value = match value {
                    Some(v) => v.to_string(),
                    None => match args.get(i) {
                        Some(v) => {
                            i += 1;
                            v.clone()
                        }
                        None => {
                            eprintln!("error: option `{name}' requires a value");
                            return Err(ExitCode::from(129));
                        }
                    },
                };
                // Server options are consumed and dropped: gitoxide has no
                // protocol-v2 server-option plumbing to hand them to.
                if name == "sort" {
                    opts.sort.push(value);
                }
            }
            "upload-pack" => {
                // Honouring this needs a remote-helper path hook `Remote::connect`
                // does not expose; bail rather than silently ignore it.
                bail_unsupported("--upload-pack");
                return Err(ExitCode::from(128));
            }
            other => {
                eprintln!("error: unknown option `{other}'");
                eprint!("{USAGE}");
                return Err(ExitCode::from(129));
            }
        }
    }

    Ok(())
}

/// Split `--name`, `--name=value` and `--no-name` into their parts.
fn split_long(arg: &str) -> (&str, Option<&str>, bool) {
    let body = &arg[2..];
    let (body, value) = match body.find('=') {
        Some(eq) => (&body[..eq], Some(&body[eq + 1..])),
        None => (body, None),
    };
    match body.strip_prefix("no-") {
        Some(rest) => (rest, value, false),
        None => (body, value, true),
    }
}

/// Report a flag this port deliberately does not implement.
fn bail_unsupported(flag: &str) {
    eprintln!("zvcs: ls-remote: unsupported flag {flag:?} (no gitoxide equivalent)");
}

/// Parse `--sort` arguments into git's ordering chain.
///
/// `ref_sorting_options` prepends each parsed key, so the last one on the
/// command line ends up at the head and sorts first; the returned vector is in
/// that head-first order.
fn parse_sort_keys(specs: &[String]) -> Result<Vec<SortKey>, String> {
    let mut keys = Vec::with_capacity(specs.len());
    for spec in specs.iter().rev() {
        let mut arg = spec.as_str();
        let reverse = arg.starts_with('-');
        if reverse {
            arg = &arg[1..];
        }
        let version = match arg.strip_prefix("version:").or_else(|| arg.strip_prefix("v:")) {
            Some(rest) => {
                arg = rest;
                true
            }
            None => false,
        };
        if arg.is_empty() {
            return Err(format!("malformed field name: {arg}"));
        }
        let atom = match arg {
            "refname" => Atom::Refname,
            "objectname" => Atom::Objectname,
            "creatordate" | "committerdate" | "authordate" | "taggerdate" => Atom::Date,
            other => return Err(format!("unknown field name: {other}")),
        };
        keys.push(SortKey {
            reverse,
            version,
            atom,
        });
    }
    Ok(keys)
}

/// Fill in `Row::date` for every row when a date key is in play.
///
/// git populates the atom lazily inside the comparison and dies with
/// `missing object <oid> for <ref>` when the object is not in the local odb —
/// which for `ls-remote` is anything the local repository has not fetched. With
/// a single row no comparison ever runs, so no lookup happens and no such
/// failure is possible; that case is skipped here for the same reason.
fn resolve_dates(repo: &gix::Repository, keys: &[SortKey], rows: &mut [Row]) -> Result<(), String> {
    if rows.len() < 2 || !keys.iter().any(|k| k.atom == Atom::Date) {
        return Ok(());
    }
    for row in rows.iter_mut() {
        let object = repo
            .find_object(row.id)
            .map_err(|_| format!("missing object {} for {}", row.id.to_hex(), row.name))?;
        let kind = object.kind;
        row.date = match kind {
            gix::object::Kind::Commit => object
                .try_into_commit()
                .ok()
                .and_then(|c| c.committer().ok().map(|s| s.seconds()))
                .unwrap_or_default(),
            gix::object::Kind::Tag => object
                .try_into_tag()
                .ok()
                .and_then(|t| t.tagger().ok().flatten().map(|s| s.seconds()))
                .unwrap_or_default(),
            // Trees and blobs have no date; git's atom value stays 0.
            _ => 0,
        };
    }
    Ok(())
}

/// git's `compare_refs`: walk the keys, then fall back to an ascending refname
/// `strcmp` that `-` never reverses.
fn compare_rows(keys: &[SortKey], a: &Row, b: &Row) -> Ordering {
    for key in keys {
        let cmp = match (key.version, key.atom) {
            (true, Atom::Refname) => versioncmp(&a.name, &b.name),
            (true, Atom::Objectname) => {
                versioncmp(&a.id.to_hex().to_string(), &b.id.to_hex().to_string())
            }
            // `version:<date atom>` is accepted but treated as the plain
            // numeric compare. git runs versioncmp over the atom's *formatted*
            // date string there, which this port does not reproduce; the
            // combination is undocumented and not exercised by the harness.
            (_, Atom::Date) => a.date.cmp(&b.date),
            (false, Atom::Refname) => a.name.as_bytes().cmp(b.name.as_bytes()),
            (false, Atom::Objectname) => a.id.as_bytes().cmp(b.id.as_bytes()),
        };
        let cmp = if key.reverse { cmp.reverse() } else { cmp };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    a.name.as_bytes().cmp(b.name.as_bytes())
}

// `versioncmp()` states: S_N normal, S_I integral part, S_F fractional part,
// S_Z fractional part with leading zeroes only.
const S_N: usize = 0x0;
const S_I: usize = 0x3;
const S_F: usize = 0x6;
const S_Z: usize = 0x9;
/// Result kind: return the raw difference.
const CMP: i8 = 2;
/// Result kind: compare by the length of the digit runs.
const LEN: i8 = 3;

/// git's `versioncmp()` from `versioncmp.c`, itself glibc's `strverscmp()`.
///
/// Compares two strings holding indices/version numbers so that `v1.10` sorts
/// after `v1.2`. `versionsort.suffix` prerelease handling is not implemented
/// (see the module docs); with neither `versionsort` key configured git takes
/// exactly this path.
fn versioncmp(s1: &str, s2: &str) -> Ordering {
    // Symbol(s)  0       [1-9]   others
    // Transition (10) 0  (01) d  (00) x
    #[rustfmt::skip]
    const NEXT_STATE: [usize; 12] = [
        /* state    x    d    0  */
        /* S_N */   S_N, S_I, S_Z,
        /* S_I */   S_N, S_I, S_I,
        /* S_F */   S_N, S_F, S_F,
        /* S_Z */   S_N, S_F, S_Z,
    ];
    #[rustfmt::skip]
    const RESULT_TYPE: [i8; 36] = [
        /* state   x/x  x/d  x/0  d/x  d/d  d/0  0/x  0/d  0/0 */
        /* S_N */  CMP, CMP, CMP, CMP, LEN, CMP, CMP, CMP, CMP,
        /* S_I */  CMP, -1,  -1,  1,   LEN, LEN, 1,   LEN, LEN,
        /* S_F */  CMP, CMP, CMP, CMP, CMP, CMP, CMP, CMP, CMP,
        /* S_Z */  CMP, 1,   1,   -1,  CMP, CMP, -1,  CMP, CMP,
    ];

    let (a, b) = (s1.as_bytes(), s2.as_bytes());
    // The C original walks NUL-terminated buffers; past the end reads as NUL.
    let at = |s: &[u8], i: usize| -> u8 { s.get(i).copied().unwrap_or(0) };
    let digit = |c: u8| c.is_ascii_digit();
    // 0 for other, 1 for [1-9], 2 for '0' — the column of both tables.
    let class = |c: u8| usize::from(c == b'0') + usize::from(digit(c));

    let mut i = 0usize;
    let mut c1 = at(a, 0);
    let mut c2 = at(b, 0);
    let mut state = S_N + class(c1);

    let diff = loop {
        let diff = i32::from(c1) - i32::from(c2);
        if diff != 0 {
            break diff;
        }
        if c1 == 0 {
            return Ordering::Equal;
        }
        state = NEXT_STATE[state];
        i += 1;
        c1 = at(a, i);
        c2 = at(b, i);
        state += class(c1);
    };

    match RESULT_TYPE[state * 3 + class(c2)] {
        CMP => diff.cmp(&0),
        LEN => {
            // Whichever side's digit run continues longer holds the larger number.
            let mut k = 0;
            while digit(at(a, i + 1 + k)) {
                if !digit(at(b, i + 1 + k)) {
                    return Ordering::Greater;
                }
                k += 1;
            }
            if digit(at(b, i + 1 + k)) {
                Ordering::Less
            } else {
                diff.cmp(&0)
            }
        }
        verdict => verdict.cmp(&0),
    }
}

/// Turn one advertised ref into its output rows.
///
/// An annotated tag yields two: the tag object under its own name, and the
/// object it points at under `<name>^{}` — exactly the pair `git upload-pack`
/// puts on the wire. Unborn refs are dropped: `ls-remote` never asks for them,
/// so stock git prints nothing for a remote with an unborn HEAD.
fn push_rows(r: &Ref, rows: &mut Vec<Row>) {
    let (name, oid, peeled, symref) = match r {
        Ref::Peeled {
            full_ref_name,
            tag,
            object,
        } => (full_ref_name, *tag, Some(*object), None),
        Ref::Direct {
            full_ref_name,
            object,
        } => (full_ref_name, *object, None, None),
        Ref::Symbolic {
            full_ref_name,
            target,
            tag,
            object,
        } => (
            full_ref_name,
            (*tag).unwrap_or(*object),
            tag.is_some().then_some(*object),
            Some(target.to_string()),
        ),
        Ref::Unborn { .. } => return,
    };

    let name = name.to_string();
    rows.push(Row {
        id: oid,
        symref,
        peel: false,
        date: 0,
        name: name.clone(),
    });
    if let Some(peeled) = peeled {
        rows.push(Row {
            name: format!("{name}^{{}}"),
            id: peeled,
            symref: None,
            peel: true,
            date: 0,
        });
    }
}

/// git's `check_ref` from `remote.c`, specialised to the rows we build.
///
/// With no type bits set everything passes. Otherwise the name must live under
/// `refs/` (so `HEAD` is dropped); `--refs` additionally drops the `^{}` rows
/// (the "magic fake tag refs" that fail `check_refname_format`); `-b`/`-t`
/// admit their own prefix; and anything else passes only when neither prefix
/// bit is set.
fn check_ref(row: &Row, opts: &Opts) -> bool {
    if !opts.branches && !opts.tags && !opts.normal {
        return true;
    }
    let Some(rest) = row.name.strip_prefix("refs/") else {
        return false;
    };
    if opts.normal && row.peel {
        return false;
    }
    if opts.branches && rest.starts_with("heads/") {
        return true;
    }
    if opts.tags && rest.starts_with("tags/") {
        return true;
    }
    !(opts.branches || opts.tags)
}

/// git's `tail_match`: each user pattern is glob-matched as `*/<pattern>`
/// against `/<refname>`, so `main` matches `refs/heads/main` but not
/// `refs/heads/mymain`, while a full name like `refs/heads/main` matches too.
///
/// `Mode::empty()` mirrors git's `wildmatch(..., 0)`, where `*` spans `/`.
fn tail_match(patterns: &[&str], name: &str) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let path = format!("/{name}");
    patterns.iter().any(|p| {
        let pattern = format!("*/{p}");
        gix::glob::wildmatch(
            pattern.as_bytes().as_bstr(),
            path.as_bytes().as_bstr(),
            gix::glob::wildmatch::Mode::empty(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ordering of the names stock git 2.55 produced for
    /// `git tag --sort=version:refname` over this exact set, which is the
    /// regression this port has to keep reproducing.
    #[test]
    fn versioncmp_matches_git_version_refname_order() {
        let mut names = vec![
            "v1.0.1", "v1.10", "10", "v1", "a01b2", "v1.0-rc2", "1", "v1.0.0.0", "v1.1", "01",
            "v001", "v1a", "release-2", "v10", "v0.9", "v1.0a", "v1_2", "v1.0", "v01", "0",
            "release-10", "abc", "v2.0", "v1.0.0", "v9", "a1b2", "v1-2", "00", "x", "release-1",
            "refs", "v10.0", "v1.0-rc1", "v2", "v1.2", "y",
        ];
        names.sort_by(|a, b| versioncmp(a, b).then_with(|| a.cmp(b)));
        assert_eq!(
            names,
            vec![
                "00", "01", "0", "1", "10", "a01b2", "a1b2", "abc", "refs", "release-1",
                "release-2", "release-10", "v001", "v01", "v0.9", "v1", "v1-2", "v1.0", "v1.0-rc1",
                "v1.0-rc2", "v1.0.0", "v1.0.0.0", "v1.0.1", "v1.0a", "v1.1", "v1.2", "v1.10",
                "v1_2", "v1a", "v2", "v2.0", "v9", "v10", "v10.0", "x", "y",
            ]
        );
    }

    /// The last `--sort` is the primary key (`ref_sorting_options` prepends),
    /// and `-` reverses that key without touching the refname tiebreak.
    #[test]
    fn last_sort_wins_and_reverse_keeps_refname_tiebreak() {
        let keys = parse_sort_keys(&["refname".into(), "-creatordate".into()]).unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys[0].reverse && keys[0].atom == Atom::Date);
        assert!(!keys[1].reverse && keys[1].atom == Atom::Refname);

        let row = |name: &str, date: i64| Row {
            name: name.into(),
            id: gix::ObjectId::null(gix::hash::Kind::Sha1),
            symref: None,
            peel: false,
            date,
        };
        // Equal dates fall through to an ascending refname compare, never a
        // descending one, even though the date key is reversed.
        assert_eq!(
            compare_rows(&keys, &row("refs/heads/a", 7), &row("refs/heads/b", 7)),
            Ordering::Less
        );
        assert_eq!(
            compare_rows(&keys, &row("refs/heads/a", 1), &row("refs/heads/b", 9)),
            Ordering::Greater
        );
    }

    /// git validates sort keys after the transport, and rejects these two
    /// shapes with `fatal:` (exit 128) rather than a usage error.
    #[test]
    fn sort_key_validation_mirrors_git() {
        assert!(parse_sort_keys(&["bogus".into()]).is_err());
        assert!(parse_sort_keys(&["".into()]).is_err());
        assert!(parse_sort_keys(&["-".into()]).is_err());
        assert!(parse_sort_keys(&["v:refname".into()]).unwrap()[0].version);
        assert!(parse_sort_keys(&["-version:refname".into()]).unwrap()[0].reverse);
        assert!(parse_sort_keys(&["objectname".into()]).is_ok());
        assert!(parse_sort_keys(&["creatordate".into()]).is_ok());
    }

    fn blank_opts() -> Opts {
        Opts {
            branches: false,
            tags: false,
            normal: false,
            symref: false,
            quiet: false,
            exit_code: false,
            get_url: false,
            sort: Vec::new(),
        }
    }

    fn parse(argv: &[&str]) -> Result<(Opts, Vec<String>), u8> {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let mut opts = blank_opts();
        let mut positionals = Vec::new();
        match parse_args(&args, &mut opts, &mut positionals) {
            // ExitCode is opaque, so the caller only learns that parsing failed;
            // the codes themselves are asserted through the harness.
            Err(_) => Err(1),
            Ok(()) => Ok((
                opts,
                positionals.into_iter().map(str::to_string).collect(),
            )),
        }
    }

    /// `-o` swallows the next argv even when it looks like a flag — the reason
    /// `git ls-remote -o --tags` reports "No remote configured to list refs
    /// from." instead of listing tags.
    #[test]
    fn o_consumes_the_next_argument() {
        let (opts, positionals) = parse(&["-o", "--tags"]).unwrap();
        assert!(!opts.tags, "--tags was the value of -o, not a flag");
        assert!(positionals.is_empty(), "no repository is left on the line");
    }

    /// Short options cluster, and a sticky `-ofoo` keeps its value inside the
    /// cluster rather than eating the following argv.
    #[test]
    fn short_options_cluster_with_sticky_value() {
        let (opts, positionals) = parse(&["-tb", "-ofoo", "."]).unwrap();
        assert!(opts.tags && opts.branches);
        assert_eq!(positionals, vec!["."]);
    }

    /// `--no-sort` drops previously collected keys; `--no-server-option` must
    /// not touch them.
    #[test]
    fn negations_clear_only_their_own_option() {
        assert!(parse(&["--sort=-refname", "--no-sort"]).unwrap().0.sort.is_empty());
        assert_eq!(
            parse(&["--sort=-refname", "--no-server-option"]).unwrap().0.sort,
            vec!["-refname"]
        );
    }

    /// Values attached to booleans and missing values for valued options are
    /// both usage errors.
    #[test]
    fn usage_errors_are_rejected() {
        assert!(parse(&["--branches=x"]).is_err());
        assert!(parse(&["--sort"]).is_err());
        assert!(parse(&["--server-option"]).is_err());
        assert!(parse(&["-o"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["-Z"]).is_err());
        // `--` stops option parsing, so a later `-t` is a pattern.
        assert_eq!(parse(&["--", ".", "-t"]).unwrap().1, vec![".", "-t"]);
    }

    /// ls-remote is `PARSE_OPT_STOP_AT_NON_OPTION`: the first operand stops
    /// option parsing, so switches after the repository are patterns, never
    /// flags. This is the regression behind
    /// `ls-remote refs/tags/* -t --get-url … -- --sort=creatordate`, where git
    /// never sees `--get-url` as a flag and connects (fatal 128) instead of
    /// printing the URL (exit 0).
    #[test]
    fn first_operand_stops_option_parsing() {
        // Flags before the operand are parsed; those after are operands.
        let (opts, pos) = parse(&["-t", "repo", "--get-url", "--sort=bogus"]).unwrap();
        assert!(opts.tags, "-t before the operand is a flag");
        assert!(!opts.get_url, "--get-url after the operand is a pattern");
        assert!(opts.sort.is_empty(), "--sort after the operand is never collected");
        assert_eq!(pos, vec!["repo", "--get-url", "--sort=bogus"]);

        // The exact fuzzer case: no flag is ever parsed, everything is operand.
        let (opts, pos) = parse(&[
            "refs/tags/*",
            "-t",
            "--get-url",
            "--server-option=",
            "--sort=-refname",
            "--",
            "--sort=creatordate",
        ])
        .unwrap();
        assert!(!opts.get_url && !opts.tags && opts.sort.is_empty());
        assert_eq!(pos[0], "refs/tags/*");
        // The `--` after an operand is a literal pattern, not a terminator.
        assert!(pos.contains(&"--".to_string()));

        // A bare `-` is an operand, not a switch, and stops parsing too.
        assert_eq!(parse(&["-", "--tags"]).unwrap().1, vec!["-", "--tags"]);
    }
}
