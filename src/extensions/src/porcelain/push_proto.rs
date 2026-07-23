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
//! that does not exist in the vendored crates): push certificates (`--signed`),
//! `--atomic`, `push-options`, shallow grafts, and the `side-band-64k` progress
//! demultiplexer — none is requested, so the server replies with a plain
//! `report-status` stream. The pack is complete but undeltified (see
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

        // Fast-forward check: unless forced or creating/deleting, the new tip must
        // be a descendant of the old one. If we do not even have the old commit
        // locally, we cannot prove it — git rejects with "fetch first". A lease
        // (`--force-with-lease`) skips this and defers to the server's CAS.
        let mut forced = false;
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
            forced = is_fast_forward(repo, remote_current, req.new) == Some(false);
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
    for (i, w) in wire.iter().enumerate() {
        let line = if i == 0 {
            format!("{} {} {}\0{}", w.old, w.new, w.name, cap_buf)
        } else {
            format!("{} {} {}", w.old, w.new, w.name)
        };
        encode::data_to_write(line.as_bytes(), &mut req_buf)?;
    }
    encode::write_packet_line(&PacketLineRef::Flush, &mut req_buf)?;

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
/// the two reachability closures (correct, though not bitmap-optimized).
fn objects_to_send(repo: &gix::Repository, wants: &[ObjectId], haves: &[ObjectId]) -> Vec<ObjectId> {
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
