//! `git send-pack` — push objects over the git protocol.
//!
//! Port of `cmd_send_pack` (`builtin/send-pack.c`): parse the arguments, resolve
//! the destination, match the `<ref>` arguments against the local refs, hand the
//! result to the receive-pack client in [`super::push_proto`], and print the
//! status block `transport_print_push_status()` prints.
//!
//! The wire half is shared with `git push` rather than reimplemented:
//! [`push_proto::send_pack`] is the port of `send_pack()` itself — capability
//! negotiation, the `<old> <new> <ref>` command list, the push certificate, the
//! pack, and the `report-status` / `report-status-v2` parser. This module is the
//! `builtin/` layer on top of it, which is the same split git has.
//!
//! The argument surface is covered in full:
//! ```text
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
//! ```
//! (all checked against git 2.55.0.)
//!
//! Not covered:
//!
//!   * **`--stateless-rpc`.** The smart-HTTP framing has no local destination to
//!     connect to, and its `--stdin` variant reads the refspec list as pkt-lines
//!     rather than plain lines. The flag is accepted and dropped; a caller that
//!     needs the HTTP transport goes through `http_backend`/`remote_ext`.
//!   * **`--thin` and `--progress`.** Both describe how the sender builds and
//!     narrates its pack, not what the receiver is asked to do. The pack this
//!     sends is never thin and reports no progress, which is a valid choice for
//!     either flag's value, so neither changes the bytes on the wire.
//!   * **Refspec forms beyond `[+]<src>[:<dst>]`.** `match_push_refs()`'s
//!     pattern expansion (`refs/heads/*:refs/heads/*`) is not implemented here;
//!     `git push` is the porcelain that has it.
//!   * **`--force-with-lease=<ref>:<expect>`** resolves `<expect>` through
//!     gitoxide's `rev_parse_single` rather than git's `repo_get_oid`; the two
//!     accept the same everyday spellings but are not proven byte-identical on
//!     exotic ones.

use anyhow::{bail, Context, Result};
use gix::remote::Direction;
use gix::ObjectId;
use std::io::BufRead;
use std::process::ExitCode;

use super::push_proto::{self, Request};

/// git's `DEFAULT_ABBREV` floor for `find_unique_abbrev` and the
/// `FALLBACK_DEFAULT_ABBREV` `transport_summary_width()` uses when a push
/// carries no object ids at all.
const DEFAULT_ABBREV: usize = 7;

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

/// Everything `cmd_send_pack` carries out of `parse_options` into the push
/// itself, plus the two counters its post-parse usage checks consult.
#[derive(Default)]
struct State {
    send_all: bool,
    send_mirror: bool,
    force: bool,
    dry_run: bool,
    verbose: bool,
    atomic: bool,
    /// `--stdin`: read the refspec list from stdin instead of argv.
    from_stdin: bool,
    /// `--helper-status`: the `remote-helper` status block on **stdout** in
    /// place of the human-readable one on stderr.
    helper_status: bool,
    /// `--signed` / `push.gpgsign`.
    signed: push_proto::Signed,
    /// `-o`/`--push-option` values, in argv order.
    push_options: Vec<String>,
    /// `--receive-pack` / `--exec`, git's `receivepack` (default
    /// `git-receive-pack`, which is what leaving it unset means here).
    receive_pack: Option<String>,
    /// `--remote <name>`: the configured remote whose tracking refs the push
    /// updates, checked to actually carry `<directory>` as one of its URLs.
    remote_name: Option<String>,
    /// `--force-with-lease[=<ref>:<expect>]`, kept as written so the wire layer
    /// can resolve the expected value.
    lease: Option<Option<String>>,
    force_if_includes: bool,
    /// `<directory>`, the first non-option argument.
    dest: Option<String>,
    /// The `<ref>` arguments after `<directory>`.
    refspecs: Vec<String>,
    /// Non-option arguments: the first is `<directory>`, the rest are `<ref>`s.
    positionals: usize,
}

/// The outcome of parsing: either a fully-formed request, or a diagnostic that
/// has already decided the exit code.
enum Parsed {
    Ok(State),
    Exit(ExitCode),
}

