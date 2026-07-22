use anyhow::{anyhow, bail, Result};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;
use gix::remote::Direction;

use super::push_proto::{self, Request};

/// `git push [<options>] [<repository> [<refspec>...]]` — upload commits and
/// update remote refs.
///
/// The object upload is a faithful port of git's `send-pack.c` (see
/// [`super::push_proto`]); this function is the porcelain around it: it parses the
/// option surface, resolves refspecs into concrete ref updates, runs the push,
/// advances the remote-tracking refs, and prints git's `To <url>` status block.
///
/// Implemented flags: `-f/--force`, `--force-with-lease[=…]`, `-n/--dry-run`,
/// `-d/--delete`, `--all`/`--branches`, `--tags`, `-u/--set-upstream`,
/// `--repo=<r>`, `--porcelain`, and the refspec forms `src`, `src:dst`, `+src:dst`,
/// `:dst`. Flags whose semantics the send-pack scope cannot honor faithfully
/// (`--mirror`, `--signed=yes|if-asked`, `--atomic`, `-o/--push-option`,
/// `--prune`, `--follow-tags`, `--recurse-submodules=on-demand|only`) are rejected
/// rather than silently ignored; inert or already-matched flags (`--thin`,
/// `--receive-pack`, `-4/-6`, `--verify`, …) are accepted.
pub fn push(args: &[String]) -> Result<ExitCode> {
    let mut f = Flags::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut end_of_options = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_options || !a.starts_with('-') || a == "-" {
            positionals.push(a.to_string());
            i += 1;
            continue;
        }
        // Split `--opt=value` up front; a value-taking flag without `=` consumes
        // the next argv entry.
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (a, None),
        };
        let mut take_value = |inline: Option<String>| -> Result<String> {
            if let Some(v) = inline {
                return Ok(v);
            }
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| anyhow!("option `{name}' requires a value"))
        };
        match name {
            "--" => end_of_options = true,
            "-f" | "--force" => f.force = true,
            "--no-force" => f.force = false,
            "-n" | "--dry-run" => f.dry_run = true,
            "--no-dry-run" => f.dry_run = false,
            "-d" | "--delete" => f.delete = true,
            "--no-delete" => f.delete = false,
            "--all" | "--branches" => f.all = true,
            "--no-all" | "--no-branches" => f.all = false,
            "--tags" => f.tags = true,
            "--no-tags" => f.tags = false,
            "-u" | "--set-upstream" => f.set_upstream = true,
            "--no-set-upstream" => f.set_upstream = false,
            "--porcelain" => f.porcelain = true,
            "--no-porcelain" => f.porcelain = false,
            "--repo" => f.repo = Some(take_value(inline)?),
            "--force-with-lease" => f.lease = parse_lease(inline)?,
            "--no-force-with-lease" => f.lease = Lease::None,
            // Accepted, but inert here or already matched by the engine's behavior.
            "-q" | "--quiet" | "-v" | "--verbose" | "--progress" | "--no-progress"
            | "--thin" | "--no-thin" | "-4" | "--ipv4" | "-6" | "--ipv6"
            | "--force-if-includes" | "--no-force-if-includes" | "--verify" | "--no-verify"
            | "--no-signed" | "--no-atomic" | "--no-mirror" | "--no-prune"
            | "--no-follow-tags" | "--no-recurse-submodules" => {}
            "--receive-pack" | "--exec" => {
                let _ = take_value(inline)?;
            }
            "--recurse-submodules" => match take_value(inline)?.as_str() {
                "no" | "check" => {}
                other => bail!("--recurse-submodules={other} is not supported"),
            },
            // Rejected: cannot be honored faithfully by the send-pack scope, and
            // silently ignoring them would change the push's meaning.
            "--mirror" => bail!("--mirror is not supported"),
            "--prune" => bail!("--prune is not supported"),
            "--follow-tags" => bail!("--follow-tags is not supported (use --tags)"),
            "--atomic" => bail!("--atomic is not supported"),
            "-o" | "--push-option" => {
                let _ = take_value(inline)?;
                bail!("--push-option is not supported");
            }
            "--signed" => match inline.as_deref() {
                None | Some("no") | Some("false") => {}
                Some(v) => bail!("--signed={v} is not supported"),
            },
            other => bail!("unsupported option {other:?}"),
        }
        i += 1;
    }

    // Conflicts git rejects before contacting the remote.
    if f.tags && f.all {
        bail!("--all can't be combined with --tags");
    }

    let repo = gix::discover(".")?;

    let remote_name: String = match f.repo.clone().or_else(|| positionals.first().cloned()) {
        Some(r) => r,
        None => default_push_remote(&repo),
    };
    // With `--repo`, all positionals are refspecs; otherwise the first is the remote.
    let specs: Vec<String> = if f.repo.is_some() {
        positionals
    } else {
        positionals.into_iter().skip(1).collect()
    };

    let remote = match repo.find_remote(remote_name.as_str()) {
        Ok(r) => r,
        Err(_) => repo.remote_at(remote_name.as_str())?,
    };

    // Build the concrete updates, plus the (local-branch, remote-ref) pairs that
    // `--set-upstream` records after a successful push.
    let (mut requests, upstreams) = build_requests(&repo, &f, &specs)?;

    // Resolve `--force-with-lease` into each request's expected old value.
    if !matches!(f.lease, Lease::None) {
        for req in &mut requests {
            req.expected = lease_for(&repo, &remote, &f.lease, &req.name);
        }
    }

    if requests.is_empty() {
        bail!("no refspec to push");
    }

    // `pre-push` runs before contacting the remote, receiving `<remote> <url>` as
    // arguments and one `<local-ref> <local-sha> <remote-ref> <remote-sha>` line
    // per update on stdin. A non-zero exit aborts the push (git behavior).
    if !f.dry_run {
        let url = remote
            .url(Direction::Push)
            .or_else(|| remote.url(Direction::Fetch))
            .map(|u| u.to_bstring().to_string())
            .unwrap_or_default();
        let null = ObjectId::null(repo.object_hash());
        let mut payload = String::new();
        for req in &requests {
            let remote_sha = tracking_oid(&repo, &remote, &req.name).unwrap_or(null);
            payload.push_str(&format!(
                "{0} {1} {0} {2}\n",
                req.name, req.new, remote_sha
            ));
        }
        if !crate::hooks::run(&repo, "pre-push", &[&remote_name, &url], Some(payload.as_bytes()))? {
            return Ok(ExitCode::from(1));
        }
    }

    let outcome = push_proto::send_pack(&repo, &remote, &requests, f.dry_run)?;

    // A dry run performs no local writes; a real push advances the tracking refs
    // and (for `-u`) records the upstream, but only for refs the remote accepted.
    if !f.dry_run {
        update_tracking_refs(&repo, &remote, &outcome);
        if f.set_upstream {
            record_upstreams(&repo, &remote_name, &outcome, &upstreams);
        }
    }

    if f.porcelain {
        report_porcelain(&outcome)
    } else {
        report(&outcome)
    }
}

