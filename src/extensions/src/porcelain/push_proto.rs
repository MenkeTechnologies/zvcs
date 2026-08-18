//! `git send-pack` — push objects and ref updates over the smart transfer
//! protocol, ported from git's `send-pack.c` (`send_pack()` / `receive_status()`,
//! git 2.55.0) and bridged onto gitoxide's transport.
//!
//! # What this ports
//!
//! git's `send_pack()` drives a `git-receive-pack` conversation: it reads the
//! server's capability advertisement, builds a `report-status` capability string,
//! writes the `<old> <new> <ref>` command list (the first line carrying the
//! capabilities after a NUL), streams a pack of the objects the remote lacks, and
//! parses the server's `report-status[-v2]`. In stock git the byte stream travels
//! down `fd[1]` to `git-remote-curl`, which POSTs it as
//! `application/x-git-receive-pack-request`.
//!
//! Here the same wire bytes are produced directly and the POST is performed by
//! `gix-transport`'s HTTP client: `handshake(Service::ReceivePack)` runs the GET
//! ref advertisement (with credential-helper auth), and `request()` performs the
//! POST with the receive-pack `Content-Type`. So the protocol logic is git's; the
//! transport is gitoxide's.
//!
//! # Deliberate scope
//!
//! Faithful to the common `git push over https` path: create, fast-forward and
//! forced ref updates, deletes, and `report-status` / `report-status-v2`.
//! `side-band-64k` is requested whenever the server advertises it and the
//! multiplexed stream is demultiplexed here (git's `sideband_demux` /
//! `demultiplex_sideband`), which is what puts the server's and its hooks'
//! `remote: …` lines on stderr and keeps the connection open until the server's
//! closing flush — the one it writes only after `post-receive` and `post-update`
//! have run. Push certificates (`--signed`), `--atomic` and `push-options` ARE
//! negotiated here, each refused when the server does not advertise the
//! capability rather than silently downgraded. Not ported: shallow grafts. The
//! pack is complete but undeltified (see [`super::pack_objects`]); a non-thin
//! pack is valid for receive-pack, `--thin` is only a size optimization.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};

use gix::hash::ObjectId;
use gix::odb::pack;
use gix::protocol::transport::client::blocking_io::{ExtendedBufRead, Transport};
use gix::protocol::transport::client::{MessageKind, TransportWithoutIO, WriteMode};
use gix::protocol::transport::packetline::blocking_io::encode;
use gix::protocol::transport::packetline::PacketLineRef;
use gix::protocol::transport::Service;
use gix::remote::Direction;

/// A ref update requested by the caller: set `name` on the remote to `new`,
/// forcing past a non-fast-forward only when `force` is set. `new` is the null
/// oid to delete the ref.
pub struct Request {
    /// Full remote ref name, e.g. `refs/heads/main`.
    pub name: String,
    /// The full name of the LOCAL ref this update comes from — git's
    /// `ref->peer_ref->name`, which is the left-hand side of the `<from> -> <to>`
    /// the status block prints. `None` for a deletion, which git prints with no
    /// source at all (`print_ref_status(..., from = NULL, ...)`, transport.c:691).
    pub src: Option<String>,
    /// The object id to push (null oid = delete).
    pub new: ObjectId,
    /// Whether a non-fast-forward update is permitted.
    pub force: bool,
    /// `--force-with-lease`: the value the remote ref is expected to hold. When
    /// set, this is sent as the command's old-oid so the server performs a
    /// compare-and-swap, and the local fast-forward check is skipped.
    pub expected: Option<ObjectId>,
    /// `--follow-tags` semantics: push this ref only when the remote does not
    /// advertise it at all. git filters followed tags against the advertisement
    /// ("annotated tags … that are **missing from the remote**"), and only the
    /// wire layer has that advertisement, so the porcelain marks the request and
    /// the filtering happens here. Without it a local tag object that differs
    /// from the remote's would be sent, rejected non-fast-forward, and turn an
    /// otherwise clean push into a failure.
    pub only_if_absent: bool,
    /// `--force-if-includes` / `push.useForceIfIncludes`: the remote-tracking ref
    /// the lease in [`expected`][Self::expected] was read from, set only when the
    /// lease actually came from one — git's `ref->check_reachable`, which
    /// `apply_cas()` arms on exactly that branch (remote.c:2837, :2851).
    ///
    /// When set, a lease that passes is not enough: the tip the remote advertises
    /// must also be reachable from the *local* ref's reflog, proving this
    /// checkout has seen it.
    pub check_reachable: Option<String>,
    /// A deletion the user asked for by name — `-d`/`--delete <name>` or the
    /// equivalent `:<name>` refspec. `match_explicit()` fails the whole push when
    /// such a name is not advertised, before a single command line is written:
    /// stock git sends only the flush packet and reports
    /// `error: unable to delete '<name>': remote ref does not exist`.
    pub explicit_delete: bool,
}

/// Which advertised refs a push may DELETE beyond the caller's explicit
/// requests — the `--mirror` / `--prune` half of the protocol, which can only be
/// computed here because it needs the ref advertisement.
pub enum DeleteScope {
    /// `--mirror`: every advertised ref the local repository no longer has.
    All,
    /// `--prune`: only advertised refs under these destination prefixes (the
    /// namespaces this push writes to), so an unrelated remote ref is never
    /// touched.
    Prefixes(Vec<String>),
}

/// Wire-level options that change the request itself rather than the ref list.
#[derive(Default)]
pub struct SendOptions {
    /// `--atomic`: ask the server to apply every update or none. Requires the
    /// `atomic` capability; git errors rather than silently pushing
    /// non-atomically, and so does this.
    pub atomic: bool,
    /// `-o/--push-option` values, sent as their own pkt-line section between the
    /// command list and the pack. Requires the `push-options` capability.
    pub push_options: Vec<String>,
    /// Extra deletions to synthesize from the advertisement.
    pub delete_scope: Option<DeleteScope>,
    /// Ref names the caller is pushing, used with `delete_scope` to decide which
    /// advertised refs have no local counterpart.
    pub local_refs: HashSet<String>,
    /// `--signed`: send a push certificate signed with gpg.
    pub signed: Signed,
    /// `--receive-pack=<path>` / `--exec=<path>`, else `remote.<name>.receivepack`:
    /// the program to run in place of `git-receive-pack` on the other end. git
    /// hands it to `git_connect()` for a push (`connect_setup()`, transport.c:314).
    pub receive_pack: Option<String>,
}

/// `--signed=<mode>` — git's `SEND_PACK_PUSH_CERT_*`.
#[derive(Default, Clone, Copy, PartialEq)]
pub enum Signed {
    /// `--signed=false` (the default): never send a certificate.
    #[default]
    Never,
    /// `--signed=if-asked`: send one only when the server advertises a nonce.
    IfAsked,
    /// `--signed` / `--signed=true`: send one, and fail if the server cannot
    /// take it.
    Always,
}

/// The server's per-ref verdict, from `report-status`.
pub struct RefStatus {
    /// The ref the push asked to update — git's `ref->name`.
    pub name: String,
    /// The local ref this update came from ([`Request::src`]).
    pub src: Option<String>,
    /// `option refname <name>` from `report-status-v2`: the ref that really moved,
    /// which a `proc-receive` hook can make different from [`name`][Self::name].
    /// git prefers it over `ref->name` everywhere it names the destination
    /// (`print_ref_status`, `print_ok_ref_status`, `transport_update_tracking_ref`).
    pub report_name: Option<String>,
    /// The remote's value before the update (null if the ref was created), after
    /// any `option old-oid` override.
    pub old: ObjectId,
    /// The value we asked it to take (null for a delete), after any
    /// `option new-oid` override.
    pub new: ObjectId,
    /// `Ok(())` on `ok`, `Err(reason)` on `ng` or a locally-rejected update.
    pub result: Result<(), String>,
    /// True when the update overwrote a non-descendant (`--force`).
    pub forced: bool,
    /// True when the local pre-flight found nothing to do (already up to date).
    pub up_to_date: bool,
    /// Rejected during matching rather than by the server: git reports these as a
    /// bare `error: <reason>` before any transport output, so they never appear in
    /// the `To <url>` block.
    pub pre_transport: bool,
    /// `REF_STATUS_REMOTE_REJECT`: the refusal came back from the server as an
    /// `ng <ref> <reason>` line rather than being decided here, which is the
    /// difference between git's `[remote rejected]` and `[rejected]` summaries.
    pub remote_rejected: bool,
}

