//! `git http-push` — push objects over HTTP/DAV. **Not ported: this module
//! bails on every path that would touch the network.**
//!
//! What *is* covered is the part of stock `http-push` that runs before it opens
//! a connection, and only because those paths are byte-verifiable without a
//! server:
//!   * the argument scanner from `http-push.c`'s `main` loop, including its two
//!     quirks — an *unrecognised* `-…` argument is not an error but falls
//!     through to become the `<remote>` URL, and option scanning stops at the
//!     first `<head>`, so `-h` after a refspec is a refspec
//!   * `-h`, and a missing `<remote>`, → git's 78-byte usage line plus a blank
//!     line, on **stderr**, exit 129
//!   * the `-d`/`-D` arity check → `fatal: You must specify only one branch name
//!     when deleting a remote branch`, exit 128
//! (all checked against git 2.55.0 by running the stock binary.)
//!
//! Everything else bails, naming the substrate that is missing. It is
//! deliberately *not* approximated: `http-push` mutates the *receiving*
//! repository, which is exactly the post-command state a differential harness
//! inspects, and a DAV push that lands the wrong objects or the wrong ref is
//! indistinguishable from success at the exit-code level.
//!
//! The missing substrate, concretely, in the vendored crates under `src/ported`:
//!
//!   1. **No WebDAV client, and no HTTP verb to build one from.** `http-push` is
//!      a DAV client: it `PROPFIND`s for `DAV:` compliance and the activity
//!      collection, `MKCOL`s object directories, `LOCK`/`UNLOCK`s `info/refs`,
//!      `PUT`s loose objects and packs to temporary names and `MOVE`s them into
//!      place. The vendored HTTP abstraction is
//!      `gix-transport/src/client/blocking_io/http/traits.rs`, whose `trait Http`
//!      declares exactly two methods, `get` and `post` — documented there as
//!      "the HTTP operations needed to power all git interactions: read via GET
//!      and write via POST". None of `PROPFIND`, `MKCOL`, `PUT`, `MOVE`, `LOCK`
//!      or `UNLOCK` occurs anywhere under `gix-transport/src`, so there is no
//!      request type to issue and no lock-token or activity handling to drive.
//!   2. **No push implementation at any layer.** `gix-protocol/src/lib.rs`
//!      exports only `handshake`, `ls_refs` and `fetch`; there is no
//!      receive-pack or dumb-push driver, and `gix/src` has no `Remote::push`.
//!      Even the smart-HTTP half of a push does not exist to fall back on.
//!   3. **No remote object-existence walk for `--all`.** `--all` verifies every
//!      object in the local ref's history against the remote by fetching remote
//!      loose objects and pack indices over HTTP; that dumb-protocol reader is
//!      the `http-fetch` side, which is likewise absent.
//!
//! Two paths are covered by the bail rather than reproduced, so this doc claims
//! no more than the code does. `fatal: invalid refspec '<spec>'` — git validates
//! `<head>...` through `refspec_appendn` *before* the `-d` arity check — is not
//! emitted, because the vendored `gix_refspec::parse` provably disagrees with
//! git's push grammar on which specs are valid: it splits at the *first* colon
//! (`spec.find_byte(b':')` in `gix-refspec/src/parse.rs`) where git splits at the
//! last, so it rejects `a:b:c` which git accepts, and it accepts a bare `+`
//! which git rejects. Emitting a verdict from a parser known to diverge would be
//! worse than not emitting one, so any invocation carrying a `<head>` bails
//! instead. Likewise `error: Cannot access URL <url>, return code <n>` is not
//! reproduced — the number is a libcurl error code from the connection attempt,
//! which is the part that does not exist.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// Stock git's `http-push` usage text, byte-for-byte (git 2.55.0), including the
/// trailing blank line. git prints it on **stderr** — for `-h` and for a missing
/// `<remote>` alike — and exits 129, leaving stdout empty in both cases.
const USAGE: &str =
    "usage: git http-push [--all] [--dry-run] [--force] [--verbose] <remote> [<head>...]\n\n";

/// The flags `http-push.c`'s scanner recognises. Only `delete_branch` feeds a
/// check that runs before the network, so the rest are accepted and dropped —
/// every path that would consult them bails first.
#[derive(Default)]
struct State {
    /// `-d` or `-D`.
    delete_branch: bool,
    /// The `<remote>` URL, with the trailing slash git's `str_end_url_with_slash`
    /// guarantees.
    url: Option<String>,
    /// The `<head>...` arguments: everything from the first non-URL positional
    /// on, verbatim, including any later `-…` entries.
    refspecs: Vec<String>,
}

