//! `git upload-pack` — the *server* half of `git fetch`.
//!
//! `upload-pack` is invoked by `git fetch-pack` over a transport. It writes a ref
//! advertisement, reads `want`/`have` negotiation from stdin, and streams a
//! generated pack back. The serving path (`serve`) is implemented for the
//! bidirectional v0 protocol: a `multi_ack_detailed` advertisement with the
//! `symref=HEAD:<target>` hint, the want/have loop, and a side-band-64k pack built
//! from the negotiated closure (`push_proto::objects_to_send` +
//! `pack_objects::pack_bytes_for`). Enough for a local (`file://`) or ssh
//! clone/fetch served by this binary. The want policy of `receive_needs()` is
//! enforced: a `want` outside the advertised ref tips is refused with
//! `ERR upload-pack: not our ref <oid>` and exit 128 unless
//! `uploadpack.allowTipSHA1InWant`, `uploadpack.allowReachableSHA1InWant` or
//! `uploadpack.allowAnySHA1InWant` widens it (see [`WantPolicy`]).
//! Shallow/deepen and object filters are not negotiated (ignored); stateless-RPC
//! (smart-HTTP) beyond `--advertise-refs` is not modelled other than in that want
//! policy. Argument parsing and repository resolution are checked against git
//! 2.55.0 on Darwin:
//!
//!   * `-h` → the 368-byte usage block on **stdout**, exit 129, before any
//!     repository is touched (so it works outside a repository).
//!   * no `<directory>`, or more than one → the 458-byte usage block (the
//!     longer variant, which also lists the hidden `--advertise-refs` alias) on
//!     **stderr**, exit 129.
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
//! Once a repository *does* resolve, this bails. The missing substrate,
//! concretely:
//!
//!   1. **There is no server-side protocol implementation in the vendored
//!      crates.** `src/ported/gix-protocol/src/` contains `handshake`, `fetch`,
//!      `ls_refs` and `command` — all of it the *client* talking to a remote.
//!      Its `Cargo.toml` gates everything behind `blocking-client` /
//!      `async-client`; there is no server feature and no server module. The
//!      only mentions of "upload-pack" under `gix-transport/src` are call sites
//!      that *spawn* someone else's `upload-pack`. Nothing implements the
//!      `want`/`have`/`ACK`/`NAK` state machine from the serving side, shallow
//!      /deepen handling, or `ref-in-want`.
//!   2. **The capability advertisement is a function of the git binary, not of
//!      the repository.** Every advertisement line stock git emits ends with
//!      `agent=git/2.55.0-Darwin` — the installed git's version string and
//!      platform. It is not derivable from the repository or from the vendored
//!      crates, and hardcoding one build's value would produce output that
//!      matches on exactly one machine. Parts of the list are equally
//!      environmental: `no-done` is advertised for `--advertise-refs` but not
//!      for the full v0 exchange, and `filter` and `ref-in-want` appear only
//!      when the matching `uploadpack.*` config is set. The two
//!      `allow-*-sha1-in-want` tokens *are* ported — see [`WantPolicy`] — because
//!      they are a policy this server can actually enforce.
//!   3. **Pack generation is not wired to a protocol.** `gix-pack` has
//!      `data::output` (`count`, `entry`) for building a pack, but nothing
//!      turns a negotiated want/have set into a side-band-multiplexed stream
//!      with progress on band 2, and there is no thin-pack or `include-tag`
//!      support on the sending side.
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
        Err(msg) => bail!("{msg}"),
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

    let Some(repo) = repo else {
        eprintln!("fatal: '{directory}' does not appear to be a git repository");
        return Ok(ExitCode::from(128));
    };

    // `--advertise-refs`/`--http-backend-info-refs` write the advertisement and exit
    // (the smart-HTTP info/refs half); the bidirectional local/ssh path negotiates.
    let advertise_only = args
        .iter()
        .any(|a| a == "--advertise-refs" || a == "--http-backend-info-refs");
    // git's want policy differs under `--stateless-rpc`: a smart-HTTP client may
    // have chosen its wants from an advertisement a *different* process wrote, so
    // a non-tip want is checked for reachability instead of refused outright.
    let stateless_rpc = args.iter().any(|a| a == "--stateless-rpc");
    serve(&repo, advertise_only, stateless_rpc)
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
    let policy = WantPolicy::from_config(repo);
    {
        let adv = advertisement(repo, policy)?;
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
                }
            }
        }
        // `shallow`/`deepen*`/`filter` lines are ignored (not negotiated).
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

    // --- haves until `done`, ACK/NAK per round -----------------------------------
    let mut common: Vec<ObjectId> = Vec::new();
    let mut out = std::io::stdout().lock();
    loop {
        match read_pkt(&mut stdin)? {
            // A flush ends a have batch without `done`: with no multi-ack advertised,
            // git answers a single NAK and the client sends more haves or `done`.
            None => {
                write_pkt(&mut out, b"NAK\n")?;
                out.flush()?;
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
                            common.push(id);
                            // multi_ack_detailed: acknowledge each object we have in common
                            // as it arrives.
                            write_pkt(&mut out, format!("ACK {} common\n", id.to_hex()).as_bytes())?;
                            out.flush()?;
                        }
                    }
                }
            }
        }
    }
    // Final acknowledgement: the last common object, or NAK when there is none.
    match common.last() {
        Some(id) => write_pkt(&mut out, format!("ACK {}\n", id.to_hex()).as_bytes())?,
        None => write_pkt(&mut out, b"NAK\n")?,
    }
    out.flush()?;

    // --- build + stream the pack -------------------------------------------------
    let objects = crate::porcelain::push_proto::objects_to_send(repo, &wants, &common);
    let pack = crate::porcelain::pack_objects::pack_bytes_for(repo, &objects)?;
    if cap_present(&want_caps, b"side-band-64k") || cap_present(&want_caps, b"side-band") {
        // Multiplex the pack on band 1 (≤ 65515 payload bytes per pkt-line), then a
        // flush closes the side-band stream.
        for chunk in pack.chunks(65515) {
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
fn advertisement(repo: &gix::Repository, policy: WantPolicy) -> Result<Vec<u8>> {
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
    let caps = capabilities(head_target.as_deref(), policy);
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

/// The upload-pack capability list. `symref=HEAD:<target>` tells the client which
/// branch HEAD follows so a clone checks it out. The two `allow-*-sha1-in-want`
/// tokens are emitted exactly when the matching bit of [`WantPolicy`] is set, as
/// `write_v0_ref()` does off `data->allow_uor`.
fn capabilities(head_target: Option<&str>, policy: WantPolicy) -> String {
    let mut caps =
        String::from("multi_ack_detailed side-band-64k thin-pack ofs-delta no-progress include-tag");
    if policy.tip {
        caps.push_str(" allow-tip-sha1-in-want");
    }
    if policy.reachable {
        caps.push_str(" allow-reachable-sha1-in-want");
    }
    if let Some(t) = head_target {
        caps.push_str(&format!(" symref=HEAD:{t}"));
    }
    caps.push_str(" object-format=sha1 agent=git/2.55.0-zvcs");
    caps
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
