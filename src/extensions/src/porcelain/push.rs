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
/// `-d/--delete`, `--all`/`--branches`, `--tags`, `--follow-tags`, `-u/--set-upstream`,
/// `--repo=<r>`, `--porcelain`, and the refspec forms `src`, `src:dst`, `+src:dst`,
/// `:dst`. `--recurse-submodules=<check|on-demand|no|only>` is honored: `no` is a
/// plain push, `check`/`on-demand`/`only` first detect submodules whose pushed
/// commit is not yet on their remote (git's `find_unpushed_submodules`). When none
/// need pushing these reduce to a plain push; `check` aborts if any do, and
/// `on-demand`/`only` abort rather than silently skip the recursive submodule push
/// (that transport recursion is not wired here — skipping it would be data-losing).
/// `--follow-tags` adds the annotated tags reachable from the refs being pushed
/// and missing from the remote (see [`append_followed_tags`]).
/// Flags whose semantics the send-pack scope cannot honor faithfully
/// (`--mirror`, `--signed=yes|if-asked`, `--atomic`, `-o/--push-option`,
/// `--prune`) are rejected
/// rather than silently ignored; inert or already-matched flags (`--thin`,
/// `--receive-pack`, `-4/-6`, `--verify`, …) are accepted.
pub fn push(args: &[String]) -> Result<ExitCode> {
    let mut f = Flags::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut end_of_options = false;
    // Whether `--recurse-submodules`/`--no-recurse-submodules` was given on the
    // command line; if so it wins over `push.recurseSubmodules` (git reads config
    // before parse_options, so the flag's assignment lands last).
    let mut recurse_explicit = false;
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
            "--follow-tags" => f.follow_tags = true,
            "-u" | "--set-upstream" => f.set_upstream = true,
            "--no-set-upstream" => f.set_upstream = false,
            "--porcelain" => f.porcelain = true,
            "--no-porcelain" => f.porcelain = false,
            "--repo" => f.repo = Some(take_value(inline)?),
            "--force-with-lease" => f.lease = parse_lease(inline)?,
            "--no-force-with-lease" => f.lease = Lease::None,
            // Accepted, but inert here or already matched by the engine's behavior.
            "-v" | "--verbose" => f.verbose = true,
            "-q" | "--quiet" | "--progress" | "--no-progress"
            | "--thin" | "--no-thin" | "-4" | "--ipv4" | "-6" | "--ipv6"
            | "--force-if-includes" | "--no-force-if-includes" | "--verify" | "--no-verify"
            | "--no-signed" => {}
            "--no-atomic" => f.atomic = false,
            "--no-mirror" => f.mirror = false,
            "--no-prune" => f.prune = false,
            "--no-follow-tags" => f.follow_tags = false,
            "--receive-pack" | "--exec" => {
                let _ = take_value(inline)?;
            }
            "--recurse-submodules" => {
                f.recurse = parse_recurse(&take_value(inline)?)?;
                recurse_explicit = true;
            }
            "--no-recurse-submodules" => {
                f.recurse = Recurse::Off;
                recurse_explicit = true;
            }
            // `--mirror`/`--prune` synthesize deletions from the ref
            // advertisement; `--atomic` and `-o` are negotiated capabilities.
            // All four are refused by the wire layer when the server lacks the
            // capability rather than being silently downgraded.
            "--mirror" => f.mirror = true,
            "--prune" => f.prune = true,
            "--atomic" => f.atomic = true,
            "-o" | "--push-option" => {
                f.push_options.push(take_value(inline)?);
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

    // Honor the `push.*` config defaults for flags not given explicitly. An
    // explicit command-line flag always wins: git reads config in `git_push_config`
    // before `parse_options`, so the flag's assignment lands after the config's.
    {
        let snap = repo.config_snapshot();

        // `push.recurseSubmodules` — the default for `--recurse-submodules`, parsed
        // with git's own value parser (`parse_push_recurse_submodules_arg` is
        // `parse_push_recurse`, which `parse_recurse` ports). The flag overrides it.
        if !recurse_explicit {
            if let Some(v) = snap.string("push.recurseSubmodules") {
                f.recurse = parse_recurse(&v.to_string())?;
            }
        }

        // `push.autoSetupRemote` — on a bare default push whose current branch has
        // no configured upstream, act as if `--set-upstream`. Ported from git's
        // `setup_default_push_refspecs` (builtin/push.c): the SET_UPSTREAM flag is
        // added when `(flags & AUTO_UPSTREAM) && branch->merge_nr == 0`, and only
        // for `push.default` simple/upstream/current — `matching` and `nothing`
        // return/die before that point. Unlike a plain flag it is not undone by
        // `--no-set-upstream` (git applies it at push time, after option parsing).
        let bare_default = specs.is_empty() && !f.all && !f.tags && !f.delete;
        if bare_default && snap.boolean("push.autoSetupRemote") == Some(true) {
            let push_default = snap
                .string("push.default")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "simple".to_string());
            let default_applies = !matches!(push_default.as_str(), "matching" | "nothing");
            let has_upstream = repo
                .head()
                .ok()
                .and_then(|h| h.referent_name().map(|n| n.shorten().to_string()))
                .map(|b| snap.string(&format!("branch.{b}.merge")).is_some())
                .unwrap_or(false);
            if default_applies && !has_upstream {
                f.set_upstream = true;
            }
        }
    }

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

    // `--recurse-submodules` handling, ported from git's `transport_push`
    // (transport.c): it runs after the pre-push hook and before the object upload.
    // `no` is a plain push; the other modes first look for submodules whose pushed
    // commit is not yet on their remote.
    if f.recurse != Recurse::Off {
        let needs = unpushed_submodules(&repo, &requests)?;
        if !needs.is_empty() {
            match f.recurse {
                // git's `die_with_unpushed_submodules` (transport.c) — abort, no writes.
                Recurse::Check => {
                    eprintln!("The following submodule paths contain changes that can");
                    eprintln!("not be found on any remote:");
                    for p in &needs {
                        eprintln!("  {p}");
                    }
                    eprintln!();
                    eprintln!("Please try");
                    eprintln!();
                    eprintln!("\tgit push --recurse-submodules=on-demand");
                    eprintln!();
                    eprintln!("or cd to the path and use");
                    eprintln!();
                    eprintln!("\tgit push");
                    eprintln!();
                    eprintln!("to push them to a remote.");
                    eprintln!();
                    bail!("Aborting.");
                }
                // git's `push_unpushed_submodules` recursively runs `git push` inside
                // each submodule (submodule.c). That transport recursion is not wired
                // here; silently skipping it would upload a superproject commit whose
                // submodule commits are absent from their remotes (data-losing), so
                // abort and tell the user to push the submodules first.
                Recurse::OnDemand | Recurse::Only => {
                    let mode = if f.recurse == Recurse::Only { "only" } else { "on-demand" };
                    let list = needs.join(", ");
                    bail!(
                        "--recurse-submodules={mode}: the submodule(s) [{list}] have commits not on their remote and must be pushed first (cd <path> && git push); recursive submodule push is not supported"
                    );
                }
                Recurse::Off => unreachable!("guarded by the outer `!= Recurse::Off`"),
            }
        }
        if f.recurse == Recurse::Only {
            // git never pushes the superproject under `only` (transport.c skips
            // `push_refs`); with no submodule to push, that leaves nothing to do.
            eprintln!("Everything up-to-date");
            return Ok(ExitCode::SUCCESS);
        }
    }

    // Every ref this repository still has, for the `--mirror`/`--prune` deletion
    // decision: an advertised ref absent from this set has no local counterpart.
    let local_refs: std::collections::HashSet<String> = repo
        .references()?
        .all()?
        .filter_map(|r| r.ok())
        .filter_map(|r| r.name().as_bstr().to_str().ok().map(str::to_owned))
        .collect();
    let delete_scope = if f.mirror {
        Some(push_proto::DeleteScope::All)
    } else if f.prune {
        // git prunes only within the namespaces the push's refspecs actually
        // COVER — a pattern refspec (`refs/heads/*:refs/heads/*`, which is what
        // `--all` and the configured default expand to). An explicit single-ref
        // push like `git push --prune origin main` covers exactly that one ref,
        // so it prunes nothing; deleting the rest of the namespace there would
        // destroy remote branches stock git leaves alone.
        let mut prefixes: Vec<String> = specs
            .iter()
            .filter(|s| s.contains('*'))
            .filter_map(|s| {
                let dst = s.rsplit_once(':').map(|(_, d)| d).unwrap_or(s.as_str());
                dst.split_once('*').map(|(prefix, _)| full_ref_name(prefix))
            })
            .collect();
        if f.all {
            prefixes.push("refs/heads/".to_string());
        }
        if prefixes.is_empty() { None } else { Some(push_proto::DeleteScope::Prefixes(prefixes)) }
    } else {
        None
    };
    let send_opts = push_proto::SendOptions {
        atomic: f.atomic,
        push_options: f.push_options.clone(),
        delete_scope,
        local_refs,
    };
    let outcome = push_proto::send_pack(&repo, &remote, &requests, f.dry_run, &send_opts)?;

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
        report(&outcome, f.verbose)
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
    mirror: bool,
    verbose: bool,
    prune: bool,
    atomic: bool,
    push_options: Vec<String>,
    follow_tags: bool,
    set_upstream: bool,
    porcelain: bool,
    repo: Option<String>,
    lease: Lease,
    recurse: Recurse,
}