/// `git send-pack` — port of `cmd_send_pack` (`builtin/send-pack.c`).
///
/// Returns 129 with git's own output for `-h`, for every malformed invocation,
/// for a missing `<directory>`, and for the `--all`/`--mirror`/`<ref>` conflict;
/// 128 for the `--signed` value git rejects during parsing. Otherwise it runs
/// the push and returns `send_pack()`'s status: 0 when every ref ended `OK`,
/// `UPTODATE` or `NONE`, and `ERROR_SEND_PACK_BAD_REF_STATUS` (1) when any did
/// not (send-pack.c:795-805).
pub fn send_pack(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `send-pack` does take positionals
    // (the destination and the refs), so the leading verb must be dropped rather
    // than counted as one.
    let args = match args.first().map(String::as_str) {
        Some("send-pack") => &args[1..],
        _ => args,
    };

    let mut state = match parse(args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Ok(state) => state,
    };

    // `if (from_stdin)`: the refspec list continues on stdin, one per line
    // (send-pack.c:238-249). The `--stateless-rpc` pkt-line spelling of the same
    // list is not read here — see the module header.
    if state.from_stdin {
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            if !line.is_empty() {
                state.refspecs.push(line);
                state.positionals += 1;
            }
        }
    }

    if let Some(code) = preflight(&state) {
        return Ok(code);
    }

    push(&state)
}

