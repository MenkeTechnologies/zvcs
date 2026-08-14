//! `git fetch-pack` — receive missing objects from another repository.
//!
//! This is the plumbing half of `git fetch`: it talks to `git-upload-pack` on
//! the far side, negotiates, receives a pack, and prints one `<oid> <ref>` line
//! per requested ref on stdout. It deliberately updates **no** local reference
//! and writes no `FETCH_HEAD` — the caller is expected to do that.
//!
//! Covered, byte-for-byte against stock git for the supported flags:
//!   * `git fetch-pack <repository> <refs>...` — each `<ref>` must be the exact
//!     full name the remote advertises (`refs/heads/main`, `HEAD`, …), which is
//!     what stock git requires; `main` and `heads/main` are rejected by git too.
//!   * `--all` — every advertised ref, `HEAD` included.
//!   * `--stdin` — additional ref names, one per line, appended after the ones
//!     given on the command line (the plain form; see below for `--stateless-rpc`).
//!   * `-q`/`--quiet`, `-v`, `--no-progress` — accepted; this port never paints
//!     progress, so they only ever affected stderr.
//!   * `--thin`/`--no-thin` — accepted, see the note on thin packs below.
//!   * `--depth=<n>`, `--shallow-since=<date>`, `--shallow-exclude=<ref>`
//!     (repeatable) and `--deepen-relative` — the shallow-clone family. Each is
//!     mapped onto the vendored `Shallow` request, which puts the same
//!     `deepen`/`deepen-since`/`deepen-not`/`deepen-relative` lines on the wire as
//!     stock git. They only affect negotiation and the `.git/shallow` boundary:
//!     exactly like stock `fetch-pack`, no `shallow`/`unshallow` line is printed
//!     and no ref is written. The one representational limit is that the vendored
//!     `Shallow` enum holds a single variant, so `--shallow-since`/`--shallow-exclude`
//!     (which git can layer under a `--depth`) take precedence over a `--depth`
//!     given in the same invocation rather than being sent alongside it.
//!   * output: `<full-hex-oid> SP <refname> LF`, sorted by refname bytes and
//!     deduplicated, with an annotated tag reported under its *tag* object id
//!     (not the peeled commit), exactly as `upload-pack` advertises it.
//!   * exit codes: 0 on success; 1 when a requested ref is not advertised (after
//!     still fetching the ones that were) and when nothing at all was asked for
//!     or advertised; 128 outside a repository or when the remote is unreachable;
//!     129 for `-h` (usage on stdout) and for a usage error (usage on stderr).
//!   * end state: the received objects are exploded into loose objects and the
//!     intermediate pack is removed, which is what git does below
//!     `fetch.unpackLimit`. No ref, reflog or `FETCH_HEAD` is touched.
//!   * `-k`/`--keep` — the received pack stays in `objects/pack` under a
//!     `.keep`, with the `.rev` git's `index-pack` writes beside it, and the
//!     `keep <hash>` line goes out before the ref listing. `<hash>` is the
//!     pack's trailer checksum, which is what both git and `gix-pack` name a
//!     pack after, so the two agree byte-for-byte as long as the received pack
//!     bytes do. See [`keep_pack`].
//!
//! Not covered — each bails rather than silently diverging:
//!   * a fetch large enough that git would keep the pack on its own
//!     (`fetch.unpackLimit`, default 100) without `--keep` having been asked
//!     for. That is the same kept-pack end state, but reached by a rule this
//!     port has no way to observe from the outside: git decides it from the pack
//!     *header* before reading the body, while the vendored bundle writer only
//!     reports the object count once the pack is already written and indexed.
//!   * `--include-tag`, **unless the want set already covers every advertised
//!     tag** (which `--all` guarantees). The capability only ever *adds* tags
//!     the server would otherwise have held back, so once all of them are
//!     explicit wants it cannot change the object set and is accepted. In every
//!     other shape it is refused: gitoxide expresses "include tags" as an
//!     implicit `refs/tags/*:refs/tags/*` refspec, which *creates local tag refs*
//!     — and `fetch-pack` must write none — while deciding client-side which
//!     tags the server would have attached needs the pack's contents before the
//!     pack exists.
//!   * `--filter=<spec>` — the high-level negotiation never emits the `filter`
//!     packet line (the vendored `Arguments::filter` is only reachable from the
//!     low-level fetch function this port does not drive), so a partial-clone
//!     filter cannot be requested faithfully.
//!   * `--refetch`.
//!   * `--upload-pack=<exec>` / `--exec=<exec>` — the vendored transport `connect`
//!     takes no per-invocation override for the remote program.
//!   * `--diag-url` **on an ssh URL**. The local, `file://` and `git://` forms
//!     are covered: [`parse_connect_url`] is a direct port of `connect.c`'s own
//!     URL splitter, written out rather than delegated to `gix-url`, which
//!     decomposes URLs differently (scp-like `host:path`, port defaulting, path
//!     normalisation) and would not print git's fields. git reaches the ssh
//!     breakdown (`userandhost`/`port` instead of `hostandport`) only after
//!     `transport_check_allowed("ssh")` and the `strange pathname` guard, and
//!     neither has a home in this tree, so that one form bails.
//!   * `--check-self-contained-and-connected`, `--stateless-rpc`, `--lock-pack`.
//!   * a `<ref>` given as a raw object hash (`uploadpack.allowTipSHA1InWant`
//!     and friends): the vendored refspec layer maps names, not bare ids.
//!
//! One deliberate wire-level difference: gitoxide always asks for a thin pack,
//! while stock `fetch-pack` only does so under `--thin`. It does not change the
//! end state — `gix-pack` completes the pack from the local object database
//! while writing it, and the explode step skips every object already present —
//! so both runs leave the same set of loose objects behind.

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::io::BufRead;
use std::num::NonZeroU32;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::hash::ObjectId;
use gix::objs::Write as _;
use gix::protocol::handshake::Ref;
use gix::remote::fetch::{Shallow, Status, Tags};