/// The push flag state.
#[derive(Default)]
struct Flags {
    force: bool,
    dry_run: bool,
    delete: bool,
    all: bool,
    tags: bool,
    set_upstream: bool,
    porcelain: bool,
    repo: Option<String>,
    lease: Lease,
}

/// `--force-with-lease` state.
#[derive(Default)]
enum Lease {
    /// Not given.
    #[default]
    None,
    /// `--force-with-lease` with no value: lease every ref against its tracking ref.
    Implicit,
    /// `--force-with-lease=<ref>[:<expect>]`: lease one ref, optionally against an
    /// explicit expected value rather than its tracking ref.
    Explicit {
        ref_name: String,
        expect: Option<ObjectId>,
    },
}

/// Parse a `--force-with-lease[=<ref>[:<expect>]]` value.
fn parse_lease(value: Option<String>) -> Result<Lease> {
    let Some(v) = value else {
        return Ok(Lease::Implicit);
    };
    let (ref_name, expect) = match v.split_once(':') {
        Some((r, e)) if !e.is_empty() => {
            let repo = gix::discover(".")?;
            let id = repo
                .rev_parse_single(e)
                .map_err(|_| anyhow!("cannot parse expected object name '{e}'"))?
                .detach();
            (r.to_string(), Some(id))
        }
        Some((r, _)) => (r.to_string(), None),
        None => (v, None),
    };
    Ok(Lease::Explicit { ref_name, expect })
}