/// The outcome of a push: the resolved destination URL and every ref's verdict.
pub struct Outcome {
    pub url: String,
    pub statuses: Vec<RefStatus>,
    /// `unpack ok`, or the server's failure reason.
    pub unpack: Result<(), String>,
}


/// The certificate's signed payload — split out from the signing so the exact
/// bytes can be asserted without a gpg key in the loop.
fn push_cert_payload(
    pusher: &str,
    url: &str,
    nonce: &str,
    commands: &[(ObjectId, ObjectId, String)],
    push_options: &[String],
) -> String {
    let mut cert = String::new();
    cert.push_str("certificate version 0.1\n");
    cert.push_str(&format!("pusher {pusher}\n"));
    cert.push_str(&format!("pushee {url}\n"));
    cert.push_str(&format!("nonce {nonce}\n"));
    for opt in push_options {
        cert.push_str(&format!("push-option {opt}\n"));
    }
    cert.push('\n');
    for (old, new, name) in commands {
        cert.push_str(&format!("{old} {new} {name}\n"));
    }
    cert
}

/// Build a signed push certificate over `wire`, ported from git's
/// `generate_push_cert` (send-pack.c).
///
/// The signed payload is exactly:
///
/// ```text
/// certificate version 0.1
/// pusher <ident> <timestamp> <tz>
/// pushee <url>
/// nonce <nonce>
/// [push-option <opt>]…
///
/// <old> <new> <ref>…
/// ```
///
/// followed by the armored detached signature. The blank line is part of the
/// signed bytes, and so is the trailing newline of every command — a certificate
/// the server cannot verify byte-for-byte is worse than no certificate, so
/// nothing here is reformatted on the way out.
///
/// The `pusher` line names the SIGNING KEY, not the committer:
/// `generate_push_cert` writes `get_signing_key_id()` followed by `datestamp()`
/// (send-pack.c:368-370). For openpgp/x509 that is `user.signingKey` when set and
/// the committer ident otherwise; for `gpg.format = ssh` it is the key's
/// `ssh-keygen -lf` fingerprint. A receiving `git receive-pack` records the line
/// verbatim, so getting it wrong misattributes every signed push.
fn build_push_cert(
    repo: &gix::Repository,
    url: &str,
    nonce: &str,
    commands: &[(ObjectId, ObjectId, String)],
    push_options: &[String],
) -> Result<Vec<u8>> {
    let signer = crate::gitsig::Signer::resolve(repo);
    let key_id = signer.signing_key_id().map_err(sign_failure)?;
    // `datestamp()` (date.c): the current time and this machine's UTC offset, in
    // the same `<seconds> <+hhmm>` shape an ident carries.
    let cert = push_cert_payload(
        &format!("{key_id} {}", datestamp()),
        url,
        nonce,
        commands,
        push_options,
    );

    let signature = signer.sign(cert.as_bytes()).map_err(sign_failure)?;

    let mut out = cert.into_bytes();
    out.extend_from_slice(&signature);
    Ok(out)
}

/// `send-pack.c:379`: `if (sign_buffer(...)) die(_("failed to sign the push
/// certificate"))`. `sign_buffer` has already written its own `error: …` line by
/// then, so the only thing left is the `die` — unless the backend died itself, in
/// which case its message *is* the last word.
fn sign_failure(e: crate::gitsig::SignFailure) -> anyhow::Error {
    match e {
        crate::gitsig::SignFailure::Silent => {
            anyhow::Error::new(crate::fatal::Silent(crate::fatal::EXIT_FATAL))
        }
        crate::gitsig::SignFailure::Fatal(m) => crate::fatal::die(m),
        crate::gitsig::SignFailure::Error(m) => {
            eprintln!("{}", crate::gitsig::report("error: ", &m));
            crate::fatal::die("failed to sign the push certificate")
        }
    }
}

/// git's `datestamp()` (date.c): `<seconds-since-epoch> <+hhmm>` for right now,
/// with this machine's local UTC offset — the second half of the certificate's
/// `pusher` line.
fn datestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let offset_minutes = local_utc_offset_seconds(now) / 60;
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let abs = offset_minutes.abs();
    format!("{now} {sign}{:02}{:02}", abs / 60, abs % 60)
}

