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
//! `--sort` goes through the one ref-filter engine `for-each-ref`, `branch` and
//! `tag` use, as `cmd_ls_remote()` does (builtin/ls-remote.c:150-164): every
//! advertised ref becomes a `ref_array_item`, each `--sort` key is any
//! `for-each-ref` atom, the *last* `--sort` is the primary key, and a final
//! ascending `strcmp` on the refname breaks ties (`compare_refs`). An atom
//! that reads the object names the first ref whose object is not local in
//! `missing object <oid> for <ref>`, in `git_qsort_s()`'s comparison order.
//! Sort keys are validated only after the refs have been fetched, exactly
//! where git validates them, so an unreachable remote still reports the
//! transport failure rather than the key.
//!
//! `--upload-pack=<exec>` (and its hidden synonym `--exec`) selects the program run in place of
//! `git-upload-pack`, defaulting to `remote.<name>.uploadpack`; `-o`/`--server-option` is transmitted as
//! protocol-v2 `server-option=<value>` capability lines on the `ls-refs` request, defaulting to
//! `remote.<name>.serverOption`.
//!
//! Outside a repository the operand is the URL, rewritten by the system, global
//! and command-line `url.<base>.insteadOf`, and the transport is opened on it
//! directly (see [`outside_repository`]); a `--sort` key whose atom needs object
//! data dies there, as in git.

use super::for_each_ref::{self, parse_atom, Atom, AtomCtx, ErrKind, Field, Key, NameMod, ObjInfo, QuoteStyle, RefInfo, RenderCtx, SortKey};
use crate::refsort::{self, Prereleases};
use anyhow::Result;
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
/// `git ls-remote -h` used with nothing else prints this and exits 0.
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
        return Ok(super::show_usage(USAGE));
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

    // `ls-remote` is `RUN_SETUP_GENTLY`: outside a repository it still lists.
    let repo = match crate::setup::discover() {
        Ok(repo) => repo,
        Err(_) => return Ok(outside_repository(repository, patterns, &opts)),
    };
    // `remote_get(dest)` (builtin/ls-remote.c) is the first lookup, which runs
    // remote.c's `read_config()`.
    crate::cmd_config::read_remote_config(&repo).map_err(|r| r.into_error())?;

    let name_or_url = repository.map(BStr::new);
    let remote = match repo.find_fetch_remote(name_or_url) {
        Ok(remote) => remote,
        // `remote_get(NULL)` comes back NULL when the branch names no remote and `origin` is not
        // configured, which `cmd_ls_remote()` answers with its own sentence.
        Err(_) if repository.is_none() => {
            eprintln!("fatal: No remote configured to list refs from.");
            return Ok(ExitCode::from(128));
        }
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
        // git has moved to the top of the work tree by now; a relative URL is read from there.
        current_dir: crate::setup::setup_cwd(&repo),
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
    // `repo_config_get_string_tmp()` dies through `git_die_config()` on a
    // valueless key.
    if let Some(v) = crate::config::config_get_string(Some(&repo), "protocol.version") {
        if !matches!(v.as_str(), "0" | "1" | "2") {
            eprintln!("fatal: unknown value for config 'protocol.version': {v}");
            return Ok(ExitCode::from(128));
        }
    }
    let mut connection = match remote.connect_with_options(gix::remote::Direction::Fetch, connect_options) {
        Ok(c) => c
            .with_server_options(server_options)
            .with_credentials(super::credential::transport_authenticate(
                repo.config_snapshot().plumbing().clone(),
            )),
        // A local path that is not a repository is `enter_repo()` coming back
        // NULL in `git_connect()`, which names the path and then adds the same
        // block an unreachable host produces.
        Err(e @ gix::remote::connect::Error::FileUrl { .. }) => {
            return Ok(crate::transport_err::file_url_fatal(&e).expect("a FileUrl error"));
        }
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };
    // The refill of a refused `POST` happens inside the transport; see
    // [`super::credential::with_post_authentication`].
    {
        use gix::protocol::transport::client::TransportWithoutIO;
        let transport_url = connection.transport_mut().to_url().into_owned();
        let remote_name = connection.remote().name().map(gix::remote::Name::as_bstr).map(ToOwned::to_owned);
        if let Ok(Some(options)) = repo.transport_options(gix::bstr::ByteSlice::as_bstr(transport_url.as_slice()), remote_name.as_ref().map(|n| gix::bstr::ByteSlice::as_bstr(n.as_slice()))) {
            connection.set_transport_options(super::credential::with_post_authentication(
                options,
                repo.config_snapshot().plumbing().clone(),
                gix::bstr::ByteSlice::as_bstr(transport_url.as_slice()),
            ));
        }
    }
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
        // A 401 to the retry that carried credentials: `discover_refs()` dies.
        Err(gix::remote::ref_map::Error::Handshake(e))
            if crate::transport_err::http_auth_fatal(&url.to_string(), Some(&repo), &e).is_some() =>
        {
            return Ok(ExitCode::from(128));
        }
        // Any other refusal of the ref advertisement: `discover_refs()` dies.
        Err(gix::remote::ref_map::Error::Handshake(e))
            if crate::transport_err::http_get_fatal(&url.to_string(), Some(&repo), &e).is_some() =>
        {
            return Ok(ExitCode::from(128));
        }
        Err(e) => {
            // An ssh transport that never connected is git's own `die()`: the
            // child's stderr, then the fixed block. An `ls-refs` answer that
            // stopped short of its flush is `get_remote_refs()`'s `die()`.
            let err = anyhow::Error::from(e);
            // `get_remote_refs()` writes its request with `packet_write_fmt()`, and a closed pipe
            // there is `check_pipe()`'s death by `SIGPIPE` (pkt-line.c:171-174, run-command.c:1150-1161).
            if crate::sigpipe::is_broken_pipe(err.as_ref()) {
                crate::sigpipe::exit_broken_pipe();
            }
            if let Some(code) = crate::transport_err::ssh_fatal(&url.to_string(), &err)
                .or_else(|| crate::transport_err::hang_up_fatal(&err))
            {
                return Ok(code);
            }
            eprintln!("fatal: {err}");
            return Ok(ExitCode::from(128));
        }
    };

    // git prints the header only when `<repository>` was left off the command
    // line, and only once `transport_get_remote_refs()` has answered
    // (builtin/ls-remote.c:149-156): an unreachable remote gets none.
    if repository.is_none() && !opts.quiet {
        eprintln!("From {url}");
    }
    let code = list(&ref_map.remote_refs, patterns, &opts, Some(&repo))?;
    // `transport_disconnect()` comes after the listing (builtin/ls-remote.c:166-177), and its
    // `disconnect_git()` writes the closing flush (transport.c:1020-1030).
    if gix::protocol::take_closing_flush_broken_pipe() {
        disconnect_into_closed_pipe();
    }
    Ok(code)
}

