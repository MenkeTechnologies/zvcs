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
//!
//! Not covered — each bails rather than silently diverging:
//!   * `-k`/`--keep` and, by extension, any fetch large enough that git would
//!     keep the pack instead of exploding it (`fetch.unpackLimit`, default 100).
//!     git names a kept pack after the hash of its sorted object names and drops
//!     a `.rev` file beside it; `gix-pack` names packs after the pack trailer
//!     checksum and writes no `.rev`, so the kept-pack end state cannot be
//!     reproduced. The `keep <hash>` line git prints for that case would be
//!     wrong for the same reason.
//!   * `--include-tag` — the high-level gitoxide fetch expresses "include tags"
//!     only as an implicit `refs/tags/*:refs/tags/*` refspec, which *creates local
//!     tag refs* (`fetch-pack` must not write refs); its negotiation path exposes
//!     no `include-tag` capability toggle, and wanting every advertised tag instead
//!     would download tags unrelated to the fetched objects, so the conditional
//!     server-side auto-include cannot be reproduced.
//!   * `--filter=<spec>` — the high-level negotiation never emits the `filter`
//!     packet line (the vendored `Arguments::filter` is only reachable from the
//!     low-level fetch function this port does not drive), so a partial-clone
//!     filter cannot be requested faithfully.
//!   * `--refetch`.
//!   * `--upload-pack=<exec>` / `--exec=<exec>` — the vendored transport `connect`
//!     takes no per-invocation override for the remote program.
//!   * `--diag-url` — git prints its `Diag:` breakdown from `connect.c`'s own URL
//!     parser (`url_scheme_name`, `hostandport`/`userandhost`, `path`, and the
//!     possibly-rewritten `url=` echo); `gix-url` decomposes URLs differently
//!     (scp-like host:path, port defaulting, path normalisation), so a reproduction
//!     could not stay byte-for-byte.
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

    // --- argument parsing -------------------------------------------------
    // git stops option parsing at the first non-option, which becomes
    // <repository>; everything after it is a ref name, even if it looks like a
    // flag (`git fetch-pack <url> --all` reports "no such remote ref --all").
    let mut all = false;
    let mut from_stdin = false;
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
            // git.c intercepts a bare `-h` and prints the usage on stdout.
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--all" => all = true,
            "--stdin" => from_stdin = true,
            // Progress and verbosity only ever reached stderr, which this port
            // leaves empty on the success path.
            "-q" | "--quiet" | "-v" | "--no-progress" => {}
            // gitoxide always requests a thin pack; the end state is identical
            // either way (see the module docs).
            "--thin" | "--no-thin" => {}
            "-k" | "--keep" | "--lock-pack" => bail!(
                "unsupported flag {a:?} — a kept pack cannot be reproduced: \
                 git names it after the hash of its sorted object names and adds a `.rev` file, \
                 gix-pack names it after the pack trailer checksum and writes none ({PORTED})"
            ),
            "--include-tag" => bail!(
                "unsupported flag {a:?} — gitoxide implements it as an implicit \
                 `refs/tags/*:refs/tags/*` refspec, which would create local tag refs ({PORTED})"
            ),
            // `--deepen-relative` is a modifier on `--depth`; git only appends the
            // `deepen-relative` line when a depth is present, so we just record it.
            "--deepen-relative" => deepen_relative = true,
            "--refetch" => bail!("unsupported flag {a:?} ({PORTED})"),
            "--check-self-contained-and-connected" | "--diag-url" | "--stateless-rpc"
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
                        bail!(
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

    // Nothing matched: git never opens a fetch and exits 1.
    if selected.is_empty() {
        return Ok(ExitCode::FAILURE);
    }

    // --- phase 2: negotiate and receive the pack --------------------------
    let shallow = build_shallow(depth, deepen_relative, shallow_since.as_deref(), &shallow_exclude)?;
    if let Err(e) = receive(&repo, dest, &selected, shallow) {
        // A failed fetch surfaces as git's `fatal:` with 128 unless it is one of
        // our own refusals, which must stay loud and unmistakable.
        if let Some(refusal) = e.downcast_ref::<Refusal>() {
            bail!("{}", refusal.0);
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
    let parse_date = |s: &str| -> Result<gix::date::Time> {
        gix::date::parse(s, Some(std::time::SystemTime::now()))
            .map_err(|e| anyhow::anyhow!("invalid --shallow-since date {s:?}: {e}"))
    };

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

/// Want exactly `selected`, receive the pack, and explode it into loose objects.
///
/// Each ref is turned into a one-sided fetch refspec (`refs/heads/main` with no
/// destination), which makes it a `want` without producing any ref edit — the
/// property `fetch-pack` depends on.
fn receive(
    repo: &gix::Repository,
    dest: &str,
    selected: &[(String, ObjectId)],
    shallow: Shallow,
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
        } => explode(repo, write_pack_bundle),
    }
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