/// `local_time_tzoffset()` (date.c): this machine's offset from UTC at `time`,
/// in seconds, as `localtime_r` reports it.
fn local_utc_offset_seconds(time: i64) -> i64 {
    let t = time as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` reads `t` and writes `tm`, both live locals of the
    // right types, and is reentrant.
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return 0;
    }
    tm.tm_gmtoff as i64
}

/// Push `requests` to `remote` over receive-pack, returning each ref's verdict.
///
/// Ports `send_pack()`: negotiate capabilities, emit the command list, build and
/// stream the pack, and parse `report-status`. Ref updates that fail the local
/// fast-forward check are reported rejected and excluded from the wire request,
/// exactly as git's `set_ref_status_for_push` does before the pack is sent.
pub fn send_pack(
    repo: &gix::Repository,
    remote: &gix::Remote<'_>,
    requests: &[Request],
    dry_run: bool,
    opts: &SendOptions,
) -> Result<Outcome> {
    let null = ObjectId::null(repo.object_hash());
    let url = remote
        .url(Direction::Push)
        .or_else(|| remote.url(Direction::Fetch))
        .map(|u| u.to_bstring().to_string())
        .unwrap_or_default();

    // Open the push transport and run the receive-pack handshake (the GET ref
    // advertisement), authenticating through the repository's credential helper
    // exactly as gitoxide's fetch does. `--receive-pack`/`remote.<name>.receivepack`
    // has to be handed to the connect, not to the handshake: it names the program
    // the other end runs, which only exists as a choice while the connection is
    // being made (`connect_setup()`, transport.c:310-317).
    let mut connection = remote.connect_with_options(
        Direction::Push,
        gix::remote::connect::Options {
            receive_pack: super::fetch::local_service_program(
                remote.url(Direction::Push).or_else(|| remote.url(Direction::Fetch)),
                opts.receive_pack.as_deref().map(Into::into),
                "receive-pack",
            ),
            ..Default::default()
        },
    )?;
    let mut authenticate = connection.configured_credentials_for_current_url();
    let transport = connection.transport_mut();
    // Apply the repository's transport configuration (user agent, http.* options)
    // the same way gix's ref_map does before a handshake.
    if let Ok(Some(config)) = repo.transport_options(url.as_str(), None) {
        transport.configure(&*config).ok();
    }

    let handshake = match gix::protocol::handshake(
        &mut *transport,
        Service::ReceivePack,
        &mut authenticate,
        Vec::new(),
        &mut gix::progress::Discard,
    ) {
        Ok(h) => h,
        Err(e) => {
            let err = anyhow::Error::from(e).context("receive-pack handshake failed");
            // An ssh transport that never connected exits the way git's does.
            // `ssh_fatal` has already written git's block; this layer returns a
            // `Result` rather than an exit code, so the status is set directly.
            if crate::transport_err::ssh_fatal(url.as_str(), &err).is_some() {
                std::process::exit(128);
            }
            return Err(err);
        }
    };

    // Map every advertised ref to its tip so we can fill in each update's old
    // value (git's `remote_refs`, matched against `refs->name`).
    let mut advertised: HashMap<String, ObjectId> = HashMap::new();
    if let Some(refs) = &handshake.refs {
        for r in refs {
            let (name, target, _peeled) = r.unpack();
            if let (Ok(name), Some(oid)) = (std::str::from_utf8(name), target) {
                advertised.insert(name.to_owned(), oid.to_owned());
            }
        }
    }

    // git's capability selection (send-pack.c, "Does the other end support…"):
    // prefer report-status-v2, fall back to report-status; request side-band-64k
    // whenever it is offered; advertise the hash algorithm and agent.
    let caps = &handshake.capabilities;
    let status_report = if caps.contains("report-status-v2") {
        Some(2u8)
    } else if caps.contains("report-status") {
        Some(1u8)
    } else {
        None
    };
    let allow_deleting_refs = caps.contains("delete-refs");
    let object_format_supported = caps.contains("object-format");
    // `if (server_supports("side-band-64k")) use_sideband = 1;` (send-pack.c:573).
    // git asks for it unconditionally, and it is what carries every `remote:` line
    // back — including everything the server's hooks wrote — and what keeps the
    // conversation open until the server's own closing flush.
    let use_sideband = caps.contains("side-band-64k");

    let mut cap_buf = String::new();
    match status_report {
        Some(2) => cap_buf.push_str(" report-status-v2"),
        Some(1) => cap_buf.push_str(" report-status"),
        _ => {}
    }
    if use_sideband {
        cap_buf.push_str(" side-band-64k");
    }
    if object_format_supported {
        cap_buf.push_str(&format!(" object-format={}", repo.object_hash()));
    }
    // `--atomic` and `-o` are refused rather than downgraded: git errors when the
    // receiving end lacks the capability, because pushing non-atomically (or
    // dropping the options) would silently do something other than what was asked.
    if opts.atomic {
        if !caps.contains("atomic") {
            crate::git_fatal!("the receiving end does not support --atomic push");
        }
        cap_buf.push_str(" atomic");
    }
    if !opts.push_options.is_empty() {
        if !caps.contains("push-options") {
            crate::git_fatal!("the receiving end does not support push options");
        }
        cap_buf.push_str(" push-options");
    }
    // `push-cert=<nonce>`: the server hands out a nonce that the certificate has
    // to quote back, which is what stops a captured certificate from being
    // replayed against a different push.
    let nonce = caps
        .capability("push-cert")
        .and_then(|c| c.value())
        .map(|v| v.to_string())
        .filter(|v| !v.is_empty());
    let sign_cert = match opts.signed {
        Signed::Never => false,
        Signed::IfAsked => nonce.is_some(),
        Signed::Always => {
            if nonce.is_none() {
                crate::git_fatal!("the receiving end does not support --signed push");
            }
            true
        }
    };
    if sign_cert {
        cap_buf.push_str(" push-cert");
    }
    cap_buf.push_str(&format!(" agent={}", agent()));

    // Resolve each requested update against the advertisement, running git's
    // pre-flight fast-forward / delete checks. Rejected updates are reported but
    // never put on the wire (send-pack.c `check_to_send_update`).
    struct Wire {
        name: String,
        src: Option<String>,
        old: ObjectId,
        new: ObjectId,
        forced: bool,
    }
    let mut wire: Vec<Wire> = Vec::new();
    let mut statuses: Vec<RefStatus> = Vec::new();
    for req in requests {
        // A followed tag (`--follow-tags`) the remote already carries is dropped
        // outright — not reported, not sent — exactly as git omits it from the
        // ref list it builds after reading the advertisement.
        if req.only_if_absent && advertised.contains_key(&req.name) {
            continue;
        }
        // The remote's current value of the ref; `--force-with-lease` overrides
        // the old-oid we send with the leased value so the server compare-and-swaps.
        let remote_current = advertised.get(&req.name).copied().unwrap_or(null);
        let old = req.expected.unwrap_or(remote_current);
        let force = req.force || req.expected.is_some();
        let deletion = req.new == null;

        let reject = |reason: &str| RefStatus {
            name: req.name.clone(),
            src: req.src.clone(),
            report_name: None,
            old,
            new: req.new,
            result: Err(reason.to_owned()),
            forced: false,
            up_to_date: false,
            pre_transport: false,
            remote_rejected: false,
        };

        if deletion && !allow_deleting_refs {
            statuses.push(reject("remote does not support deleting refs"));
            continue;
        }
        // A deletion whose destination the remote does not advertise and which the
        // user wrote unqualified fails during matching (`match_explicit()`), before
        // anything is sent: git reports `unable to delete '<name>': remote ref does
        // not exist` and the push fails. A `refs/`-qualified destination instead
        // becomes a new linked ref and its null→null command goes out on the wire,
        // which the branch below lets through.
        if deletion && req.explicit_delete && remote_current == null {
            statuses.push(RefStatus {
                result: Err(format!(
                    "unable to delete '{}': remote ref does not exist",
                    req.name.strip_prefix("refs/heads/").unwrap_or(&req.name)
                )),
                pre_transport: true,
                ..reject("")
            });
            continue;
        }
        // A deletion is always sent, even when the remote has nothing at that name:
        // `match_explicit()` made a linked ref for it, so `send_pack()` writes the
        // null→null command and the server answers it (with a
        // `warning: deleting a non-existent ref` on band 2). Only a non-deletion
        // that already matches is dropped as up to date.
        if remote_current == req.new && !deletion {
            // Nothing to do — git reports this ref up to date and sends no command.
            statuses.push(RefStatus {
                name: req.name.clone(),
                src: req.src.clone(),
                report_name: None,
                old: remote_current,
                new: req.new,
                result: Ok(()),
                forced: false,
                up_to_date: true,
                pre_transport: false,
                remote_rejected: false,
            });
            continue;
        }

        // `--force-if-includes` (remote.c:1698-1711), checked before the
        // fast-forward rules and after the lease itself. A lease that no longer
        // matches what the remote advertises is left to the server's
        // compare-and-swap, which is where this port does the staleness check;
        // when it *does* match, the advertised tip must additionally be reachable
        // from the local ref's reflog, or this checkout never saw the update.
        // `--force` defeats the rejection, as it defeats every other one
        // (remote.c:1750-1753).
        let mut forced = false;
        if let (Some(expected), Some(tracking)) = (req.expected, req.check_reachable.as_deref()) {
            if remote_current == expected
                && remote_current != null
                && !is_reachable_in_reflog(repo, &req.name, tracking, remote_current)
            {
                if !req.force {
                    statuses.push(reject("remote ref updated since checkout"));
                    continue;
                }
                forced = true;
            }
        }

        // `set_ref_status_for_push()`'s "must fast-forward" ladder
        // (remote.c:1734-1745), in git's order — the first rung that fires is the
        // rejection, and each maps to the summary `print_ref_status()` prints for
        // that `REF_STATUS_REJECT_*` (transport.c:752-772). Creating a ref and
        // deleting one are never rejected here.
        let reject_reason = if deletion || remote_current == null {
            None
        } else if req.name.starts_with("refs/tags/") {
            // A tag the remote already has: overwriting it is never a
            // fast-forward question, it is a clobber.
            Some("already exists")
        } else if repo.find_object(remote_current).is_err() {
            // We do not have what the remote is on, so we cannot prove anything
            // about it — `odb_has_object()` fails before the commit lookups.
            Some("fetch first")
        } else if !peels_to_commit(repo, remote_current) || !peels_to_commit(repo, req.new) {
            // `lookup_commit_reference_gently()` on either side: with a non-commit
            // involved there is no ancestry to test.
            Some("needs force")
        } else if is_fast_forward(repo, remote_current, req.new) != Some(true) {
            Some("non-fast-forward")
        } else {
            None
        };
        // "`--force` will defeat any rejection implemented by the rules above"
        // (remote.c:1747-1753) — it only records that the update was forced, which
        // is the leading `+` in the report.
        match reject_reason {
            Some(reason) if !force => {
                statuses.push(reject(reason));
                continue;
            }
            Some(_) => forced = true,
            None => {}
        }

        wire.push(Wire {
            name: req.name.clone(),
            src: req.src.clone(),
            old,
            new: req.new,
            forced,
        });
    }

    // `--dry-run`: everything up to the wire request has run (handshake, the local
    // fast-forward checks above), but nothing is sent. Report the surviving updates
    // as they would land, exactly as git's dry run does.
    if dry_run {
        for w in &wire {
            statuses.push(RefStatus {
                name: w.name.clone(),
                src: w.src.clone(),
                report_name: None,
                old: w.old,
                new: w.new,
                result: Ok(()),
                forced: w.forced,
            up_to_date: false,
            pre_transport: false,
            remote_rejected: false,
        });
        }
        return Ok(Outcome {
            url,
            statuses,
            unpack: Ok(()),
        });
    }

    // `--mirror` / `--prune`: an advertised ref with no local counterpart is
    // deleted. This has to happen here rather than in the porcelain because only
    // the handshake knows what the remote actually has. `delete-refs` is required
    // for the same reason git requires it — without it the deletion cannot be
    // expressed on the wire at all.
    if let Some(scope) = &opts.delete_scope {
        let requested: HashSet<&str> = wire.iter().map(|w| w.name.as_str()).collect();
        let mut doomed: Vec<(&String, &ObjectId)> = advertised
            .iter()
            .filter(|(name, _)| !opts.local_refs.contains(name.as_str()))
            .filter(|(name, _)| !requested.contains(name.as_str()))
            .filter(|(name, _)| match scope {
                DeleteScope::All => true,
                DeleteScope::Prefixes(prefixes) => {
                    prefixes.iter().any(|p| name.starts_with(p.as_str()))
                }
            })
            .collect();
        // Deterministic order so the status block reads the same run to run.
        doomed.sort_by(|a, b| a.0.cmp(b.0));
        for (name, old) in doomed {
            if !allow_deleting_refs {
                statuses.push(RefStatus {
                    name: name.clone(),
                    src: None,
                    report_name: None,
                    old: *old,
                    new: null,
                    result: Err("remote does not support deleting refs".to_owned()),
                    forced: false,
            up_to_date: false,
            pre_transport: false,
            remote_rejected: false,
        });
                continue;
            }
            // Only the wire entry: the per-ref status is produced from the
            // server report alongside every other command, so pushing one here
            // too would report the deletion twice.
            wire.push(Wire {
                name: name.clone(),
                src: None,
                old: *old,
                new: null,
                forced: true,
            });
        }
    }

    // Nothing survived the checks: no request to send. Report what we have.
    if wire.is_empty() {
        return Ok(Outcome {
            url,
            statuses,
            unpack: Ok(()),
        });
    }

    // Build the command list. The first command carries the capability string
    // after a NUL; the rest are bare (send-pack.c `packet_buf_write`).
    let mut req_buf: Vec<u8> = Vec::new();
    if sign_cert {
        // git's `generate_push_cert`: a header block (version, pusher, pushee,
        // nonce, then any push options), a blank line, the same `<old> <new>
        // <ref>` command lines the unsigned form would send, and a gpg
        // signature over exactly those bytes. The whole thing rides between
        // `push-cert` and `push-cert-end` pkt-lines, with the capability string
        // attached to the OPENING line rather than to a command.
        let commands: Vec<(ObjectId, ObjectId, String)> =
            wire.iter().map(|w| (w.old, w.new, w.name.clone())).collect();
        let cert = build_push_cert(
            repo,
            &url,
            nonce.as_deref().unwrap_or_default(),
            &commands,
            &opts.push_options,
        )?;
        encode::data_to_write(format!("push-cert\0{cap_buf}").as_bytes(), &mut req_buf)?;
        for line in cert.split_inclusive(|b| *b == b'\n') {
            encode::data_to_write(line, &mut req_buf)?;
        }
        encode::data_to_write(b"push-cert-end\n", &mut req_buf)?;
    } else {
        for (i, w) in wire.iter().enumerate() {
            let line = if i == 0 {
                format!("{} {} {}\0{}", w.old, w.new, w.name, cap_buf)
            } else {
                format!("{} {} {}", w.old, w.new, w.name)
            };
            encode::data_to_write(line.as_bytes(), &mut req_buf)?;
        }
    }
    encode::write_packet_line(&PacketLineRef::Flush, &mut req_buf)?;

    // Push options ride in their own pkt-line section between the command list's
    // flush and the pack, one option per line, terminated by a flush (git's
    // `send_pack()` after `advertise_push_options`).
    if !opts.push_options.is_empty() {
        for opt in &opts.push_options {
            encode::data_to_write(opt.as_bytes(), &mut req_buf)?;
        }
        encode::write_packet_line(&PacketLineRef::Flush, &mut req_buf)?;
    }

    // Build the pack of objects the remote lacks: everything reachable from the
    // new tips, minus everything reachable from the advertised/old tips it already
    // has (git's `pack-objects --revs <new> --not <haves>`). A delete needs no
    // pack.
    let need_pack = wire.iter().any(|w| w.new != null);
    let pack_bytes = if need_pack {
        let wants: Vec<ObjectId> = wire
            .iter()
            .filter(|w| w.new != null)
            .map(|w| w.new)
            .collect();
        // Haves are everything the remote advertised plus the old tips, restricted
        // to objects we actually hold locally (git's `feed_object(negative)` skips
        // objects the local odb lacks).
        let mut haves: Vec<ObjectId> = advertised.values().copied().collect();
        haves.extend(wire.iter().map(|w| w.old).filter(|o| *o != null));
        let objects = objects_to_send(repo, &wants, &haves);
        super::pack_objects::pack_bytes_for(repo, &objects)?
    } else {
        Vec::new()
    };

    // POST the request: command list + flush + pack, written verbatim (the pack is
    // not pkt-line framed). `into_parts` hands back the raw writer and the response
    // reader; the writer must be dropped before the response is read.
    let (mut writer, mut reader) = transport
        .request(WriteMode::Binary, MessageKind::Flush, false)?
        .into_parts();
    writer.write_all(&req_buf)?;
    if need_pack {
        writer.write_all(&pack_bytes)?;
    }
    writer.flush()?;
    drop(writer);

    // Read the response. With `side-band-64k` the whole stream is multiplexed:
    // band 1 carries the report-status pkt-lines, bands 2 and 3 carry everything
    // the server and its hooks wrote, and the closing flush comes only after
    // `post-receive` and `post-update` have run. git forks `sideband_demux`
    // (send-pack.c:284) so the two arrive in parallel and then joins it with
    // `finish_async()` before printing the status block; draining the stream here
    // and parsing the primary band afterwards is the same sequence, because the
    // server writes the entire report before those hooks start and the client has
    // nothing to say in between.
    let report_lines: Vec<String> = if use_sideband {
        pkt_lines(&demultiplex_sideband(repo, &mut reader)?)
    } else {
        read_pkt_lines(&mut reader)?
    };

    // Parse report-status (send-pack.c `receive_status`). The first line is the
    // unpack status; each following `ok`/`ng <ref>` updates that ref's verdict, and
    // under `report-status-v2` an `ok` may be followed by `option` lines describing
    // what the server actually did.
    let unpack;
    let mut remote_status: HashMap<String, RemoteVerdict> = HashMap::new();
    if status_report.is_some() {
        let mut lines = report_lines.into_iter();
        // First pkt-line: "unpack ok" or "unpack <error>".
        match lines.next() {
            Some(text) => {
                let text = text.trim_end();
                unpack = match text.strip_prefix("unpack ") {
                    Some("ok") => Ok(()),
                    Some(err) => Err(err.to_owned()),
                    None => Err(format!("unable to parse remote unpack status: {text}")),
                };
            }
            None => crate::git_fatal!("unexpected flush packet while reading remote unpack status"),
        }
        // `hint`, the ref the `option` lines that follow belong to.
        let mut hint: Option<String> = None;
        let mut new_report = false;
        for text in lines {
            let text = text.trim_end();
            let Some((head, rest)) = text.split_once(' ') else {
                continue;
            };
            if head == "option" {
                // `'option' without a matching 'ok/ng' directive` — git errors and
                // keeps reading, which is what skipping does here.
                let Some(name) = hint.as_ref() else { continue };
                let Some(verdict) = remote_status.get_mut(name) else { continue };
                if verdict.result.is_err() {
                    continue;
                }
                if new_report {
                    verdict.reports.push(PushReport::default());
                    new_report = false;
                }
                let Some(report) = verdict.reports.last_mut() else { continue };
                let (key, val) = rest.split_once(' ').unwrap_or((rest, ""));
                match key {
                    "refname" => report.ref_name = Some(val.to_owned()),
                    "old-oid" => report.old = ObjectId::from_hex(val.as_bytes()).ok(),
                    "new-oid" => report.new = ObjectId::from_hex(val.as_bytes()).ok(),
                    "forced-update" => report.forced_update = true,
                    _ => {}
                }
                continue;
            }
            new_report = false;
            let (name, extra) = rest.split_once(' ').unwrap_or((rest, ""));
            match head {
                "ok" => {
                    hint = Some(name.to_owned());
                    // Every `ok <ref>` opens a fresh report slot: a proc-receive
                    // hook reports one per ref it created, all under the one ref
                    // the client asked for.
                    remote_status
                        .entry(name.to_owned())
                        .or_default()
                        .result = Ok(());
                    new_report = true;
                }
                "ng" => {
                    hint = Some(name.to_owned());
                    let reason = if extra.is_empty() { "failed" } else { extra };
                    remote_status.entry(name.to_owned()).or_default().result =
                        Err(reason.to_owned());
                }
                _ => {}
            }
        }
    } else {
        unpack = Ok(());
    }

    // Fold the server verdicts into the per-ref statuses. With no report-status
    // capability, git optimistically marks everything ok. A ref the server
    // reported through `option refname` produces one status per report, exactly as
    // `print_one_push_report` prints one line per `ref_push_report`.
    for w in &wire {
        let (result, reports) = match remote_status.remove(&w.name) {
            Some(v) => (v.result, v.reports),
            None if status_report.is_none() => (Ok(()), Vec::new()),
            None => (Err("remote end did not report status".into()), Vec::new()),
        };
        if reports.is_empty() {
            statuses.push(RefStatus {
                name: w.name.clone(),
                src: w.src.clone(),
                report_name: None,
                old: w.old,
                new: w.new,
                result,
                forced: w.forced,
                up_to_date: false,
                pre_transport: false,
                remote_rejected: true,
            });
            continue;
        }
        for report in reports {
            statuses.push(RefStatus {
                name: w.name.clone(),
                src: w.src.clone(),
                report_name: report.ref_name,
                old: report.old.unwrap_or(w.old),
                new: report.new.unwrap_or(w.new),
                result: result.clone(),
                forced: w.forced || report.forced_update,
                up_to_date: false,
                pre_transport: false,
                remote_rejected: true,
            });
        }
    }

    Ok(Outcome {
        url,
        statuses,
        unpack,
    })
}