/// `disconnect_git()`'s `packet_flush()` met a closed pipe (pkt-line.c:93-98): ls-remote runs with
/// `SIGPIPE` at its default (common-init.c:27-35), so the `write()` kills it, and what `printf()` put
/// in a fully buffered stdout is never written out.
fn disconnect_into_closed_pipe() -> ! {
    crate::sigpipe::exit_broken_pipe()
}

/// The listing itself, from the advertised refs on: filter, sort, print.
///
/// `repo` is `None` outside a repository, where a date key has no object data
/// to read and `ref_sorting_options()` dies on it — after the refs are in.
fn list(refs: &[Ref], patterns: &[&str], opts: &Opts, repo: Option<&gix::Repository>) -> Result<ExitCode> {
    let mut rows: Vec<Row> = Vec::new();
    for r in refs {
        push_rows(r, &mut rows);
    }
    // The items are pushed in advertisement order (builtin/ls-remote.c:152-161), each peeled
    // `<tag>^{}` right behind its tag, so `refs/tags/v1.0^{}` precedes `refs/tags/v1.0.1`.
    rows.retain(|row| check_ref(row, opts) && tail_match(patterns, &row.name));

    // `ref_sorting_options()` leaves `sorting` NULL when no `--sort` was given, and
    // `ref_array_sort()` sorts only `if (sorting)` (ref-filter.c:3624-3628): the advertisement
    // order stands. With `--sort` it is the order `git_qsort_s()` starts from, which decides
    // the ref a `missing object` names.
    if !opts.sort.is_empty() {
        let sorts = match parse_sort_keys(&opts.sort, repo) {
            Ok(sorts) => sorts,
            Err(SortFailure::OutsideRepositoryBug) => outside_repository_bug(),
            Err(SortFailure::Atom(e)) => match e.kind {
                ErrKind::Unported => anyhow::bail!("{}", e.msg),
                ErrKind::Fatal | ErrKind::Usage => {
                    eprintln!("fatal: {}", e.msg);
                    return Ok(ExitCode::from(128));
                }
            },
        };
        sort_rows(repo, &sorts, &mut rows)?;
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
    // `printf()` into stdio's stdout, fully buffered off a terminal: the listing is written out
    // at `exit()`, after `transport_disconnect()` (builtin/ls-remote.c:166-177).
    crate::cstdio::defer();
    crate::cstdio::write_bytes(out.as_bytes());

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
            return Err(super::show_usage(USAGE_ALL));
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

/// `ref_sorting_options()` (ref-filter.c:3775-3791) over the `--sort` arguments:
/// `parse_ref_sorting()` (:3757-3773) strips `-` and `version:`/`v:` and hands
/// the rest to `parse_sorting_atom()` (:3741-3755), which runs
/// `parse_ref_filter_atom()` against a dummy format and `die()`s on the first
/// argument it rejects, in command-line order. Each key is prepended, so the
/// returned chain is head-first: the last `--sort` sorts first.
///
/// Without a repository `parse_ref_filter_atom()` refuses every atom whose
/// `valid_atom[]` source is not `SOURCE_NONE` (ref-filter.c:1074-1077) before
/// its parser runs; `head_atom_parser()` then resolves `HEAD` through
/// `get_main_ref_store()`, which is a `BUG()` there (refs.c:2367).
fn parse_sort_keys(specs: &[String], repo: Option<&gix::Repository>) -> Result<Vec<SortKey>, SortFailure> {
    let ctx = AtomCtx {
        repo,
        color_on: false,
        quote_style: QuoteStyle::None,
    };
    let mut keys = Vec::with_capacity(specs.len());
    for spec in specs {
        let (rest, descending) = match spec.strip_prefix('-') {
            Some(rest) => (rest, true),
            None => (spec.as_str(), false),
        };
        let (rest, versioned) = match rest.strip_prefix("version:").or_else(|| rest.strip_prefix("v:")) {
            Some(rest) => (rest, true),
            None => (rest, false),
        };
        if repo.is_none() {
            for_each_ref::check_atom_outside_repository(rest).map_err(SortFailure::Atom)?;
            if rest.split_once(':').map_or(rest, |(name, _)| name) == "HEAD" {
                return Err(SortFailure::OutsideRepositoryBug);
            }
        }
        let atom = parse_atom(rest, &ctx).map_err(SortFailure::Atom)?;
        keys.push(SortKey {
            atom: ls_remote_atom(atom),
            descending,
            versioned,
        });
    }
    keys.reverse();
    Ok(keys)
}

/// Why the sort chain could not be built.
enum SortFailure {
    /// `parse_ref_filter_atom()`'s message, `die()`d by `parse_sorting_atom()`.
    Atom(for_each_ref::AtomError),
    /// `BUG: refs.c:2367: attempting to get main_ref_store outside of repository`.
    OutsideRepositoryBug,
}

/// The values `populate_value()` gives the atoms that read the array item's
/// `kind`, `flag`, `counts` or `is_base`: `ref_array_push()` leaves all four
/// zero for an advertised ref (builtin/ls-remote.c:159-160, ref-filter.c:2939-2951),
/// and `cmd_ls_remote()` runs neither `filter_ahead_behind()` nor
/// `filter_is_base()`. So `%(worktreepath)` is `""` (`ref->kind ==
/// FILTER_REFS_BRANCHES` fails, ref-filter.c:2473-2477), `%(flag)` is `""`,
/// and `%(ahead-behind:…)` and `%(is-base:…)` are `""` (:2574-2594).
fn ls_remote_atom(atom: Atom) -> Atom {
    match atom.field {
        Field::WorktreePath | Field::Flag | Field::AheadBehind(_) | Field::IsBase(..) => Atom {
            deref: atom.deref,
            field: Field::Constant(Vec::new()),
        },
        _ => atom,
    }
}

/// `ref_array_sort()` (ref-filter.c:3624-3628): `QSORT_S` with `compare_refs()`,
/// over the items `cmd_ls_remote()` pushed for the advertised refs.
///
/// `cmp_ref_sorting()` reads each side through `get_ref_atom_value()`
/// (ref-filter.c:3548-3551), which fills every atom of an item the first time
/// a comparison reaches it and `die()`s on what `populate_value()` rejects —
/// `missing object <oid> for <ref>` for an object this repository does not
/// have, which for `ls-remote` is anything not fetched yet. `git_qsort_s()`'s
/// merge sort therefore decides which ref the error names, and a single row is
/// never compared and never read.
fn sort_rows(repo: Option<&gix::Repository>, sorts: &[SortKey], rows: &mut Vec<Row>) -> Result<()> {
    if rows.len() < 2 {
        return Ok(());
    }
    let mut infos: Vec<RefInfo> = match repo {
        Some(repo) => ref_infos(repo, sorts, rows)?,
        None => {
            // Outside a repository every atom left is `SOURCE_NONE`. Of those, `%(upstream)`,
            // `%(push)` and the `:short` names reach `get_main_ref_store()` while the first
            // comparison populates (ref-filter.c:2484-2509, refs.c:2367), and a deref atom then
            // sets `need_tagged`, whose `get_object()` stock git does not survive.
            let reaches_refs = sorts.iter().any(|s| {
                matches!(
                    s.atom.field,
                    Field::Upstream(_) | Field::Push(_) | Field::RefName(NameMod::Short) | Field::SymRef(NameMod::Short)
                )
            });
            if reaches_refs {
                outside_repository_bug();
            }
            if sorts.iter().any(|s| s.atom.deref) {
                // `git ls-remote --sort='*refname' <url>` outside a repository is
                // killed by SIGSEGV in stock git 2.56. The Rust runtime's own SIGSEGV
                // handler would swallow a raised signal, so the default action is
                // restored first, as receive_pack.rs does.
                unsafe {
                    libc::signal(libc::SIGSEGV, libc::SIG_DFL);
                    libc::raise(libc::SIGSEGV);
                }
                unreachable!("SIGSEGV at its default action terminates the process");
            }
            rows.iter()
                .map(|row| RefInfo {
                    refname: row.name.clone().into_bytes(),
                    short: Vec::new(),
                    symref: row.symref.clone().unwrap_or_default().into_bytes(),
                    symref_short: Vec::new(),
                    obj: ObjInfo {
                        id: row.id,
                        kind: gix::objs::Kind::Commit,
                        size: 0,
                        data: None,
                    },
                    peeled: None,
                    is_head: false,
                    packed: false,
                    is_base: Vec::new(),
                    head_desc: None,
                    missing: false,
                    bad_tag: false,
                })
                .collect()
        }
    };

    let used: Vec<&Atom> = sorts.iter().rev().map(|s| &s.atom).collect();
    let prereleases = match repo {
        // git seeds `versioncmp`'s prerelease list from config once, lazily.
        Some(repo) => Prereleases::new(repo),
        None => Prereleases::none(),
    };
    let ctx = repo.map(|repo| RenderCtx {
        repo,
        worktrees: std::cell::OnceCell::new(),
    });
    let mut values: Vec<Option<Vec<Key>>> = (0..infos.len()).map(|_| None).collect();
    let mut order: Vec<usize> = (0..infos.len()).collect();
    refsort::git_qsort_s(&mut order, &mut |&a: &usize, &b: &usize| -> Result<Ordering> {
        for i in [a, b] {
            if values[i].is_none() {
                values[i] = Some(match &ctx {
                    Some(ctx) => {
                        for_each_ref::populate_value_errors(&used, &infos[i])?;
                        for_each_ref::sort_key_values(ctx, sorts, &infos[i])?
                    }
                    None => sorts
                        .iter()
                        .map(|s| {
                            Key::Str(for_each_ref::render_from_ref(&s.atom, &infos[i]).unwrap_or_default())
                        })
                        .collect(),
                });
            }
        }
        let (ka, kb) = (values[a].as_deref().unwrap_or_default(), values[b].as_deref().unwrap_or_default());
        Ok(for_each_ref::compare_sort_keys(
            sorts,
            ka,
            kb,
            &infos[a].refname,
            &infos[b].refname,
            false,
            &prereleases,
        ))
    })?;
    infos.clear();
    let mut taken: Vec<Option<Row>> = rows.drain(..).map(Some).collect();
    rows.extend(order.iter().map(|&i| taken[i].take().expect("each row once")));
    Ok(())
}

/// The `ref_array_item`s `cmd_ls_remote()` pushes, as far as `populate_value()`
/// will read them: `refname` and `symref` from the advertisement, no peeled
/// oid (`ref_array_push(..., NULL)`, so a `*`-atom peels the tag itself), and
/// the object loaded only when an atom reads it ([`for_each_ref::atom_reads_object`]).
/// A missing object is recorded, not reported: see [`sort_rows`].
fn ref_infos(repo: &gix::Repository, sorts: &[SortKey], rows: &[Row]) -> Result<Vec<RefInfo>> {
    let atoms = || sorts.iter().map(|s| &s.atom);
    let needs_data = atoms().any(for_each_ref::atom_reads_contents);
    let needs_peel = atoms().any(|a| a.deref);
    let reads_object = atoms().any(for_each_ref::atom_reads_object);
    let needs_short = atoms().any(|a| matches!(a.field, Field::RefName(NameMod::Short)));
    let needs_symref_short = atoms().any(|a| matches!(a.field, Field::SymRef(NameMod::Short)));
    let all_names = if needs_short || needs_symref_short {
        super::ref_filter::existing_ref_names(repo)?
    } else {
        Default::default()
    };
    // `head_atom_parser()` (ref-filter.c:928-938): `HEAD` resolved for reading, which is the
    // name `HEAD` itself when it is detached, and no marker at all when it does not resolve.
    let head_name: Option<Vec<u8>> = match repo.head_name() {
        Ok(Some(name)) => Some(name.as_bstr().to_vec()),
        Ok(None) => repo.head_id().is_ok().then(|| b"HEAD".to_vec()),
        Err(_) => None,
    };

    let mut infos = Vec::with_capacity(rows.len());
    for row in rows {
        let refname = row.name.clone().into_bytes();
        let symref = row.symref.clone().unwrap_or_default().into_bytes();
        let loaded = match reads_object {
            true => for_each_ref::load(repo, row.id, needs_data).ok(),
            false => None,
        };
        let missing = reads_object && loaded.is_none();
        let obj = loaded.unwrap_or(ObjInfo {
            id: row.id,
            kind: gix::objs::Kind::Commit,
            size: 0,
            data: None,
        });
        let (peeled, bad_tag) = match needs_peel && !missing {
            true => for_each_ref::peel_for_deref(repo, &obj, needs_data),
            false => (None, false),
        };
        infos.push(RefInfo {
            is_head: head_name.as_deref() == Some(refname.as_slice()),
            short: match needs_short {
                true => for_each_ref::short_name(repo, &refname, &all_names),
                false => Vec::new(),
            },
            symref_short: match needs_symref_short && !symref.is_empty() {
                true => for_each_ref::short_name(repo, &symref, &all_names),
                false => Vec::new(),
            },
            refname,
            symref,
            obj,
            peeled,
            packed: false,
            is_base: Vec::new(),
            head_desc: None,
            missing,
            bad_tag,
        });
    }
    Ok(infos)
}

/// `BUG()` (usage.c): the message, then `abort()`.
fn outside_repository_bug() -> ! {
    eprintln!("BUG: refs.c:2367: attempting to get main_ref_store outside of repository");
    std::process::abort();
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
        name: name.clone(),
    });
    if let Some(peeled) = peeled {
        rows.push(Row {
            name: format!("{name}^{{}}"),
            id: peeled,
            symref: None,
            peel: true,
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
        let Ok(keys) = parse_sort_keys(&["refname".into(), "-symref".into()], None) else {
            panic!("both keys are SOURCE_NONE atoms");
        };
        assert_eq!(keys.len(), 2);
        assert!(keys[0].descending && matches!(keys[0].atom.field, Field::SymRef(NameMod::Full)));
        assert!(!keys[1].descending && matches!(keys[1].atom.field, Field::RefName(NameMod::Full)));

        let cmp = |a: &str, b: &str, sa: &str, sb: &str| {
            let ka = [Key::Str(sa.into()), Key::Str(a.into())];
            let kb = [Key::Str(sb.into()), Key::Str(b.into())];
            for_each_ref::compare_sort_keys(&keys, &ka, &kb, a.as_bytes(), b.as_bytes(), false, &Prereleases::none())
        };
        // Equal symrefs fall through to an ascending refname compare, never a
        // descending one, even though the symref key is reversed.
        assert_eq!(cmp("refs/heads/a", "refs/heads/b", "x", "x"), Ordering::Less);
        assert_eq!(cmp("refs/heads/a", "refs/heads/b", "w", "x"), Ordering::Greater);
    }

    /// git validates sort keys after the transport and `die()`s (exit 128) on
    /// the first it rejects. Outside a repository an atom that needs object
    /// data is refused before its parser runs (ref-filter.c:1074-1077).
    #[test]
    fn sort_key_validation_mirrors_git() {
        let msg = |spec: &str| match parse_sort_keys(&[spec.to_string()], None) {
            Err(SortFailure::Atom(e)) => e.msg,
            _ => panic!("{spec} is refused"),
        };
        assert_eq!(msg("bogus"), "unknown field name: bogus");
        assert_eq!(msg(""), "malformed field name: ");
        assert_eq!(msg("-"), "malformed field name: ");
        assert_eq!(msg("*"), "malformed field name: *");
        assert_eq!(
            msg("objectname"),
            "not a git repository, but the field 'objectname' requires access to object data"
        );
        assert_eq!(
            msg("-v:creatordate"),
            "not a git repository, but the field 'creatordate' requires access to object data"
        );
        let first = |specs: &[&str]| match parse_sort_keys(&specs.iter().map(|s| s.to_string()).collect::<Vec<_>>(), None) {
            Ok(keys) => keys,
            Err(_) => panic!("{specs:?} parse"),
        };
        assert!(first(&["v:refname"])[0].versioned);
        assert!(first(&["-version:refname"])[0].descending);
        // The first rejected key on the command line is the one reported.
        match parse_sort_keys(&["nope1".into(), "nope2".into()], None) {
            Err(SortFailure::Atom(e)) => assert_eq!(e.msg, "unknown field name: nope1"),
            _ => panic!("refused"),
        }
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

/// `cmd_ls_remote()` without a repository: `remote_get(dest)` has only the
/// system, global and command-line configuration to read, so the operand is
/// the URL (after `url.<base>.insteadOf`), and the transport is built from it
/// directly — `transport_get()` needs no repository either.
fn outside_repository(repository: Option<&str>, patterns: &[&str], opts: &Opts) -> ExitCode {
    // `remote_get(NULL)` finds no default remote to list.
    let Some(dest) = repository else {
        eprintln!("fatal: No remote configured to list refs from.");
        return ExitCode::from(128);
    };
    let config = crate::config::global_config();
    let mut raw = gix::bstr::BString::from(dest);
    gix::remote::url::Rewrite::from_config(&config, |_| true)
        .rewrite_url_in_place(&mut raw, gix::remote::Direction::Fetch);
    if opts.get_url {
        println!("{raw}");
        return ExitCode::SUCCESS;
    }
    let url = match gix::url::parse(raw.as_ref()) {
        Ok(url) => url,
        Err(e) => {
            eprintln!("fatal: {e}");
            return ExitCode::from(128);
        }
    };
    if let Some(code) = crate::setup::check_url_allowed(&url) {
        return code;
    }
    let version = match crate::config::config_get_string(None, "protocol.version").as_deref() {
        None | Some("2") => gix::protocol::transport::Protocol::V2,
        Some("1") => gix::protocol::transport::Protocol::V1,
        Some("0") => gix::protocol::transport::Protocol::V0,
        Some(v) => {
            eprintln!("fatal: unknown value for config 'protocol.version': {v}");
            return ExitCode::from(128);
        }
    };
    let upload_pack = super::fetch::local_service_program(
        Some(&url),
        opts.upload_pack.as_deref().map(Into::into),
        "upload-pack",
    );
    let (refs, closing_flush_broken_pipe) = match advertised_refs(url.clone(), version, upload_pack, &opts.server_options, config) {
        Ok(listed) => listed,
        Err(err) => {
            // A 401 to the retry that carried credentials: `discover_refs()` dies.
            if let Some(e) = err.downcast_ref::<gix::protocol::handshake::Error>() {
                if let Some(code) = crate::transport_err::http_auth_fatal(&raw.to_string(), None, e)
                    .or_else(|| crate::transport_err::http_get_fatal(&raw.to_string(), None, e))
                {
                    return code;
                }
            }
            // A closed pipe under the `ls-refs` request: `check_pipe()` (pkt-line.c:171-174).
            if crate::sigpipe::is_broken_pipe(err.as_ref()) {
                crate::sigpipe::exit_broken_pipe();
            }
            if let Some(code) = crate::transport_err::ssh_fatal(&url.to_bstring().to_string(), &err)
                .or_else(|| crate::transport_err::hang_up_fatal(&err))
            {
                return code;
            }
            eprintln!("fatal: {err}");
            return ExitCode::from(128);
        }
    };
    // Without a repository no sort key reaches a fallible step: every atom that would has
    // already been refused by `parse_sort_keys()`.
    let code = list(&refs, patterns, opts, None).unwrap_or_else(|e| {
        eprintln!("fatal: {e}");
        ExitCode::from(128)
    });
    if closing_flush_broken_pipe {
        disconnect_into_closed_pipe();
    }
    code
}

/// Connect, shake hands and list: the advertisement a v0/v1 server sends
/// unasked, or the answer to a v2 `ls-refs`, and whether the closing flush met a closed pipe:
/// `disconnect_git()` writes it after the caller has listed them (transport.c:1020-1030).
fn advertised_refs(
    url: gix::Url,
    version: gix::protocol::transport::Protocol,
    upload_pack: Option<gix::bstr::BString>,
    server_options: &[gix::bstr::BString],
    config: gix::config::File,
) -> anyhow::Result<(Vec<Ref>, bool)> {
    use gix::protocol::transport::client::blocking_io::connect;
    let is_http = matches!(url.scheme, gix::url::Scheme::Http | gix::url::Scheme::Https);
    let url_string = url.to_bstring();
    let mut transport = connect::connect(
        url,
        connect::Options {
            version,
            upload_pack,
            ..Default::default()
        },
    )?;
    // The refill of a refused `POST` happens inside the transport; see
    // [`super::credential::with_post_authentication`].
    if is_http {
        let options = super::credential::with_post_authentication(
            Box::new(gix::protocol::transport::client::blocking_io::http::Options::default()),
            config.clone(),
            url_string.as_ref(),
        );
        transport.configure(&*options).ok();
    }
    let mut handshake = gix::protocol::handshake(
        &mut transport,
        gix::protocol::transport::Service::UploadPack,
        super::credential::transport_authenticate(config),
        Vec::new(),
        &mut gix::progress::Discard,
    )?;
    let refs = match handshake.refs.take() {
        Some(refs) => refs,
        None => gix::protocol::LsRefsCommand::new(
            None,
            &handshake.capabilities,
            ("agent", Some(super::version::user_agent_sanitized())),
            server_options,
            None,
        )
        .invoke_blocking(&mut transport, &mut gix::progress::Discard, false)?,
    };
    match gix::protocol::indicate_end_of_interaction(&mut transport, false) {
        Ok(()) => Ok((refs, false)),
        Err(err) if crate::sigpipe::is_broken_pipe(&err) => Ok((refs, true)),
        Err(err) => Err(err.into()),
    }
}
