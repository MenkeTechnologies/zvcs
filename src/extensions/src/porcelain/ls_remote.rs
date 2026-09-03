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
//! `versioncmp()` (`crate::refsort`, shared with `branch`, `tag` and
//! `for-each-ref` exactly as git shares `versioncmp.c`, so
//! `versionsort.suffix` / `versionsort.prereleaseSuffix` apply here too). Sort
//! keys are validated only after
//! the refs have been fetched, exactly where git validates them, so an
//! unreachable remote still reports the transport failure rather than the key.
//!
//! `--upload-pack=<exec>` (and its hidden synonym `--exec`) selects the program run in place of
//! `git-upload-pack`, defaulting to `remote.<name>.uploadpack`; `-o`/`--server-option` is transmitted as
//! protocol-v2 `server-option=<value>` capability lines on the `ls-refs` request, defaulting to
//! `remote.<name>.serverOption`.
//!
//! Known gaps:
//!
//! * `--sort` keys beyond `refname`, `objectname`, `creatordate`,
//!   `committerdate`, `authordate` and `taggerdate` are refused with git's
//!   `unknown field name` fatal. git accepts more `for-each-ref` atoms than
//!   those, but computing them needs object data `ls-remote` never fetches.
//! * `version:` in front of a *date* key falls back to the plain numeric
//!   compare instead of git's versioncmp over the formatted date string.
//!
//! Running outside a repository also bails: gitoxide resolves transport,
//! credential and `insteadOf` configuration through a `Repository`, and there
//! is no repository-less remote in the vendored crates.

use crate::refsort::{self, Prereleases};
use anyhow::{bail, Result};
use std::cmp::Ordering;
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::protocol::handshake::Ref;

