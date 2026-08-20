//! `git upload-pack` — the *server* half of `git fetch`.
//!
//! `upload-pack` is invoked by `git fetch-pack` over a transport. It writes a ref
//! advertisement, reads `want`/`have` negotiation from stdin, and streams a
//! generated pack back. The serving path (`serve`) is implemented for the
//! bidirectional v0 protocol: the `write_v0_ref()` capability advertisement with
//! the `symref=HEAD:<target>` hint, the want/have loop in either acknowledgement
//! dialect (see [`MultiAck`]), and a side-banded pack built
//! from the negotiated closure (`push_proto::objects_to_send` +
//! `pack_objects::pack_bytes_for`). Enough for a local (`file://`) or ssh
//! clone/fetch served by this binary. The want policy of `receive_needs()` is
//! enforced: a `want` outside the advertised ref tips is refused with
//! `ERR upload-pack: not our ref <oid>` and exit 128 unless
//! `uploadpack.allowTipSHA1InWant`, `uploadpack.allowReachableSHA1InWant` or
//! `uploadpack.allowAnySHA1InWant` widens it (see [`WantPolicy`]).
//! Object filters *are* negotiated when `uploadpack.allowFilter` is on: the
//! `filter` capability is advertised, a `filter <spec>` line is honoured by
//! leaving the filtered objects out of the pack, and a spec that
//! `uploadpackfilter.*` bans — or that this server cannot apply — is refused
//! with an `ERR` pkt-line (see [`filter_ban_reason`]). Shallow/deepen is still
//! not negotiated (ignored). `--stateless-rpc` (the smart-HTTP POST half)
//! suppresses the advertisement, which belongs to the separate
//! `--advertise-refs` request, and relaxes the want policy to a reachability
//! check. Argument parsing and repository resolution are checked against git
//! 2.55.0 on Darwin:
//!
//!   * `-h` → the 368-byte usage block on **stdout**, exit 129, before any
//!     repository is touched (so it works outside a repository).
//!   * `--help-all` → the 532-byte block on **stdout**, exit 129: the same
//!     table with the two hidden entries (`--http-backend-info-refs` and its
//!     `--advertise-refs` alias) left in, each on its own line.
//!   * no `<directory>`, or more than one → the 458-byte usage block (a third
//!     variant, which lists only the `--advertise-refs` alias and renders it
//!     with the `...` argument marker) on **stderr**, exit 129.
//!   * an unknown long option → ``error: unknown option `<name>'`` followed by
//!     the short usage block, both on **stderr**, exit 129.
//!   * an unknown short switch → ``error: unknown switch `<c>'`` followed by the
//!     short usage block, both on **stderr**, exit 129.
//!   * an ambiguous abbreviation → ``error: ambiguous option: <name> (could be
//!     --<a> or --<b>)`` on **stderr** with the short usage block on **stdout**,
//!     exit 129. The split across the two streams is git's, not a mistake here.
//!   * `--timeout` diagnostics — missing value, non-numeric value, empty value,
//!     and out-of-`i32`-range value — each on **stderr** with no usage block,
//!     exit 129.
//!   * `--strict=<v>` and friends → ``error: option `<name>' takes no value``,
//!     exit 129.
//!   * `<directory>` that resolves to no repository → `fatal: '<directory>' does
//!     not appear to be a git repository` on **stderr**, exit 128, quoting the
//!     argument exactly as given.
//!
//! Protocol **v2** is served as well: `GIT_PROTOCOL=version=2` selects the
//! `serve.c` capability advertisement and command loop instead of the v0
//! advertisement, with the `ls-refs`, `fetch` and `object-info` commands (see
//! [`serve_v2`]). Protocol v1 is v0 preceded by a `version 1` pkt-line, as
//! `builtin/upload-pack.c` has it. Which v2 capabilities are advertised is
//! documented on [`V2Config`]; `packfile-uris` is deliberately withheld because
//! the honouring code for it is not written.
//!
//! Shallow clients are served on both protocols. `shallow`, `deepen-since`,
//! `deepen-not` and `deepen-relative` are advertised, the client's `shallow` and
//! `deepen*` lines are parsed, and [`crate::shallow_serve`] computes the boundary
//! they describe: the `shallow`/`unshallow` lines go back before the have loop on
//! v0 and in the `shallow-info` section on v2, and the pack is cut to the window
//! so nothing behind the boundary is sent. A repository that is itself shallow
//! contributes its own grafts to that boundary and, with no deepen request in
//! play, still reports them in the advertisement (upload-pack.c:1438).
//!
//! What is *not* served, and why:
//!
//!   1. **The pack is built in one piece, not streamed.**
//!      `pack_objects::pack_bytes_for` returns a finished `Vec<u8>` before a
//!      byte goes out, so there is no silent producer to interleave keepalives
//!      with (`uploadpack.keepAlive`, upload-pack.c:382-498) and no progress on
//!      band 2. It is also built in-process, so there is no `pack-objects`
//!      argument vector to hand to `uploadpack.packObjectsHook` — which would
//!      additionally need the protected-config scope that keeps a cloned
//!      repository's own `.git/config` from running commands on the serving
//!      machine (upload-pack.c:1387, `git_protected_config`). Both keys are
//!      therefore unread rather than half-honoured. The pack is also never
//!      thin.
//!
//! These paths are deliberately not approximated. An `upload-pack` that exited 0
//! having written a plausible-looking but wrong advertisement would corrupt the
//! fetch of whoever ran it, while looking like a success to a harness that
//! compares exit codes.

use anyhow::{bail, Result};
use gix::ObjectId;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// The usage block git prints for `-h` and for option errors: 368 bytes, with
/// the hidden `--advertise-refs` alias omitted.
const USAGE_SHORT: &str = concat!(
    "usage: git-upload-pack [--[no-]strict] [--timeout=<n>] [--stateless-rpc]\n",
    "                       [--advertise-refs] <directory>\n",
    "\n",
    "    --[no-]stateless-rpc  quit after a single request/response exchange\n",
    "    --[no-]strict         do not try <directory>/.git/ if <directory> is no Git directory\n",
    "    --[no-]timeout <n>    interrupt transfer after <n> seconds of inactivity\n",
    "\n",
);

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE_SHORT`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]http-backend-info-refs`, `--[no-]advertise-refs`.
/// Captured byte-for-byte from stock git 2.55.0's `git upload-pack --help-all`.
const USAGE_HELP_ALL: &str = r#"usage: git-upload-pack [--[no-]strict] [--timeout=<n>] [--stateless-rpc]
                       [--advertise-refs] <directory>

    --[no-]stateless-rpc  quit after a single request/response exchange
    --[no-]http-backend-info-refs
                          serve up the info/refs for git-http-backend
    --[no-]advertise-refs alias of --http-backend-info-refs
    --[no-]strict         do not try <directory>/.git/ if <directory> is no Git directory
    --[no-]timeout <n>    interrupt transfer after <n> seconds of inactivity

"#;

/// The usage block git prints when the `<directory>` count is wrong: 458 bytes.
/// This is the explicit `usage_with_options()` call in the command itself, which
/// unlike the `-h`/error path also lists the hidden `--advertise-refs` alias.
const USAGE_FULL: &str = concat!(
    "usage: git-upload-pack [--[no-]strict] [--timeout=<n>] [--stateless-rpc]\n",
    "                       [--advertise-refs] <directory>\n",
    "\n",
    "    --[no-]stateless-rpc  quit after a single request/response exchange\n",
    "    --[no-]advertise-refs ...\n",
    "                          alias of --http-backend-info-refs\n",
    "    --[no-]strict         do not try <directory>/.git/ if <directory> is no Git directory\n",
    "    --[no-]timeout <n>    interrupt transfer after <n> seconds of inactivity\n",
    "\n",
);

/// The long options, in the order they appear in git's option table. The order
/// is load-bearing: ambiguous abbreviations are reported against it.
const LONG_OPTS: [&str; 5] = [
    "stateless-rpc",
    "http-backend-info-refs",
    "advertise-refs",
    "strict",
    "timeout",
];

/// Index into [`LONG_OPTS`] for `--strict`, the only option this module acts on.
const OPT_STRICT: usize = 3;

/// Index into [`LONG_OPTS`] for the only option that takes a value.
const OPT_TIMEOUT: usize = 4;

/// The suffixes git's integer parser accepts, and the factor each applies.
const MAGNITUDES: [(char, i128); 6] = [
    ('k', 1024),
    ('K', 1024),
    ('m', 1024 * 1024),
    ('M', 1024 * 1024),
    ('g', 1024 * 1024 * 1024),
    ('G', 1024 * 1024 * 1024),
];

/// One resolved long option: which entry of [`LONG_OPTS`], and whether it was
/// spelled with the `no-` prefix.
#[derive(Clone, Copy)]
struct Resolved {
    index: usize,
    negated: bool,
}

impl Resolved {
    /// The canonical spelling git uses when naming this option in a diagnostic,
    /// i.e. the full long name including any `no-` prefix, without dashes.
    fn name(self) -> String {
        if self.negated {
            format!("no-{}", LONG_OPTS[self.index])
        } else {
            LONG_OPTS[self.index].to_owned()
        }
    }
}