/// One `ref_push_report` (remote.h): what the server says it actually did with a
/// ref, which a `proc-receive` hook can make differ from what the client asked for.
#[derive(Default)]
struct PushReport {
    /// `option refname <name>`.
    ref_name: Option<String>,
    /// `option old-oid <oid>`.
    old: Option<ObjectId>,
    /// `option new-oid <oid>`.
    new: Option<ObjectId>,
    /// `option forced-update`.
    forced_update: bool,
}

/// The server's verdict for one ref plus the `report-status-v2` reports attached
/// to it — `hint->status`/`hint->remote_status` and the `hint->report` chain.
struct RemoteVerdict {
    result: Result<(), String>,
    reports: Vec<PushReport>,
}

impl Default for RemoteVerdict {
    fn default() -> Self {
        RemoteVerdict {
            result: Ok(()),
            reports: Vec::new(),
        }
    }
}

/// Read every pkt-line of the response up to the first flush, as text. Ports the
/// `packet_reader_read` loop's `PACKET_READ_NORMAL` handling for the plain
/// (non-multiplexed) stream a server without `side-band-64k` sends.
fn read_pkt_lines(reader: &mut Box<dyn ExtendedBufRead<'_> + Unpin + '_>) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    loop {
        match reader.readline() {
            None => break,
            Some(Ok(Ok(PacketLineRef::Data(data)))) => {
                lines.push(String::from_utf8_lossy(data).into_owned());
            }
            // Flush / delimiter / response-end all terminate the report.
            Some(Ok(Ok(_))) => break,
            Some(Ok(Err(e))) => crate::git_fatal!("malformed packet line from remote: {e}"),
            Some(Err(e)) => bail!("error reading from remote: {e}"),
        }
    }
    Ok(lines)
}