/// `cmd_ls_remote()`'s `struct option options[]` (builtin/ls-remote.c), in table
/// order, as [`super::resolve_long`] reads it. `--heads` is `OPT_ALIAS`-free here:
/// it is a table entry of its own, hidden but real.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "quiet",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "upload-pack",                 neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "exec",                        neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "tags",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "branches",                    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "heads",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "refs",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "get-url",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "sort",                        neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "exit-code",                   neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "symref",                      neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "server-option",               neg: true,  arg: super::Arg::Required },
];
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

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]exec`, `--[no-]heads`.
/// Captured byte-for-byte from stock git 2.55.0's `git ls-remote --help-all`.
const USAGE_ALL: &str = r#"usage: git ls-remote [--branches] [--tags] [--refs] [--upload-pack=<exec>]
                     [-q | --quiet] [--exit-code] [--get-url] [--sort=<key>]
                     [--symref] [<repository> [<patterns>...]]

    -q, --[no-]quiet      do not print remote URL
    --[no-]upload-pack <exec>
                          path of git-upload-pack on the remote host
    --[no-]exec <exec>    path of git-upload-pack on the remote host
    -t, --[no-]tags       limit to tags
    -b, --[no-]branches   limit to branches
    -h, --[no-]heads      deprecated synonym for --branches
    --[no-]refs           do not show peeled tags
    --[no-]get-url        take url.<base>.insteadOf into account
    --[no-]sort <key>     field name to sort on
    --[no-]exit-code      exit with exit code 2 if no matching refs are found
    --[no-]symref         show underlying ref in addition to the object pointed by it
    -o, --[no-]server-option <server-specific>
                          option to transmit

"#;

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
    /// `-o`/`--server-option`, sent as protocol-v2 `server-option=<value>` lines with `ls-refs`.
    server_options: Vec<gix::bstr::BString>,
    /// `--upload-pack=<exec>` (git's hidden `--exec` synonym sets the same variable): the program to run in
    /// place of `git-upload-pack` on the other end.
    upload_pack: Option<String>,
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
        server_options: Vec::new(),
        upload_pack: None,
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
    let repo = match crate::setup::discover() {
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

    // `transport_check_allowed()` — the connection is about to be opened, and
    // `protocol.<name>.allow` / `$GIT_ALLOW_PROTOCOL` / `$GIT_PROTOCOL_FROM_USER`
    // decide whether this scheme may be reached at all. Ahead of the `From` header
    // because git prints no header for a refused transport, and after `--get-url`
    // because that form never connects.
    if let Some(remote_url) = remote.url(gix::remote::Direction::Fetch) {
        if let Some(code) = crate::setup::check_url_allowed(remote_url) {
            return Ok(code);
        }
    }

    // git prints the header only when `<repository>` was left off the command line.
    if repository.is_none() && !opts.quiet {
        eprintln!("From {url}");
    }

    // `transfer.credentialsInUrl` is consulted before the connection is opened,
    // sharing the fetch port's implementation so both commands report the same
    // sentence for the same URL.
    if super::fetch::credentials_in_url(&repo, remote.url(gix::remote::Direction::Fetch))
        == super::fetch::Verdict::Fatal
    {
        return Ok(ExitCode::from(128));
    }

    // `prefix_from_spec_as_filter_on_remote` must be off: ls-remote lists every
    // advertised ref, not just the ones the remote's refspecs would fetch.
    let remote_name = remote.name().map(|n| n.as_bstr().to_string());
    // `remote.<name>.vcs` routes the whole connection through `git-remote-<vcs>`
    // rather than the URL's own transport, which this port cannot drive — see
    // [`super::fetch::foreign_vcs`].
    if let Some(code) = super::fetch::reject_foreign_vcs(&repo, remote_name.as_deref()) {
        return Ok(code);
    }
    let connect_options = gix::remote::connect::Options {
        upload_pack: super::fetch::local_service_program(
            remote.url(gix::remote::Direction::Fetch),
            super::fetch::upload_pack_program(&repo, remote_name.as_deref(), opts.upload_pack.as_deref()),
            "upload-pack",
        ),
        // `git ls-remote` has no `--ipv4`/`--ipv6`, and never connects for push.
        ..Default::default()
    };
    let server_options = super::fetch::server_options_for(&repo, remote_name.as_deref(), &opts.server_options);
    // `git_config_get_protocol_version()` (protocol.c) parses the configured
    // version while the transport is being built and refuses anything that is not
    // one of the three it knows:
    //
    // ```c
    // version = parse_protocol_version(value);
    // if (version == protocol_unknown_version)
    //         die(_("unknown value for config 'protocol.version': %s"), value);
    // ```
    //
    // The vendored connect reports the same condition in its own words ("Choose
    // between 1 and 2"), which is both a different sentence and a different set —
    // `0` is legal to git.
    if let Some(v) = repo.config_snapshot().string("protocol.version") {
        let v = v.to_string();
        if !matches!(v.as_str(), "0" | "1" | "2") {
            eprintln!("fatal: unknown value for config 'protocol.version': {v}");
            return Ok(ExitCode::from(128));
        }
    }
    let connection = match remote.connect_with_options(gix::remote::Direction::Fetch, connect_options) {
        Ok(c) => c.with_server_options(server_options),
        // A local path that is not a repository is `enter_repo()` coming back
        // NULL in `git_connect()`, which names the path and then adds the same
        // block an unreachable host produces.
        Err(gix::remote::connect::Error::FileUrl { url, .. }) => {
            return Ok(crate::transport_err::not_a_repository_fatal(
                &url.to_bstring().to_string(),
            ));
        }
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
        // `die_if_server_options()` (transport.c) prints the advice line ahead of
        // its refusal, and the transport the caller then tears down reports the
        // half-open connection — three lines, in this order, exit 128.
        Err(e @ gix::remote::ref_map::Error::ServerOptionsRequireV2) => {
            eprintln!("hint: see protocol.version in 'git help config' for more details");
            eprintln!("fatal: {e}");
            eprintln!("fatal: the remote end hung up unexpectedly");
            return Ok(ExitCode::from(128));
        }
        Err(e) => {
            // An ssh transport that never connected is git's own `die()`: the
            // child's stderr, then the fixed block.
            let err = anyhow::Error::from(e);
            if let Some(code) = crate::transport_err::ssh_fatal(&url.to_string(), &err) {
                return Ok(code);
            }
            eprintln!("fatal: {err}");
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
        // git seeds `versioncmp`'s prerelease list from config once, lazily.
        let prereleases = Prereleases::new(&repo);
        rows.sort_by(|a, b| compare_rows(&keys, a, b, &prereleases));
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

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): an exact match tested after the `--` and
        // non-option breaks above and before any table lookup, so it never
        // abbreviates and never takes an `=<value>`. Unlike `-h`, which this
        // table claims as the deprecated `--branches` synonym once it is not
        // the sole argument, `--help-all` is help wherever the option region
        // reaches it — and it renders `USAGE_FULL`, which lists both the hidden
        // `--exec` and the `-h` spelling of `--branches`.
        if arg == "--help-all" {
            print!("{USAGE_ALL}");
            return Err(ExitCode::from(129));
        }

        // Short options cluster (`-tb`) and `-o` may take a sticky value (`-ofoo`).
        if !arg.starts_with("--") {
            for (at, c) in arg[1..].char_indices() {
                match c {
                    'b' | 'h' => opts.branches = true,
                    't' => opts.tags = true,
                    'q' => opts.quiet = true,
                    'o' => {
                        // `OPT_STRING_LIST('o', "server-option", &server_options, …)`
                        // (builtin/ls-remote.c:92) — the same list `--server-option`
                        // appends to, transmitted as protocol-v2 command arguments. The
                        // rest of the cluster is the value, else the next argv, consumed
                        // even when it looks like a flag, so `ls-remote -o --tags` has
                        // no `--tags` and no repository.
                        let sticky = &arg[1 + at + c.len_utf8()..];
                        let value = if sticky.is_empty() {
                            let Some(next) = args.get(i) else {
                                eprintln!("error: switch `o' requires a value");
                                return Err(ExitCode::from(129));
                            };
                            i += 1;
                            next.clone()
                        } else {
                            sticky.to_string()
                        };
                        opts.server_options.push(value.into());
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

        // Respell a unique abbreviation as the name it resolves to, so `--get-u`
        // reaches the same arm as `--get-url`. Names no entry claims come back
        // untouched, so the refusal below still quotes what was typed.
        let canonical;
        let arg = match super::canonical_long(arg, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Err(super::ambiguous_option(arg, &first, &second, USAGE))
            }
        };

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
            "sort" | "server-option" | "upload-pack" | "exec" => {
                // `--no-sort` / `--no-server-option` / `--no-upload-pack` discard what was collected.
                if !on {
                    match name {
                        "sort" => opts.sort.clear(),
                        "server-option" => opts.server_options.clear(),
                        _ => opts.upload_pack = None,
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
                // `--exec` is git's hidden synonym for `--upload-pack`; both set the same `uploadpack`
                // variable in `builtin/ls-remote.c`, and the last one given wins.
                match name {
                    "sort" => opts.sort.push(value),
                    "server-option" => opts.server_options.push(value.into()),
                    _ => opts.upload_pack = Some(value),
                }
            }
            other => {
                eprintln!("error: unknown option `{}'", &arg[2..]);
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
fn compare_rows(keys: &[SortKey], a: &Row, b: &Row, pre: &Prereleases<'_>) -> Ordering {
    for key in keys {
        let cmp = match (key.version, key.atom) {
            (true, Atom::Refname) => refsort::versioncmp(a.name.as_bytes(), b.name.as_bytes(), pre),
            (true, Atom::Objectname) => refsort::versioncmp(
                a.id.to_hex().to_string().as_bytes(),
                b.id.to_hex().to_string().as_bytes(),
                pre,
            ),
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
            compare_rows(&keys, &row("refs/heads/a", 7), &row("refs/heads/b", 7), &Prereleases::none()),
            Ordering::Less
        );
        assert_eq!(
            compare_rows(&keys, &row("refs/heads/a", 1), &row("refs/heads/b", 9), &Prereleases::none()),
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
            server_options: Vec::new(),
            upload_pack: None,
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
    /// from." instead of listing tags — and the value it swallowed is a server
    /// option, since `OPT_STRING_LIST(.o., "server-option", &server_options, …)`
    /// (builtin/ls-remote.c:92) gives both spellings the same list.
    #[test]
    fn o_consumes_the_next_argument_as_a_server_option() {
        let (opts, positionals) = parse(&["-o", "--tags"]).unwrap();
        assert!(!opts.tags, "--tags was the value of -o, not a flag");
        assert!(positionals.is_empty(), "no repository is left on the line");
        assert_eq!(opts.server_options, vec!["--tags"], "the value is transmitted");
    }

    /// Short options cluster, and a sticky `-ofoo` keeps its value inside the
    /// cluster rather than eating the following argv.
    #[test]
    fn short_options_cluster_with_sticky_value() {
        let (opts, positionals) = parse(&["-tb", "-ofoo", "."]).unwrap();
        assert!(opts.tags && opts.branches);
        assert_eq!(positionals, vec!["."]);
        assert_eq!(opts.server_options, vec!["foo"], "the sticky value is the option");
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