/// `git upload-pack` — argument parsing and repository resolution only; serving
/// the fetch protocol is not ported.
///
/// See the module documentation for the exact set of invocations reproduced
/// byte-for-byte, and for the substrate the rest would need.
pub fn upload_pack(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `upload-pack`'s only positional is
    // `<directory>`, so a leading literal verb is unambiguous only as the verb;
    // strip exactly one. Both spellings git installs are accepted.
    let args = match args.first().map(String::as_str) {
        Some("upload-pack" | "git-upload-pack") => &args[1..],
        _ => args,
    };

    let mut strict = false;
    let mut directories: Vec<&str> = Vec::new();
    let mut end_of_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();

        if end_of_opts {
            directories.push(a);
            i += 1;
            continue;
        }

        if a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): an exact match tested after the `--` break
        // above and before `resolve_long()`, so it neither abbreviates nor takes
        // an `=<value>`. `USAGE_FULL` here is not [`USAGE_FULL`] the constant:
        // that one is `usage_with_options()`'s rendering of the *original*
        // option array for the argument-count error, whose alias slot still
        // prints `...`.
        if a == "--help-all" {
            print!("{USAGE_HELP_ALL}");
            return Ok(ExitCode::from(129));
        }

        // A long option, possibly abbreviated, possibly `--name=value`.
        if let Some(body) = a.strip_prefix("--") {
            let (name, inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (body, None),
            };

            let opt = match resolve_long(name) {
                Ok(opt) => opt,
                Err(LongError::Unknown) => {
                    eprint!("error: unknown option `{name}'\n{USAGE_SHORT}");
                    return Ok(ExitCode::from(129));
                }
                // git splits this one across the streams: the diagnostic on
                // stderr, the usage block on stdout.
                Err(LongError::Ambiguous(first, second)) => {
                    eprintln!(
                        "error: ambiguous option: {name} (could be --{first} or --{second})"
                    );
                    print!("{USAGE_SHORT}");
                    return Ok(ExitCode::from(129));
                }
            };

            // Only `--timeout` takes a value, and its negated form does not.
            if opt.index != OPT_TIMEOUT || opt.negated {
                if inline.is_some() {
                    eprintln!("error: option `{}' takes no value", opt.name());
                    return Ok(ExitCode::from(129));
                }
                if opt.index == OPT_STRICT {
                    strict = !opt.negated;
                }
                i += 1;
                continue;
            }

            let value = match inline {
                Some(v) => v,
                None => match args.get(i + 1) {
                    Some(v) => {
                        i += 1;
                        v.as_str()
                    }
                    None => {
                        eprintln!("error: option `{}' requires a value", opt.name());
                        return Ok(ExitCode::from(129));
                    }
                },
            };
            // The value is parsed for its diagnostics only; nothing here can
            // time out, because nothing here reads from the transport.
            if let Err(msg) = parse_timeout(value, &opt.name()) {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            i += 1;
            continue;
        }

        // A short switch cluster. `upload-pack` defines none of its own, so the
        // only one that resolves is parse-options' built-in `-h`.
        // Either way the first letter of the cluster decides, so the rest is
        // never reached.
        if let Some(c) = a.strip_prefix('-').and_then(|s| s.chars().next()) {
            if c == 'h' {
                print!("{USAGE_SHORT}");
                return Ok(ExitCode::from(129));
            }
            eprint!("error: unknown switch `{c}'\n{USAGE_SHORT}");
            return Ok(ExitCode::from(129));
        }

        directories.push(a);
        i += 1;
    }

    // Exactly one `<directory>` is required; anything else is the command's own
    // `usage_with_options()` call, which prints the longer block to stderr.
    if directories.len() != 1 {
        eprint!("{USAGE_FULL}");
        return Ok(ExitCode::from(129));
    }
    let directory = directories[0];

    // Resolution mirrors git's `enter_repo()`: `~` is expanded, and unless
    // `--strict` was given the suffix list is tried in order, so a worktree wins
    // over a sibling bare repository of the same name.
    let expanded = match expand_tilde(directory) {
        Ok(p) => p,
        Err(msg) => crate::git_fatal!("{msg}"),
    };
    let candidates: Vec<PathBuf> = if strict {
        vec![expanded]
    } else {
        let base = expanded.as_os_str().to_owned();
        ["/.git", "", ".git/.git", ".git"]
            .iter()
            .map(|suffix| {
                let mut p = base.clone();
                p.push(suffix);
                PathBuf::from(p)
            })
            .collect()
    };

    // `open_path_as_is` keeps gix from silently appending `/.git` itself, which
    // would make `--strict` accept a worktree root that git rejects.
    let options = gix::open::Options::default().open_path_as_is(true);
    let repo = candidates
        .into_iter()
        .find_map(|c| gix::open_opts(c, options.clone()).ok());

    let Some(mut repo) = repo else {
        eprintln!("fatal: '{directory}' does not appear to be a git repository");
        return Ok(ExitCode::from(128));
    };

    // `GIT_NAMESPACE` applies here and at almost nowhere else — upload-pack is one
    // of the three programs git namespaces (see [`crate::namespace`]). Installing
    // it on the ref store reproduces all of git's namespace behaviour in this
    // command at once: `for_each_namespaced_ref_1()`'s
    // `opts.namespace = get_git_namespace()` restricts `advertisement()`'s
    // iteration to the namespace (`upload-pack.c:892`), `strip_namespace()` takes
    // the prefix back off each advertised name (`upload-pack.c:1200,1220`), and
    // `refs_head_ref_namespaced()` makes `repo.head()` resolve
    // `refs/namespaces/<ns>/HEAD` (`upload-pack.c:1090-1107`, `refs.c:1053`).
    //
    // Refs outside the namespace disappear from the advertisement as a direct
    // consequence, which is the point: "git-upload-pack and git-receive-pack will
    // ignore all references outside the specified namespace"
    // (`Documentation/gitnamespaces.adoc`).
    crate::namespace::apply(&mut repo)?;
    let repo = repo;

    // `--advertise-refs`/`--http-backend-info-refs` write the advertisement and exit
    // (the smart-HTTP info/refs half); the bidirectional local/ssh path negotiates.
    let advertise_only = args
        .iter()
        .any(|a| a == "--advertise-refs" || a == "--http-backend-info-refs");
    // git's want policy differs under `--stateless-rpc`: a smart-HTTP client may
    // have chosen its wants from an advertisement a *different* process wrote, so
    // a non-tip want is checked for reachability instead of refused outright.
    let stateless_rpc = args.iter().any(|a| a == "--stateless-rpc");

    // `cmd_upload_pack()`'s version switch (builtin/upload-pack.c:63-81). v1 is
    // v0 with a `version 1` pkt-line in front of the advertisement, and that
    // line is written on exactly the paths that go on to write one.
    match protocol_version_from_env() {
        2 => {
            if advertise_only {
                let mut out = std::io::stdout().lock();
                out.write_all(&v2_advertisement(&repo)?)?;
                out.flush()?;
                Ok(ExitCode::SUCCESS)
            } else {
                serve_v2(&repo, stateless_rpc)
            }
        }
        1 => {
            if advertise_only || !stateless_rpc {
                let mut out = std::io::stdout().lock();
                write_pkt(&mut out, b"version 1\n")?;
                out.flush()?;
            }
            serve(&repo, advertise_only, stateless_rpc)
        }
        _ => serve(&repo, advertise_only, stateless_rpc),
    }
}

/// `determine_protocol_version_server()` (protocol.c:49-84): the greatest
/// `version=<n>` the client listed in `GIT_PROTOCOL`, which is a `:`-separated
/// key list. An unparseable or absent value means v0.
fn protocol_version_from_env() -> u8 {
    match std::env::var("GIT_PROTOCOL") {
        Ok(value) => protocol_version_of(&value),
        Err(_) => 0,
    }
}