/// The expected old value a lease requires for `remote_ref`, or `None` when the
/// lease does not cover this ref. A missing tracking ref yields the null oid,
/// which asks the server to confirm the ref does not yet exist.
fn lease_for(
    repo: &gix::Repository,
    remote: &gix::Remote<'_>,
    lease: &Lease,
    remote_ref: &str,
) -> Option<ObjectId> {
    match lease {
        Lease::None => None,
        Lease::Implicit => Some(tracking_oid(repo, remote, remote_ref).unwrap_or_else(|| null(repo))),
        Lease::Explicit { ref_name, expect } => {
            if ref_matches(ref_name, remote_ref) {
                Some(
                    expect
                        .or_else(|| tracking_oid(repo, remote, remote_ref))
                        .unwrap_or_else(|| null(repo)),
                )
            } else {
                None
            }
        }
    }
}

/// The null object id for the repository's hash.
fn null(repo: &gix::Repository) -> ObjectId {
    ObjectId::null(repo.object_hash())
}

/// The current value of the local remote-tracking ref for `remote_ref`.
fn tracking_oid(
    repo: &gix::Repository,
    remote: &gix::Remote<'_>,
    remote_ref: &str,
) -> Option<ObjectId> {
    let tracking = tracking_ref_for(remote, remote_ref)?;
    repo.find_reference(tracking.as_str())
        .ok()?
        .try_id()
        .map(|id| id.detach())
}

/// Whether a lease's `<ref>` names the same ref as `remote_ref`, comparing both
/// the full and the shortened (`refs/heads/` / `refs/tags/`) forms.
fn ref_matches(lease_ref: &str, remote_ref: &str) -> bool {
    lease_ref == remote_ref || lease_ref == short_ref(remote_ref)
}

/// A `(local branch short name, remote ref)` pair recorded for `--set-upstream`.
type Upstream = (String, String);

/// Turn the flags and refspecs into concrete ref updates, plus the upstream pairs
/// `-u` records. Covers `--all`, `--tags`, `--delete`, explicit refspecs, and the
/// default current-branch push.
fn build_requests(
    repo: &gix::Repository,
    f: &Flags,
    specs: &[String],
) -> Result<(Vec<Request>, Vec<Upstream>)> {
    let mut requests = Vec::new();
    let mut upstreams = Vec::new();

    if f.all {
        if !specs.is_empty() {
            bail!("--all can't be combined with refspecs");
        }
        for r in repo.references()?.local_branches()? {
            let r = r.map_err(|e| anyhow!("{e}"))?;
            let name = r.name().as_bstr().to_str().map_err(|e| anyhow!("{e}"))?.to_string();
            if let Some(id) = r.try_id() {
                let short = short_ref(&name).to_string();
                requests.push(Request { name: name.clone(), new: id.detach(), force: f.force, expected: None });
                upstreams.push((short, name));
            }
        }
        return Ok((requests, upstreams));
    }

    if f.tags {
        if !specs.is_empty() {
            bail!("--tags can't be combined with refspecs");
        }
        for r in repo.references()?.tags()? {
            let mut r = r.map_err(|e| anyhow!("{e}"))?;
            let name = r.name().as_bstr().to_str().map_err(|e| anyhow!("{e}"))?.to_string();
            if let Ok(id) = r.peel_to_id_in_place() {
                requests.push(Request { name, new: id.detach(), force: f.force, expected: None });
            }
        }
        return Ok((requests, upstreams));
    }

    if f.delete {
        for spec in specs {
            requests.push(Request {
                name: full_ref_name(spec),
                new: null(repo),
                force: f.force,
                expected: None,
            });
        }
        return Ok((requests, upstreams));
    }

    if specs.is_empty() {
        let (req, up) = current_branch_request(repo, f.force)?;
        requests.push(req);
        upstreams.push(up);
    } else {
        for spec in specs {
            let (req, up) = parse_refspec(repo, spec, f.force)?;
            requests.push(req);
            if let Some(up) = up {
                upstreams.push(up);
            }
        }
    }
    Ok((requests, upstreams))
}