/// `--recurse-submodules=<mode>` state. Ported from git's `RECURSE_SUBMODULES_*`
/// (submodule.h) as resolved by `parse_push_recurse` (submodule-config.c).
#[derive(Default, Clone, Copy, PartialEq)]
enum Recurse {
    /// `no` / `--no-recurse-submodules` — plain push, the flag is inert.
    #[default]
    Off,
    /// `check` — abort if any pushed submodule commit is missing from its remote.
    Check,
    /// `on-demand` — push the needed submodules first, then the superproject.
    OnDemand,
    /// `only` — push the submodules only, never the superproject.
    Only,
}

/// Parse a `--recurse-submodules` argument, a faithful port of git's
/// `parse_push_recurse` (submodule-config.c): a boolean-true value is rejected
/// (there is no plain "on" for pushing), boolean-false means `off`, and the named
/// modes map through directly.
fn parse_recurse(arg: &str) -> Result<Recurse> {
    match maybe_bool(arg) {
        Some(true) => bail!("bad recurse-submodules argument: {arg}"),
        Some(false) => Ok(Recurse::Off),
        None => match arg {
            "on-demand" => Ok(Recurse::OnDemand),
            "check" => Ok(Recurse::Check),
            "only" => Ok(Recurse::Only),
            _ => bail!("bad recurse-submodules argument: {arg}"),
        },
    }
}