/// The parse behind [`protocol_version_from_env`], split out so it can be
/// exercised without touching this process's environment.
fn protocol_version_of(value: &str) -> u8 {
    value
        .split(':')
        .filter_map(|item| item.strip_prefix("version="))
        .filter_map(|v| match v {
            "0" => Some(0),
            "1" => Some(1),
            "2" => Some(2),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Serving half: advertise refs, negotiate want/have, stream the pack.
// ---------------------------------------------------------------------------

/// Serve a fetch/clone: write the ref advertisement, run the `want`/`have` v0
/// negotiation, then build and stream the pack of everything the client wants and
/// does not already have. Enough for a local (`file://`) or ssh clone/fetch served
/// by this binary. `advertise_only` (smart-HTTP info/refs) writes the advertisement
/// and stops. Shallow/deepen and object filters are not negotiated (ignored).
fn serve(repo: &gix::Repository, advertise_only: bool, stateless_rpc: bool) -> Result<ExitCode> {
    // Every failure inside `upload-pack` is a `die()` in git: a truncated
    // request stream, a bad pkt-line length, a filter the policy bans. All of
    // them print `fatal: <reason>` and exit 128, so the error must not escape to
    // the dispatcher — which would report it as a zvcs-level failure with exit 1
    // and tell a client waiting on the far end nothing useful about the code.
    match serve_inner(repo, advertise_only, stateless_rpc) {
        Ok(code) => Ok(code),
        Err(err) => {
            eprintln!("fatal: {err}");
            Ok(ExitCode::from(128))
        }
    }
}

/// The body of [`serve`], written the way git writes it: anything that goes
/// wrong is an error return, and the caller turns that into git's `die`.
fn serve_inner(repo: &gix::Repository, advertise_only: bool, stateless_rpc: bool) -> Result<ExitCode> {
    let policy = WantPolicy::from_config(repo);
    // `if (advertise_refs || !data.stateless_rpc)` (upload-pack.c:1418): a
    // smart-HTTP POST body starts at the `want` lines, so writing an
    // advertisement there would corrupt the response.
    if advertise_only || !stateless_rpc {
        // `if (advertise_refs) data.no_done = 1;` (upload-pack.c:1420-1421).
        let adv = advertisement(repo, policy, advertise_only)?;
        let mut out = std::io::stdout().lock();
        out.write_all(&adv)?;
        out.flush()?;
    }
    if advertise_only {
        return Ok(ExitCode::SUCCESS);
    }

    let mut stdin = std::io::stdin().lock();

    // --- wants (until flush); the first `want` carries the client's caps ---------
    let mut wants: Vec<ObjectId> = Vec::new();
    let mut want_caps: Vec<u8> = Vec::new();
    // `data->filter_capability_requested` and `data->filter_options`. The first
    // `want` carries the client's caps, and only a client that asked for
    // `filter` there may send a `filter` line (upload-pack.c:1143-1145).
    let mut filter_requested = false;
    let mut filter_spec: Option<String> = None;
    // `data->shallows` and `data->depth`/`deepen_*` — collected here, answered
    // after the flush that ends the want section.
    let mut shallow_req = crate::shallow_serve::Request::default();
    while let Some(line) = read_pkt(&mut stdin)? {
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end();
        if let Some(rest) = text.strip_prefix("want ") {
            let mut it = rest.splitn(2, ' ');
            if let Some(hex) = it.next() {
                if let Ok(id) = ObjectId::from_hex(hex.as_bytes()) {
                    wants.push(id);
                }
            }
            if want_caps.is_empty() {
                if let Some(caps) = it.next() {
                    want_caps = caps.as_bytes().to_vec();
                    filter_requested = policy.filter && cap_present(&want_caps, b"filter");
                }
            }
            continue;
        }
        // `filter <spec>` (upload-pack.c:1109-1116).
        if let Some(spec) = text.strip_prefix("filter ") {
            if !filter_requested {
                crate::git_fatal!("git upload-pack: filtering capability not negotiated");
            }
            // `list_objects_filter_die_if_populated()`.
            if filter_spec.is_some() {
                crate::git_fatal!("multiple filter-specs cannot be combined");
            }
            if let Some(err) = filter_ban_reason(repo, spec) {
                // `send_err_and_die()`: the reason reaches the client as an
                // `ERR` pkt-line before the command dies.
                let mut out = std::io::stdout().lock();
                write_pkt(&mut out, format!("ERR {err}").as_bytes())?;
                out.flush()?;
                crate::git_fatal!("{err}");
            }
            filter_spec = Some(spec.to_string());
            continue;
        }
        // `shallow <oid>` and the four `deepen*` tokens (upload-pack.c:1046-1104).
        match shallow_req.absorb(text) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(message) => {
                let mut out = std::io::stdout().lock();
                write_pkt(&mut out, format!("ERR {message}").as_bytes())?;
                out.flush()?;
                crate::git_fatal!("{message}");
            }
        }
    }
    if wants.is_empty() {
        return Ok(ExitCode::SUCCESS); // client hung up after the advertisement (ls-remote)
    }

    // `receive_needs`: every `want` must be one this server is willing to serve.
    if let Some(refused) = policy.refuse(repo, &wants, stateless_rpc)? {
        let mut out = std::io::stdout().lock();
        // git reports the refusal twice: an `ERR` pkt-line so the client sees a
        // protocol-level reason, and the same text on stderr for the server log.
        write_pkt(&mut out, format!("ERR upload-pack: not our ref {refused}").as_bytes())?;
        out.flush()?;
        eprintln!("error: git upload-pack: not our ref {refused}");
        return Ok(ExitCode::from(128));
    }

    // `if (data->depth > 0 || data->deepen_rev_list)` (upload-pack.c:1106-1119):
    // a deepening request is answered with the new boundary and a flush, right
    // here, before the have loop starts. A request carrying only `shallow` lines
    // and no `deepen*` says nothing more than where the client's history stops —
    // git registers those as grafts and writes nothing back, and so does this: the
    // walk below stops at them through `client_side_commits`.
    let deepening = shallow_req.deepen.requested();
    let boundary = deepening.then(|| crate::shallow_serve::compute(repo, &wants, &shallow_req));
    if let Some(boundary) = &boundary {
        let mut out = std::io::stdout().lock();
        for id in &boundary.shallow {
            write_pkt(&mut out, format!("shallow {}\n", id.to_hex()).as_bytes())?;
        }
        for id in &boundary.unshallow {
            write_pkt(&mut out, format!("unshallow {}\n", id.to_hex()).as_bytes())?;
        }
        out.write_all(b"0000")?;
        out.flush()?;
    }

    // Which acknowledgement dialect the client picked, and whether it wants to be
    // allowed to skip its final `done` (upload-pack.c:1125-1130).
    let multi_ack = MultiAck::from_caps(&want_caps);
    let no_done = cap_present(&want_caps, b"no-done");
    // git's `sent_ready`, set when it answers `ACK <oid> ready` because
    // `ok_to_give_up()` found the wants already reachable from the haves. This
    // server has no such early cut-off — it negotiates to the client's `done` and
    // then packs — so nothing sets it, and the `no-done` shortcut below is
    // consequently never taken. That is a shortcut the *server* may take, so a
    // client that asked for `no-done` and is not offered it simply sends its
    // `done` as usual; the capability stays truthful either way.
    let sent_ready = false;

    // --- haves until `done`, ACK/NAK per round -----------------------------------
    // Every `have` this server can answer, in arrival order; its last element is
    // git's `last_hex`.
    let mut common: Vec<ObjectId> = Vec::new();
    // `got_oid()`'s two pieces of state (upload-pack.c:551-577). `THEY_HAVE` is
    // set on each accepted have *and on its parents*, and `have_obj` only grows
    // for a have that did not already carry the flag. The distinction is what
    // makes `data->have_obj.nr == 1` stay true across a run of haves that walk
    // straight down one ancestry — which is the condition the bare `ACK` is
    // gated on when no multi-ack dialect was negotiated.
    let mut they_have: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();
    let mut have_obj_nr = 0usize;
    // Whether the negotiation already closed with its final acknowledgement, as
    // the `no-done` shortcut does when it takes the pack path early.
    let mut acked = false;
    let mut out = std::io::stdout().lock();
    loop {
        match read_pkt(&mut stdin)? {
            // A flush ends a have batch without `done`. `get_common_commits`
            // (upload-pack.c:587-606) answers NAK when nothing is common yet or
            // when a multi-ack dialect is in force, then reads the next batch.
            None => {
                if have_obj_nr == 0 || multi_ack != MultiAck::None {
                    write_pkt(&mut out, b"NAK\n")?;
                    out.flush()?;
                }
                // `if (data->no_done && sent_ready)` (upload-pack.c:598-601):
                // acknowledge and go straight to the pack without waiting for
                // the client's `done`.
                if no_done && sent_ready {
                    if let Some(id) = common.last() {
                        write_pkt(&mut out, format!("ACK {}\n", id.to_hex()).as_bytes())?;
                        out.flush()?;
                    }
                    acked = true;
                    break;
                }
            }
            Some(line) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end();
                if text == "done" {
                    break;
                }
                if let Some(hex) = text.strip_prefix("have ") {
                    if let Ok(id) = ObjectId::from_hex(hex.as_bytes()) {
                        if repo.find_object(id).is_ok() {
                            // `got_oid()`: mark the commit and its parents, and
                            // count it only if it was not already marked.
                            if they_have.insert(id) {
                                have_obj_nr += 1;
                            }
                            if let Ok(commit) = repo.find_commit(id) {
                                for parent in commit.parent_ids() {
                                    they_have.insert(parent.detach());
                                }
                            }
                            common.push(id);
                            let hex = id.to_hex();
                            // The `default:` arm of `get_common_commits`
                            // (upload-pack.c:622-631): `common` under
                            // multi_ack_detailed, `continue` under multi_ack, and
                            // a bare ACK while `have_obj.nr` is still 1 when
                            // neither was negotiated.
                            let ack = match multi_ack {
                                MultiAck::Detailed => Some(format!("ACK {hex} common\n")),
                                MultiAck::Plain => Some(format!("ACK {hex} continue\n")),
                                MultiAck::None if have_obj_nr == 1 => Some(format!("ACK {hex}\n")),
                                MultiAck::None => None,
                            };
                            if let Some(ack) = ack {
                                write_pkt(&mut out, ack.as_bytes())?;
                                out.flush()?;
                            }
                        }
                    }
                }
            }
        }
    }
    // `if (!strcmp(reader->line, "done"))` (upload-pack.c:635-642): the final ACK
    // names the last common object and is sent only under a multi-ack dialect —
    // without one the per-have ACK above was already the answer.
    if !acked {
        match common.last() {
            Some(id) if multi_ack != MultiAck::None => {
                write_pkt(&mut out, format!("ACK {}\n", id.to_hex()).as_bytes())?
            }
            Some(_) => {}
            None => write_pkt(&mut out, b"NAK\n")?,
        }
        out.flush()?;
    }

    // --- build + stream the pack -------------------------------------------------
    // Under a deepening request the pack is the window, not the full ancestry:
    // sending anything behind the boundary would contradict the `shallow` lines
    // just written.
    let mut objects = match &boundary {
        Some(boundary) => crate::shallow_serve::objects_within(
            repo,
            &wants,
            &boundary.commits,
            &common,
            &shallow_req.client_shallow,
        ),
        // `register_shallow()` for each `shallow` line (upload-pack.c:1117-1119):
        // the client's cutoff becomes a graft for this walk too, so a plain fetch
        // into a shallow clone packs the new tips without reaching behind it.
        None if !shallow_req.client_shallow.is_empty() => {
            let window =
                crate::shallow_serve::client_side_commits(repo, &wants, &shallow_req.client_shallow);
            crate::shallow_serve::objects_within(
                repo,
                &wants,
                &window,
                &common,
                &shallow_req.client_shallow,
            )
        }
        None => crate::porcelain::push_proto::objects_to_send(repo, &wants, &common),
    };
    // `--filter=<spec>` on the `pack-objects` git spawns (upload-pack.c:340-344).
    crate::porcelain::pack_objects::apply_filter(repo, filter_spec.as_deref(), &mut objects);
    // `include-tag` off the client's capability list, the v0 spelling of the v2
    // argument. A shallow clone depends on it: its tags are never `want`ed, so a
    // tag whose target landed inside the window arrives only if the pack carries
    // it (`write_followtags()`, clone.c:686-700).
    if cap_present(&want_caps, b"include-tag") {
        add_included_tags(repo, &mut objects);
    }
    // `data->use_ofs_delta`, set by `receive_needs()` from the client's
    // `ofs-delta` capability and turned into `--delta-base-offset` on the
    // `pack-objects` git spawns (`create_pack_file()`). It is off until the
    // client asks, because a receiver that predates offset deltas cannot read
    // them — and on once it does, because an `OBJ_OFS_DELTA` names its base in
    // a two-or-three byte varint where an `OBJ_REF_DELTA` spends a full object
    // id, which is 18 bytes per delta of pure overhead.
    let use_ofs_delta = cap_present(&want_caps, b"ofs-delta");
    let pack = crate::porcelain::pack_objects::pack_bytes_with(repo, &objects, use_ofs_delta)?;
    // `data->use_sideband` (upload-pack.c:1135-1138) is the packet size the
    // selected band carries, not a flag: `LARGE_PACKET_MAX` for `side-band-64k`
    // and `DEFAULT_PACKET_MAX` for plain `side-band`. `send_sideband()` chunks at
    // that size minus the 4-byte length and the 1-byte band, so a client that
    // asked for the 1000-byte dialect must not be handed 65 KiB packets.
    let use_sideband = if cap_present(&want_caps, b"side-band-64k") {
        Some(LARGE_PACKET_MAX - 5)
    } else if cap_present(&want_caps, b"side-band") {
        Some(DEFAULT_PACKET_MAX - 5)
    } else {
        None
    };
    if let Some(band_max) = use_sideband {
        // Multiplex the pack on band 1, then a flush closes the side-band stream.
        for chunk in pack.chunks(band_max) {
            let mut framed = Vec::with_capacity(chunk.len() + 1);
            framed.push(1);
            framed.extend_from_slice(chunk);
            write_pkt(&mut out, &framed)?;
        }
        out.write_all(b"0000")?;
    } else {
        out.write_all(&pack)?; // raw, self-delimiting
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The fetch ref advertisement: `HEAD` first (carrying the caps and the
/// `symref=HEAD:<target>` so the client checks out the right branch), then every
/// ref in name order, each annotated tag followed by its peeled `^{}` line, then a
/// flush. Mirrors git's `send_ref`/`upload-pack` advertisement shape.
fn advertisement(repo: &gix::Repository, policy: WantPolicy, no_done: bool) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    // `upload-pack` filters its ref list through `ref_is_hidden()` exactly as
    // `receive-pack` does, off `uploadpack.hideRefs` plus the shared
    // `transfer.hideRefs`.
    let config = repo.config_snapshot();
    let hidden = super::receive_pack::hide_ref_patterns(&config, "uploadpack.hideRefs");
    let head_target = repo
        .head_ref()
        .ok()
        .flatten()
        .map(|r| r.name().as_bstr().to_string());
    let caps = capabilities(head_target.as_deref(), policy, no_done);
    let mut sent_caps = false;

    // HEAD first, when it resolves to an object.
    if let Ok(mut head) = repo.head() {
        if let Ok(Some(id)) = head.try_peel_to_id() {
            pkt_line(&mut out, format!("{} HEAD\0{caps}\n", id.detach().to_hex()).as_bytes());
            sent_caps = true;
        }
    }

    for reference in repo.references()?.all()? {
        let Ok(mut reference) = reference else { continue };
        let name = reference.name().as_bstr().to_string();
        if super::receive_pack::ref_is_hidden(&hidden, &name) {
            continue;
        }
        let Ok(id) = reference.follow_to_object() else { continue };
        let oid = id.detach();
        let line = if sent_caps {
            format!("{} {name}\n", oid.to_hex())
        } else {
            sent_caps = true;
            format!("{} {name}\0{caps}\n", oid.to_hex())
        };
        pkt_line(&mut out, line.as_bytes());
        // An annotated tag advertises its peeled target on a `^{}` line.
        if let Ok(obj) = repo.find_object(oid) {
            if let Ok(peeled) = obj.peel_to_kind(gix::objs::Kind::Commit) {
                if peeled.id != oid {
                    pkt_line(&mut out, format!("{} {name}^{{}}\n", peeled.id.to_hex()).as_bytes());
                }
            }
        }
    }

    if !sent_caps {
        // Empty repository: advertise the capabilities on the synthetic line.
        let null = repo.object_hash().null();
        pkt_line(&mut out, format!("{} capabilities^{{}}\0{caps}\n", null.to_hex()).as_bytes());
    }

    if let Ok(Some(commits)) = repo.shallow_commits() {
        for id in commits.iter() {
            pkt_line(&mut out, format!("shallow {}\n", id.to_hex()).as_bytes());
        }
    }
    flush_pkt(&mut out);
    Ok(out)
}

/// `LARGE_PACKET_MAX` (pkt-line.h): the packet size `side-band-64k` carries.
const LARGE_PACKET_MAX: usize = 65520;

/// `DEFAULT_PACKET_MAX` (pkt-line.h): the packet size plain `side-band` carries.
const DEFAULT_PACKET_MAX: usize = 1000;

/// Which acknowledgement dialect the client selected in its first `want` line —
/// git's `data->multi_ack`, whose three states change what `get_common_commits`
/// writes for every `have` (upload-pack.c:1125-1128).
#[derive(PartialEq, Clone, Copy)]
enum MultiAck {
    /// Neither capability requested: one bare `ACK <oid>` for the first common
    /// object and nothing after it.
    None,
    /// `multi_ack`: `ACK <oid> continue` per common object.
    Plain,
    /// `multi_ack_detailed`: `ACK <oid> common` per common object.
    Detailed,
}

impl MultiAck {
    /// `parse_feature_request(features, "multi_ack_detailed")` first, then
    /// `"multi_ack"` — the detailed dialect wins when both are offered.
    fn from_caps(caps: &[u8]) -> Self {
        if cap_present(caps, b"multi_ack_detailed") {
            Self::Detailed
        } else if cap_present(caps, b"multi_ack") {
            Self::Plain
        } else {
            Self::None
        }
    }
}

/// The fixed part of `write_v0_ref()`'s capability string (upload-pack.c:1235).
/// Every token here is one this server honours on the v0 path:
///
///   * `multi_ack` / `multi_ack_detailed` — both acknowledgement dialects are
///     driven by [`MultiAck`] in the have loop.
///   * `thin-pack` / `no-progress` — permissions, not obligations. A client that
///     sends them asks this server to be *allowed* to accept a thin pack and to
///     skip progress; a pack that is not thin and carries no progress satisfies
///     both, which is why git advertises them from a fixed string regardless of
///     what `pack-objects` will produce.
///   * `ofs-delta` — a permission this server *takes*, as git does. A client
///     that sends it sets `data->use_ofs_delta`, which becomes
///     `--delta-base-offset` on `pack-objects`' command line
///     (`create_pack_file()`), so every delta names its base by a backwards
///     pack offset instead of spending a full object id on it. Not taking it
///     costs 18 bytes per delta and makes the pack — and therefore its name —
///     differ from git's for the same object set.
///   * `side-band` / `side-band-64k` — [`serve`] frames the pack at whichever
///     of the two the client selects, with the packet cap that goes with it.
///   * `include-tag` — [`add_included_tags`] adds the tags pointing into the
///     pack; a client that omits it gets no extra tags.
///
///   * `shallow` and the three `deepen-*` tokens — the client's `shallow` lines
///     and its `deepen`, `deepen-since`, `deepen-not` and `deepen-relative`
///     requests are answered by [`crate::shallow_serve`], which computes the
///     boundary, writes the `shallow`/`unshallow` lines the client records, and
///     limits the pack to the window.
const V0_CAPABILITIES: &str =
    "multi_ack thin-pack side-band side-band-64k ofs-delta shallow deepen-since \
     deepen-not deepen-relative no-progress include-tag multi_ack_detailed";

/// The upload-pack capability list. `symref=HEAD:<target>` tells the client which
/// branch HEAD follows so a clone checks it out. The two `allow-*-sha1-in-want`
/// tokens are emitted exactly when the matching bit of [`WantPolicy`] is set, as
/// `write_v0_ref()` does off `data->allow_uor`.
///
/// `no-done` follows `data->no_done`, which `cmd_main` sets only under
/// `--advertise-refs` (upload-pack.c:1420-1421) — see [`serve`] for the branch
/// that honours a client asking for it back.
fn capabilities(head_target: Option<&str>, policy: WantPolicy, no_done: bool) -> String {
    let mut caps = String::from(V0_CAPABILITIES);
    if policy.tip {
        caps.push_str(" allow-tip-sha1-in-want");
    }
    if policy.reachable {
        caps.push_str(" allow-reachable-sha1-in-want");
    }
    if no_done {
        caps.push_str(" no-done");
    }
    if let Some(t) = head_target {
        caps.push_str(&format!(" symref=HEAD:{t}"));
    }
    // `data->allow_filter ? " filter" : ""`, which `write_v0_ref()` places right
    // after the symref info and before `object-format=` (upload-pack.c:1249-1261).
    if policy.filter {
        caps.push_str(" filter");
    }
    // One agent string for both servers: `git_user_agent_sanitized()`, ported in
    // `receive_pack::agent` — `$GIT_USER_AGENT` when set, else
    // `git/<version>-<uname -s>`. Deriving it keeps `upload-pack` and
    // `receive-pack` from disagreeing about what this binary is, and keeps the
    // platform suffix honest off Darwin, which a literal cannot do.
    caps.push_str(&format!(" object-format=sha1 agent={}", super::receive_pack::agent()));
    caps
}

/// How a `filter <spec>` line is classified: the key it is allowed or banned
/// under, whether this server can actually apply it, and the depth of a
/// `tree:<n>`.
struct FilterSpec {
    /// `list_object_filter_config_name()` (list-objects-filter-options.c:17) —
    /// the spec with its parameter dropped, which is what
    /// `uploadpackfilter.<key>.allow` is keyed on.
    key: &'static str,
    /// Whether [`pack_objects::apply_filter`] removes the objects this spec
    /// names. A spec it would silently ignore must be refused instead: a pack
    /// that quietly contains everything the client asked to be left out is
    /// worse than an error, because the client records the filter in
    /// `remote.<name>.partialclonefilter` and treats the result as complete.
    appliable: bool,
    /// `opts->tree_exclude_depth`, for the `uploadpackfilter.tree.maxDepth` cap.
    depth: Option<u64>,
}

/// `gently_parse_list_objects_filter()` — classify a filter spec, or `None`
/// when it is not one.
fn classify_filter(spec: &str) -> Option<FilterSpec> {
    let simple = |key, appliable| Some(FilterSpec { key, appliable, depth: None });
    match spec {
        "blob:none" => simple("blob:none", true),
        s if s.starts_with("sparse:oid=") => simple("sparse:oid", false),
        s if s.starts_with("combine:") => simple("combine", false),
        s => {
            if let Some(limit) = s.strip_prefix("blob:limit=") {
                // An unparseable magnitude is a spec error in git, and here it
                // would make `apply_filter` a no-op.
                return simple("blob:limit", super::pack_objects::magnitude(limit).is_some());
            }
            if let Some(kind) = s.strip_prefix("object:type=") {
                let known = matches!(kind, "blob" | "tree" | "commit" | "tag");
                return simple("object:type", known);
            }
            if let Some(depth) = s.strip_prefix("tree:") {
                let depth = depth.parse::<u64>().ok()?;
                // Only depth 0 is expressible without the walk's depth
                // bookkeeping; see `apply_filter`.
                return Some(FilterSpec { key: "tree", appliable: depth == 0, depth: Some(depth) });
            }
            None
        }
    }
}

/// `check_one_filter()` (upload-pack.c:1041) — why this server refuses to run
/// `spec`, or `None` when it will.
///
/// Three things can refuse it. The spec may not parse. It may be banned by
/// `uploadpackfilter.<key>.allow`, falling back to `uploadpackfilter.allow`,
/// which itself defaults to true (`data->allow_filter_fallback = 1`,
/// upload-pack.c:151); a `tree:<n>` is additionally capped by
/// `uploadpackfilter.tree.maxDepth`, which defaults to zero. Or it may be one
/// this server cannot apply, which git has no equivalent of — git implements
/// every spec — and which reuses git's "not supported" wording because that is
/// exactly what it means to the client.
fn filter_ban_reason(repo: &gix::Repository, spec: &str) -> Option<String> {
    // `gently_parse_list_objects_filter`'s catch-all.
    let Some(parsed) = classify_filter(spec) else {
        return Some(format!("invalid filter-spec '{spec}'"));
    };
    let config = repo.config_snapshot();

    let fallback = config.boolean("uploadpackfilter.allow").unwrap_or(true);
    let allowed =
        config.boolean(format!("uploadpackfilter.{}.allow", parsed.key).as_str()).unwrap_or(fallback);
    if !allowed || !parsed.appliable {
        return Some(format!("filter '{}' not supported", parsed.key));
    }
    if let Some(depth) = parsed.depth {
        let max = config
            .integer("uploadpackfilter.tree.maxDepth")
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(0);
        if depth > max {
            return Some(format!("tree filter allows max depth {max}, but got {depth}"));
        }
    }
    None
}

/// git's `allow_uor` bitset: which object ids a client may name in a `want` line
/// beyond the ref tips this server advertised.
///
/// Ported from `upload_pack_config()` (upload-pack.c): `uploadpack.allowAnySHA1InWant`
/// is `ALLOW_ANY_SHA1`, which is defined as *implying* the other two, so setting it
/// also lights up both `allow-*-sha1-in-want` advertisement tokens.
#[derive(Clone, Copy, Default)]
struct WantPolicy {
    /// `uploadpack.allowTipSHA1InWant` — a want may be the tip of *any* ref,
    /// including one hidden from the advertisement.
    tip: bool,
    /// `uploadpack.allowReachableSHA1InWant` — a want may be any object reachable
    /// from a ref.
    reachable: bool,
    /// `uploadpack.allowAnySHA1InWant` — a want may be any object in the repository.
    any: bool,
    /// `uploadpack.allowFilter` — advertise `filter` and honour a `filter <spec>`
    /// line by leaving the filtered objects out of the pack.
    filter: bool,
}

impl WantPolicy {
    /// Read the three `uploadpack.allow*SHA1InWant` booleans, applying git's
    /// "any implies tip and reachable" relation.
    fn from_config(repo: &gix::Repository) -> Self {
        let config = repo.config_snapshot();
        let any = config.boolean("uploadpack.allowAnySHA1InWant").unwrap_or(false);
        WantPolicy {
            tip: any || config.boolean("uploadpack.allowTipSHA1InWant").unwrap_or(false),
            reachable: any
                || config
                    .boolean("uploadpack.allowReachableSHA1InWant")
                    .unwrap_or(false),
            any,
            filter: config.boolean("uploadpack.allowFilter").unwrap_or(false),
        }
    }

    /// git's `allow_hidden_refs()`: whether hidden refs are left out of the set of
    /// "our refs" a want is matched against. They are, unless exactly one of the
    /// tip/reachable relaxations is in play without `allowAnySHA1InWant`.
    fn hides_hidden_refs(self) -> bool {
        self.any || !(self.tip || self.reachable)
    }

    /// The object ids a `want` may name for free — `mark_our_ref()`'s `OUR_REF`
    /// set: every ref tip, minus the ones hidden from the advertisement when
    /// [`hides_hidden_refs`][Self::hides_hidden_refs] says so.
    fn our_refs(self, repo: &gix::Repository) -> Result<Vec<ObjectId>> {
        let config = repo.config_snapshot();
        let hidden = super::receive_pack::hide_ref_patterns(&config, "uploadpack.hideRefs");
        let skip_hidden = self.hides_hidden_refs();
        let mut out = Vec::new();
        for reference in repo.references()?.all()? {
            let Ok(mut reference) = reference else { continue };
            let name = reference.name().as_bstr().to_string();
            if skip_hidden && super::receive_pack::ref_is_hidden(&hidden, &name) {
                continue;
            }
            if let Ok(id) = reference.follow_to_object() {
                out.push(id.detach());
            }
        }
        Ok(out)
    }

    /// The first `want` this server refuses to serve, or `None` when all of them
    /// are acceptable. Ports `receive_needs()`'s want loop plus `check_non_tip()`:
    /// an object that is not present at all is always refused; otherwise a want
    /// outside the "our refs" set is refused unless `allowAnySHA1InWant` is on, or
    /// it is reachable from one of them and either `allowReachableSHA1InWant` is on
    /// or we are answering a stateless RPC.
    fn refuse(
        self,
        repo: &gix::Repository,
        wants: &[ObjectId],
        stateless_rpc: bool,
    ) -> Result<Option<ObjectId>> {
        if let Some(missing) = wants.iter().find(|id| repo.find_object(**id).is_err()) {
            return Ok(Some(*missing));
        }
        if self.any {
            return Ok(None);
        }
        let ours = self.our_refs(repo)?;
        let non_tip: Vec<ObjectId> = wants.iter().copied().filter(|id| !ours.contains(id)).collect();
        if non_tip.is_empty() {
            return Ok(None);
        }
        // `check_non_tip()` refuses immediately unless the reachability check is
        // allowed to run at all.
        if !stateless_rpc && !self.reachable {
            return Ok(Some(non_tip[0]));
        }
        // `has_unreachable()`: `rev-list --stdin` with every "our ref" negated. A
        // want that survives that walk is not an ancestor of anything we advertised.
        let tips: Vec<ObjectId> = ours
            .iter()
            .filter_map(|id| peel_to_commit(repo, *id))
            .collect();
        for want in non_tip {
            let Some(commit) = peel_to_commit(repo, want) else {
                // `rev-list` dies on a want that is not commit-ish, which
                // `has_unreachable()` turns into "unreachable".
                return Ok(Some(want));
            };
            let reachable = repo
                .merge_bases_many(commit, &tips)
                .map(|bases| bases.iter().any(|base| base.detach() == commit))
                .unwrap_or(false);
            if !reachable {
                return Ok(Some(want));
            }
        }
        Ok(None)
    }
}

/// Peel `id` to the commit it names, or `None` when it names no commit — the
/// distinction `rev-list` makes between a commit-ish argument and any other object.
fn peel_to_commit(repo: &gix::Repository, id: ObjectId) -> Option<ObjectId> {
    let object = repo.find_object(id).ok()?;
    object.peel_to_kind(gix::objs::Kind::Commit).ok().map(|c| c.id)
}

/// Whether the client advertised capability `want` (a whole space-separated token).
fn cap_present(caps: &[u8], want: &[u8]) -> bool {
    caps.split(|&b| b == b' ' || b == b'\n' || b == 0).any(|tok| tok == want)
}

/// Append a pkt-line (four-hex length header covering itself, then the payload).
fn pkt_line(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

/// Append a flush packet.
fn flush_pkt(out: &mut Vec<u8>) {
    out.extend_from_slice(b"0000");
}

/// Write one pkt-line to a stream.
fn write_pkt(out: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(payload.len() + 4);
    pkt_line(&mut buf, payload);
    out.write_all(&buf)
}

/// Read one pkt-line: `None` on flush (`0000`), else its payload with the header
/// stripped. A missing/short header or a non-hex length is a protocol error.
fn read_pkt(r: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut hdr = [0u8; 4];
    read_exact(r, &mut hdr).map_err(|_| anyhow::anyhow!("the remote end hung up unexpectedly"))?;
    let len = u16::from_str_radix(
        std::str::from_utf8(&hdr).map_err(|_| anyhow::anyhow!("protocol error: bad line length character"))?,
        16,
    )
    .map_err(|_| anyhow::anyhow!("protocol error: bad line length character"))?;
    match len {
        0 => Ok(None),
        1..=4 => Ok(Some(Vec::new())),
        _ => {
            let mut buf = vec![0u8; len as usize - 4];
            read_exact(r, &mut buf)?;
            Ok(Some(buf))
        }
    }
}

fn read_exact(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        match r.read(&mut buf[off..])? {
            0 => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof")),
            n => off += n,
        }
    }
    Ok(())
}

/// Why a long option could not be resolved to a single table entry.
enum LongError {
    /// No entry matched, even as an abbreviation.
    Unknown,
    /// Two or more matched. Carries the two git names in its message, which are
    /// the *last* two matches in table order — that is what git's
    /// `abbrev_option`/`ambiguous_option` pair ends up holding.
    Ambiguous(String, String),
}

/// Resolve the text after `--` (with any `=value` already split off) against the
/// option table, honouring exact matches, `no-` negation, and unique-prefix
/// abbreviation.
///
/// Each entry is tried plain first and then negated, matching the order git
/// scans in; an exact match returns immediately, so a later exact spelling wins
/// over an earlier abbreviation.
fn resolve_long(name: &str) -> Result<Resolved, LongError> {
    let mut matches: Vec<Resolved> = Vec::new();

    for (index, long) in LONG_OPTS.iter().enumerate() {
        for negated in [false, true] {
            let spelling = if negated {
                format!("no-{long}")
            } else {
                (*long).to_owned()
            };
            if spelling == name {
                return Ok(Resolved { index, negated });
            }
            if !name.is_empty() && spelling.starts_with(name) {
                matches.push(Resolved { index, negated });
            }
        }
    }

    match matches.len() {
        0 => Err(LongError::Unknown),
        1 => Ok(matches[0]),
        n => Err(LongError::Ambiguous(
            matches[n - 2].name(),
            matches[n - 1].name(),
        )),
    }
}

/// Validate a `--timeout` value the way git's `git_parse_int` does, returning
/// the diagnostic line git would print on failure.
///
/// Accepted: optional leading whitespace, an optional sign, a base-0 integer
/// (so `0x10` is hex and `010` is octal), and an optional `k`/`m`/`g` magnitude
/// suffix. The result must fit in an `i32`.
fn parse_timeout(value: &str, name: &str) -> Result<i32, String> {
    if value.is_empty() {
        return Err(format!("error: option `{name}' expects a numerical value"));
    }
    let invalid =
        || format!("error: option `{name}' expects an integer value with an optional k/m/g suffix");

    let rest = value.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (negative, rest) = match rest.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, rest.strip_prefix('+').unwrap_or(rest)),
    };

    // Base 0, exactly as strtoimax reads it.
    let (radix, digits) = if let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X"))
    {
        (16, r)
    } else if rest.len() > 1 && rest.starts_with('0') {
        (8, &rest[1..])
    } else {
        (10, rest)
    };

    let split = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    let (number, tail) = digits.split_at(split);
    if number.is_empty() {
        return Err(invalid());
    }
    let mut magnitude: i128 = match i128::from_str_radix(number, radix) {
        Ok(v) => v,
        // Longer than an i128 can hold; git reports this as out of range.
        Err(_) => return Err(range_error(value, name)),
    };

    // At most one magnitude suffix, and nothing may follow it.
    if !tail.is_empty() {
        let mut chars = tail.chars();
        let suffix = chars.next().expect("tail is non-empty");
        let Some((_, factor)) = MAGNITUDES.iter().find(|(c, _)| *c == suffix) else {
            return Err(invalid());
        };
        if chars.next().is_some() {
            return Err(invalid());
        }
        magnitude *= factor;
    }

    if negative {
        magnitude = -magnitude;
    }
    i32::try_from(magnitude).map_err(|_| range_error(value, name))
}

/// git's out-of-range diagnostic, which quotes the value exactly as written,
/// magnitude suffix included.
fn range_error(value: &str, name: &str) -> String {
    format!(
        "error: value {value} for option `{name}' not in range [{},{}]",
        i32::MIN,
        i32::MAX
    )
}

/// Expand a leading `~` against `$HOME`, as `enter_repo()` does.
///
/// `~<user>` needs a passwd lookup that the vendored crates do not provide, so
/// it is refused rather than passed through unexpanded — silently treating it as
/// a literal path would report "not a git repository" for a directory that
/// exists.
fn expand_tilde(path: &str) -> Result<PathBuf, String> {
    let Some(rest) = path.strip_prefix('~') else {
        return Ok(PathBuf::from(path));
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        return Err(format!(
            "~<user> expansion in {path:?} is not ported: it needs a passwd-database lookup that \
             the vendored crates do not provide"
        ));
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(PathBuf::from(path));
    };
    let mut out = PathBuf::from(home);
    if let Some(tail) = rest.strip_prefix('/') {
        out.push(tail);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Protocol v2: serve.c, ls-refs.c, protocol-caps.c and upload-pack.c's v2 half.
// ---------------------------------------------------------------------------

/// git's `die()` inside the v2 serve loop: the reason, which the caller reports
/// as `fatal: <reason>` on stderr before exiting 128. Any `ERR` pkt-line the
/// client is owed has already been written by then, exactly as
/// `packet_writer_error()` + `die()` pair up in upload-pack.c.
struct Die(String);

impl From<std::io::Error> for Die {
    fn from(e: std::io::Error) -> Self {
        Die(e.to_string())
    }
}

/// `die!` with `format!` syntax, for the many one-line protocol errors.
macro_rules! die {
    ($($arg:tt)*) => { return Err(Die(format!($($arg)*))) };
}

/// One pkt-line as `packet_reader_read()` classifies it.
#[derive(PartialEq, Eq)]
enum Pkt {
    /// A normal line, with the trailing newline chomped
    /// (`PACKET_READ_CHOMP_NEWLINE`).
    Line(Vec<u8>),
    /// `0000`.
    Flush,
    /// `0001`.
    Delim,
    /// `0002`.
    ResponseEnd,
    /// The peer closed the connection (`PACKET_READ_EOF`).
    Eof,
}

/// A one-packet-lookahead reader, the shape `packet_reader_peek()` /
/// `packet_reader_read()` give the v2 serve loop: `process_request` has to look
/// at a flush without consuming it, so the command that follows sees the same
/// terminator it would have seen after a delim.
struct PktReader<R: Read> {
    inner: R,
    peeked: Option<Pkt>,
}

impl<R: Read> PktReader<R> {
    fn new(inner: R) -> Self {
        PktReader { inner, peeked: None }
    }

    fn peek(&mut self) -> Result<&Pkt, Die> {
        if self.peeked.is_none() {
            self.peeked = Some(self.read_raw()?);
        }
        Ok(self.peeked.as_ref().expect("just filled"))
    }

    fn read(&mut self) -> Result<Pkt, Die> {
        match self.peeked.take() {
            Some(p) => Ok(p),
            None => self.read_raw(),
        }
    }

    fn read_raw(&mut self) -> Result<Pkt, Die> {
        let mut hdr = [0u8; 4];
        let mut off = 0;
        while off < 4 {
            match self.inner.read(&mut hdr[off..])? {
                // A clean EOF only at a packet boundary is `PACKET_READ_EOF`.
                0 if off == 0 => return Ok(Pkt::Eof),
                0 => die!("protocol error: bad line length character: {}", String::from_utf8_lossy(&hdr[..off])),
                n => off += n,
            }
        }
        let text = std::str::from_utf8(&hdr)
            .map_err(|_| Die("protocol error: bad line length character".into()))?;
        let len = u16::from_str_radix(text, 16)
            .map_err(|_| Die(format!("protocol error: bad line length character: {text}")))?;
        match len {
            0 => Ok(Pkt::Flush),
            1 => Ok(Pkt::Delim),
            2 => Ok(Pkt::ResponseEnd),
            3 => die!("protocol error: bad line length 3"),
            4 => Ok(Pkt::Line(Vec::new())),
            _ => {
                let mut buf = vec![0u8; len as usize - 4];
                read_exact(&mut self.inner, &mut buf)?;
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                }
                Ok(Pkt::Line(buf))
            }
        }
    }
}

/// `struct packet_writer`: every v2 response line goes through here so that a
/// client which negotiated `sideband-all` gets the whole exchange multiplexed —
/// section headers on band 1 and errors on band 3 — instead of only the pack.
struct PktWriter<W: Write> {
    out: W,
    /// `writer->use_sideband`, set by the `sideband-all` fetch argument.
    sideband: bool,
}

impl<W: Write> PktWriter<W> {
    /// `packet_writer_write()`. The caller supplies the trailing newline, as
    /// git's format strings do.
    fn write(&mut self, line: &str) -> Result<(), Die> {
        let mut payload = Vec::with_capacity(line.len() + 1);
        if self.sideband {
            payload.push(1);
        }
        payload.extend_from_slice(line.as_bytes());
        write_pkt(&mut self.out, &payload)?;
        Ok(())
    }

    /// `packet_writer_error()`: `ERR ` in front, or band 3 under `sideband-all`.
    fn error(&mut self, msg: &str) -> Result<(), Die> {
        let mut payload = Vec::with_capacity(msg.len() + 4);
        if self.sideband {
            payload.push(3);
        } else {
            payload.extend_from_slice(b"ERR ");
        }
        payload.extend_from_slice(msg.as_bytes());
        write_pkt(&mut self.out, &payload)?;
        self.out.flush()?;
        Ok(())
    }

    /// One raw band of pack data. Never routed through `sideband`: in v2 the
    /// pack is always multiplexed, `sideband-all` or not
    /// (`data.use_sideband = LARGE_PACKET_MAX`, upload-pack.c:1778).
    fn band(&mut self, band: u8, data: &[u8]) -> Result<(), Die> {
        let mut framed = Vec::with_capacity(data.len() + 1);
        framed.push(band);
        framed.extend_from_slice(data);
        write_pkt(&mut self.out, &framed)?;
        Ok(())
    }

    fn delim(&mut self) -> Result<(), Die> {
        self.out.write_all(b"0001")?;
        Ok(())
    }

    fn flush_pkt(&mut self) -> Result<(), Die> {
        self.out.write_all(b"0000")?;
        self.out.flush()?;
        Ok(())
    }
}

/// `lsrefs.unborn` (ls-refs.c:16-42).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unborn {
    /// Neither advertised nor honoured.
    Ignore,
    /// Honoured when the client asks, but not advertised.
    Allow,
    /// Honoured and advertised as `ls-refs=unborn`. The default.
    Advertise,
}

/// The v2 capability set this server advertises, and the config behind it.
///
/// Every entry is a capability whose honouring code is in this module. One of
/// git's is deliberately withheld:
///
///   * **`packfile-uris`** (`uploadpack.blobPackfileURI`), which needs
///     `pack-objects` to emit URI lines ahead of the pack; the pack here is one
///     finished `Vec<u8>`.
struct V2Config {
    /// `lsrefs.unborn`.
    unborn: Unborn,
    /// The v0 want policy, reused for `uploadpack.allowFilter`. The three
    /// `allow*SHA1InWant` bits it also carries are v0-only: git's `check_non_tip`
    /// is called from `receive_needs()` alone (upload-pack.c:1181), so a v2
    /// `want` is accepted for any object the repository has.
    policy: WantPolicy,
    /// `uploadpack.allowRefInWant` — accept `want-ref <ref>` and answer with a
    /// `wanted-refs` section.
    ref_in_want: bool,
    /// `uploadpack.allowSidebandAll` — accept `sideband-all` and multiplex the
    /// whole response, not just the pack.
    sideband_all: bool,
    /// `transfer.advertiseSID` — advertise this process's trace2 session id, and
    /// accept the client's.
    session_id: Option<String>,
    /// `transfer.advertiseObjectInfo` — advertise and serve `object-info`.
    object_info: bool,
    /// The `<pr-info>` of the `promisor-remote` capability, from
    /// `promisor.advertise` and `promisor.sendFields` (`promisor_remote_info()`).
    /// `None` withholds the capability entirely, which is git's default.
    promisor_info: Option<String>,
    /// The repository's hash algorithm, which the client must agree with.
    object_format: String,
}

impl V2Config {
    fn from_repo(repo: &gix::Repository) -> Self {
        let config = repo.config_snapshot();
        let unborn = match config.string("lsrefs.unborn").map(|v| v.to_string()).as_deref() {
            Some("allow") => Unborn::Allow,
            Some("ignore") => Unborn::Ignore,
            // Missing, `advertise`, or — where git dies on a misconfigured
            // server — anything else.
            _ => Unborn::Advertise,
        };
        V2Config {
            unborn,
            policy: WantPolicy::from_config(repo),
            ref_in_want: config.boolean("uploadpack.allowRefInWant").unwrap_or(false),
            sideband_all: config.boolean("uploadpack.allowSidebandAll").unwrap_or(false),
            session_id: config
                .boolean("transfer.advertiseSID")
                .unwrap_or(false)
                .then(|| crate::trace2::session_id().to_owned()),
            object_info: config.boolean("transfer.advertiseObjectInfo").unwrap_or(false),
            promisor_info: gix::promisor::remote_info(repo),
            object_format: repo.object_hash().to_string(),
        }
    }

    /// The values of the `fetch=` capability, in git's order
    /// (`upload_pack_advertise()`, upload-pack.c:1843-1857) minus the withheld
    /// `packfile-uris`. `shallow` leads, as it does in git, and is unconditional:
    /// the boundary computation behind it needs no config.
    fn fetch_values(&self) -> String {
        let mut v = String::from("shallow wait-for-done");
        if self.policy.filter {
            v.push_str(" filter");
        }
        if self.ref_in_want {
            v.push_str(" ref-in-want");
        }
        if self.sideband_all {
            v.push_str(" sideband-all");
        }
        v
    }
}

/// `protocol_v2_advertise_capabilities()` (serve.c:186-216): `version 2`, then
/// one pkt-line per advertised capability in table order, then a flush.
fn v2_advertisement(repo: &gix::Repository) -> Result<Vec<u8>> {
    let cfg = V2Config::from_repo(repo);
    let mut out = Vec::new();
    pkt_line(&mut out, b"version 2\n");
    pkt_line(&mut out, format!("agent={}\n", super::receive_pack::agent()).as_bytes());
    match cfg.unborn {
        Unborn::Advertise => pkt_line(&mut out, b"ls-refs=unborn\n"),
        _ => pkt_line(&mut out, b"ls-refs\n"),
    }
    pkt_line(&mut out, format!("fetch={}\n", cfg.fetch_values()).as_bytes());
    pkt_line(&mut out, b"server-option\n");
    pkt_line(&mut out, format!("object-format={}\n", cfg.object_format).as_bytes());
    if let Some(sid) = &cfg.session_id {
        pkt_line(&mut out, format!("session-id={sid}\n").as_bytes());
    }
    if cfg.object_info {
        pkt_line(&mut out, b"object-info\n");
    }
    // Last in git's `capabilities[]` table (serve.c:180-183), after the
    // `bundle-uri` entry this server does not carry.
    if let Some(info) = &cfg.promisor_info {
        pkt_line(&mut out, format!("promisor-remote={info}\n").as_bytes());
    }
    flush_pkt(&mut out);
    Ok(out)
}

/// `protocol_v2_serve_loop()` (serve.c:356-372): advertise unless this is a
/// stateless RPC, then serve one request (stateless) or requests until the
/// client closes the connection.
fn serve_v2(repo: &gix::Repository, stateless_rpc: bool) -> Result<ExitCode> {
    if !stateless_rpc {
        let adv = v2_advertisement(repo)?;
        let mut out = std::io::stdout().lock();
        out.write_all(&adv)?;
        out.flush()?;
    }
    let cfg = V2Config::from_repo(repo);
    let mut reader = PktReader::new(std::io::stdin());
    let result = if stateless_rpc {
        process_request(repo, &cfg, &mut reader).map(|_| ())
    } else {
        loop {
            match process_request(repo, &cfg, &mut reader) {
                Ok(true) => break Ok(()),
                Ok(false) => continue,
                Err(e) => break Err(e),
            }
        }
    };
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(Die(msg)) => {
            eprintln!("fatal: {msg}");
            Ok(ExitCode::from(128))
        }
    }
}