/// Split a buffer of pkt-lines into their payloads as text, stopping at the first
/// flush/delim. This is what the demultiplexed band-1 stream is: the server
/// chunks the report across side-band packets with no regard for pkt-line
/// boundaries, so the framing can only be read back once the band is reassembled.
fn pkt_lines(buf: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut i = 0;
    while i + 4 <= buf.len() {
        let Ok(hex) = std::str::from_utf8(&buf[i..i + 4]) else { break };
        let Ok(len) = usize::from_str_radix(hex, 16) else { break };
        // 0000/0001/0002 are flush, delim and response-end; all end the report.
        if len < 4 {
            break;
        }
        if i + len > buf.len() {
            break;
        }
        lines.push(String::from_utf8_lossy(&buf[i + 4..i + len]).into_owned());
        i += len;
    }
    lines
}

/// `recv_sideband()` (pkt-line.c) driving `demultiplex_sideband()` (sideband.c:301):
/// read the multiplexed response to its closing flush, writing band 2 (progress)
/// and band 3 (error) to stderr as `remote: …` lines and returning the band-1
/// payload, which is the primary stream — here, the `report-status` pkt-lines.
///
/// Reading to that flush is also what keeps the connection alive long enough for
/// the server to finish: `receive-pack` writes it as the very last byte of the
/// session, after `post-receive` and `post-update` have run.
fn demultiplex_sideband(
    repo: &gix::Repository,
    reader: &mut Box<dyn ExtendedBufRead<'_> + Unpin + '_>,
) -> Result<Vec<u8>> {
    let mut sideband = Sideband::new(repo);
    let mut primary: Vec<u8> = Vec::new();
    loop {
        match reader.readline() {
            // EOF without a flush: `PACKET_READ_EOF` — git reports the disconnect
            // and stops.
            None => {
                sideband.finish();
                break;
            }
            Some(Ok(Ok(PacketLineRef::Data(data)))) => match data.split_first() {
                Some((3, text)) => sideband.remote_error(text),
                Some((2, text)) => sideband.progress(text),
                Some((1, payload)) => primary.extend_from_slice(payload),
                // `protocol error: missing sideband designator` / `bad band #n`.
                _ => {
                    sideband.finish();
                    crate::git_fatal!("protocol error: bad side-band packet from remote");
                }
            },
            // The flush that ends the multiplexed stream — `SIDEBAND_FLUSH`.
            Some(Ok(Ok(_))) => {
                sideband.finish();
                break;
            }
            Some(Ok(Err(e))) => {
                sideband.finish();
                crate::git_fatal!("malformed packet line from remote: {e}");
            }
            Some(Err(e)) => {
                sideband.finish();
                bail!("error reading from remote: {e}");
            }
        }
    }
    Ok(primary)
}