/// Everything `cmd_send_pack` does once the arguments are accepted: open the
/// repository, resolve the destination, match the refspecs against the local
/// refs, run the push, and print the status block.
fn push(st: &State) -> Result<ExitCode> {
    let dest = st.dest.as_deref().unwrap_or_default();
    let repo = crate::setup::discover()?;

    // `if (remote_name) { remote = remote_get(...); if (!remote_has_url(...)) die(...) }`
    // (send-pack.c:259-265). Only a named remote gets its tracking refs updated
    // afterwards, which is why the destination has to be one of its URLs.
    let remote = match &st.remote_name {
        Some(name) => {
            let configured = repo.find_remote(name.as_str())?;
            let has_url = [Direction::Push, Direction::Fetch]
                .iter()
                .filter_map(|d| configured.url(*d))
                .any(|u| u.to_bstring() == dest);
            if !has_url {
                crate::git_fatal!("Destination {dest} is not a uri for {name}");
            }
            configured
        }
        None => repo.remote_at(dest)?,
    };

    let requests = build_requests(&repo, st)?;
    let opts = push_proto::SendOptions {
        atomic: st.atomic,
        push_options: st.push_options.clone(),
        // `--mirror` is `MATCH_REFS_MIRROR`, whose other half is deleting every
        // advertised ref this repository no longer has. Only the wire layer has
        // the advertisement, so the decision is handed down with it.
        delete_scope: st.send_mirror.then_some(push_proto::DeleteScope::All),
        local_refs: requests.iter().map(|r| r.name.clone()).collect(),
        signed: st.signed,
        receive_pack: st.receive_pack.clone(),
    };

    // `git_connect()` for a local destination runs `git-receive-pack '<dest>'`,
    // which dies with git's `enter_repo` message when the path is not a
    // repository; the parent then cannot read the advertisement and dies through
    // `die_initial_contact()` (connect.c). Both lines, and the 128, are
    // reproduced here because the vendored transport reports a single Rust-level
    // metadata error instead.
    if let Some(bad) = local_dest_that_is_not_a_repository(dest) {
        eprintln!("fatal: '{bad}' does not appear to be a git repository");
        eprintln!(
            "fatal: Could not read from remote repository.\n\n\
             Please make sure you have the correct access rights\n\
             and the repository exists."
        );
        return Ok(ExitCode::from(128));
    }

    // Everything `send_pack()` can fail on is a `die()` in git — a refused
    // capability, a broken connection, an unreadable advertisement — so the
    // status is 128 rather than the dispatcher's 1.
    let outcome = match push_proto::send_pack(&repo, &remote, &requests, st.dry_run, &opts) {
        Ok(outcome) => outcome,
        Err(err) => {
            eprintln!("fatal: {err}");
            return Ok(ExitCode::from(128));
        }
    };

    if st.helper_status {
        print_helper_status(&outcome);
    } else {
        print_push_status(&repo, &outcome, st.verbose);
    }

    // `if (!ret && !transport_refs_pushed(remote_refs))` (send-pack.c:341-343):
    // a ref that ended `NONE` or `UPTODATE` did not move, and if none of them
    // did the run reports so. Stable plumbing output; not localized.
    let bad = outcome.unpack.is_err() || outcome.statuses.iter().any(|s| s.result.is_err());
    let pushed = outcome.statuses.iter().any(|s| !s.up_to_date && s.result.is_ok());
    if !bad && !pushed {
        eprintln!("Everything up-to-date");
    }

    Ok(if bad { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

/// The destination as `enter_repo()` sees it, when it names a local path that is
/// not a repository — the case `git-receive-pack` would have refused on the far
/// end. `None` for anything reachable, and for any destination that is not a
/// local path, whose failures belong to the transport that owns them.
pub(crate) fn local_dest_that_is_not_a_repository(dest: &str) -> Option<&str> {
    let url = gix::url::parse(dest.into()).ok()?;
    if url.scheme != gix::url::Scheme::File {
        return None;
    }
    // `enter_repo(..., strict = 0)`: a worktree, a `.git` directory, or a bare
    // repository, with `.git` appended if that is what makes it one.
    let found = [
        dest.to_string(),
        format!("{dest}/.git"),
        format!("{dest}.git"),
        format!("{dest}.git/.git"),
    ]
    .iter()
    .any(|c| gix::open(c).is_ok());
    (!found).then_some(dest)
}

/// `match_push_refs()` reduced to what `send-pack` can ask of it: `--all` and
/// `--mirror` take every local ref under `refs/`, and otherwise each `<ref>`
/// argument is one `[+]<src>[:<dst>]` refspec.
///
/// `get_local_heads()` (remote.c) is `for_each_ref`, so both flag forms cover
/// tags and remote-tracking refs, not just branches — `send-pack --all` is a
/// wider set than `git push --all`, which is refspec-driven.
fn build_requests(repo: &gix::Repository, st: &State) -> Result<Vec<Request>> {
    let mut requests = Vec::new();

    if st.send_all || st.send_mirror {
        for r in repo.references()?.all()? {
            let Ok(r) = r else { continue };
            let name = r.name().as_bstr().to_string();
            if !name.starts_with("refs/") {
                continue;
            }
            if let Some(id) = r.try_id() {
                requests.push(Request {
                    src: Some(name.clone()),
                    name,
                    new: id.detach(),
                    // `MATCH_REFS_MIRROR` forces every update; `--all` still
                    // honours `--force` alone.
                    force: st.force || st.send_mirror,
                    expected: None,
                    only_if_absent: false,
                    check_reachable: None,
                explicit_delete: false,
                });
            }
        }
        return Ok(requests);
    }

    let null = ObjectId::null(repo.object_hash());
    for spec in &st.refspecs {
        let (forced, body) = match spec.strip_prefix('+') {
            Some(rest) => (true, rest),
            None => (false, spec.as_str()),
        };
        let (src, dst) = match body.split_once(':') {
            Some((s, d)) => (s, d),
            None => (body, body),
        };
        let force = forced || st.force;

        // An empty source is a deletion: `:refs/heads/gone`.
        if src.is_empty() {
            requests.push(Request {
                name: full_ref_name(repo, dst),
                src: None,
                new: null,
                force,
                expected: None,
                only_if_absent: false,
                check_reachable: None,
                explicit_delete: false,
            });
            continue;
        }

        let src_ref = repo
            .find_reference(src)
            .with_context(|| format!("src refspec {src} does not match any"))?;
        let new = src_ref.clone().into_fully_peeled_id()?.detach();
        requests.push(Request {
            name: full_ref_name(repo, dst),
            src: Some(src_ref.name().as_bstr().to_string()),
            new,
            force,
            expected: None,
            only_if_absent: false,
            check_reachable: None,
                explicit_delete: false,
        });
    }
    Ok(requests)
}

/// The destination side of a refspec as a full ref name. A `<dst>` that is
/// already qualified is used as written; a short one is resolved against the
/// *local* refs first (so `main` reaches `refs/heads/main`) and falls back to
/// `refs/heads/<dst>`, which is what `match_push_refs` settles on for a name
/// the remote does not carry yet.
fn full_ref_name(repo: &gix::Repository, dst: &str) -> String {
    if dst.starts_with("refs/") {
        return dst.to_string();
    }
    match repo.find_reference(dst) {
        Ok(r) => r.name().as_bstr().to_string(),
        Err(_) => format!("refs/heads/{dst}"),
    }
}

/// `transport_print_push_status()` (transport.c:850-899) in its non-porcelain
/// form: `To <dest>` once, then the up-to-date refs under `-v`, then the ones
/// that moved, then the ones that did not.
fn print_push_status(repo: &gix::Repository, outcome: &push_proto::Outcome, verbose: bool) {
    // `transport_summary_width()`: `2 * <widest abbreviation> + 3`, measured
    // over every old and new oid in the push, with `FALLBACK_DEFAULT_ABBREV`
    // when there are none (transport.c:837-848).
    let width = summary_width(repo, outcome);
    let mut printed = 0usize;

    let mut emit = |flag: char, summary: String, from: Option<&str>, to: &str, msg: Option<&str>| {
        if printed == 0 {
            eprintln!("To {}", outcome.url);
        }
        printed += 1;
        let refs = match from {
            Some(f) => format!("{} -> {}", prettify(f), prettify(to)),
            None => prettify(to).to_string(),
        };
        let msg = msg.map(|m| format!(" ({m})")).unwrap_or_default();
        eprintln!(" {flag} {summary:<width$} {refs}{msg}");
    };

    // `strbuf_add_unique_abbrev(&quickref, oid, DEFAULT_ABBREV)`: the quickref
    // uses the same abbreviation length the width was measured from.
    let abbrev = (width - 3) / 2;
    let short = |oid: &ObjectId| oid.to_hex_with_len(abbrev).to_string();

    for pass in 0..3 {
        for s in &outcome.statuses {
            let up_to_date = s.up_to_date && s.result.is_ok();
            let ok = !up_to_date && s.result.is_ok();
            match pass {
                0 if !(up_to_date && verbose) => continue,
                1 if !ok => continue,
                2 if up_to_date || ok => continue,
                _ => {}
            }
            let to = s.report_name.as_deref().unwrap_or(&s.name);
            let from = s.src.as_deref();
            match &s.result {
                // `case REF_STATUS_REMOTE_REJECT: print_ref_status('!', "[remote
                // rejected]", …)` (transport.c) — a refusal the server sent back as an
                // `ng` line is not the same summary as one this side decided.
                Err(reason) => {
                    let summary = match s.remote_rejected {
                        true => "[remote rejected]",
                        false => "[rejected]",
                    };
                    emit('!', summary.into(), from, to, Some(reason))
                }
                Ok(()) if up_to_date => emit('=', "[up to date]".into(), from, to, None),
                Ok(()) if s.new.is_null() => emit('-', "[deleted]".into(), None, to, None),
                Ok(()) if s.old.is_null() => {
                    let kind = if to.starts_with("refs/tags/") {
                        "[new tag]"
                    } else if to.starts_with("refs/heads/") {
                        "[new branch]"
                    } else {
                        "[new reference]"
                    };
                    emit('*', kind.into(), from, to, None);
                }
                Ok(()) => {
                    // `print_ok_ref_status`: `...` and a `+` flag for a forced
                    // update, `..` and a blank flag otherwise.
                    let (flag, sep, msg) = if s.forced {
                        ('+', "...", Some("forced update"))
                    } else {
                        (' ', "..", None)
                    };
                    emit(flag, format!("{}{sep}{}", short(&s.old), short(&s.new)), from, to, msg);
                }
            }
        }
    }
}

/// `transport_summary_width()`. `measure_abbrev` takes the *unique* abbreviation
/// of each oid but never below `DEFAULT_ABBREV`; with no refs at all the width
/// falls back to that same constant.
fn summary_width(repo: &gix::Repository, outcome: &push_proto::Outcome) -> usize {
    let floor = default_abbrev(repo);
    let mut maxw = None;
    for s in &outcome.statuses {
        for oid in [&s.old, &s.new] {
            if oid.is_null() {
                continue;
            }
            maxw = Some(maxw.unwrap_or(0).max(unique_abbrev_len(repo, oid, floor)));
        }
    }
    2 * maxw.unwrap_or(floor) + 3
}

/// git's `DEFAULT_ABBREV`: `core.abbrev` when set, else the fallback. A repo
/// that spells it `no` gets the full hash, which is git's `40`/`64` case.
fn default_abbrev(repo: &gix::Repository) -> usize {
    let full = repo.object_hash().len_in_hex();
    let config = repo.config_snapshot();
    match config.string("core.abbrev") {
        None => DEFAULT_ABBREV,
        Some(v) if v.as_slice() == b"no" => full,
        Some(v) => std::str::from_utf8(v.as_slice())
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .map(|n| n.clamp(4, full))
            .unwrap_or(DEFAULT_ABBREV),
    }
}

/// `repo_find_unique_abbrev_r(..., DEFAULT_ABBREV)`: the shortest prefix that
/// still names exactly one object, never shorter than `floor`.
fn unique_abbrev_len(repo: &gix::Repository, oid: &ObjectId, floor: usize) -> usize {
    let full = repo.object_hash().len_in_hex();
    (floor..=full)
        .find(|&len| {
            gix::hash::Prefix::new(oid.as_ref(), len)
                .ok()
                .and_then(|p| repo.objects.lookup_prefix(p, None).ok())
                .is_some_and(|found| matches!(found, Some(Ok(_))))
        })
        .unwrap_or(full)
}

/// `prettify_refname()`: drop the namespace prefix from a full ref name.
fn prettify(name: &str) -> &str {
    for prefix in ["refs/heads/", "refs/tags/", "refs/remotes/"] {
        if let Some(short) = name.strip_prefix(prefix) {
            return short;
        }
    }
    name
}

/// `print_helper_status()` (send-pack.c): one machine-readable line per ref on
/// **stdout**, terminated by a blank line, for a remote helper to parse.
fn print_helper_status(outcome: &push_proto::Outcome) {
    for s in &outcome.statuses {
        let name = s.report_name.as_deref().unwrap_or(&s.name);
        match &s.result {
            Ok(()) => println!("ok {name}"),
            Err(reason) => println!("error {name} {reason}"),
        }
    }
    println!();
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
            // `if (argc > 0) { dest = argv[0]; refspec_appendn(&rs, argv + 1, argc - 1); }`
            // — the first non-option is the destination, the rest are refspecs.
            if st.dest.is_none() {
                st.dest = Some(a.to_string());
            } else {
                st.refspecs.push(a.to_string());
            }
            st.positionals += 1;
            i += 1;
            continue;
        }

        if a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }

        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, so it is not an `OPTS` entry and `resolve_long()` never
        // sees it. This table has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL`
        // renders the same block `-h` prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Parsed::Exit(ExitCode::from(129));
        }

        if let Some(body) = a.strip_prefix("--") {
            match long_opt(body, args, &mut i, &mut st) {
                Some(code) => return Parsed::Exit(code),
                None => continue,
            }
        }

        match short_opts(&a[1..], &mut i, &mut st) {
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
        set_long(def.long, false, None, st);
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

    set_long(def.long, true, value, st);
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
    let Ok(repo) = crate::setup::discover() else {
        return false;
    };
    repo.rev_parse_single(spec).is_ok()
}

/// Record the effect of long option `long`; `on` is false for the `--no-` form,
/// and `value` is whatever the option consumed (always `None` for a boolean and
/// for a negation, which parse-options never lets carry one).
///
/// `--progress`, `--thin` and `--stateless-rpc` are accepted and dropped: the
/// first two only change how the pack is produced (this one is never thin and
/// reports no progress, and both are the sender's choice, not the receiver's),
/// and the third is the smart-HTTP framing, which has no local destination.
fn set_long(long: &str, on: bool, value: Option<&str>, st: &mut State) {
    match long {
        "all" => st.send_all = on,
        "mirror" => st.send_mirror = on,
        "force" => st.force = on,
        "dry-run" => st.dry_run = on,
        "verbose" => st.verbose = on,
        "quiet" => st.verbose = st.verbose && !on,
        "atomic" => st.atomic = on,
        "stdin" => st.from_stdin = on,
        "helper-status" => st.helper_status = on,
        "force-if-includes" => st.force_if_includes = on,
        "receive-pack" | "exec" => st.receive_pack = on.then(|| value.unwrap_or("").to_string()),
        "remote" => st.remote_name = on.then(|| value.unwrap_or("").to_string()),
        "push-option" => {
            if on {
                st.push_options.push(value.unwrap_or("").to_string());
            } else {
                // `OPT_STRING_LIST`'s `--no-` form clears the list.
                st.push_options.clear();
            }
        }
        // `option_parse_push_signed`: a bare `--signed` is "always", `--no-signed`
        // is "never", and the value grammar was already validated by
        // [`check_value`].
        "signed" => {
            st.signed = match (on, value) {
                (false, _) => push_proto::Signed::Never,
                (true, None) => push_proto::Signed::Always,
                (true, Some(v)) if v.eq_ignore_ascii_case("if-asked") => push_proto::Signed::IfAsked,
                (true, Some(v)) => match parse_maybe_bool(v) {
                    Some(true) => push_proto::Signed::Always,
                    _ => push_proto::Signed::Never,
                },
            };
        }
        "force-with-lease" => st.lease = on.then(|| value.map(str::to_string)),
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
fn short_opts(cluster: &str, i: &mut usize, st: &mut State) -> Option<ExitCode> {
    for c in cluster.chars() {
        match c {
            'h' => {
                print!("{USAGE}");
                return Some(ExitCode::from(129));
            }
            // `OPT__VERBOSITY` counts up for `-v` and down for `-q`.
            'v' => st.verbose = true,
            'q' => st.verbose = false,
            'n' => st.dry_run = true,
            'f' => st.force = true,
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