/// git's `git_parse_maybe_bool`: recognized boolean spellings plus any integer
/// (non-zero is true); anything else is `None` so the caller can treat it as a
/// named value.
fn maybe_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" | "" => Some(false),
        s => s.parse::<i64>().ok().map(|n| n != 0),
    }
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

    // `--mirror` pushes EVERY ref under `refs/` — branches, tags, remote-tracking
    // refs, notes — each forced, each to its own name. The deletion half (remote
    // refs this repository no longer has) is synthesized in the wire layer, which
    // is the only place the advertisement exists.
    if f.mirror {
        if !specs.is_empty() {
            bail!("--mirror can't be combined with refspecs");
        }
        for r in repo.references()?.all()? {
            let r = r.map_err(|e| anyhow!("{e}"))?;
            let name = r.name().as_bstr().to_str().map_err(|e| anyhow!("{e}"))?.to_string();
            // Only real refs under refs/ travel: HEAD and other root refs are not
            // part of the mirror set.
            if !name.starts_with("refs/") {
                continue;
            }
            if let Some(id) = r.try_id() {
                requests.push(Request {
                    name,
                    new: id.detach(),
                    force: true,
                    expected: None,
                    only_if_absent: false,
                });
            }
        }
        return Ok((requests, upstreams));
    }

    if f.all {
        if !specs.is_empty() {
            bail!("--all can't be combined with refspecs");
        }
        for r in repo.references()?.local_branches()? {
            let r = r.map_err(|e| anyhow!("{e}"))?;
            let name = r.name().as_bstr().to_str().map_err(|e| anyhow!("{e}"))?.to_string();
            if let Some(id) = r.try_id() {
                let short = short_ref(&name).to_string();
                requests.push(Request { name: name.clone(), new: id.detach(), force: f.force, expected: None, only_if_absent: false });
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
            let r = r.map_err(|e| anyhow!("{e}"))?;
            let name = r.name().as_bstr().to_str().map_err(|e| anyhow!("{e}"))?.to_string();
            // The ref's OWN target, never the peeled commit: for an annotated tag
            // that is the tag object, and pushing the peeled commit instead would
            // publish a LIGHTWEIGHT tag under an annotated tag's name — the
            // message, tagger and signature silently dropped, and every later
            // fetch reporting "would clobber existing tag" because the two sides
            // now name different objects.
            if let Some(id) = r.try_id() {
                requests.push(Request { name, new: id.detach(), force: f.force, expected: None, only_if_absent: false });
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
                only_if_absent: false,
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
            // A PATTERN refspec (`refs/heads/*:refs/heads/*`) expands to one
            // update per matching local ref, with the matched tail substituted
            // into the destination — git's `match_push_refs` glob handling. This
            // is also the shape `--prune` needs to be meaningful, since only a
            // pattern covers a whole namespace.
            if spec.contains('*') {
                requests.extend(expand_pattern_refspec(repo, spec, f.force)?);
                continue;
            }
            let (req, up) = parse_refspec(repo, spec, f.force)?;
            requests.push(req);
            if let Some(up) = up {
                upstreams.push(up);
            }
        }
    }

    if f.follow_tags {
        append_followed_tags(repo, &mut requests)?;
    }
    Ok((requests, upstreams))
}

/// `--follow-tags` — add the **annotated** tags reachable from the refs already
/// being pushed (git: "annotated tags in `refs/tags` … pointing at commit-ish
/// that are reachable from the refs being pushed").
///
/// Two filters, both of them git's:
///   * annotated only — a lightweight tag is a ref straight to a commit and is
///     never followed; the ref must resolve to a *tag object*.
///   * reachable only — the tag's peeled commit has to be an ancestor of (or
///     equal to) one of the tips this push already carries, so tagging an
///     unrelated branch does not smuggle it along.
///
/// Deletions are excluded: `--follow-tags` with `--delete` would be adding refs
/// to a removal, which git does not do either.
///
/// The "missing from the remote" half is enforced one layer down: each followed
/// tag is marked `only_if_absent`, and `push_proto::send_pack` drops it once the
/// ref advertisement (which only it has read by then) shows the remote already
/// carries that tag. Sending them anyway would turn a differing remote tag into
/// a non-fast-forward rejection and fail an otherwise clean push.
fn append_followed_tags(repo: &gix::Repository, requests: &mut Vec<Request>) -> Result<()> {
    let tips: Vec<ObjectId> = requests.iter().filter(|r| !r.new.is_null()).map(|r| r.new).collect();
    if tips.is_empty() {
        return Ok(());
    }
    let already: std::collections::HashSet<String> =
        requests.iter().map(|r| r.name.clone()).collect();

    for r in repo.references()?.tags()? {
        let mut r = r.map_err(|e| anyhow!("{e}"))?;
        let name = r.name().as_bstr().to_str().map_err(|e| anyhow!("{e}"))?.to_string();
        if already.contains(&name) {
            continue;
        }
        // Annotated means the ref's own target is a tag object; peeling it yields
        // the commit the tag names. BOTH ids matter and they are not
        // interchangeable: reachability is asked of the peeled commit, but what
        // gets pushed is the tag object itself. Pushing the peeled id would put a
        // lightweight tag on the remote under an annotated tag's name.
        let tag_object = match r.try_id() {
            Some(id) => id.detach(),
            None => continue,
        };
        let is_annotated = repo
            .find_object(tag_object)
            .map(|o| o.kind == gix::object::Kind::Tag)
            .unwrap_or(false);
        if !is_annotated {
            continue;
        }
        let Ok(peeled) = r.peel_to_id_in_place() else { continue };
        let peeled = peeled.detach();
        if tips.iter().any(|tip| reachable(repo, peeled, *tip)) {
            // Never forced: a followed tag is an addition, and git refuses to
            // clobber a differing remote tag here just as it does for `--tags`.
            requests.push(Request { name, new: tag_object, force: false, expected: None, only_if_absent: true });
        }
    }
    Ok(())
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
    let (src, dst_spec) = match spec.split_once(':') {
        Some((s, d)) => (s, Some(d)),
        None => (spec, None),
    };

    let new = if src.is_empty() {
        null(repo) // `:dst` deletes the remote ref.
    } else {
        repo.rev_parse_single(src)
            .map_err(|_| anyhow!("src refspec {src} does not match any"))?
            .detach()
    };

    // The source's full local ref name, resolved with git's DWIM precedence (tags
    // before heads), so the destination lands in the right namespace: `git push
    // origin v0.1.0` on a tag pushes `refs/tags/v0.1.0`, not `refs/heads/v0.1.0`.
    let src_full = if src.is_empty() { None } else { resolve_src_full(repo, src) };

    let dst_full = match dst_spec {
        // Bare `src` (or `src:`): push to the source's own full name — git's
        // `matched_src->name` default. A tag stays a tag, a branch stays a branch.
        None => src_full.clone().unwrap_or_else(|| full_ref_name(src)),
        Some(d) if d.is_empty() => src_full.clone().unwrap_or_else(|| full_ref_name(src)),
        // A fully-qualified destination is used verbatim.
        Some(d) if d.starts_with("refs/") => d.to_string(),
        // A short explicit destination is prefixed by the source ref's kind, exactly
        // as git's `guess_ref` does (tag → refs/tags/, otherwise refs/heads/).
        Some(d) => match &src_full {
            Some(sf) if sf.starts_with("refs/tags/") => format!("refs/tags/{d}"),
            _ => full_ref_name(d),
        },
    };

    // Record an upstream only when the source is a local branch.
    let upstream = match &src_full {
        Some(sf) if sf.starts_with("refs/heads/") => Some((src.to_string(), dst_full.clone())),
        _ => None,
    };
    Ok((
        Request {
            name: dst_full,
            new,
            force,
            expected: None,
            only_if_absent: false,
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
            only_if_absent: false,
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

/// Resolve a (possibly short) push source name to its full local ref name, using
/// git's DWIM precedence — `refs/<name>`, then `refs/tags/<name>`, then
/// `refs/heads/<name>`, then `refs/remotes/<name>` (git's `ref_rev_parse_rules`
/// order, tags before heads). So `git push origin v0.1.0` finds the tag
/// `refs/tags/v0.1.0` before a same-named branch, and the destination mirrors it.
/// A name already starting with `refs/` is looked up directly. `None` if no local
/// ref matches (the caller falls back to treating it as a branch). The bare-name
/// (`HEAD`) rule is intentionally omitted so `push HEAD` keeps its branch fallback.
fn resolve_src_full(repo: &gix::Repository, name: &str) -> Option<String> {
    let full = |candidate: &str| {
        repo.find_reference(candidate)
            .ok()
            .and_then(|r| r.name().as_bstr().to_str().ok().map(str::to_string))
    };
    if name.starts_with("refs/") {
        return full(name);
    }
    [
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
    ]
    .iter()
    .find_map(|candidate| full(candidate))
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
fn report(outcome: &push_proto::Outcome, verbose: bool) -> Result<ExitCode> {
    let mut any_failed = outcome.unpack.is_err();
    if let Err(reason) = &outcome.unpack {
        eprintln!("error: unpack failed: {reason}");
    }

    let did_update = outcome
        .statuses
        .iter()
        .any(|s| !s.up_to_date && s.result.is_ok());
    let nothing_moved =
        !did_update && !any_failed && outcome.statuses.iter().all(|s| s.result.is_ok());
    // Under `-v` git still prints the `To <url>` block listing each unchanged
    // ref, and only THEN the summary line; the default output is the summary
    // alone.
    if nothing_moved && !verbose {
        eprintln!("Everything up-to-date");
        return Ok(ExitCode::SUCCESS);
    }

    eprintln!("To {}", outcome.url);
    for s in &outcome.statuses {
        let short = |oid: &ObjectId| oid.to_hex_with_len(7).to_string();
        let src_dst = format!("{} -> {}", short_ref(&s.name), short_ref(&s.name));
        match &s.result {
            // git lists an unchanged ref only under `-v`; the default block shows
            // just what moved.
            Ok(()) if s.up_to_date => {
                if verbose {
                    eprintln!(" = [up to date]      {src_dst}");
                }
            }
            Ok(()) if s.old.is_null() => {
                // git names the created ref by its namespace: a branch, a tag, or
                // — for anything else, notes and remote-tracking refs included,
                // which `--mirror` pushes — the generic "new reference".
                let kind = if s.name.starts_with("refs/tags/") {
                    "[new tag]     "
                } else if s.name.starts_with("refs/heads/") {
                    "[new branch]  "
                } else {
                    "[new reference]"
                };
                eprintln!(" * {kind}    {src_dst}");
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
    if nothing_moved {
        eprintln!("Everything up-to-date");
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
    // git's `prettify_refname`: the three well-known namespaces are stripped,
    // anything else (notes, replace, a custom hierarchy) prints in full.
    name.strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/tags/"))
        .or_else(|| name.strip_prefix("refs/remotes/"))
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

/// The submodule paths whose commit referenced by the pushed superproject tips is
/// present locally but not yet reachable from any of the submodule's remotes — the
/// submodules `git push` would have to push first.
///
/// This ports git's `find_unpushed_submodules` / `submodule_needs_pushing`
/// (submodule.c): for each active submodule, take the gitlink commit it is pinned
/// to in each pushed superproject commit, and flag the submodule when that commit
/// exists in the submodule's object store yet is not reachable from any
/// `refs/remotes/*` ref (git's `rev-list <commit> --not --remotes`). A submodule
/// that is not checked out, or has no remote-tracking refs, is treated as "no push
/// needed" exactly as git does.
///
/// Scope note: git additionally walks the whole pushed commit range (`--not
/// --remotes`) to collect submodule commits that appear only mid-range; here only
/// the pushed ref tips are inspected, which is faithful for the ordinary
/// tip-advancing push but does not catch a submodule bumped and then reverted
/// within the pushed range.
fn unpushed_submodules(repo: &gix::Repository, requests: &[Request]) -> Result<Vec<String>> {
    let submodules = match repo.submodules()? {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    // The superproject commit tips being pushed (git collects every non-null
    // `ref->new_oid`).
    let tips: Vec<ObjectId> = requests
        .iter()
        .filter(|r| !r.new.is_null())
        .map(|r| r.new)
        .collect();

    let mut needs_pushing: Vec<String> = Vec::new();
    for sub in submodules {
        if !sub.is_active().unwrap_or(false) {
            continue;
        }
        let path = match sub.path() {
            Ok(p) => match p.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        // The submodule's own repository. Absent (not checked out) => git considers
        // it safe to skip (`submodule_has_commits` is 0).
        let sub_repo = match sub.open() {
            Ok(Some(r)) => r,
            _ => continue,
        };
        // The submodule's remote-tracking refs (git's `--remotes`). With none, git
        // reports "no push needed".
        let remote_tips: Vec<ObjectId> = match sub_repo.references() {
            Ok(platform) => match platform.remote_branches() {
                Ok(iter) => iter
                    .filter_map(|r| r.ok())
                    .filter_map(|mut r| r.peel_to_id_in_place().ok().map(|id| id.detach()))
                    .collect(),
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        if remote_tips.is_empty() {
            continue;
        }

        let mut flagged = false;
        for tip in &tips {
            // The commit this submodule is pinned to in the pushed superproject tip.
            let tree = match repo.find_commit(*tip) {
                Ok(commit) => match commit.tree() {
                    Ok(tree) => tree,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            let gitlink = match tree.lookup_entry_by_path(std::path::Path::new(&path)) {
                Ok(Some(entry)) if entry.mode().is_commit() => entry.object_id(),
                _ => continue,
            };
            // Present in the submodule's object store? Absent => safe to skip.
            if sub_repo.find_object(gitlink).is_err() {
                continue;
            }
            // Already reachable from one of the submodule's remotes => nothing to push.
            if !remote_tips.iter().any(|t| reachable(&sub_repo, gitlink, *t)) {
                flagged = true;
                break;
            }
        }
        if flagged {
            needs_pushing.push(path);
        }
    }
    Ok(needs_pushing)
}

/// Whether `commit` is reachable from `tip` (git's `repo_in_merge_bases`): true
/// when `commit` is a merge-base of `tip` with itself, i.e. an ancestor of `tip`.
fn reachable(repo: &gix::Repository, commit: ObjectId, tip: ObjectId) -> bool {
    if commit == tip {
        return true;
    }
    match repo.merge_bases_many(tip, &[commit]) {
        Ok(bases) => bases.iter().any(|b| b.detach() == commit),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the fixture repo with **no external git and no subprocess** — every
    /// object is written through `gix`, the same library the engine itself runs
    /// on. A test suite for a VCS must not ask another VCS to set up its state:
    /// on a machine where `git` on `PATH` *is* this binary that is circular, and
    /// on one where it is not, the fixture is whatever foreign implementation
    /// happened to be installed.
    ///
    /// Layout:
    ///
    ///   main:  c1 ──(v1, annotated)── c2 ──(light, lightweight)
    ///   side:  c1 ── s1 ──(vside, annotated)
    ///
    /// so one tag of each kind sits on `main`'s history and one annotated tag
    /// sits off it.
    fn fixture(tag: &str) -> std::path::PathBuf {
        use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

        let dir = std::env::temp_dir().join(format!("zvcs-followtags-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir fixture");
        let repo = gix::init(&dir).expect("init fixture");

        let sig = gix::actor::Signature {
            name: "t".into(),
            email: "t@e".into(),
            time: gix::date::Time::new(0, 0),
        };
        let empty_tree = repo.write_object(gix::objs::Tree::empty()).expect("tree").detach();

        // One commit on top of `parents`, returning its id.
        let commit = |message: &str, parents: Vec<ObjectId>| -> ObjectId {
            repo.write_object(gix::objs::Commit {
                tree: empty_tree,
                parents: parents.into(),
                author: sig.clone(),
                committer: sig.clone(),
                encoding: None,
                message: message.into(),
                extra_headers: Vec::new(),
            })
            .expect("commit")
            .detach()
        };
        // An annotated tag object pointing at `target`, returning the TAG's id —
        // the thing that makes the ref annotated rather than lightweight.
        let annotate = |name: &str, target: ObjectId| -> ObjectId {
            repo.write_object(gix::objs::Tag {
                target,
                target_kind: gix::object::Kind::Commit,
                name: name.into(),
                tagger: Some(sig.clone()),
                message: format!("annotated {name}").into(),
                pgp_signature: None,
            })
            .expect("tag object")
            .detach()
        };
        let point = |full_name: &str, id: ObjectId| {
            repo.edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange { mode: RefLog::AndReference, force_create_reflog: false, message: "fixture".into() },
                    expected: PreviousValue::Any,
                    new: Target::Object(id),
                },
                name: full_name.try_into().expect("ref name"),
                deref: false,
            })
            .expect("edit ref");
        };

        let c1 = commit("c1", vec![]);
        let c2 = commit("c2", vec![c1]);
        let s1 = commit("s1", vec![c1]);

        point("refs/heads/main", c2);
        point("refs/heads/side", s1);
        point("refs/tags/v1", annotate("v1", c1)); // annotated, on main
        point("refs/tags/light", c2); // lightweight, on main
        point("refs/tags/vside", annotate("vside", s1)); // annotated, off main

        // HEAD → main, so `head_id()` is the tip the tests push.
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange { mode: RefLog::AndReference, force_create_reflog: false, message: "fixture".into() },
                expected: PreviousValue::Any,
                new: Target::Symbolic("refs/heads/main".try_into().expect("ref name")),
            },
            name: "HEAD".try_into().expect("ref name"),
            deref: false,
        })
        .expect("point HEAD");

        dir
    }

    /// The name of every ref `append_followed_tags` added to a push of `main`.
    fn followed(dir: &std::path::Path) -> Vec<String> {
        let repo = gix::open(dir).expect("open fixture");
        let tip = repo.head_id().expect("head").detach();
        let mut requests = vec![Request {
            name: "refs/heads/main".into(),
            new: tip,
            force: false,
            expected: None,
            only_if_absent: false,
        }];
        append_followed_tags(&repo, &mut requests).expect("append");
        requests.into_iter().skip(1).map(|r| r.name).collect()
    }

    #[test]
    fn follows_only_annotated_tags_reachable_from_the_pushed_tip() {
        let dir = fixture("select");
        assert_eq!(
            followed(&dir),
            vec!["refs/tags/v1".to_string()],
            "v1 is annotated and on main's history; `light` is lightweight and \
             `vside` is annotated but only on side"
        );
    }

    #[test]
    fn followed_tags_are_marked_absent_only_and_never_forced() {
        let dir = fixture("flags");
        let repo = gix::open(&dir).expect("open fixture");
        let tip = repo.head_id().expect("head").detach();
        let mut requests = vec![Request {
            name: "refs/heads/main".into(),
            new: tip,
            // Even under `--force`, a followed tag must not inherit the force bit.
            force: true,
            expected: None,
            only_if_absent: false,
        }];
        append_followed_tags(&repo, &mut requests).expect("append");

        let tag = requests.last().expect("a tag was added");
        assert_eq!(tag.name, "refs/tags/v1");
        // The pushed id must be the TAG OBJECT, not the commit it peels to.
        // Pushing the peeled id publishes a lightweight tag under an annotated
        // tag's name and makes every later fetch report "would clobber
        // existing tag" — which is exactly what happened to six real tags.
        let pushed = repo.find_object(tag.new).expect("pushed object exists");
        assert_eq!(pushed.kind, gix::object::Kind::Tag, "pushed the tag object");
        assert!(!tag.force, "a followed tag is an addition, never a clobber");
        assert!(tag.only_if_absent, "the wire layer drops it when the remote has it");
    }

    #[test]
    fn a_deletion_only_push_follows_nothing() {
        let dir = fixture("delete");
        let repo = gix::open(&dir).expect("open fixture");
        let mut requests = vec![Request {
            name: "refs/heads/gone".into(),
            new: gix::hash::ObjectId::null(repo.object_hash()),
            force: false,
            expected: None,
            only_if_absent: false,
        }];
        append_followed_tags(&repo, &mut requests).expect("append");
        assert_eq!(requests.len(), 1, "no tips to be reachable from, so no tags");
    }
}

/// Expand a pattern refspec — `[+]<src-prefix>*<src-suffix>:<dst-prefix>*<dst-suffix>`
/// — into one [`Request`] per matching local ref.
///
/// Ports the glob half of git's `match_push_refs`: exactly one `*` on each side,
/// the text it matched on the source carried into the destination, and a missing
/// destination meaning "same name over there". The refs are walked once, in name
/// order, so the pushed set is deterministic.
///
/// Only refs are matched, never revisions: `refs/heads/*` cannot select a commit,
/// which is why this is separate from [`parse_refspec`]'s `rev_parse_single`.
fn expand_pattern_refspec(
    repo: &gix::Repository,
    spec: &str,
    force: bool,
) -> Result<Vec<Request>> {
    let (spec, force) = match spec.strip_prefix('+') {
        Some(rest) => (rest, true),
        None => (spec, force),
    };
    let (src, dst) = match spec.split_once(':') {
        Some((s, d)) => (s, d),
        None => (spec, spec),
    };
    if src.matches('*').count() != 1 || dst.matches('*').count() != 1 {
        bail!("invalid refspec '{spec}': a pattern needs exactly one '*' on each side");
    }
    let (src_prefix, src_suffix) = src.split_once('*').expect("checked above");
    let (dst_prefix, dst_suffix) = dst.split_once('*').expect("checked above");
    let src_prefix = full_ref_name(src_prefix);
    let dst_prefix = full_ref_name(dst_prefix);

    let mut out = Vec::new();
    let mut names: Vec<(String, ObjectId)> = Vec::new();
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow!("{e}"))?;
        let name = r.name().as_bstr().to_str().map_err(|e| anyhow!("{e}"))?.to_string();
        if let Some(id) = r.try_id() {
            names.push((name, id.detach()));
        }
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, id) in names {
        let Some(rest) = name.strip_prefix(src_prefix.as_str()) else { continue };
        let Some(matched) = rest.strip_suffix(src_suffix) else { continue };
        if matched.is_empty() {
            continue;
        }
        out.push(Request {
            name: format!("{dst_prefix}{matched}{dst_suffix}"),
            new: id,
            force,
            expected: None,
            only_if_absent: false,
        });
    }
    Ok(out)
}