/// The state `demultiplex_sideband()` keeps between packets: the `remote: `
/// prefix and clear-to-eol suffix it picked once from stderr's terminal-ness, the
/// `scratch` buffer holding a line that straddles a packet boundary, and the
/// colors `maybe_colorize_sideband()` paints keywords with.
struct Sideband {
    /// `prefix` — `"\033[K" "remote: "` on a real terminal, plain `"remote: "` otherwise.
    prefix: &'static str,
    /// `suffix` — empty on a terminal (the ANSI prefix already clears the line),
    /// `DUMB_SUFFIX`'s eight spaces otherwise.
    suffix: &'static str,
    /// The partial line carried over from the previous packet.
    scratch: String,
    colors: SidebandColors,
}

/// `keywords[]` (sideband.c:23) as resolved SGR sequences, plus whether to use
/// them at all.
struct SidebandColors {
    /// `want_color_stderr(use_sideband_colors())`.
    enabled: bool,
    /// `color.remote.hint`, default yellow.
    hint: String,
    /// `color.remote.warning`, default bold yellow.
    warning: String,
    /// `color.remote.success`, default bold green.
    success: String,
    /// `color.remote.error`, default bold red.
    error: String,
}

impl SidebandColors {
    /// `use_sideband_colors()` (sideband.c:110): `color.remote`, falling back to
    /// `color.ui` and then to `auto`, with `auto` decided by stderr. Note this is
    /// the one diagnostics slot that DOES consult `color.ui`.
    fn resolve(repo: &gix::Repository) -> Self {
        let snapshot = repo.config_snapshot();
        let raw = snapshot
            .string("color.remote")
            .or_else(|| snapshot.string("color.ui"))
            .map(|v| v.to_string());
        let enabled = match raw.as_deref() {
            Some(v) if v.eq_ignore_ascii_case("always") => true,
            Some(v) if v.eq_ignore_ascii_case("never") => false,
            // Everything else runs through `git_config_bool`: true means `auto`,
            // false means off. An unset key is `auto` too.
            Some(v) if !boolean_value(v) => false,
            _ => std::io::stderr().is_terminal() && !terminal_is_dumb(),
        };
        let slot = |key: &str, default_spec: &str| {
            super::color::slot(&snapshot, &format!("color.remote.{key}"), default_spec)
        };
        SidebandColors {
            enabled,
            hint: slot("hint", "yellow"),
            warning: slot("warning", "bold yellow"),
            success: slot("success", "bold green"),
            error: slot("error", "bold red"),
        }
    }

    /// The keyword table in git's order, which is also the order it matches in.
    fn keywords(&self) -> [(&'static str, &str); 4] {
        [
            ("hint", self.hint.as_str()),
            ("warning", self.warning.as_str()),
            ("success", self.success.as_str()),
            ("error", self.error.as_str()),
        ]
    }
}

/// git's `git_config_bool` for a non-`always`/`never` color value.
fn boolean_value(v: &str) -> bool {
    if v.is_empty() {
        return false;
    }
    for t in ["true", "yes", "on", "auto"] {
        if v.eq_ignore_ascii_case(t) {
            return true;
        }
    }
    for f in ["false", "no", "off"] {
        if v.eq_ignore_ascii_case(f) {
            return false;
        }
    }
    v.parse::<i64>().map(|n| n != 0).unwrap_or(false)
}

/// git's `is_terminal_dumb`.
fn terminal_is_dumb() -> bool {
    match std::env::var("TERM") {
        Ok(term) => term == "dumb",
        Err(_) => true,
    }
}

impl Sideband {
    fn new(repo: &gix::Repository) -> Self {
        // `if (isatty(2) && !is_terminal_dumb())` — decided once per process in git,
        // and once per demultiplexed stream here.
        let tty = std::io::stderr().is_terminal() && !terminal_is_dumb();
        Sideband {
            prefix: if tty { "\x1b[Kremote: " } else { "remote: " },
            suffix: if tty { "" } else { "        " },
            scratch: String::new(),
            colors: SidebandColors::resolve(repo),
        }
    }

    /// `case 3:` — a remote error. git prints the whole packet as one `remote: `
    /// line; there is no line splitting on this band.
    fn remote_error(&mut self, text: &[u8]) {
        if !self.scratch.is_empty() {
            self.scratch.push('\n');
        }
        self.scratch.push_str(self.prefix);
        let text = String::from_utf8_lossy(text);
        let colored = self.colorize(text.trim_end_matches(['\n', '\r']));
        self.scratch.push_str(&colored);
        self.flush_scratch(true);
    }

    /// `case 2:` — progress text. Split on `\n`/`\r`, prefix each complete line,
    /// append the clear-to-eol suffix to the nonempty ones, and write it out as
    /// soon as it is complete so a hook's output appears while it is still running.
    /// A trailing partial line stays in `scratch` for the next packet.
    fn progress(&mut self, text: &[u8]) {
        let text = String::from_utf8_lossy(text);
        let mut rest = text.as_ref();
        while let Some(at) = rest.find(['\n', '\r']) {
            let (line, tail) = rest.split_at(at);
            let brk = tail.as_bytes()[0] as char;
            rest = &tail[1..];

            // A packet boundary in the middle of a line leaves `scratch` nonempty;
            // a leading break then needs the clear-to-eol to wipe what is already
            // on that terminal line.
            if !self.scratch.is_empty() && line.is_empty() {
                self.scratch.push_str(self.suffix);
            }
            if self.scratch.is_empty() {
                self.scratch.push_str(self.prefix);
            }
            // An empty line keeps no suffix: a lone `\n` after a run of `\r`
            // progress updates must not wipe the final status it just drew.
            if !line.is_empty() {
                let colored = self.colorize(line);
                self.scratch.push_str(&colored);
                self.scratch.push_str(self.suffix);
            }
            self.scratch.push(brk);
            self.flush_scratch(false);
        }
        if !rest.is_empty() {
            if self.scratch.is_empty() {
                self.scratch.push_str(self.prefix);
            }
            let colored = self.colorize(rest);
            self.scratch.push_str(&colored);
        }
    }

    /// The `cleanup:` tail of `demultiplex_sideband()`: whatever is still buffered
    /// gets a newline and goes out.
    fn finish(&mut self) {
        if !self.scratch.is_empty() {
            self.scratch.push('\n');
            self.flush_scratch(false);
        }
    }

    /// `write_in_full(2, …)` — one write per line so the output interleaves
    /// atomically with anything else writing to the same stderr.
    fn flush_scratch(&mut self, add_newline: bool) {
        if add_newline {
            self.scratch.push('\n');
        }
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(self.scratch.as_bytes());
        let _ = err.flush();
        self.scratch.clear();
    }

