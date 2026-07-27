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
//! forced ref updates, deletes, and `report-status` / `report-status-v2`. Not
//! ported (git only sends these when explicitly asked, and each needs substrate
//! that does not exist in the vendored crates): shallow grafts and the
//! `side-band-64k` progress demultiplexer — neither is requested, so the server
//! replies with a plain `report-status` stream. Push certificates (`--signed`),
//! `--atomic` and `push-options` ARE negotiated here, each refused when the
//! server does not advertise the capability rather than silently downgraded. The pack is complete but undeltified (see
//! [`super::pack_objects`]); a non-thin pack is valid for receive-pack, `--thin`
//! is only a size optimization.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;

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
    pub name: String,
    /// The remote's value before the update (null if the ref was created).
    pub old: ObjectId,
    /// The value we asked it to take (null for a delete).
    pub new: ObjectId,
    /// `Ok(())` on `ok`, `Err(reason)` on `ng` or a locally-rejected update.
    pub result: Result<(), String>,
    /// True when the update overwrote a non-descendant (`--force`).
    pub forced: bool,
    /// True when the local pre-flight found nothing to do (already up to date).
    pub up_to_date: bool,
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
fn build_push_cert(
    repo: &gix::Repository,
    url: &str,
    nonce: &str,
    commands: &[(ObjectId, ObjectId, String)],
    push_options: &[String],
) -> Result<Vec<u8>> {
    let committer = repo
        .committer()
        .transpose()?
        .ok_or_else(|| anyhow!("cannot sign a push without a committer identity"))?;
    let cert = push_cert_payload(
        &format!("{} <{}> {}", committer.name, committer.email, committer.time),
        url,
        nonce,
        commands,
        push_options,
    );

    let snap = repo.config_snapshot();
    let program = snap
        .string("gpg.program")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "gpg".to_string());
    let key = snap.string("user.signingKey").map(|v| v.to_string());
    let signature = crate::gitsig::sign(cert.as_bytes(), &program, key.as_deref())
        .map_err(|e| anyhow!("gpg failed to sign the push certificate: {e}"))?;

    let mut out = cert.into_bytes();
    out.extend_from_slice(&signature);
    Ok(out)
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
    // exactly as gitoxide's fetch does.
    let mut connection = remote.connect(Direction::Push)?;
    let mut authenticate = connection.configured_credentials_for_current_url();
    let transport = connection.transport_mut();
    // Apply the repository's transport configuration (user agent, http.* options)
    // the same way gix's ref_map does before a handshake.
    if let Ok(Some(config)) = repo.transport_options(url.as_str(), None) {
        transport.configure(&*config).ok();
    }

    let handshake = gix::protocol::handshake(
        &mut *transport,
        Service::ReceivePack,
        &mut authenticate,
        Vec::new(),
        &mut gix::progress::Discard,
    )
    .context("receive-pack handshake failed")?;

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
    // prefer report-status-v2, fall back to report-status; advertise the hash
    // algorithm and agent. side-band-64k is deliberately not requested, so the
    // report comes back as a plain pkt-line stream.
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

    let mut cap_buf = String::new();
    match status_report {
        Some(2) => cap_buf.push_str(" report-status-v2"),
        Some(1) => cap_buf.push_str(" report-status"),
        _ => {}
    }
    if object_format_supported {
        cap_buf.push_str(&format!(" object-format={}", repo.object_hash()));
    }
    // `--atomic` and `-o` are refused rather than downgraded: git errors when the
    // receiving end lacks the capability, because pushing non-atomically (or
    // dropping the options) would silently do something other than what was asked.
    if opts.atomic {
        if !caps.contains("atomic") {
            bail!("the receiving end does not support --atomic push");
        }
        cap_buf.push_str(" atomic");
    }
    if !opts.push_options.is_empty() {
        if !caps.contains("push-options") {
            bail!("the receiving end does not support push options");
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
                bail!("the receiving end does not support --signed push");
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
            old,
            new: req.new,
            result: Err(reason.to_owned()),
            forced: false,
            up_to_date: false,
        };

        if deletion && !allow_deleting_refs {
            statuses.push(reject("remote does not support deleting refs"));
            continue;
        }
        if remote_current == req.new {
            // Nothing to do — git reports this ref up to date and sends no command.
            statuses.push(RefStatus {
                name: req.name.clone(),
                old: remote_current,
                new: req.new,
                result: Ok(()),
                forced: false,
                up_to_date: true,
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

        // Fast-forward check: unless forced or creating/deleting, the new tip must
        // be a descendant of the old one. If we do not even have the old commit
        // locally, we cannot prove it — git rejects with "fetch first". A lease
        // (`--force-with-lease`) skips this and defers to the server's CAS.
        if !deletion && remote_current != null && !force {
            match is_fast_forward(repo, remote_current, req.new) {
                Some(true) => {}
                Some(false) => {
                    statuses.push(reject("non-fast-forward"));
                    continue;
                }
                None => {
                    statuses.push(reject("fetch first"));
                    continue;
                }
            }
        } else if !deletion && remote_current != null && force {
            // Forced past a non-descendant is flagged with a leading '+' in output.
            forced |= is_fast_forward(repo, remote_current, req.new) == Some(false);
        }

        wire.push(Wire {
            name: req.name.clone(),
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
                old: w.old,
                new: w.new,
                result: Ok(()),
                forced: w.forced,
                up_to_date: false,
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
                    old: *old,
                    new: null,
                    result: Err("remote does not support deleting refs".to_owned()),
                    forced: false,
                    up_to_date: false,
                });
                continue;
            }
            // Only the wire entry: the per-ref status is produced from the
            // server report alongside every other command, so pushing one here
            // too would report the deletion twice.
            wire.push(Wire {
                name: name.clone(),
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

    // Parse report-status (send-pack.c `receive_status`). The first line is the
    // unpack status; each following `ok`/`ng <ref>` updates that ref's verdict.
    let unpack;
    let mut remote_status: HashMap<String, Result<(), String>> = HashMap::new();
    if status_report.is_some() {
        let mut line = String::new();
        // First pkt-line: "unpack ok" or "unpack <error>".
        match read_pkt_text(&mut reader, &mut line)? {
            Some(text) => {
                let text = text.trim_end();
                unpack = match text.strip_prefix("unpack ") {
                    Some("ok") => Ok(()),
                    Some(err) => Err(err.to_owned()),
                    None => Err(format!("unable to parse remote unpack status: {text}")),
                };
            }
            None => bail!("unexpected flush packet while reading remote unpack status"),
        }
        // Following lines: "ok <ref>" / "ng <ref> <reason>" (report-status-v2 adds
        // "option …" lines after an "ok", which carry no pass/fail signal here).
        loop {
            line.clear();
            let Some(text) = read_pkt_text(&mut reader, &mut line)? else {
                break;
            };
            let text = text.trim_end();
            if let Some(rest) = text.strip_prefix("ok ") {
                remote_status.insert(rest.to_owned(), Ok(()));
            } else if let Some(rest) = text.strip_prefix("ng ") {
                let (name, reason) = rest.split_once(' ').unwrap_or((rest, "failed"));
                remote_status.insert(name.to_owned(), Err(reason.to_owned()));
            }
            // "option …" and anything else: ignored for status purposes.
        }
    } else {
        unpack = Ok(());
    }

    // Fold the server verdicts into the per-ref statuses. With no report-status
    // capability, git optimistically marks everything ok.
    for w in &wire {
        let result = match remote_status.get(&w.name) {
            Some(r) => r.clone(),
            None if status_report.is_none() => Ok(()),
            None => Err("remote end did not report status".into()),
        };
        statuses.push(RefStatus {
            name: w.name.clone(),
            old: w.old,
            new: w.new,
            result,
            forced: w.forced,
            up_to_date: false,
        });
    }

    Ok(Outcome {
        url,
        statuses,
        unpack,
    })
}

/// Read one pkt-line of text into `line`, returning `Some(&line)` for a data
/// line or `None` at a flush / end of stream. Ports the `packet_reader_read`
/// loop's `PACKET_READ_NORMAL` handling.
fn read_pkt_text<'a>(
    reader: &mut Box<dyn ExtendedBufRead<'_> + Unpin + '_>,
    line: &'a mut String,
) -> Result<Option<&'a str>> {
    match reader.readline() {
        None => Ok(None),
        Some(Ok(Ok(PacketLineRef::Data(data)))) => {
            *line = String::from_utf8_lossy(data).into_owned();
            Ok(Some(line.as_str()))
        }
        // Flush / delimiter / response-end all terminate the report.
        Some(Ok(Ok(_))) => Ok(None),
        Some(Ok(Err(e))) => Err(anyhow!("malformed packet line from remote: {e}")),
        Some(Err(e)) => Err(anyhow!("error reading from remote: {e}")),
    }
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
pub(crate) fn objects_to_send(repo: &gix::Repository, wants: &[ObjectId], haves: &[ObjectId]) -> Vec<ObjectId> {
    let want_closure = reachable_objects(repo, wants);
    if haves.is_empty() {
        return want_closure.into_iter().collect();
    }
    let have_closure = reachable_objects(repo, haves);
    want_closure
        .into_iter()
        .filter(|id| !have_closure.contains(id))
        .collect()
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
        Err(_) => roots.into_iter().collect(),
    }
}

/// The `agent=` capability value git advertises, as `git/<version>`.
fn agent() -> String {
    format!("git/{}", env!("CARGO_PKG_VERSION"))
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