/// Which command a request selected.
#[derive(Clone, Copy)]
enum V2Command {
    LsRefs,
    Fetch,
    ObjectInfo,
}

/// `process_request()` (serve.c:280-354): read `command=<name>` and the client's
/// capability lines up to the delim (or flush), then run the command. Returns
/// `true` when the client is done with the connection.
fn process_request(
    repo: &gix::Repository,
    cfg: &V2Config,
    reader: &mut PktReader<std::io::Stdin>,
) -> Result<bool, Die> {
    if matches!(reader.peek()?, Pkt::Eof) {
        return Ok(true);
    }

    let mut command: Option<V2Command> = None;
    let mut seen_capability_or_command = false;
    // `client_hash_algo` starts at SHA-1, so a client that says nothing is held
    // to it (serve.c:17).
    let mut client_hash = String::from("sha1");

    loop {
        match reader.peek()? {
            Pkt::Eof => die!("unexpected end of request"),
            Pkt::Line(_) => {
                let Pkt::Line(line) = reader.read()? else {
                    unreachable!("just peeked a line")
                };
                let key = String::from_utf8_lossy(&line).into_owned();
                if let Some(name) = key.strip_prefix("command=") {
                    if command.is_some() {
                        die!("command '{name}' requested after already requesting a command");
                    }
                    command = Some(match name {
                        "ls-refs" => V2Command::LsRefs,
                        "fetch" => V2Command::Fetch,
                        "object-info" if cfg.object_info => V2Command::ObjectInfo,
                        _ => die!("invalid command '{name}'"),
                    });
                } else if !receive_client_capability(repo, cfg, &key, &mut client_hash) {
                    die!("unknown capability '{key}'");
                }
                seen_capability_or_command = true;
            }
            // Not consumed: the command's own argument loop reads it as its
            // terminating flush (serve.c:322-329).
            Pkt::Flush => {
                if !seen_capability_or_command {
                    return Ok(true);
                }
                break;
            }
            Pkt::Delim => {
                reader.read()?;
                break;
            }
            Pkt::ResponseEnd => die!("unexpected response end packet"),
        }
    }

    let Some(command) = command else {
        die!("no command requested");
    };
    if client_hash != cfg.object_format {
        die!(
            "mismatched object format: server {}; client {client_hash}",
            cfg.object_format
        );
    }

    let mut writer = PktWriter { out: std::io::stdout(), sideband: false };
    match command {
        V2Command::LsRefs => ls_refs_command(repo, cfg, reader, &mut writer)?,
        V2Command::Fetch => fetch_command(repo, cfg, reader, &mut writer)?,
        V2Command::ObjectInfo => object_info_command(repo, reader, &mut writer)?,
    }
    Ok(false)
}