    /// `maybe_colorize_sideband()` (sideband.c:254): with color off, sanitize and
    /// return; otherwise pass leading whitespace through, paint the first keyword
    /// if the line starts with one, and sanitize the rest.
    fn colorize(&self, line: &str) -> String {
        if !self.colors.enabled {
            return sanitize_control_characters(line);
        }
        let mut out = String::new();
        let ws = line.len() - line.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']).len();
        out.push_str(&line[..ws]);
        let mut rest = &line[ws..];
        for (keyword, color) in self.colors.keywords() {
            if rest.len() < keyword.len() {
                continue;
            }
            let (head, tail) = rest.split_at(keyword.len());
            // Matched case-insensitively so servers using any spelling are colored,
            // but only as a whole word — "successful" stays unpainted.
            if head.eq_ignore_ascii_case(keyword)
                && !tail.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            {
                out.push_str(color);
                out.push_str(head);
                out.push_str("\x1b[m");
                rest = tail;
                break;
            }
        }
        out.push_str(&sanitize_control_characters(rest));
        out
    }
}

/// `strbuf_add_sanitized()` (sideband.c:220) at its default
/// `sideband.allowControlCharacters` setting, which permits ANSI *color*
/// sequences and renders every other control character as `^X`. A server that
/// echoes attacker-controlled text must not be able to drive the pusher's
/// terminal.
fn sanitize_control_characters(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !b.is_ascii_control() || b == b'\t' || b == b'\n' {
            // Multi-byte UTF-8 continues here too: only ASCII controls are escaped.
            let ch_len = utf8_len(b);
            match std::str::from_utf8(&bytes[i..(i + ch_len).min(bytes.len())]) {
                Ok(text) => out.push_str(text),
                Err(_) => out.push(char::REPLACEMENT_CHARACTER),
            }
            i += ch_len;
            continue;
        }
        if let Some(len) = ansi_color_sequence_len(&bytes[i..]) {
            out.push_str(&src[i..i + len]);
            i += len;
            continue;
        }
        out.push('^');
        out.push(if b == 0x7f { '?' } else { (0x40 + b) as char });
        i += 1;
    }
    out
}

/// The byte length of the UTF-8 character starting with `b`.
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// `handle_ansi_sequence()` (sideband.c:157) for the default
/// `ALLOW_ANSI_COLOR_SEQUENCES`: `ESC [ [<n> [; <n>]*] m` and nothing else.
/// Returns the sequence's length including the terminating `m`.
fn ansi_color_sequence_len(src: &[u8]) -> Option<usize> {
    if src.len() < 3 || src[0] != 0x1b || src[1] != b'[' {
        return None;
    }
    for (i, b) in src.iter().enumerate().skip(2) {
        if *b == b'm' {
            return Some(i + 1);
        }
        if !b.is_ascii_digit() && *b != b';' {
            return None;
        }
    }
    None
}

/// `is_reachable_in_reflog()` (remote.c:2752) — whether this checkout has ever
/// seen `remote_tip`, the value the remote currently advertises for the ref
/// being pushed.
///
/// Walks `local`'s reflog newest-first. An entry whose new value *is* the tip
/// settles it. Otherwise every commit passed on the way is collected, and the
/// walk stops once entries are older than the newest entry of `tracking`'s own
/// reflog — nothing recorded before the tracking ref last moved can explain the
/// tip. Finally the tip is accepted if it is an ancestor of any collected
/// commit, which covers the tip having been merged in rather than checked out.
///
/// git batches that last test eight commits at a time purely to bound
/// `repo_in_merge_bases_many`'s working set; `merge_bases_many` here takes the
/// whole array, so the batching has nothing to do.
fn is_reachable_in_reflog(
    repo: &gix::Repository,
    local: &str,
    tracking: &str,
    remote_tip: ObjectId,
) -> bool {
    // `lookup_commit_reference`: a tip that is not a commit we have is never
    // reachable.
    let Some(tip) = repo
        .find_object(remote_tip)
        .ok()
        .and_then(|o| o.peel_to_kind(gix::objs::Kind::Commit).ok())
        .map(|c| c.id)
    else {
        return false;
    };

    // `peek_reflog`: the timestamp of the newest entry of the tracking ref's
    // reflog, or the beginning of time when it has none.
    let newest_entry_time = |name: &str| -> i64 {
        let Ok(mut reference) = repo.find_reference(name) else { return 0 };
        let mut platform = reference.log_iter();
        let Ok(Some(mut iter)) = platform.rev() else { return 0 };
        match iter.next() {
            Some(Ok(line)) => line.signature.time.seconds,
            _ => 0,
        }
    };
    let cutoff = newest_entry_time(tracking);

    // `check_and_collect_until` over the local ref's reflog.
    let Ok(mut reference) = repo.find_reference(local) else { return false };
    let mut platform = reference.log_iter();
    let Ok(Some(iter)) = platform.rev() else { return false };
    let mut seen: Vec<ObjectId> = Vec::new();
    for line in iter {
        let Ok(line) = line else { break };
        if line.new_oid == tip {
            return true;
        }
        if let Some(commit) = repo
            .find_object(line.new_oid)
            .ok()
            .and_then(|o| o.peel_to_kind(gix::objs::Kind::Commit).ok())
        {
            seen.push(commit.id);
        }
        if line.signature.time.seconds < cutoff {
            break;
        }
    }

    if seen.is_empty() {
        return false;
    }
    repo.merge_bases_many(tip, &seen)
        .map(|bases| bases.iter().any(|base| base.detach() == tip))
        .unwrap_or(false)
}

/// Whether `new` is a descendant of `old` (a fast-forward). `None` when `old` is
/// not present locally, so descendancy cannot be decided — git treats that as
/// "fetch first".
/// `lookup_commit_reference_gently(oid, 1)`: whether the object is a commit, or a
/// tag chain ending at one. Anything else (a tree, a blob, a missing object) is
/// what makes an update need `--force`.
fn peels_to_commit(repo: &gix::Repository, id: ObjectId) -> bool {
    repo.find_object(id)
        .ok()
        .is_some_and(|o| o.peel_to_kind(gix::objs::Kind::Commit).is_ok())
}

fn is_fast_forward(repo: &gix::Repository, old: ObjectId, new: ObjectId) -> Option<bool> {
    if repo.find_object(old).is_err() {
        return None;
    }
    // `new` fast-forwards `old` iff `old` is an ancestor of `new`, i.e. the
    // merge base of the two is `old`.
    match repo.merge_base(new, old) {
        Ok(base) => Some(base.detach() == old),
        Err(_) => None,
    }
}

/// The objects to pack: reachable from `wants` but not from `haves` — git's
/// `pack-objects --revs <wants> --not <haves>`. Computed as the set difference of
/// the two reachability closures (correct, though not bitmap-optimized). Shared with
/// `upload-pack`, whose server side needs the same closure for a negotiated fetch.
///
/// The want side is enumerated by
/// [`super::pack_objects::traverse_commit_list`] rather than by a set walk, for
/// the same reason git's caller runs `pack-objects --revs`: that is the order
/// `to_pack.objects` ends up in, and it is what `compute_write_order()` then
/// works from. A `HashSet` here made the resulting pack bytes differ from run to
/// run — `git bundle create` produced a different file five times out of five
/// where stock produced the same one every time — because iteration order is
/// unspecified and the pack writer honours the order it is handed.
pub(crate) fn objects_to_send(repo: &gix::Repository, wants: &[ObjectId], haves: &[ObjectId]) -> Vec<ObjectId> {
    let mut want_closure = super::pack_objects::traverse_commit_list(repo, wants.to_vec());
    if haves.is_empty() {
        return want_closure;
    }
    let have_closure = reachable_objects(repo, haves);
    want_closure.retain(|id| !have_closure.contains(id));
    want_closure
}