/// Turn one `<refspec>` into a ref update (and its `-u` upstream pair, when the
/// source is a local branch). Handles a leading `+` (force), `src:dst`, bare `src`,
/// and `:dst` (delete).
fn parse_refspec(
    repo: &gix::Repository,
    spec: &str,
    force: bool,
) -> Result<(Request, Option<Upstream>)> {
    let (spec, force) = match spec.strip_prefix('+') {
        Some(rest) => (rest, true),
        None => (spec, force),
    };
    let (src, dst) = match spec.split_once(':') {
        Some((s, d)) => (s, d),
        None => (spec, spec),
    };

    let new = if src.is_empty() {
        null(repo) // `:dst` deletes the remote ref.
    } else {
        repo.rev_parse_single(src)
            .map_err(|_| anyhow!("src refspec {src} does not match any"))?
            .detach()
    };
    let dst = if dst.is_empty() { src } else { dst };
    let dst_full = full_ref_name(dst);
    // Record an upstream only when the source is a local branch.
    let upstream = if !src.is_empty()
        && repo
            .find_reference(&full_ref_name(src))
            .ok()
            .filter(|_| !src.starts_with("refs/") || src.starts_with("refs/heads/"))
            .is_some()
    {
        Some((src.to_string(), dst_full.clone()))
    } else {
        None
    };
    Ok((
        Request {
            name: dst_full,
            new,
            force,
            expected: None,
        },
        upstream,
    ))
}

/// The update for a bare `git push`: the current branch to a same-named remote
/// branch. Rejects a detached HEAD and an unborn branch exactly as git does.
fn current_branch_request(repo: &gix::Repository, force: bool) -> Result<(Request, Upstream)> {
    let head = repo.head()?;
    let branch = head
        .referent_name()
        .ok_or_else(|| anyhow!("You are not currently on a branch."))?
        .shorten()
        .to_string();
    let new = repo
        .head_id()
        .map_err(|_| anyhow!("src refspec {branch} does not match any"))?
        .detach();
    let name = format!("refs/heads/{branch}");
    Ok((
        Request {
            name: name.clone(),
            new,
            force,
            expected: None,
        },
        (branch, name),
    ))
}

/// Expand a short ref name to its full form. A name that already starts with
/// `refs/` is kept; anything else is treated as a branch.
fn full_ref_name(name: &str) -> String {
    if name.starts_with("refs/") {
        name.to_string()
    } else {
        format!("refs/heads/{name}")
    }
}

/// Record `branch.<name>.remote`/`.merge` for every branch the remote accepted,
/// as `git push -u` does. Best-effort: a config-write failure does not fail the push.
fn record_upstreams(
    repo: &gix::Repository,
    remote_name: &str,
    outcome: &push_proto::Outcome,
    upstreams: &[Upstream],
) {
    for (branch, remote_ref) in upstreams {
        let accepted = outcome
            .statuses
            .iter()
            .any(|s| &s.name == remote_ref && s.result.is_ok() && !s.new.is_null());
        if accepted {
            let _ = super::config::set_branch_upstream(repo, branch, remote_name, remote_ref);
        }
    }
}

/// Advance (or delete) the local remote-tracking refs for every ref the remote
/// accepted, mapping each pushed ref through the remote's fetch refspec.
fn update_tracking_refs(
    repo: &gix::Repository,
    remote: &gix::Remote<'_>,
    outcome: &push_proto::Outcome,
) {
    let mut edits: Vec<RefEdit> = Vec::new();
    for s in &outcome.statuses {
        if s.result.is_err() {
            continue;
        }
        let Some(tracking) = tracking_ref_for(remote, &s.name) else {
            continue;
        };
        let Ok(name) = gix::refs::FullName::try_from(tracking.as_str()) else {
            continue;
        };
        let change = if s.new.is_null() {
            Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
            }
        } else {
            Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "update by push".into(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(s.new),
            }
        };
        edits.push(RefEdit {
            change,
            name,
            deref: false,
        });
    }
    if !edits.is_empty() {
        let _ = repo.edit_references(edits);
    }
}

/// Map a pushed remote ref name to its local remote-tracking ref via the remote's
/// fetch refspecs. Handles the wildcard form (`refs/heads/*:refs/remotes/origin/*`)
/// and exact refspecs.
fn tracking_ref_for(remote: &gix::Remote<'_>, pushed: &str) -> Option<String> {
    for spec in remote.refspecs(Direction::Fetch) {
        let spec = spec.to_ref();
        let src = spec.source()?.to_str().ok()?;
        let dst = spec.destination()?.to_str().ok()?;
        match (src.strip_suffix('*'), dst.strip_suffix('*')) {
            (Some(src_pre), Some(dst_pre)) => {
                if let Some(rest) = pushed.strip_prefix(src_pre) {
                    return Some(format!("{dst_pre}{rest}"));
                }
            }
            _ if src == pushed => return Some(dst.to_string()),
            _ => {}
        }
    }
    None
}