/// `receive_client_capability()` (serve.c:241-252): accept a non-command
/// capability the server advertised, or report that it is unknown. `agent` and
/// `server-option` carry no server-side effect; `object-format` picks the hash
/// the client will be held to; `session-id` is only accepted when it was
/// advertised.
///
/// `promisor-remote` is likewise only accepted when it was advertised, and its
/// value — the `;`-joined names the client picked out of the advertisement — is
/// handed to `mark_promisor_remotes_as_accepted()`, which warns about every name
/// that is not one of this repository's promisor remotes. What git does with the
/// accepted set afterwards is pass `--missing=allow-promisor` to the
/// `pack-objects` child (upload-pack.c:338), which stops that child dying on an
/// object the repository does not have. This server spawns no such child: it
/// builds the pack in process from `push_proto::reachable_objects`, which drops
/// a tip it cannot find and falls back rather than failing, so there is no
/// second knob for the accepted set to turn.
fn receive_client_capability(
    repo: &gix::Repository,
    cfg: &V2Config,
    key: &str,
    client_hash: &mut String,
) -> bool {
    let (name, value) = match key.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (key, None),
    };
    match name {
        "agent" | "server-option" => true,
        "object-format" => {
            if let Some(v) = value {
                v.clone_into(client_hash);
            }
            true
        }
        // git logs the client's id via trace2; there is nothing else to do with
        // it, and it is refused outright when `transfer.advertiseSID` is off.
        "session-id" => cfg.session_id.is_some(),
        // Unlike `session-id`, this one is *not* gated on having advertised it:
        // `receive_client_capability()` admits a capability whose
        // `advertise(r, NULL)` returns non-zero, and `promisor_remote_advertise()`
        // returns 1 unconditionally when asked with a NULL buffer (serve.c:33-44) —
        // it only consults `promisor.advertise` when actually building the value.
        // Verified against stock 2.55.0, which answers a `promisor-remote=` sent to
        // a server with `promisor.advertise=false` normally rather than dying.
        "promisor-remote" => {
            if let Some(names) = value {
                gix::promisor::accept_reply(repo, names);
            }
            true
        }
        _ => false,
    }
}