/// Every object reachable from `tips` (commits, their trees, and blobs). The
/// commit ancestry is walked with `rev_walk` first — `ObjectExpansion::TreeContents`
/// only expands a commit's own tree, not its parents — then every reached commit
/// is expanded. Tips absent from the local odb are dropped, matching git's
/// `feed_object` tolerance.
fn reachable_objects(repo: &gix::Repository, tips: &[ObjectId]) -> HashSet<ObjectId> {
    let tips: Vec<ObjectId> = tips
        .iter()
        .filter(|id| repo.find_object(**id).is_ok())
        .copied()
        .collect();
    if tips.is_empty() {
        return HashSet::new();
    }

    // Walk the commit ancestry; fall back to the bare tips if the walk fails.
    let mut roots: Vec<ObjectId> = Vec::new();
    match repo.rev_walk(tips.iter().copied()).all() {
        Ok(walk) => roots.extend(walk.filter_map(|info| info.ok().map(|info| info.id))),
        Err(_) => roots.extend(tips.iter().copied()),
    }
    // Include the tips themselves so a tag object (not a commit) is packed too.
    roots.extend(tips.iter().copied());
    expand_roots(repo, &roots)
}

/// Every object an explicit root list names — the roots themselves, their trees
/// and their blobs — with no ancestry walk of its own. [`reachable_objects`] uses
/// it after walking, and the shallow server path uses it directly, because there
/// the commit set is a boundary computation's output rather than everything
/// reachable from the tips.
pub(crate) fn expand_roots(repo: &gix::Repository, roots: &[ObjectId]) -> HashSet<ObjectId> {
    if roots.is_empty() {
        return HashSet::new();
    }
    let mut input = roots
        .iter()
        .copied()
        .map(Ok::<_, Box<dyn std::error::Error + Send + Sync + 'static>>);
    match pack::data::output::count::objects_unthreaded(
        &*repo.objects,
        &mut input,
        &gix::progress::Discard,
        &std::sync::atomic::AtomicBool::new(false),
        pack::data::output::count::objects::ObjectExpansion::TreeContents,
    ) {
        Ok((counts, _)) => counts.into_iter().map(|c| c.id).collect(),
        // A corrupt object aborts the counter; fall back to the walked roots so a
        // pack is still produced rather than failing the push.
        Err(_) => roots.iter().copied().collect(),
    }
}

/// The `agent=` capability value git advertises, as `git/<version>`.
fn agent() -> String {
    format!("git/{}", env!("CARGO_PKG_VERSION"))
}


#[cfg(test)]
mod sideband_tests {
    use super::*;

    /// git's default keyword table, so a test does not need a repository to read
    /// `color.remote.*` from.
    fn colors(enabled: bool) -> SidebandColors {
        SidebandColors {
            enabled,
            hint: "\x1b[33m".into(),
            warning: "\x1b[1;33m".into(),
            success: "\x1b[1;32m".into(),
            error: "\x1b[1;31m".into(),
        }
    }

    fn sideband(enabled: bool) -> Sideband {
        Sideband {
            prefix: "remote: ",
            suffix: "        ",
            scratch: String::new(),
            colors: colors(enabled),
        }
    }

    /// The band-1 stream is a pkt-line stream that the server chunked at
    /// side-band packet boundaries, so its framing can only be read once the band
    /// is reassembled — splitting the side-band packets themselves would lose the
    /// report whenever it crossed a boundary.
    #[test]
    fn primary_band_is_reframed_as_pkt_lines_and_stops_at_the_flush() {
        let buf = b"000eunpack ok\n0017ok refs/heads/main\n0000001bng refs/heads/x reason\n";
        assert_eq!(
            pkt_lines(buf),
            vec!["unpack ok\n".to_string(), "ok refs/heads/main\n".to_string()],
            "the flush ends the report; anything after it is not part of it"
        );
        // A truncated final packet is dropped rather than half-parsed.
        assert_eq!(pkt_lines(b"000eunpack ok\n0017ok refs/he"), vec!["unpack ok\n".to_string()]);
        assert!(pkt_lines(b"0000").is_empty());
    }

    /// `maybe_colorize_sideband` paints only a leading keyword, only as a whole
    /// word, and only after any leading whitespace — a server that says
    /// "successful" must not come out red.
    #[test]
    fn only_a_whole_leading_keyword_is_colored() {
        let sb = sideband(true);
        assert_eq!(sb.colorize("warning: x"), "\x1b[1;33mwarning\x1b[m: x");
        assert_eq!(sb.colorize("ERROR: x"), "\x1b[1;31mERROR\x1b[m: x", "matched case-insensitively");
        assert_eq!(sb.colorize("  hint: x"), "  \x1b[33mhint\x1b[m: x", "leading space is not painted");
        assert_eq!(sb.colorize("successful: x"), "successful: x", "only whole words match");
        assert_eq!(sb.colorize("a warning: x"), "a warning: x", "only a LEADING keyword matches");
        assert_eq!(sb.colorize("success"), "\x1b[1;32msuccess\x1b[m", "a bare keyword still matches");
        // With color off the text is only sanitized.
        assert_eq!(sideband(false).colorize("warning: x"), "warning: x");
    }

    /// `strbuf_add_sanitized`: a remote can put arbitrary bytes on band 2, so
    /// everything but tab, newline and an ANSI *color* sequence is escaped before
    /// it reaches the pusher's terminal.
    #[test]
    fn control_characters_are_escaped_but_color_sequences_survive() {
        assert_eq!(sanitize_control_characters("bel\x07l"), "bel^Gl");
        assert_eq!(sanitize_control_characters("a\tb\nc"), "a\tb\nc", "tab and newline pass through");
        assert_eq!(
            sanitize_control_characters("\x1b[31mred\x1b[m"),
            "\x1b[31mred\x1b[m",
            "SGR sequences are the one control sequence git still allows"
        );
        assert_eq!(
            sanitize_control_characters("\x1b[2Jwipe"),
            "^[[2Jwipe",
            "cursor/erase sequences are not: a hook must not be able to clear the screen"
        );
        assert_eq!(sanitize_control_characters("del\x7f"), "del^?");
        assert_eq!(sanitize_control_characters("héllo ✓"), "héllo ✓", "non-ASCII is not an escape");
    }
}

#[cfg(test)]
mod push_cert_tests {
    use super::*;

    fn oid(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.as_bytes()).expect("oid")
    }

    /// The payload git signs, byte for byte: header block, a blank line, then one
    /// line per ref update. A server verifies this text against the signature, so
    /// any reformatting here would produce certificates that fail remotely while
    /// looking fine locally.
    #[test]
    fn payload_has_gits_exact_shape() {
        let a = oid("1111111111111111111111111111111111111111");
        let b = oid("2222222222222222222222222222222222222222");
        let payload = push_cert_payload(
            "A U Thor <author@example.com> 1700000000 +0000",
            "https://example.com/repo.git",
            "1700000000-abcdef",
            &[(a, b, "refs/heads/main".to_string())],
            &[],
        );

        assert_eq!(
            payload,
            "certificate version 0.1\n\
             pusher A U Thor <author@example.com> 1700000000 +0000\n\
             pushee https://example.com/repo.git\n\
             nonce 1700000000-abcdef\n\
             \n\
             1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 refs/heads/main\n"
        );
    }

    /// Push options are part of the SIGNED text, in the header block — a server
    /// that receives options not covered by the signature must reject the push.
    #[test]
    fn push_options_are_inside_the_signed_header() {
        let a = oid("1111111111111111111111111111111111111111");
        let payload = push_cert_payload(
            "P <p@e> 1 +0000",
            "u",
            "n",
            &[(a, a, "refs/heads/x".to_string())],
            &["ci.skip".to_string(), "deploy=staging".to_string()],
        );

        let header = payload.split("\n\n").next().expect("header block");
        assert!(header.contains("push-option ci.skip"));
        assert!(header.contains("push-option deploy=staging"));
    }
}