/// The usage line stock `git fetch-pack` prints, verbatim (one line, then LF).
const USAGE: &str = "usage: git fetch-pack [--all] [--stdin] [--quiet | -q] [--keep | -k] [--thin] [--include-tag] [--upload-pack=<git-upload-pack>] [--depth=<n>] [--no-progress] [--diag-url] [-v] [<host>:]<directory> [<refs>...]\n";

/// The flags this port implements, quoted in every rejection message.
const PORTED: &str = "ported: --all, --stdin, -q/--quiet, -v, --no-progress, --thin/--no-thin, \
                      --depth, --shallow-since, --shallow-exclude, --deepen-relative";

/// git's built-in `unpack_limit`, overridable via `fetch.unpackLimit` and then
/// `transfer.unpackLimit`.
const DEFAULT_UNPACK_LIMIT: i64 = 100;

/// `git fetch-pack` — download the objects needed for the named remote refs.
///
/// See the module docs for the supported flag set and the deliberate gaps.
pub fn fetch_pack(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands over the arguments after the subcommand; tolerate a leading
    // `fetch-pack` in case a caller passes argv unsliced. The token can never be
    // a legal first argument here (it would be read as the repository URL and
    // fail to connect), so dropping it costs no fidelity.
    let args = match args.split_first() {
        Some((first, rest)) if first == "fetch-pack" => rest,
        _ => args,
    };

    // `show_usage_if_asked(argc, argv, fetch_pack_usage)` (builtin/fetch-pack.c:78):
    // a LONE `-h` on stdout at 129. With anything else on the line `-h` is just
    // an unrecognized flag, which the scan below reports on stderr.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }

    // --- argument parsing -------------------------------------------------
    // git stops option parsing at the first non-option, which becomes
    // <repository>; everything after it is a ref name, even if it looks like a
    // flag (`git fetch-pack <url> --all` reports "no such remote ref --all").
    let mut all = false;
    let mut from_stdin = false;
    let mut diag_url = false;
    let mut include_tag = false;
    let mut keep = false;
    let mut dest: Option<&str> = None;
    let mut sought: Vec<String> = Vec::new();
    // The shallow-clone family. git keeps `depth` (`strtol` of `--depth=`),
    // `deepen_relative` (a modifier on `depth`), a `deepen_since` string and a
    // list of `deepen_not` refs, and folds them into the deepen request after
    // parsing; we mirror that, collecting the raw pieces here.
    let mut depth: Option<i64> = None;
    let mut deepen_relative = false;
    let mut shallow_since: Option<String> = None;
    let mut shallow_exclude: Vec<String> = Vec::new();

    for a in args {
        let a = a.as_str();
        if dest.is_some() {
            sought.push(a.to_string());
            continue;
        }
        if !a.starts_with('-') {
            dest = Some(a);
            continue;
        }
        match a {
            "--all" => all = true,
            "--stdin" => from_stdin = true,
            // Progress and verbosity only ever reached stderr, which this port
            // leaves empty on the success path.
            "-q" | "--quiet" | "-v" | "--no-progress" => {}
            // gitoxide always requests a thin pack; the end state is identical
            // either way (see the module docs).
            "--thin" | "--no-thin" => {}
            "-k" | "--keep" => keep = true,
            // `--lock-pack` additionally makes `index-pack` hold a `.keep` lock
            // whose path `cmd_fetch_pack()` prints as `lock <path>` and expects
            // the caller to release; there is no lockfile protocol here to hand
            // that ownership to.
            "--lock-pack" => bail!("unsupported flag {a:?} ({PORTED}, -k/--keep)"),
            "--include-tag" => include_tag = true,
            // `--deepen-relative` is a modifier on `--depth`; git only appends the
            // `deepen-relative` line when a depth is present, so we just record it.
            "--deepen-relative" => deepen_relative = true,
            "--diag-url" => diag_url = true,
            "--refetch" => bail!("unsupported flag {a:?} ({PORTED})"),
            "--check-self-contained-and-connected" | "--stateless-rpc"
            | "--no-filter" => bail!("unsupported flag {a:?} ({PORTED})"),
            // `--depth=<n>` — git does `strtol(arg, NULL, 0)`; a non-numeric value
            // there degrades to 0 (no deepen), but we surface it as an error rather
            // than silently dropping the request.
            _ if a.starts_with("--depth=") => {
                let v = &a["--depth=".len()..];
                depth = Some(
                    v.parse::<i64>()
                        .map_err(|_| anyhow::anyhow!("--depth expects an integer, got {v:?}"))?,
                );
            }
            _ if a.starts_with("--shallow-since=") => {
                shallow_since = Some(a["--shallow-since=".len()..].to_string());
            }
            // Repeatable, exactly like git's `string_list_append(&deepen_not, arg)`.
            _ if a.starts_with("--shallow-exclude=") => {
                shallow_exclude.push(a["--shallow-exclude=".len()..].to_string());
            }
            _ if a.starts_with("--upload-pack=")
                || a.starts_with("--exec=")
                || a.starts_with("--filter=") =>
            {
                let flag = &a[..a.find('=').unwrap_or(a.len())];
                bail!("unsupported flag {flag:?} ({PORTED})")
            }
            // Anything else is a usage error for git: usage on stderr, 129.
            _ => {
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    let Some(dest) = dest else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };

    // `--diag-url` short-circuits everything: `git_connect()` decomposes the URL,
    // prints the breakdown, returns NULL, and `cmd_fetch_pack()` exits 0 without
    // reading stdin, opening a repository or contacting anyone (fetch-pack.c:224).
    if diag_url {
        return diag_connect_url(dest);
    }

    // `--stdin` refs are processed after the ones on the command line.
    if from_stdin {
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            if !line.is_empty() {
                sought.push(line);
            }
        }
    }

    // Nothing asked for at all: git exits 1 without a word.
    if !all && sought.is_empty() {
        return Ok(ExitCode::FAILURE);
    }

    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    // We write objects, so serialize behind the repo coordinator like the other
    // write commands; a no-op guard when no daemon is running.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // --- phase 1: read the whole advertisement ----------------------------
    // `fetch-pack` takes a URL, never a configured remote name, so build the
    // remote from the URL alone: that also guarantees it carries no configured
    // refspecs which could write tracking refs behind our back. `Tags::None`
    // suppresses gitoxide's implicit tag refspec for the same reason.
    let advertised = match list_refs(&repo, dest) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };

    // --- select the refs to report and to want ----------------------------
    let advertised_tags: Vec<String> = advertised
        .iter()
        .filter(|(name, _)| name.starts_with("refs/tags/"))
        .map(|(name, _)| name.clone())
        .collect();
    let mut selected: Vec<(String, ObjectId)> = Vec::new();
    let mut missing = false;
    if all {
        selected = advertised;
    } else {
        let mut seen: HashSet<&str> = HashSet::new();
        for name in &sought {
            match advertised.iter().find(|(n, _)| n == name) {
                Some(row) => {
                    if seen.insert(name.as_str()) {
                        selected.push(row.clone());
                    }
                }
                None => {
                    if looks_like_object_hash(name) {
                        anyhow::bail!(
                            "ref {name:?} looks like an object id — wanting a raw id \
                             (uploadpack.allow*SHA1InWant) has no substrate in the vendored \
                             refspec layer, which maps names only ({PORTED})"
                        );
                    }
                    eprintln!("error: no such remote ref {name}");
                    missing = true;
                }
            }
        }
    }
    selected.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    // `--include-tag` asks `upload-pack` to add, to the pack it was already going
    // to send, the tag objects under `refs/tags/` whose target is in that pack.
    // It can therefore only ever add tags the server is holding — so when every
    // advertised tag is already an explicit `want`, the capability has nothing
    // left to contribute and the received object set is the same either way.
    // That is the case `--all` produces, and it is checked rather than assumed.
    //
    // Anywhere else it is refused. The vendored fetch spells "include tags" as
    // `Tags::Included`, which `Tags::to_refspec()` turns into an implicit
    // `refs/tags/*:refs/tags/*` (gix-protocol/src/fetch/types.rs:270) — that
    // writes local tag refs, and `fetch-pack` must write none. Deciding
    // client-side which tags the server *would* have attached needs the pack's
    // contents before the pack exists; gitoxide's own note on the capability
    // says the same ("we would have to implement another pass to fetch attached
    // tags separately"). Wanting every tag unconditionally instead would
    // download tags unrelated to the fetched objects, which is a different
    // operation, so the flag stops here rather than approximating one.
    if include_tag {
        let selected_names: HashSet<&str> = selected.iter().map(|(n, _)| n.as_str()).collect();
        let unwanted_tags = advertised_tags
            .iter()
            .filter(|name| !selected_names.contains(name.as_str()))
            .count();
        if unwanted_tags > 0 {
            bail!(
                "unsupported flag \"--include-tag\" for a fetch that does not already want every \
                 advertised tag ({unwanted_tags} not wanted here) — gitoxide spells the \
                 capability as an implicit `refs/tags/*:refs/tags/*` refspec, which would create \
                 local tag refs that `fetch-pack` must not write ({PORTED}, and --include-tag \
                 alongside a want set that already covers every tag)"
            );
        }
    }

    // Nothing matched: git never opens a fetch and exits 1.
    if selected.is_empty() {
        return Ok(ExitCode::FAILURE);
    }

    // --- phase 2: negotiate and receive the pack --------------------------
    let shallow = build_shallow(depth, deepen_relative, shallow_since.as_deref(), &shallow_exclude)?;
    if let Err(e) = receive(&repo, dest, &selected, shallow, keep) {
        // A failed fetch surfaces as git's `fatal:` with 128 unless it is one of
        // our own refusals, which must stay loud and unmistakable.
        if let Some(refusal) = e.downcast_ref::<Refusal>() {
            crate::git_fatal!("{}", refusal.0);
        }
        eprintln!("fatal: {e}");
        return Ok(ExitCode::from(128));
    }

    let mut out = String::new();
    for (name, oid) in &selected {
        out.push_str(&format!("{} {name}\n", oid.to_hex()));
    }
    print!("{out}");

    Ok(if missing {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// git's `enum url_scheme` (url.h:26). `Unknown` is the value
/// `parse_connect_url()` dies on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UrlScheme {
    Local,
    File,
    Ssh,
    Git,
    Unknown,
}

impl UrlScheme {
    /// `url_scheme_name()` (connect.c:703) — what `Diag: protocol=` prints.
    fn name(self) -> &'static str {
        match self {
            UrlScheme::Local | UrlScheme::File => "file",
            UrlScheme::Ssh => "ssh",
            UrlScheme::Git => "git",
            UrlScheme::Unknown => "unknown protocol",
        }
    }

    /// `url_get_scheme()` (url.c:144), including the two deprecated spellings.
    fn from_name(name: &str) -> Self {
        match name {
            "ssh" | "git+ssh" | "ssh+git" => UrlScheme::Ssh,
            "git" => UrlScheme::Git,
            "file" => UrlScheme::File,
            _ => UrlScheme::Unknown,
        }
    }
}

/// `--diag-url`: print `git_connect()`'s decomposition of `url` and exit 0.
///
/// Port of the non-SSH arm of `git_connect()` (connect.c:1425-1430):
///
/// ```c
/// scheme = parse_connect_url(url, &hostandport, &path);
/// if ((flags & CONNECT_DIAG_URL) && (scheme != URL_SCHEME_SSH)) {
///         printf("Diag: url=%s\n", url ? url : "NULL");
///         printf("Diag: protocol=%s\n", url_scheme_name(scheme));
///         printf("Diag: hostandport=%s\n", hostandport ? hostandport : "NULL");
///         printf("Diag: path=%s\n", path ? path : "NULL");
///         conn = NULL;
/// }
/// ```
///
/// `url=` echoes the argument as given, not the decoded copy the parser works
/// on, so a percent-encoded URL prints twice in two forms — that is git's
/// output and it is reproduced here.
///
/// An SSH URL takes git's other arm, which prints `userandhost`/`port` instead
/// and reaches it only after `transport_check_allowed("ssh")` and the
/// `strange pathname` guard. Neither has a home in this tree, so that arm bails
/// rather than printing a breakdown git might have refused to produce.
fn diag_connect_url(url: &str) -> Result<ExitCode> {
    let (scheme, hostandport, path) = match parse_connect_url(url) {
        Ok(parts) => parts,
        Err(message) => {
            eprintln!("fatal: {message}");
            return Ok(ExitCode::from(128));
        }
    };
    if scheme == UrlScheme::Ssh {
        bail!(
            "--diag-url on an ssh URL is not ported: git reaches that breakdown \
             (userandhost/port) only after `transport_check_allowed(\"ssh\")` and the \
             `strange pathname` guard, neither of which exists here ({PORTED}, --diag-url \
             for local, file:// and git:// URLs)"
        );
    }
    print!(
        "Diag: url={url}\nDiag: protocol={}\nDiag: hostandport={hostandport}\nDiag: path={path}\n",
        scheme.name()
    );
    Ok(ExitCode::SUCCESS)
}

/// Port of `parse_connect_url()` (connect.c:1054), returning
/// `(scheme, hostandport, path)` or the message git would `die()` with.
///
/// The C walks one mutable buffer with pointers and NUL-terminates it in place;
/// this walks the same buffer with byte indices, which is why the boundaries are
/// named after the C variables (`host`, `end`, `path`) rather than being
/// re-derived.
///
/// Two branches of the C are unreachable on the platforms this tree builds for
/// and are therefore not carried over: both `URL_SCHEME_FILE` special cases turn
/// on `has_dos_drive_prefix()` or on `offset_1st_component(host - 2) > 1`, and
/// POSIX's `offset_1st_component()` returns at most 1 (`is_dir_sep(s[0])`), so a
/// `file://` URL takes the same `strchr(end, separator)` path every other
/// non-local scheme takes.
fn parse_connect_url(url_orig: &str) -> std::result::Result<(UrlScheme, String, String), String> {
    let url = if is_url(url_orig) {
        url_decode(url_orig)
    } else {
        url_orig.to_string()
    };
    let bytes = url.as_bytes();

    let mut scheme = UrlScheme::Local;
    let mut separator = b'/';
    let host = match url.find("://") {
        Some(at) => {
            scheme = UrlScheme::from_name(&url[..at]);
            if scheme == UrlScheme::Unknown {
                return Err(format!("protocol '{}' is not supported", &url[..at]));
            }
            at + 3
        }
        None => {
            if !url_is_local_not_ssh(&url) {
                scheme = UrlScheme::Ssh;
                separator = b':';
            }
            0
        }
    };

    // "Don't do destructive transforms as protocol code does '[]' unwrapping in
    // get_host_and_port()" — hence `removebrackets = 0`.
    let end = host_end(bytes, host);

    let path = if scheme == UrlScheme::Local {
        Some(end)
    } else {
        bytes[end..].iter().position(|&b| b == separator).map(|k| end + k)
    };
    let Some(path) = path.filter(|&p| p < bytes.len()) else {
        return Err("no path specified; see 'git help pull' for valid url syntax".to_string());
    };

    // `end = path` here: the host is terminated where the path begins, whatever
    // the two adjustments below do to the path's own start.
    let host_end_index = path;
    let mut path = path;
    if separator == b':' {
        path += 1; // the path starts after the ':'
    }
    // "null-terminate hostname and point path to ~ for URL's like this:
    //  ssh://host.xz/~user/repo"
    if matches!(scheme, UrlScheme::Git | UrlScheme::Ssh) && bytes.get(path + 1) == Some(&b'~') {
        path += 1;
    }

    Ok((
        scheme,
        url[host..host_end_index].to_string(),
        url[path..].to_string(),
    ))
}

/// `host_end()` (connect.c:718) with `removebrackets = 0`: the index just past
/// the bracketed IPv6 literal a host may start with, or the host's own start.
fn host_end(bytes: &[u8], host: usize) -> usize {
    let rest = &bytes[host..];
    // `strstr(host, "@[")`, then `start++` to jump over the '@'.
    let start = rest
        .windows(2)
        .position(|w| w == b"@[")
        .map_or(0, |at| at + 1);
    if rest.get(start) != Some(&b'[') {
        return host;
    }
    match rest[start + 1..].iter().position(|&b| b == b']') {
        // `end` is left ON the ']' — the `end++` that skips it is inside the
        // `if (removebrackets)` block, which this caller does not take.
        Some(k) => host + start + 1 + k,
        None => host,
    }
}

/// `url_is_local_not_ssh()` (url.c:136). The `has_dos_drive_prefix()` clause is
/// Windows-only and always false here.
fn url_is_local_not_ssh(url: &str) -> bool {
    let colon = url.find(':');
    let slash = url.find('/');
    match (colon, slash) {
        (None, _) => true,
        (Some(c), Some(s)) => s < c,
        (Some(_), None) => false,
    }
}

/// `is_url()` (url.c:34): a `[A-Za-z0-9][A-Za-z0-9+.-]*` scheme followed by
/// `://`. The first character is deliberately allowed to be a digit — git
/// loosened the RFC3986 rule so as not to break existing remote helpers.
fn is_url(url: &str) -> bool {
    let b = url.as_bytes();
    let scheme_char = |first: bool, c: u8| c.is_ascii_alphanumeric() || (!first && matches!(c, b'+' | b'-' | b'.'));
    if b.is_empty() || !scheme_char(true, b[0]) {
        return false;
    }
    let mut i = 1;
    while i < b.len() && b[i] != b':' {
        if !scheme_char(false, b[i]) {
            return false;
        }
        i += 1;
    }
    b.get(i) == Some(&b':') && b.get(i + 1) == Some(&b'/') && b.get(i + 2) == Some(&b'/')
}

/// `url_decode()` (url.c:85) by way of `url_decode_mem`: everything up to the
/// first colon is copied verbatim (it is the scheme), and `%XX` escapes are
/// decoded from the colon on. `+` is NOT turned into a space — that is
/// `decode_plus`, which this call site leaves off.
fn url_decode(url: &str) -> String {
    let b = url.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = match b.iter().position(|&c| c == b':') {
        // `if (colon && url < colon)`: a leading colon is not a scheme.
        Some(at) if at > 0 => {
            out.extend_from_slice(&b[..at]);
            at
        }
        _ => 0,
    };
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            // `hex2chr` rejects a NUL byte and any non-hex digit, and git only
            // takes the escape when the decoded value is strictly positive.
            if let Some(v) = hex2chr(b[i + 1], b[i + 2]).filter(|&v| v > 0) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `hex2chr()`: two hex digits as one byte, `None` when either is not hex.
fn hex2chr(hi: u8, lo: u8) -> Option<u8> {
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    Some(digit(hi)? << 4 | digit(lo)?)
}

/// Every ref the remote advertises, as `(full name, id)` pairs.
///
/// The id is the ref's own target, so an annotated tag reports the tag object
/// rather than the commit it peels to — that is the pair `upload-pack` puts on
/// the wire and what stock `fetch-pack` prints. Unborn refs are skipped: they
/// name no object, and git prints nothing for them.
fn list_refs(repo: &gix::Repository, dest: &str) -> Result<Vec<(String, ObjectId)>> {
    let remote = repo.remote_at(dest)?.with_fetch_tags(Tags::None);
    // With no refspecs configured, the server must not pre-filter by prefix or
    // the listing would come back empty.
    let (ref_map, _handshake) = remote.connect(gix::remote::Direction::Fetch)?.ref_map(
        gix::progress::Discard,
        gix::remote::ref_map::Options {
            prefix_from_spec_as_filter_on_remote: false,
            ..Default::default()
        },
    )?;

    let mut rows = Vec::with_capacity(ref_map.remote_refs.len());
    for r in &ref_map.remote_refs {
        let (name, oid) = match r {
            Ref::Peeled {
                full_ref_name, tag, ..
            } => (full_ref_name, *tag),
            Ref::Direct {
                full_ref_name,
                object,
            } => (full_ref_name, *object),
            Ref::Symbolic {
                full_ref_name,
                tag,
                object,
                ..
            } => (full_ref_name, tag.unwrap_or(*object)),
            Ref::Unborn { .. } => continue,
        };
        rows.push((name.to_string(), oid));
    }
    Ok(rows)
}

/// A refusal raised from inside the fetch, to be reported as an error rather
/// than mistaken for an unreachable remote.
#[derive(Debug)]
struct Refusal(String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// Fold the parsed shallow-clone flags into a single vendored [`Shallow`] request,
/// matching git's deepen wire lines.
///
/// git can layer `--shallow-since`/`--shallow-exclude` under a `--depth`, but the
/// vendored `Shallow` enum is a single variant: `--shallow-exclude` (with an
/// optional `--shallow-since` cutoff) wins over a lone `--shallow-since`, which in
/// turn wins over `--depth`. `--deepen-relative` only takes effect together with a
/// `--depth`, exactly as git only appends its `deepen-relative` line then.
fn build_shallow(
    depth: Option<i64>,
    deepen_relative: bool,
    shallow_since: Option<&str>,
    shallow_exclude: &[String],
) -> Result<Shallow> {
    // `fetch-pack.c:439` runs `--shallow-since` through `approxidate()`, which never fails.
    let parse_date =
        |s: &str| -> Result<gix::date::Time> { Ok(gix::date::Time::new(crate::date::approxidate(s), 0)) };

    if !shallow_exclude.is_empty() {
        let remote_refs = shallow_exclude
            .iter()
            .map(|s| {
                gix::refs::PartialName::try_from(s.as_str())
                    .map_err(|e| anyhow::anyhow!("invalid --shallow-exclude ref {s:?}: {e}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let since_cutoff = shallow_since.map(parse_date).transpose()?;
        return Ok(Shallow::Exclude {
            remote_refs,
            since_cutoff,
        });
    }
    if let Some(s) = shallow_since {
        return Ok(Shallow::Since {
            cutoff: parse_date(s)?,
        });
    }
    if let Some(d) = depth {
        // `--deepen-relative --depth=<n>` deepens the local boundary by `n`
        // (`deepen <n>` + `deepen-relative`); a plain `--depth=<n>` sets the
        // boundary to `n` from the remote tips (`deepen <n>`). git sends no deepen
        // line for a non-positive depth, so a `NonZeroU32` guards that here.
        if deepen_relative {
            return Ok(Shallow::Deepen(d.max(0) as u32));
        }
        if let Some(n) = u32::try_from(d).ok().and_then(NonZeroU32::new) {
            return Ok(Shallow::DepthAtRemote(n));
        }
    }
    Ok(Shallow::NoChange)
}

/// Want exactly `selected`, receive the pack, and either keep it (`--keep`) or
/// explode it into loose objects.
///
/// Each ref is turned into a one-sided fetch refspec (`refs/heads/main` with no
/// destination), which makes it a `want` without producing any ref edit — the
/// property `fetch-pack` depends on.
fn receive(
    repo: &gix::Repository,
    dest: &str,
    selected: &[(String, ObjectId)],
    shallow: Shallow,
    keep: bool,
) -> Result<()> {
    let remote = repo
        .remote_at(dest)?
        .with_fetch_tags(Tags::None)
        .with_refspecs(
            selected.iter().map(|(name, _)| name.as_str()),
            gix::remote::Direction::Fetch,
        )?;

    let should_interrupt = AtomicBool::new(false);
    let outcome = remote
        .connect(gix::remote::Direction::Fetch)?
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )?
        // Deepen exactly as `--depth`/`--shallow-*` asked; `Shallow::NoChange`
        // (the common case) leaves negotiation untouched.
        .with_shallow(shallow)
        .receive(gix::progress::Discard, &should_interrupt)?;

    match outcome.status {
        // Nothing new on the wire — every wanted object is already local.
        Status::NoPackReceived { .. } => Ok(()),
        Status::Change {
            write_pack_bundle, ..
        } if keep => keep_pack(write_pack_bundle),
        Status::Change {
            write_pack_bundle, ..
        } => explode(repo, write_pack_bundle),
    }
}

/// `--keep`: leave the received pack in `objects/pack` under a `.keep`, and
/// print the `keep <hash>` line `index-pack --stdin --keep` prints.
///
/// git reaches this by running `index-pack --stdin --keep=<msg>` instead of
/// `unpack-objects` (`get_pack()`, fetch-pack.c:1007-1018); the child inherits
/// stdout, so its `keep\t<hash>` lands before the ref listing `fetch-pack` prints
/// afterwards. `<hash>` is the pack's own trailer checksum — which is also what
/// the pack is named after, both here and in git — so the two agree whenever the
/// received bytes do.
///
/// The `.keep` message is git's, `fetch-pack <pid> on <hostname>`
/// (`add_index_pack_keep_option()`, fetch-pack.c:947-950). gitoxide writes a
/// `.keep` of its own as a collection guard and expects the caller to deal with
/// it; here it is filled in with git's text rather than removed, which is
/// exactly the file `--keep` is asking for.
///
/// `finalize_object_file()` leaves the pack, its index and its reverse index
/// read-only, so the three are chmod'd to match.
fn keep_pack(bundle: gix::odb::pack::bundle::write::Outcome) -> Result<()> {
    let (Some(index_path), Some(data_path)) = (bundle.index_path.clone(), bundle.data_path.clone())
    else {
        // gitoxide found a pack with these bytes already on disk and reused it,
        // so there is nothing to keep and nothing new to name.
        return Ok(());
    };
    if let Some(keep_path) = bundle.keep_path.clone() {
        std::fs::write(
            &keep_path,
            format!(
                "fetch-pack {} on {}\n",
                std::process::id(),
                super::send_email::hostname()
            ),
        )?;
        // `odb_pack_keep()` creates it `O_CREAT|O_EXCL, 0600`, unlike the three
        // read-only files below.
        let _ = std::fs::set_permissions(&keep_path, std::fs::Permissions::from_mode(0o600));
    }
    // git's `index-pack` writes the reverse index for the pack it just built;
    // `gix-pack` does not, so it is filled in beside the index here.
    if let Some(pack_dir) = data_path.parent() {
        super::index_pack::write_missing_rev_indexes(pack_dir);
    }
    for path in [&data_path, &index_path] {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444));
    }

    println!("keep\t{}", bundle.index.data_hash.to_hex());
    Ok(())
}

/// Turn the freshly written pack into loose objects and remove it, which is what
/// git does whenever the pack stays below `fetch.unpackLimit`.
fn explode(repo: &gix::Repository, bundle: gix::odb::pack::bundle::write::Outcome) -> Result<()> {
    // `keep_path` is `None` only when a pack with this content was already on
    // disk, in which case gitoxide reused it and every object is already
    // reachable — exactly the case git's "already exists, don't unpack" covers.
    let (Some(index_path), Some(data_path), Some(keep_path)) = (
        bundle.index_path.clone(),
        bundle.data_path.clone(),
        bundle.keep_path.clone(),
    ) else {
        return Ok(());
    };

    let num_objects = i64::from(bundle.index.num_objects);
    let limit = unpack_limit(repo);
    if limit > 0 && num_objects >= limit {
        return Err(Refusal(format!(
            "received pack holds {num_objects} objects, at or above the unpack limit of {limit} \
             (fetch.unpackLimit/transfer.unpackLimit), so git would keep it as a `.keep` pack; \
             that end state cannot be reproduced, as git names a kept pack after the hash of its \
             sorted object names and adds a `.rev` file while gix-pack names it after the pack \
             trailer checksum and writes none. The pack is left at {}",
            data_path.display()
        ))
        .into());
    }

    // Move the pack out of `objects/pack` before reading it, so the object
    // database we consult for "do we already have this?" below cannot see it —
    // otherwise every object would look present and nothing would be written.
    let scratch = Scratch::new(repo)?;
    let scratch_index = scratch.path.join("pack.idx");
    let scratch_data = scratch.path.join("pack.pack");
    std::fs::rename(&data_path, &scratch_data)?;
    std::fs::rename(&index_path, &scratch_index)?;
    std::fs::remove_file(&keep_path)?;

    // A repository opened now indexes the pre-fetch object set only.
    let before = gix::open(repo.git_dir())?;
    let bundle = gix::odb::pack::Bundle::at(&scratch_index, before.object_hash())?;

    let mut buf = Vec::with_capacity(64 * 1024);
    let mut inflate = gix::zlib::Inflate::default();
    let mut cache = gix::odb::pack::cache::Never;

    for idx in 0..bundle.index.num_objects() {
        let id = bundle.index.oid_at_index(idx).to_owned();
        // Resolving through the index reconstructs `OFS_DELTA`/`REF_DELTA`
        // chains, including thin-pack bases gix-pack appended while writing.
        let (object, _location) = bundle.get_object_by_index(idx, &mut buf, &mut inflate, &mut cache)?;
        // Skips ids the object database already holds, which is git's
        // "objects that already exist are not unpacked".
        before
            .write_buf_with_known_id(object.kind, object.data, id)
            .map_err(|e| anyhow::anyhow!(e))?;
    }

    Ok(())
}

/// git's unpack limit: `fetch.unpackLimit`, then `transfer.unpackLimit`, then
/// the built-in 100. A value of zero or less disables the check entirely.
fn unpack_limit(repo: &gix::Repository) -> i64 {
    let config = repo.config_snapshot();
    config
        .integer("fetch.unpackLimit")
        .or_else(|| config.integer("transfer.unpackLimit"))
        .unwrap_or(DEFAULT_UNPACK_LIMIT)
}

/// Whether `name` is a full object id rather than a ref name, using the same
/// "all hex, at least as long as the shortest hash" test git's refspec parser
/// applies.
fn looks_like_object_hash(name: &str) -> bool {
    name.len() >= gix::hash::Kind::shortest().len_in_hex()
        && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A scratch directory under the git dir, removed on drop so the intermediate
/// pack never survives an early return. It lives beside `objects/pack` so the
/// renames stay on one filesystem.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(repo: &gix::Repository) -> Result<Self> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = repo
            .git_dir()
            .join(format!("zvcs-fetch-pack-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Scratch { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_connect_url, UrlScheme};

    /// Every expectation below was read off stock git 2.55.0's own
    /// `git fetch-pack --diag-url <url>`, which prints exactly the three values
    /// `parse_connect_url()` returns. They are the cases where the C walks its
    /// buffer with pointer arithmetic this port had to re-express as indices:
    /// the empty host of a local path, the `//` a `file://` URL leaves behind,
    /// the `~user` rewind, the `:` separator of an scp-like address, and the
    /// bracketed IPv6 literal `host_end()` exists for.
    #[test]
    fn parse_connect_url_matches_gits_diag_url() {
        let cases: &[(&str, UrlScheme, &str, &str)] = &[
            // A bare local path: the host is empty and the whole string is the path.
            (".", UrlScheme::Local, "", "."),
            ("/tmp/repo", UrlScheme::Local, "", "/tmp/repo"),
            // A local path whose *later* component holds a colon stays local,
            // because `url_is_local_not_ssh()` sees the slash first.
            ("./sub:dir/repo", UrlScheme::Local, "", "./sub:dir/repo"),
            // `file://` with an empty authority.
            ("file:///tmp/repo", UrlScheme::File, "", "/tmp/repo"),
            // Percent-decoding runs before the split, and only past the scheme.
            ("file://%2Ftmp/repo", UrlScheme::File, "", "/tmp/repo"),
            ("git://host.xz/repo.git", UrlScheme::Git, "host.xz", "/repo.git"),
            // The port stays with the host; `~user` pulls the path start back
            // onto the tilde.
            (
                "git://host.xz:9418/~user/repo.git",
                UrlScheme::Git,
                "host.xz:9418",
                "~user/repo.git",
            ),
            // `host_end()`: the path search starts after the ']' so a colon
            // inside an IPv6 literal is not mistaken for the port separator.
            ("git://[::1]/repo.git", UrlScheme::Git, "[::1]", "/repo.git"),
            ("git://[::1]:9418/repo.git", UrlScheme::Git, "[::1]:9418", "/repo.git"),
            (
                "git://user@[::1]:9418/repo.git",
                UrlScheme::Git,
                "user@[::1]:9418",
                "/repo.git",
            ),
            // scp-like: no scheme, a colon before any slash, so the separator
            // becomes ':' and the path starts after it.
            ("host.xz:repo.git", UrlScheme::Ssh, "host.xz", "repo.git"),
            ("ssh://host.xz/repo.git", UrlScheme::Ssh, "host.xz", "/repo.git"),
        ];
        for (url, scheme, host, path) in cases {
            let (got_scheme, got_host, got_path) =
                parse_connect_url(url).unwrap_or_else(|e| panic!("{url}: {e}"));
            assert_eq!(got_scheme.name(), scheme.name(), "scheme for {url}");
            assert_eq!(got_host, *host, "hostandport for {url}");
            assert_eq!(got_path, *path, "path for {url}");
        }
    }

    /// The two `die()`s `parse_connect_url()` can reach, worded as git words them.
    #[test]
    fn parse_connect_url_rejects_what_git_rejects() {
        assert_eq!(
            parse_connect_url("git://host.xz").unwrap_err(),
            "no path specified; see 'git help pull' for valid url syntax"
        );
        assert_eq!(
            parse_connect_url("bogus://host/x").unwrap_err(),
            "protocol 'bogus' is not supported"
        );
    }
}