/// `ls_refs()` (ls-refs.c:161-216): HEAD (possibly unborn) first, then every ref
/// that survives `uploadpack.hideRefs` and the client's `ref-prefix` filters.
fn ls_refs_command(
    repo: &gix::Repository,
    cfg: &V2Config,
    reader: &mut PktReader<std::io::Stdin>,
    writer: &mut PktWriter<std::io::Stdout>,
) -> Result<(), Die> {
    /// `TOO_MANY_PREFIXES` (ls-refs.c:48): past this the prefix list can no
    /// longer be the comprehensive list it is meant to be, so it is dropped.
    const TOO_MANY_PREFIXES: usize = 65536;

    let mut peel = false;
    let mut symrefs = false;
    let mut unborn = false;
    let mut prefixes: Vec<String> = Vec::new();

    loop {
        match reader.read()? {
            Pkt::Line(line) => {
                let arg = String::from_utf8_lossy(&line).into_owned();
                match arg.as_str() {
                    "peel" => peel = true,
                    "symrefs" => symrefs = true,
                    "unborn" => unborn = cfg.unborn != Unborn::Ignore,
                    _ => match arg.strip_prefix("ref-prefix ") {
                        Some(p) if prefixes.len() < TOO_MANY_PREFIXES => prefixes.push(p.to_owned()),
                        Some(_) => {}
                        None => die!("unexpected line: '{arg}'"),
                    },
                }
            }
            Pkt::Flush => break,
            _ => die!("expected flush after ls-refs arguments"),
        }
    }
    if prefixes.len() >= TOO_MANY_PREFIXES {
        prefixes.clear();
    }

    let config = repo.config_snapshot();
    let hidden = super::receive_pack::hide_ref_patterns(&config, "uploadpack.hideRefs");
    let matches = |name: &str| {
        !super::receive_pack::ref_is_hidden(&hidden, name)
            && (prefixes.is_empty() || prefixes.iter().any(|p| name.starts_with(p)))
    };

    // `send_possibly_unborn_head()` (ls-refs.c:123-146). An unborn HEAD is only
    // reported when the client asked for both `unborn` and `symrefs`, because
    // the target is the only useful part of that line.
    if matches("HEAD") {
        match repo.head().map(|h| h.kind) {
            Ok(gix::head::Kind::Symbolic(r)) => {
                let target = r.name.as_bstr().to_string();
                let oid = repo
                    .find_reference(r.name.as_ref())
                    .ok()
                    .and_then(|mut r| r.follow_to_object().ok())
                    .map(|id| id.detach());
                if let Some(oid) = oid {
                    let peeled = peel.then(|| peeled_oid(repo, oid)).flatten();
                    send_v2_ref(writer, Some(oid), "HEAD", symrefs.then_some(target.as_str()), peeled)?;
                }
            }
            Ok(gix::head::Kind::Detached { target, .. }) => {
                let peeled = peel.then(|| peeled_oid(repo, target)).flatten();
                send_v2_ref(writer, Some(target), "HEAD", None, peeled)?;
            }
            Ok(gix::head::Kind::Unborn(name)) => {
                if unborn && symrefs {
                    let target = name.as_bstr().to_string();
                    send_v2_ref(writer, None, "HEAD", Some(target.as_str()), None)?;
                }
            }
            Err(_) => {}
        }
    }

    let platform = repo.references().map_err(|e| Die(e.to_string()))?;
    for reference in platform.all().map_err(|e| Die(e.to_string()))? {
        let Ok(mut reference) = reference else { continue };
        let name = reference.name().as_bstr().to_string();
        if !matches(&name) {
            continue;
        }
        let symref_target = match reference.target() {
            gix::refs::TargetRef::Symbolic(target) => Some(target.as_bstr().to_string()),
            gix::refs::TargetRef::Object(_) => None,
        };
        let Ok(id) = reference.follow_to_object() else { continue };
        let oid = id.detach();
        let peeled = peel.then(|| peeled_oid(repo, oid)).flatten();
        send_v2_ref(
            writer,
            Some(oid),
            &name,
            symrefs.then(|| symref_target.as_deref()).flatten(),
            peeled,
        )?;
    }

    writer.flush_pkt()?;
    Ok(())
}