/// `git http-push` — argument scanning and pre-flight checks only; the DAV push
/// itself is not ported.
///
/// Returns 129 with git's usage text on stderr for `-h` and for a missing
/// `<remote>`, and 128 with git's own message for the `-d`/`-D` arity check. Any
/// invocation that survives both bails, naming the substrate that is missing;
/// see the module documentation for the full list.
pub fn http_push(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `http-push` takes positionals (the
    // URL and the refspecs), so the verb must be dropped rather than scanned as
    // one — it does not start with '-', and would otherwise become the URL.
    let args = match args.first().map(String::as_str) {
        Some("http-push") => &args[1..],
        _ => args,
    };

    let state = match scan(args) {
        Scanned::Exit(code) => return Ok(code),
        Scanned::Ok(state) => state,
    };

    // `if (!repo->url) usage(http_push_usage);`
    let Some(url) = state.url.as_deref() else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };

    // git's next step is `refspec_appendn`, which dies with `invalid refspec` on
    // a malformed `<head>` *before* the arity check below. That grammar is not
    // reproducible here (see the module docs), so rather than run the later
    // checks on unvalidated input — and risk reporting the arity error where git
    // reports a refspec error — any invocation carrying a `<head>` stops here.
    if !state.refspecs.is_empty() {
        bail!(
            "http-push is not ported: gix-transport's HTTP client exposes only GET and POST \
             (client/blocking_io/http/traits.rs), so none of the WebDAV verbs http-push is \
             built from — PROPFIND, MKCOL, LOCK/UNLOCK, PUT, MOVE — can be issued, and \
             gix-protocol implements no push at all; the <head> arguments are additionally \
             left unvalidated because gix-refspec's push grammar diverges from git's \
             (ported: -h, the usage check, and the -d/-D arity check only)"
        )
    }

    // `if (delete_branch && rc != 1) die(...)` — `rc` is the number of `<head>`
    // arguments, which is zero on this path, so the check always fires.
    if state.delete_branch {
        eprintln!("fatal: You must specify only one branch name when deleting a remote branch");
        return Ok(ExitCode::from(128));
    }

    bail!(
        "http-push is not ported: pushing to {url} needs a WebDAV client (PROPFIND for DAV \
         compliance and the activity collection, MKCOL, LOCK/UNLOCK of info/refs, PUT and \
         MOVE of objects), but gix-transport's `trait Http` declares only `get` and `post` \
         and gix-protocol exports only handshake, ls-refs and fetch (ported: -h, the usage \
         check, and the -d/-D arity check only)"
    )
}

/// The outcome of scanning: either a fully-formed request, or a diagnostic that
/// has already decided the exit code.
enum Scanned {
    Ok(State),
    Exit(ExitCode),
}

/// Walk `args` exactly the way `http-push.c`'s `main` loop walks them.
///
/// Each argument is offered to the option table first; a `-…` argument the table
/// does not know is *not* rejected — it falls through and is treated as a
/// positional, which is why `git http-push -x <url> master` ends up with `-x/`
/// as its URL. The first positional past the URL starts `<head>...` and ends
/// option scanning outright, so `-h` there is a refspec, not a request for help.
fn scan(args: &[String]) -> Scanned {
    let mut st = State::default();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();

        if a.starts_with('-') {
            match a {
                // Accepted and dropped: no pre-flight check consults them.
                "--all" | "--force" | "--dry-run" | "--helper-status" | "--verbose" => {
                    i += 1;
                    continue;
                }
                // `-D` is `-d` plus `force_delete`, which only matters once the
                // remote HEAD is known — i.e. past the network.
                "-d" | "-D" => {
                    st.delete_branch = true;
                    i += 1;
                    continue;
                }
                "-h" => {
                    eprint!("{USAGE}");
                    return Scanned::Exit(ExitCode::from(129));
                }
                // Anything else falls through to the positional handling below.
                _ => {}
            }
        }

        if st.url.is_none() {
            st.url = Some(end_url_with_slash(a));
            i += 1;
            continue;
        }

        // `refspec = argv + i; rc = argc - i; break;`
        st.refspecs = args[i..].to_vec();
        break;
    }

    Scanned::Ok(st)
}

/// git's `str_end_url_with_slash`: the URL is stored with exactly one trailing
/// slash, which is why its diagnostics name `<url>/` rather than `<url>`.
fn end_url_with_slash(url: &str) -> String {
    if url.ends_with('/') {
        url.to_owned()
    } else {
        format!("{url}/")
    }
}