/// Print the human `To <url>` status block (git prints it on stderr) and return
/// the exit code: failure if the unpack failed or any ref was rejected.
fn report(outcome: &push_proto::Outcome) -> Result<ExitCode> {
    let mut any_failed = outcome.unpack.is_err();
    if let Err(reason) = &outcome.unpack {
        eprintln!("error: unpack failed: {reason}");
    }

    let did_update = outcome
        .statuses
        .iter()
        .any(|s| !s.up_to_date && s.result.is_ok());
    if !did_update && !any_failed && outcome.statuses.iter().all(|s| s.result.is_ok()) {
        eprintln!("Everything up-to-date");
        return Ok(ExitCode::SUCCESS);
    }

    eprintln!("To {}", outcome.url);
    for s in &outcome.statuses {
        let short = |oid: &ObjectId| oid.to_hex_with_len(7).to_string();
        let src_dst = format!("{} -> {}", short_ref(&s.name), short_ref(&s.name));
        match &s.result {
            Ok(()) if s.up_to_date => eprintln!(" = [up to date]      {src_dst}"),
            Ok(()) if s.old.is_null() => {
                let kind = if s.name.starts_with("refs/tags/") {
                    "[new tag]   "
                } else {
                    "[new branch]"
                };
                eprintln!(" * {kind}      {src_dst}");
            }
            Ok(()) if s.new.is_null() => {
                eprintln!(" - [deleted]         {}", short_ref(&s.name));
            }
            Ok(()) => {
                let sep = if s.forced { "..." } else { ".." };
                let flag = if s.forced { "+" } else { " " };
                eprintln!("{flag}  {}{sep}{}  {src_dst}", short(&s.old), short(&s.new));
            }
            Err(reason) => {
                any_failed = true;
                eprintln!(" ! [rejected]        {src_dst} ({reason})");
            }
        }
    }

    if any_failed {
        eprintln!("error: failed to push some refs to '{}'", outcome.url);
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

/// `--porcelain`: machine-readable output — `<flag>\t<ref>\t<summary>` per ref,
/// framed by `To <url>` and a trailing `Done`, on stdout.
fn report_porcelain(outcome: &push_proto::Outcome) -> Result<ExitCode> {
    let mut any_failed = outcome.unpack.is_err();
    println!("To {}", outcome.url);
    for s in &outcome.statuses {
        let short = |oid: &ObjectId| oid.to_hex_with_len(7).to_string();
        let refpair = format!("{0}:{0}", s.name);
        match &s.result {
            Ok(()) if s.up_to_date => println!("=\t{refpair}\t[up to date]"),
            Ok(()) if s.old.is_null() => {
                let kind = if s.name.starts_with("refs/tags/") {
                    "[new tag]"
                } else {
                    "[new branch]"
                };
                println!("*\t{refpair}\t{kind}");
            }
            Ok(()) if s.new.is_null() => println!("-\t:{0}\t[deleted]", s.name),
            Ok(()) => {
                let flag = if s.forced { "+" } else { " " };
                let sep = if s.forced { "..." } else { ".." };
                println!("{flag}\t{refpair}\t{}{sep}{}", short(&s.old), short(&s.new));
            }
            Err(reason) => {
                any_failed = true;
                println!("!\t{refpair}\t[rejected] ({reason})");
            }
        }
    }
    println!("Done");
    if any_failed {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

/// Shorten a full ref name for display (`refs/heads/main` → `main`).
fn short_ref(name: &str) -> &str {
    name.strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/tags/"))
        .unwrap_or(name)
}

/// The remote `git push` targets with no `<remote>` argument, in git's order:
/// the current branch's `pushRemote`, then `remote.pushDefault`, then the
/// branch's `remote`, then `origin`.
fn default_push_remote(repo: &gix::Repository) -> String {
    let snap = repo.config_snapshot();
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.referent_name().map(|n| n.shorten().to_string()));

    if let Some(b) = &branch {
        if let Some(r) = snap.string(&format!("branch.{b}.pushRemote")) {
            return r.to_string();
        }
    }
    if let Some(r) = snap.string("remote.pushDefault") {
        return r.to_string();
    }
    if let Some(b) = &branch {
        if let Some(r) = snap.string(&format!("branch.{b}.remote")) {
            return r.to_string();
        }
    }
    "origin".to_string()
}