/// `send_ref()`'s line format (ls-refs.c:78-121).
fn send_v2_ref(
    writer: &mut PktWriter<std::io::Stdout>,
    oid: Option<ObjectId>,
    name: &str,
    symref_target: Option<&str>,
    peeled: Option<ObjectId>,
) -> Result<(), Die> {
    let mut line = match oid {
        Some(oid) => format!("{} {name}", oid.to_hex()),
        None => format!("unborn {name}"),
    };
    if let Some(target) = symref_target {
        line.push_str(&format!(" symref-target:{target}"));
    }
    if let Some(peeled) = peeled {
        line.push_str(&format!(" peeled:{}", peeled.to_hex()));
    }
    line.push('\n');
    writer.write(&line)
}

/// `reference_get_peeled_oid()`: the object a tag chain finally names, or `None`
/// when `oid` is not a tag and so does not peel.
fn peeled_oid(repo: &gix::Repository, oid: ObjectId) -> Option<ObjectId> {
    let object = repo.find_object(oid).ok()?;
    if object.kind != gix::objs::Kind::Tag {
        return None;
    }
    object.peel_tags_to_end().ok().map(|o| o.id)
}

/// Everything one `command=fetch` request asked for (`process_args()`,
/// upload-pack.c:1590-1684).
#[derive(Default)]
struct FetchArgs {
    /// `want <oid>` and the ids behind `want-ref`.
    wants: Vec<ObjectId>,
    /// `want-ref <ref>`, answered with a `wanted-refs` section.
    wanted_refs: Vec<(String, ObjectId)>,
    /// The `have` ids this repository actually has — git's `have_obj`, which is
    /// what gets ACKed. A `have` for an object we lack is counted but not kept.
    haves: Vec<ObjectId>,
    /// `data->seen_haves`: whether any `have` line arrived at all, however
    /// useless, which is what decides between the ack round and the pack.
    seen_haves: bool,
    done: bool,
    wait_for_done: bool,
    include_tag: bool,
    /// `data->use_ofs_delta`, set by the `ofs-delta` argument. It reaches
    /// `pack-objects` as `--delta-base-offset` in `create_pack_file()`, so the
    /// deltas name their base by pack offset rather than by object id.
    ofs_delta: bool,
    filter: Option<String>,
    /// `data->shallows` plus the `deepen*` request, answered by the
    /// `shallow-info` section in [`send_pack_section`].
    shallow: crate::shallow_serve::Request,
}

/// `upload_pack_v2()` (upload-pack.c:1770-1833): read the arguments, then run
/// the three-state machine — no wants means nothing to do, `have` lines mean an
/// acknowledgment round first, otherwise go straight to the pack.
fn fetch_command(
    repo: &gix::Repository,
    cfg: &V2Config,
    reader: &mut PktReader<std::io::Stdin>,
    writer: &mut PktWriter<std::io::Stdout>,
) -> Result<(), Die> {
    let args = process_fetch_args(repo, cfg, reader, writer)?;

    if args.wants.is_empty() && !args.wait_for_done {
        return Ok(());
    }
    if args.seen_haves && !process_haves_and_send_acks(repo, &args, writer)? {
        return Ok(());
    }
    send_pack_section(repo, &args, writer)
}

/// `process_args()`. An argument this server did not advertise support for is a
/// protocol error, not something to ignore: a client that sent `deepen` and got
/// a full pack back would record a shallow boundary it never received.
fn process_fetch_args(
    repo: &gix::Repository,
    cfg: &V2Config,
    reader: &mut PktReader<std::io::Stdin>,
    writer: &mut PktWriter<std::io::Stdout>,
) -> Result<FetchArgs, Die> {
    let mut args = FetchArgs::default();
    let config = repo.config_snapshot();
    let hidden = super::receive_pack::hide_ref_patterns(&config, "uploadpack.hideRefs");

    loop {
        let line = match reader.read()? {
            Pkt::Line(line) => line,
            Pkt::Flush => break,
            _ => die!("expected flush after fetch arguments"),
        };
        let arg = String::from_utf8_lossy(&line).into_owned();

        // `parse_want()`: an oid we do not have is refused before anything else.
        if let Some(hex) = arg.strip_prefix("want ") {
            let Ok(id) = ObjectId::from_hex(hex.as_bytes()) else {
                die!("git upload-pack: protocol error, expected to get oid, not '{arg}'");
            };
            if repo.find_object(id).is_err() {
                writer.error(&format!("upload-pack: not our ref {}", id.to_hex()))?;
                die!("git upload-pack: not our ref {}", id.to_hex());
            }
            if !args.wants.contains(&id) {
                args.wants.push(id);
            }
            continue;
        }

        // `parse_want_ref()`, gated on `uploadpack.allowRefInWant`.
        if cfg.ref_in_want {
            if let Some(name) = arg.strip_prefix("want-ref ") {
                let resolved = (!super::receive_pack::ref_is_hidden(&hidden, name))
                    .then(|| repo.find_reference(name).ok())
                    .flatten()
                    .and_then(|mut r| r.follow_to_object().ok())
                    .map(|id| id.detach());
                let Some(oid) = resolved else {
                    writer.error(&format!("unknown ref {name}"))?;
                    die!("unknown ref {name}");
                };
                if args.wanted_refs.iter().any(|(r, _)| r == name) {
                    writer.error(&format!("duplicate want-ref {name}"))?;
                    die!("duplicate want-ref {name}");
                }
                args.wanted_refs.push((name.to_owned(), oid));
                if !args.wants.contains(&oid) {
                    args.wants.push(oid);
                }
                continue;
            }
        }

        // `parse_have()`: only ids we have join `have_obj`, but any `have` line
        // sets `seen_haves`.
        if let Some(hex) = arg.strip_prefix("have ") {
            args.seen_haves = true;
            if let Ok(id) = ObjectId::from_hex(hex.as_bytes()) {
                if repo.find_object(id).is_ok() && !args.haves.contains(&id) {
                    args.haves.push(id);
                }
            }
            continue;
        }

        match arg.as_str() {
            // The pack this server builds is never thin, which `thin-pack` only
            // *permits*.
            "thin-pack" => continue,
            // `if (!strcmp(arg, "ofs-delta")) { data->use_ofs_delta = 1; ... }`
            // (`process_args()`). Unlike `thin-pack` this one is acted on: it is
            // what puts `--delta-base-offset` on `pack-objects`' command line.
            "ofs-delta" => {
                args.ofs_delta = true;
                continue;
            }
            // No progress is ever written on band 2, so this is already true.
            "no-progress" => continue,
            "include-tag" => {
                args.include_tag = true;
                continue;
            }
            "done" => {
                args.done = true;
                continue;
            }
            "wait-for-done" => {
                args.wait_for_done = true;
                continue;
            }
            "sideband-all" if cfg.sideband_all => {
                writer.sideband = true;
                continue;
            }
            _ => {}
        }

        // `shallow <oid>` and the `deepen*` tokens, gated on the `shallow` fetch
        // capability this server advertises.
        match args.shallow.absorb(&arg) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(message) => {
                writer.error(&message)?;
                die!("{message}");
            }
        }

        if cfg.policy.filter {
            if let Some(spec) = arg.strip_prefix("filter ") {
                if args.filter.is_some() {
                    die!("multiple filter-specs cannot be combined");
                }
                if let Some(err) = filter_ban_reason(repo, spec) {
                    writer.error(&err)?;
                    die!("{err}");
                }
                args.filter = Some(spec.to_owned());
                continue;
            }
        }

        die!("unexpected line: '{arg}'");
    }

    Ok(args)
}

/// `process_haves_and_send_acks()` (upload-pack.c:1710-1726). `true` means go on
/// to the pack, `false` means this round ends with a flush and the client will
/// come back with more `have`s.
fn process_haves_and_send_acks(
    repo: &gix::Repository,
    args: &FetchArgs,
    writer: &mut PktWriter<std::io::Stdout>,
) -> Result<bool, Die> {
    if args.done {
        return Ok(true);
    }
    // `send_acks()`.
    writer.write("acknowledgments\n")?;
    if args.haves.is_empty() {
        writer.write("NAK\n")?;
    }
    for id in &args.haves {
        writer.write(&format!("ACK {}\n", id.to_hex()))?;
    }
    if !args.wait_for_done && ok_to_give_up(repo, args) {
        writer.write("ready\n")?;
        writer.delim()?;
        return Ok(true);
    }
    writer.flush_pkt()?;
    Ok(false)
}

/// `ok_to_give_up()` (upload-pack.c:561-571): negotiation can stop once every
/// `want` can reach one of the objects the client said it has, i.e. once the
/// common history is deep enough to cut the pack at.
fn ok_to_give_up(repo: &gix::Repository, args: &FetchArgs) -> bool {
    if args.haves.is_empty() {
        return false;
    }
    args.wants.iter().all(|want| {
        let Some(want) = peel_to_commit(repo, *want) else {
            return false;
        };
        args.haves.iter().any(|have| {
            // `have` is an ancestor of `want` exactly when it is their merge base.
            repo.merge_base(want, *have)
                .map(|base| base.detach() == *have)
                .unwrap_or(false)
        })
    })
}

/// The `UPLOAD_SEND_PACK` state: the optional `wanted-refs` and `shallow-info`
/// sections, then `packfile` and the pack itself multiplexed on band 1.
fn send_pack_section(
    repo: &gix::Repository,
    args: &FetchArgs,
    writer: &mut PktWriter<std::io::Stdout>,
) -> Result<(), Die> {
    // `send_wanted_ref_info()` (upload-pack.c:1728-1745).
    if !args.wanted_refs.is_empty() {
        writer.write("wanted-refs\n")?;
        for (name, oid) in &args.wanted_refs {
            writer.write(&format!("{} {name}\n", oid.to_hex()))?;
        }
        writer.delim()?;
    }

    // `send_shallow_info()` (upload-pack.c:1747-1761): the section is written when
    // a deepening was asked for, when the client declared a boundary of its own,
    // or when this repository is itself shallow — and it is skipped entirely
    // otherwise, so an ordinary fetch sees no shallow-info at all.
    let deepening = args.shallow.deepen.requested();
    let boundary = deepening.then(|| crate::shallow_serve::compute(repo, &args.wants, &args.shallow));
    let own: Vec<ObjectId> = repo
        .shallow_commits()
        .ok()
        .flatten()
        .map(|c| c.iter().copied().collect())
        .unwrap_or_default();
    match &boundary {
        Some(boundary) => {
            writer.write("shallow-info\n")?;
            for id in &boundary.shallow {
                writer.write(&format!("shallow {}\n", id.to_hex()))?;
            }
            for id in &boundary.unshallow {
                writer.write(&format!("unshallow {}\n", id.to_hex()))?;
            }
            writer.delim()?;
        }
        // Serving an already-shallow repository: the client is told where this
        // server's own history stops, which is the boundary it inherits.
        None if !own.is_empty() => {
            writer.write("shallow-info\n")?;
            for id in &own {
                writer.write(&format!("shallow {}\n", id.to_hex()))?;
            }
            writer.delim()?;
        }
        None => {}
    }

    writer.write("packfile\n")?;

    let mut objects = match &boundary {
        Some(boundary) => crate::shallow_serve::objects_within(
            repo,
            &args.wants,
            &boundary.commits,
            &args.haves,
            &args.shallow.client_shallow,
        ),
        // `register_shallow()` for the client's own cutoff: its grafts bound this
        // walk too, so a plain fetch into a shallow clone stays inside it.
        None if !args.shallow.client_shallow.is_empty() => {
            let window = crate::shallow_serve::client_side_commits(
                repo,
                &args.wants,
                &args.shallow.client_shallow,
            );
            crate::shallow_serve::objects_within(
                repo,
                &args.wants,
                &window,
                &args.haves,
                &args.shallow.client_shallow,
            )
        }
        None => crate::porcelain::push_proto::objects_to_send(repo, &args.wants, &args.haves),
    };
    crate::porcelain::pack_objects::apply_filter(repo, args.filter.as_deref(), &mut objects);
    if args.include_tag {
        add_included_tags(repo, &mut objects);
    }
    let pack = crate::porcelain::pack_objects::pack_bytes_with(repo, &objects, args.ofs_delta)
        .map_err(|e| Die(format!("{e:#}")))?;
    // The band byte eats one of the 65516 payload bytes a pkt-line can carry.
    for chunk in pack.chunks(65515) {
        writer.band(1, chunk)?;
    }
    writer.flush_pkt()?;
    Ok(())
}

/// `pack-objects --include-tag`: an annotated tag whose target is already in the
/// pack rides along, so the client ends up with the tag object it will need when
/// it writes `refs/tags/*`. Hidden tags are left out, as they are everywhere else.
fn add_included_tags(repo: &gix::Repository, objects: &mut Vec<ObjectId>) {
    use std::collections::HashSet;
    let present: HashSet<ObjectId> = objects.iter().copied().collect();
    let config = repo.config_snapshot();
    let hidden = super::receive_pack::hide_ref_patterns(&config, "uploadpack.hideRefs");
    let Ok(refs) = repo.references() else { return };
    let Ok(tags) = refs.prefixed("refs/tags/") else { return };
    for reference in tags {
        let Ok(reference) = reference else { continue };
        let name = reference.name().as_bstr().to_string();
        if super::receive_pack::ref_is_hidden(&hidden, &name) {
            continue;
        }
        let gix::refs::TargetRef::Object(oid) = reference.target() else { continue };
        let oid = oid.to_owned();
        if present.contains(&oid) {
            continue;
        }
        // Only a tag *object* is worth adding; a lightweight tag names the
        // commit directly and is already covered by the reachability walk.
        match peeled_oid(repo, oid) {
            Some(peeled) if present.contains(&peeled) => objects.push(oid),
            _ => {}
        }
    }
}

/// `cap_object_info()` (protocol-caps.c:78-113): answer `size` for each `oid`
/// the client listed. An oid this repository does not have gets an empty size
/// field rather than an error.
fn object_info_command(
    repo: &gix::Repository,
    reader: &mut PktReader<std::io::Stdin>,
    writer: &mut PktWriter<std::io::Stdout>,
) -> Result<(), Die> {
    let mut want_size = false;
    let mut oids: Vec<String> = Vec::new();
    loop {
        match reader.read()? {
            Pkt::Line(line) => {
                let arg = String::from_utf8_lossy(&line).into_owned();
                if arg == "size" {
                    want_size = true;
                } else if let Some(oid) = arg.strip_prefix("oid ") {
                    oids.push(oid.to_owned());
                } else {
                    writer.error(&format!("object-info: unexpected line: '{arg}'"))?;
                }
            }
            Pkt::Flush => break,
            _ => {
                writer.error("object-info: expected flush after arguments")?;
                die!("object-info: expected flush after arguments");
            }
        }
    }

    if !oids.is_empty() {
        if want_size {
            writer.write("size")?;
        }
        for text in &oids {
            let Ok(id) = ObjectId::from_hex(text.as_bytes()) else {
                writer.error(&format!(
                    "object-info: protocol error, expected to get oid, not '{text}'"
                ))?;
                continue;
            };
            let mut line = text.clone();
            if want_size {
                match repo.find_object(id) {
                    Ok(object) => line.push_str(&format!(" {}", object.data.len())),
                    Err(_) => line.push(' '),
                }
            }
            writer.write(&line)?;
        }
    }
    writer.flush_pkt()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four specs `apply_filter` implements are the four this server may
    /// accept. Everything else has to be refused, because accepting it would
    /// send a pack containing exactly what the client asked to be left out
    /// while the client records the filter and treats the clone as complete.
    #[test]
    fn only_appliable_filter_specs_are_accepted() {
        for spec in ["blob:none", "blob:limit=1k", "blob:limit=42", "tree:0", "object:type=blob"] {
            let parsed = classify_filter(spec).unwrap_or_else(|| panic!("{spec} should parse"));
            assert!(parsed.appliable, "{spec} is implemented by apply_filter");
        }
        for spec in [
            "sparse:oid=main:paths",
            "combine:blob:none+tree:0",
            "tree:1",
            "blob:limit=nonsense",
            "object:type=nonsense",
        ] {
            let parsed = classify_filter(spec).unwrap_or_else(|| panic!("{spec} should parse"));
            assert!(!parsed.appliable, "{spec} would be silently ignored, so it must be refused");
        }
        assert!(classify_filter("nonsense").is_none());
        assert!(classify_filter("tree:notanumber").is_none());
    }

    /// The ban key drops the parameter, as `list_object_filter_config_name()`
    /// does, so `uploadpackfilter.blob:limit.allow` covers every limit.
    #[test]
    fn filter_ban_key_drops_the_parameter() {
        assert_eq!(classify_filter("blob:limit=1k").unwrap().key, "blob:limit");
        assert_eq!(classify_filter("object:type=blob").unwrap().key, "object:type");
        assert_eq!(classify_filter("tree:0").unwrap().key, "tree");
        assert_eq!(classify_filter("blob:none").unwrap().key, "blob:none");
    }

    /// `filter` is advertised exactly when `uploadpack.allowFilter` is on, and
    /// `write_v0_ref()` puts it after the symref info and before
    /// `object-format=` (upload-pack.c:1249-1261).
    #[test]
    fn filter_capability_is_gated_and_positioned() {
        let off = capabilities(Some("refs/heads/main"), WantPolicy::default(), false);
        assert!(!off.contains(" filter"), "{off}");

        let policy = WantPolicy { filter: true, ..WantPolicy::default() };
        let on = capabilities(Some("refs/heads/main"), policy, false);
        assert!(on.contains(" symref=HEAD:refs/heads/main filter object-format=sha1 "), "{on}");
    }

    /// `no-done` is `data->no_done`, which `cmd_main` sets only on the
    /// `--advertise-refs` path, and `write_v0_ref()` emits it after the
    /// `allow-*-sha1-in-want` pair and before the symref info
    /// (upload-pack.c:1252-1257). Advertising it on the bidirectional path would
    /// promise a shortcut in an exchange stock git never offers it in.
    #[test]
    fn no_done_is_advertised_only_for_advertise_refs() {
        let policy = WantPolicy::default();
        assert!(!capabilities(Some("refs/heads/main"), policy, false).contains(" no-done"));
        let adv = capabilities(Some("refs/heads/main"), policy, true);
        assert!(adv.contains(" no-done symref=HEAD:refs/heads/main "), "{adv}");
    }

    /// Every token of the fixed capability string is one this server acts on.
    /// The four shallow/deepen tokens are advertised because the boundary is
    /// computed: a client that sees `shallow` sends `deepen` lines and must get
    /// `shallow`/`unshallow` back plus a pack cut at the boundary, which is what
    /// [`crate::shallow_serve`] produces.
    #[test]
    fn advertised_capabilities_are_the_honoured_ones() {
        let caps = capabilities(None, WantPolicy::default(), true);
        for present in [
            "multi_ack",
            "multi_ack_detailed",
            "side-band",
            "side-band-64k",
            "thin-pack",
            "ofs-delta",
            "no-progress",
            "include-tag",
            "shallow",
            "deepen-since",
            "deepen-not",
            "deepen-relative",
        ] {
            assert!(caps.split(' ').any(|tok| tok == present), "{present} missing: {caps}");
        }
    }

    /// The dialect is picked off the client's first `want` line, and
    /// `multi_ack_detailed` wins over `multi_ack` when a client offers both —
    /// git tests the detailed spelling first (upload-pack.c:1125-1128). A
    /// substring match here would read `multi_ack` out of `multi_ack_detailed`
    /// and answer `continue` where the client expects `common`.
    #[test]
    fn multi_ack_dialect_is_selected_by_exact_token() {
        assert!(MultiAck::from_caps(b"multi_ack_detailed side-band-64k") == MultiAck::Detailed);
        assert!(MultiAck::from_caps(b"multi_ack side-band-64k") == MultiAck::Plain);
        assert!(MultiAck::from_caps(b"multi_ack multi_ack_detailed") == MultiAck::Detailed);
        assert!(MultiAck::from_caps(b"side-band-64k ofs-delta") == MultiAck::None);
        // A capability that merely starts with the token is not the token.
        assert!(MultiAck::from_caps(b"multi_ack_detailedx") == MultiAck::None);
    }

    /// `determine_protocol_version_server()` takes the *greatest* version the
    /// client listed, across a `:`-separated key list that may also carry keys
    /// that are not versions at all. Getting this wrong either strands every
    /// client on v0 or answers v2 to a client that only offered v0.
    #[test]
    fn git_protocol_env_selects_the_greatest_version_offered() {
        assert_eq!(protocol_version_of("version=2"), 2);
        assert_eq!(protocol_version_of("version=0"), 0);
        assert_eq!(protocol_version_of("version=1"), 1);
        // Multiple offers: the greatest wins regardless of the order they came in.
        assert_eq!(protocol_version_of("version=0:version=2:version=1"), 2);
        assert_eq!(protocol_version_of("version=2:version=1"), 2);
        // Non-version keys are skipped, not treated as a version.
        assert_eq!(protocol_version_of("key=value:version=2"), 2);
        assert_eq!(protocol_version_of("key=value"), 0);
        // Anything unparseable is v0, never a higher guess.
        assert_eq!(protocol_version_of(""), 0);
        assert_eq!(protocol_version_of("version=3"), 0);
        assert_eq!(protocol_version_of("version=two"), 0);
        assert_eq!(protocol_version_of("2"), 0);
    }

    /// The values of the v2 `fetch=` capability are the contract for what the
    /// client may then send. Every token here has honouring code in
    /// [`process_fetch_args`]; `packfile-uris` is absent because its is not
    /// written, and a regression that added it back would make clients send
    /// `packfile-uris` lines this server can only reject.
    #[test]
    fn v2_fetch_capability_lists_only_honoured_tokens() {
        let base = V2Config {
            unborn: Unborn::Advertise,
            policy: WantPolicy::default(),
            ref_in_want: false,
            sideband_all: false,
            session_id: None,
            object_info: false,
            promisor_info: None,
            object_format: "sha1".into(),
        };
        assert_eq!(base.fetch_values(), "shallow wait-for-done");

        let all = V2Config {
            policy: WantPolicy { filter: true, ..WantPolicy::default() },
            ref_in_want: true,
            sideband_all: true,
            ..base
        };
        // git's own order: filter, then ref-in-want, then sideband-all
        // (`upload_pack_advertise()`, upload-pack.c:1843-1857).
        assert_eq!(all.fetch_values(), "shallow wait-for-done filter ref-in-want sideband-all");
        assert!(!all.fetch_values().contains("packfile-uris"), "{}", all.fetch_values());
    }
}
