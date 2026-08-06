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
/// `--mirror`, `--prune`, `--atomic`, `--signed[=<mode>]` and `-o/--push-option`
/// are negotiated by [`super::push_proto`], which refuses the push when the
/// receiving end lacks the matching capability rather than downgrading it
/// silently; inert or already-matched flags (`--thin`, `-4/-6`, `--verify`, …)
/// are accepted. `--receive-pack=<path>` / `--exec=<path>` (else
/// `remote.<name>.receivepack`) reaches the transport, which runs it in place of
/// `git-receive-pack` on the other end.
///
/// The response is read as a `side-band-64k` stream whenever the server offers
/// one, so everything the server and its hooks write comes back as `remote: …`
/// lines on stderr, colored per `color.remote[.hint|.warning|.success|.error]`.
///
/// `--force-if-includes` is honored for real: alongside a `--force-with-lease`
/// whose expected value came from a remote-tracking ref, the tip the remote
/// advertises must also be reachable from the pushed ref's reflog, or the update
/// is rejected with `remote ref updated since checkout` (see
/// [`super::push_proto`]).
///
/// The `push.*` defaults honored here are `push.recurseSubmodules`,
/// `push.followTags`, `push.useForceIfIncludes`, `push.autoSetupRemote` (with
/// `push.default`), `push.gpgSign` and `push.pushOption`; an explicit
/// command-line flag always wins, because git reads config in `git_push_config`
/// *before* `parse_options`.
pub fn push(args: &[String]) -> Result<ExitCode> {
    let mut f = Flags::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut end_of_options = false;
    // Whether `--recurse-submodules`/`--no-recurse-submodules` was given on the
    // command line; if so it wins over `push.recurseSubmodules` (git reads config
    // before parse_options, so the flag's assignment lands last).
    let mut recurse_explicit = false;
    // Same for `--follow-tags`/`--no-follow-tags` against `push.followTags`.
    let mut follow_tags_explicit = false;
    let mut force_if_includes_explicit = false;
    // Same for `--signed`/`--no-signed` against `push.gpgSign`.
    let mut signed_explicit = false;
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
            "--follow-tags" => {
                f.follow_tags = true;
                follow_tags_explicit = true;
            }
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
            | "--verify" | "--no-verify"
            => {}
            // `--force-if-includes` only ever does anything alongside a
            // `--force-with-lease` that takes its expected value from the
            // remote-tracking ref (`apply_cas()` sets `check_reachable` exactly
            // there). Anywhere else git documents it as a no-op.
            "--force-if-includes" => {
                f.force_if_includes = true;
                force_if_includes_explicit = true;
            }
            "--no-force-if-includes" => {
                f.force_if_includes = false;
                force_if_includes_explicit = true;
            }
            "--no-signed" => {
                f.signed = push_proto::Signed::Never;
                signed_explicit = true;
            }
            "--no-atomic" => f.atomic = false,
            "--no-mirror" => f.mirror = false,
            "--no-prune" => f.prune = false,
            "--no-follow-tags" => {
                f.follow_tags = false;
                follow_tags_explicit = true;
            }
            // `--receive-pack=<path>` and its hidden synonym `--exec`: the program
            // to run in place of `git-receive-pack` on the other end. git passes it
            // straight to `git_connect()` for the push direction, so it reaches the
            // transport rather than the protocol (`connect_setup()`, transport.c:314).
            "--receive-pack" | "--exec" => f.receive_pack = Some(take_value(inline)?),
            "--no-receive-pack" | "--no-exec" => f.receive_pack = None,
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
            // `--signed[=<mode>]`: send a gpg-signed push certificate. The
            // modes are git's — `if-asked` only signs when the server offers a
            // nonce, plain/`true` insists on one.
            "--signed" => {
                f.signed = match inline.as_deref() {
                    None | Some("true") | Some("yes") => push_proto::Signed::Always,
                    Some("false") | Some("no") => push_proto::Signed::Never,
                    Some("if-asked") => push_proto::Signed::IfAsked,
                    Some(v) => crate::git_fatal!("bad signed argument: {v}"),
                };
                signed_explicit = true;
            }
            other => bail!("unsupported option {other:?}"),
        }
        i += 1;
    }

    // Conflicts git rejects before contacting the remote.
    if f.tags && f.all {
        crate::git_fatal!("--all can't be combined with --tags");
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

        // `push.followTags` — the default for `--follow-tags`
        // (`git_push_config`: `TRANSPORT_PUSH_FOLLOW_TAGS`). The flag, in
        // either direction, overrides it.
        if !follow_tags_explicit {
            if let Some(on) = snap.boolean("push.followTags") {
                f.follow_tags = on;
            }
        }

        // `push.useForceIfIncludes` — the default for `--force-if-includes`
        // (`git_push_config`: `TRANSPORT_PUSH_FORCE_IF_INCLUDES`, builtin/push.c:537).
        // The flag, in either direction, overrides it.
        if !force_if_includes_explicit {
            if let Some(on) = snap.boolean("push.useForceIfIncludes") {
                f.force_if_includes = on;
            }
        }

        // `push.gpgSign` — the default for `--signed`. git's `git_push_config`
        // runs the value through `git_parse_maybe_bool` and maps false/true to
        // NEVER/ALWAYS, the literal `if-asked` to IF_ASKED, and dies on anything
        // else. `--signed` in any form is parsed afterwards and so wins.
        if !signed_explicit {
            if let Some(v) = snap.string("push.gpgSign") {
                let v = v.to_str_lossy().into_owned();
                f.signed = match maybe_bool(&v) {
                    Some(false) => push_proto::Signed::Never,
                    Some(true) => push_proto::Signed::Always,
                    None if v.eq_ignore_ascii_case("if-asked") => push_proto::Signed::IfAsked,
                    None => crate::git_fatal!("invalid value for 'push.gpgSign'"),
                };
            }
        }

        // `push.pushOption` — the default list for `-o/--push-option`. git reads
        // it with `parse_transport_option`, where an empty value *clears* what
        // earlier occurrences accumulated, and then picks the command-line list
        // over the configured one whole (`push_options_cmdline.nr ? … : …`) —
        // the two are never merged.
        if f.push_options.is_empty() {
            f.push_options = snap
                .plumbing()
                .strings("push.pushOption")
                .unwrap_or_default()
                .iter()
                .map(|v| v.to_str_lossy().into_owned())
                .fold(Vec::new(), |mut acc, value| {
                    if value.is_empty() {
                        acc.clear();
                    } else {
                        acc.push(value);
                    }
                    acc
                });
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

    // git validates the push-option list once, after the command line and
    // `push.pushOption` have been reconciled, so a configured value is checked too.
    if f.push_options.iter().any(|o| o.contains('\n')) {
        crate::git_fatal!("push options must not have new line characters");
    }

    let remote = match repo.find_remote(remote_name.as_str()) {
        Ok(r) => r,
        Err(_) => {
            // Not a configured remote, so the name is a URL or a path. When it
            // is neither — no such directory — `git_connect()` runs
            // `git-receive-pack '<dest>'`, which dies with `enter_repo`'s
            // message, and the parent follows with `die_initial_contact`. The
            // vendored transport reports one Rust-level metadata error instead,
            // so both lines and the 128 are reproduced here, as `send-pack`
            // already does for the same case.
            if let Some(bad) =
                super::send_pack::local_dest_that_is_not_a_repository(remote_name.as_str())
            {
                eprintln!("fatal: '{bad}' does not appear to be a git repository");
                eprintln!(
                    "fatal: Could not read from remote repository.\n\n\
                     Please make sure you have the correct access rights\n\
                     and the repository exists."
                );
                return Ok(ExitCode::from(128));
            }
            repo.remote_at(remote_name.as_str())?
        }
    };

    // Build the concrete updates, plus the (local-branch, remote-ref) pairs that
    // `--set-upstream` records after a successful push.
    let (mut requests, upstreams) = build_requests(&repo, &f, &specs)?;

    // Resolve `--force-with-lease` into each request's expected old value.
    if !matches!(f.lease, Lease::None) {
        for req in &mut requests {
            req.expected = lease_for(&repo, &remote, &f.lease, &req.name);
        }
        // `--force-if-includes` turns each of those leases into an additional
        // reachability check (`check_if_includes_upstream`) over the reflog of the
        // ref being pushed. `apply_cas()` arms it on exactly one branch: a lease
        // whose expected value came from a remote-tracking ref that was actually
        // read (remote.c:2837, :2851). An explicit `<ref>:<expect>`, or a tracking
        // ref that does not exist, leaves it disarmed — which is why git documents
        // the flag as a no-op there.
        //
        // The check itself needs the tip the remote advertises, so it runs in
        // `push_proto`; all that is decided here is which tracking ref each
        // request is measured against.
        if f.force_if_includes {
            for req in &mut requests {
                if !lease_uses_tracking(&f.lease, &req.name) {
                    continue;
                }
                req.check_reachable = tracking_ref_for(&remote, &req.name)
                    .filter(|name| repo.find_reference(name.as_str()).is_ok());
            }
        }
    }

    if requests.is_empty() {
        crate::git_fatal!("no refspec to push");
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
                    crate::git_fatal!("Aborting.");
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
    // `remote.<name>.receivepack` is the default for `--receive-pack`
    // (`transport_get`, transport.c:1252-1254: the smart option starts at
    // `git-receive-pack` and the remote's value replaces it). The flag wins,
    // because `set_git_option(TRANS_OPT_RECEIVEPACK)` runs afterwards.
    let receive_pack = f.receive_pack.clone().or_else(|| {
        repo.config_snapshot()
            .plumbing()
            .string_by("remote", Some(remote_name.as_str().into()), "receivepack")
            .map(|v| v.to_string())
    });
    let send_opts = push_proto::SendOptions {
        atomic: f.atomic,
        signed: f.signed,
        push_options: f.push_options.clone(),
        delete_scope,
        local_refs,
        receive_pack,
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
    signed: push_proto::Signed,
    push_options: Vec<String>,
    follow_tags: bool,
    set_upstream: bool,
    porcelain: bool,
    repo: Option<String>,
    lease: Lease,
    /// `--force-if-includes`. Inert unless a lease resolves its expected value
    /// from a remote-tracking ref; see the option-parsing arm for why.
    force_if_includes: bool,
    /// `--receive-pack`/`--exec`, else `remote.<name>.receivepack`.
    receive_pack: Option<String>,
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
        Some(true) => crate::git_fatal!("bad recurse-submodules argument: {arg}"),
        Some(false) => Ok(Recurse::Off),
        None => match arg {
            "on-demand" => Ok(Recurse::OnDemand),
            "check" => Ok(Recurse::Check),
            "only" => Ok(Recurse::Only),
            _ => crate::git_fatal!("bad recurse-submodules argument: {arg}"),
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

/// Whether the lease covering `remote_ref` takes its expected value from the
/// remote-tracking ref rather than from a value spelled on the command line —
/// git's `entry->use_tracking` / `use_tracking_for_rest`, the only case in which
/// `--force-if-includes` has any effect.
fn lease_uses_tracking(lease: &Lease, remote_ref: &str) -> bool {
    match lease {
        Lease::None => false,
        Lease::Implicit => true,
        Lease::Explicit { ref_name, expect } => expect.is_none() && ref_matches(ref_name, remote_ref),
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
            crate::git_fatal!("--mirror can't be combined with refspecs");
        }
        for r in repo.references()?.all()? {
            let mut r = r.map_err(|e| anyhow!("{e}"))?;
            let name = r.name().as_bstr().to_str().map_err(|e| anyhow!("{e}"))?.to_string();
            // Only real refs under refs/ travel: HEAD and other root refs are not
            // part of the mirror set.
            let Some(tail) = name.strip_prefix("refs/") else { continue };
            // `update()` on the receiving end refuses a name whose part after
            // `refs/` is not a valid refname (`check_refname_format(name + 5, 0)`,
            // receive-pack.c:1096) — a one-level name such as `refs/stash` is a
            // "funny refname" there. git's matching pass leaves such a ref out
            // rather than pushing it to be rejected, so `--mirror` does not carry
            // `refs/stash`.
            if !super::check_ref_format::check_refname_format(tail.as_bytes(), 0) {
                continue;
            }
            // A symbolic ref (`refs/remotes/<remote>/HEAD`, which a clone writes)
            // is mirrored as the object it resolves to; `try_id()` alone would skip
            // it, since a symref holds no id of its own.
            if let Some(id) = r.try_id().map(|id| id.detach()).or_else(|| {
                r.peel_to_id().ok().map(|id| id.detach())
            }) {
                requests.push(Request {
                    src: Some(name.clone()),
                    name,
                    new: id,
                    force: true,
                    expected: None,
                    only_if_absent: false, check_reachable: None, explicit_delete: false,
                });
            }
        }
        return Ok((requests, upstreams));
    }

    if f.all {
        if !specs.is_empty() {
            crate::git_fatal!("--all can't be combined with refspecs");
        }
        for r in repo.references()?.local_branches()? {
            let r = r.map_err(|e| anyhow!("{e}"))?;
            let name = r.name().as_bstr().to_str().map_err(|e| anyhow!("{e}"))?.to_string();
            if let Some(id) = r.try_id() {
                let short = short_ref(&name).to_string();
                requests.push(Request { src: Some(name.clone()), name: name.clone(), new: id.detach(), force: f.force, expected: None, only_if_absent: false, check_reachable: None, explicit_delete: false });
                upstreams.push((short, name));
            }
        }
        return Ok((requests, upstreams));
    }

    // `--tags` ADDS every tag to whatever was asked for: git documents it as "all
    // refs under refs/tags are pushed, in addition to refspecs explicitly listed
    // on the command line". Refusing the combination broke the ordinary release
    // command, `git push origin main --tags`, which stock git accepts. (`--all`
    // above is different: git really does reject it alongside a refspec.)
    let mut tag_requests: Vec<Request> = Vec::new();
    if f.tags {
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
                tag_requests.push(Request { src: Some(name.clone()), name, new: id.detach(), force: f.force, expected: None, only_if_absent: false, check_reachable: None, explicit_delete: false });
            }
        }
        // Only `--tags` on its own is complete here; with refspecs alongside, the
        // named refs still have to be resolved below and pushed with the tags.
        if specs.is_empty() {
            requests.append(&mut tag_requests);
            return Ok((requests, upstreams));
        }
    }

    if f.delete {
        for spec in specs {
            requests.push(Request {
                name: full_ref_name(spec),
                src: None,
                new: null(repo),
                force: f.force,
                expected: None,
                only_if_absent: false,
                check_reachable: None,
                // `match_explicit()`: a destination that matches no advertised ref
                // is turned into a new linked ref when it is `refs/`-qualified —
                // the delete then goes out and the server answers it — and is an
                // error only when it is not, because there is nothing to qualify
                // it with (remote.c:1152-1163).
                explicit_delete: !spec.starts_with("refs/"),
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
    requests.append(&mut tag_requests);
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
        let Ok(peeled) = r.peel_to_id() else { continue };
        let peeled = peeled.detach();
        if tips.iter().any(|tip| reachable(repo, peeled, *tip)) {
            // Never forced: a followed tag is an addition, and git refuses to
            // clobber a differing remote tag here just as it does for `--tags`.
            requests.push(Request { src: Some(name.clone()), name, new: tag_object, force: false, expected: None, only_if_absent: true, check_reachable: None, explicit_delete: false });
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
        // Bare `src` (or `src:`): `match_explicit()` resolves the destination as
        // `refs_resolve_ref_unsafe(matched_src->name)` — the *resolved* name, so
        // a symbolic source such as `HEAD` pushes the branch it points at. A tag
        // stays a tag, a branch stays a branch.
        None | Some("") => resolve_bare_dst(repo, src_full.as_deref().unwrap_or(src))?,
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
            // `ref->peer_ref`: the local ref the update reads from. A `:dst`
            // deletion has none, which is why git prints it with no source.
            // `match_explicit_lhs()` also accepts a `<src>` that is not a ref at
            // all — a raw object name, or `HEAD` — and `matched_src->name` is
            // then the text the user wrote, which is what the report shows.
            src: src_full.or_else(|| (!src.is_empty()).then(|| src.to_string())),
            new,
            force,
            expected: None,
            only_if_absent: false,
            check_reachable: None,
            // `:<dst>` is `--delete <dst>` written the other way round
            // (git-push(1): "--delete … is the same as prefixing the refname with
            // a colon"), and both take the same `match_explicit()` branch: an
            // unadvertised destination is an error when it is unqualified and a
            // blind delete when it is `refs/`-qualified.
            explicit_delete: src.is_empty()
                && !dst_spec.is_some_and(|d| d.starts_with("refs/")),
        },
        upstream,
    ))
}

/// The destination for a refspec written without one, as `match_explicit()`
/// computes it:
///
/// ```c
/// dst_value = refs_resolve_ref_unsafe(..., matched_src->name,
///                                     RESOLVE_REF_READING, NULL, &flag);
/// if (!dst_value ||
///     ((flag & REF_ISSYMREF) && !starts_with(dst_value, "refs/heads/")))
///         die(_("%s cannot be resolved to branch"), matched_src->name);
/// ```
///
/// The name is resolved *through* symrefs, so `git push . HEAD` updates the
/// branch HEAD points at rather than creating a ref literally named
/// `refs/heads/HEAD` — which shadows HEAD itself and leaves the repository with
/// an ambiguous `HEAD`. A symref that lands outside `refs/heads/` (a detached
/// HEAD resolves to no ref at all) is fatal.
fn resolve_bare_dst(repo: &gix::Repository, name: &str) -> Result<String> {
    let Ok(mut reference) = repo.find_reference(name) else {
        // Not a ref at all — a raw object name. git's `resolve_ref_unsafe`
        // returns NULL for it and dies.
        crate::git_fatal!("{name} cannot be resolved to branch");
    };
    let symbolic = matches!(reference.target(), gix::refs::TargetRef::Symbolic(_));
    while let Some(Ok(next)) = reference.follow() {
        reference = next;
    }
    let resolved = reference.name().as_bstr().to_str()?.to_string();
    if symbolic && !resolved.starts_with("refs/heads/") {
        crate::git_fatal!("{name} cannot be resolved to branch");
    }
    Ok(resolved)
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
            src: Some(name.clone()),
            name: name.clone(),
            new,
            force,
            expected: None,
            only_if_absent: false, check_reachable: None, explicit_delete: false,
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
        // `transport_update_tracking_ref` (transport.c:612-616) follows the report:
        // the tracking ref that moves is the one the SERVER named, not the one the
        // client asked for.
        let Some(tracking) = tracking_ref_for(remote, s.report_name.as_deref().unwrap_or(&s.name))
        else {
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
    // git's two independent switches over this block: `color.transport` for the
    // per-ref summary field and `color.push` for the trailing error line. Both are
    // `auto` against stderr and neither consults `color.ui`.
    let colors = super::color::PushColors::resolve(gix::discover(".").ok().as_ref());
    let mut any_failed = outcome.unpack.is_err();
    if let Err(reason) = &outcome.unpack {
        eprintln!("error: unpack failed: {reason}");
    }

    // Refs rejected while matching against the advertisement — `--delete` of a
    // ref the remote does not have — are git's `error()` calls from
    // `match_push_refs`, printed before the transport says anything and never
    // listed in the `To <url>` block.
    for s in &outcome.statuses {
        if let (true, Err(reason)) = (s.pre_transport, &s.result) {
            eprintln!("error: {reason}");
            any_failed = true;
        }
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

    // Nothing but matcher rejections: git never opened a transport report, so the
    // `To <url>` block is not printed at all.
    let wire_rows = outcome.statuses.iter().any(|s| !s.pre_transport);
    if wire_rows {
        eprintln!("To {}", outcome.url);
    }
    // Every rejected ref with its reason, which `advise_rejections` folds into
    // the single advice block git prints under the status list.
    let mut rejected: Vec<(&str, &str)> = Vec::new();
    for s in outcome.statuses.iter().filter(|s| !s.pre_transport) {
        let short = |oid: &ObjectId| oid.to_hex_with_len(7).to_string();
        // `print_ref_status` (transport.c:620): the left side is the LOCAL ref
        // (`ref->peer_ref`) and the right side is `report->ref_name` when the
        // server named one — a `proc-receive` hook turns `refs/for/main` into
        // whatever it really created — else the ref that was asked for. Both are
        // run through `prettify_refname`.
        let dst = s.report_name.as_deref().unwrap_or(&s.name);
        let src_dst = format!(
            "{} -> {}",
            short_ref(s.src.as_deref().unwrap_or(&s.name)),
            short_ref(dst)
        );
        match &s.result {
            // git lists an unchanged ref only under `-v`; the default block shows
            // just what moved.
            Ok(()) if s.up_to_date => {
                if verbose {
                    eprintln!(" = [up to date]      {src_dst}");
                }
            }
            // `print_ok_ref_status()` tests `ref->deletion` first, which is what
            // keeps a delete of a ref the remote never had — old and new both
            // null — from being announced as a new branch.
            Ok(()) if s.new.is_null() => {
                eprintln!(" - [deleted]         {}", short_ref(dst));
            }
            Ok(()) if s.old.is_null() => {
                // git names the created ref by its namespace: a branch, a tag, or
                // — for anything else, notes and remote-tracking refs included,
                // which `--mirror` pushes — the generic "new reference".
                let kind = if dst.starts_with("refs/tags/") {
                    "[new tag]"
                } else if dst.starts_with("refs/heads/") {
                    "[new branch]"
                } else {
                    "[new reference]"
                };
                // `%-*s` at `transport_summary_width()`, which is
                // `2 * FALLBACK_DEFAULT_ABBREV + 3` = 17, then one space
                // (transport.c:647). `[new reference]` is the one summary long
                // enough for the difference between padding and a fixed run of
                // spaces to show.
                eprintln!(" * {kind:<17} {src_dst}");
            }
            Ok(()) => {
                // `print_ok_ref_status`: a forced update is `+` with `...` between
                // the abbreviations and a trailing `(forced update)`; a plain one
                // is a blank flag with `..` and no trailer. `print_ref_status`
                // prints the summary as ` %c %-*s ` at `TRANSPORT_SUMMARY_WIDTH`
                // (`2 * DEFAULT_ABBREV + 3` = 17), which is why the shorter `..`
                // form ends up one space wider than the `...` one.
                let (flag, sep, msg) = if s.forced {
                    ('+', "...", " (forced update)")
                } else {
                    (' ', "..", "")
                };
                let summary = format!("{}{sep}{}", short(&s.old), short(&s.new));
                eprintln!(" {flag} {summary:<17} {src_dst}{msg}");
            }
            Err(reason) => {
                any_failed = true;
                rejected.push((s.name.as_str(), reason.as_str()));
                // git colors the padded summary field alone (`color.transport` /
                // `color.transport.rejected`), leaving the space before the
                // refspec outside the span.
                // `print_one_push_status()`: a refusal that came back from the
                // server is `[remote rejected]`; one this side decided is
                // `[rejected]`. Both are padded to `TRANSPORT_SUMMARY_WIDTH`.
                let label = if s.remote_rejected {
                    "! [remote rejected]"
                } else {
                    "! [rejected]       "
                };
                let summary = super::color::PushColors::paint(&colors.rejected, label);
                eprintln!(" {summary} {src_dst} ({reason})");
            }
        }
    }
    if nothing_moved {
        eprintln!("Everything up-to-date");
    }

    if any_failed {
        // `color.push` / `color.push.error`. git closes the span *after* the
        // newline, so the reset lands at the start of the following line.
        let line = format!("error: failed to push some refs to '{}'", outcome.url);
        if colors.error.is_empty() {
            eprintln!("{line}");
        } else {
            eprint!("{}{line}\n\x1b[m", colors.error);
        }
        advise_rejections(&rejected);
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

/// The advice tail `do_push` (builtin/push.c) prints after the rejection block.
/// git folds every rejected ref into a reason bitmask and prints **one** hint,
/// picking the first reason in this fixed priority order; each hint is
/// suppressed by its own `advice.*` slot *and* by the umbrella
/// `advice.pushUpdateRejected` (which `advice.pushNonFastForward` also gates).
///
/// The reasons `set_ref_status_for_push()` produces map one-to-one onto the
/// bits, with the non-fast-forward rejection split by whether the rejected ref is
/// the checked-out branch, as `transport_push` splits `REJECT_NON_FF_HEAD` from
/// `REJECT_NON_FF_OTHER`.
fn advise_rejections(rejected: &[(&str, &str)]) {
    use crate::advice::Advice;

    if rejected.is_empty() {
        return;
    }
    let Ok(repo) = gix::discover(".") else { return };
    if !Advice::PushUpdateRejected.enabled_in(&repo) {
        return;
    }
    // `resolve_refdup("HEAD")`: the full name of the branch HEAD points at, so a
    // rejected `refs/heads/main` on a checked-out `main` is the "current branch"
    // case and any other rejected branch is the "pushed branch tip" case.
    let head = repo
        .head_ref()
        .ok()
        .flatten()
        .map(|r| r.name().as_bstr().to_string());
    let non_ff_head = head.as_deref().is_some_and(|h| {
        rejected
            .iter()
            .any(|(n, reason)| *reason == "non-fast-forward" && *n == h)
    });
    let non_ff_other = rejected
        .iter()
        .any(|(n, reason)| *reason == "non-fast-forward" && Some(*n) != head.as_deref());
    let already_exists = rejected.iter().any(|(_, reason)| *reason == "already exists");
    let fetch_first = rejected.iter().any(|(_, reason)| *reason == "fetch first");
    let needs_force = rejected.iter().any(|(_, reason)| *reason == "needs force");
    let needs_update =
        rejected.iter().any(|(_, reason)| *reason == "remote ref updated since checkout");

    if non_ff_head {
        Advice::PushNonFFCurrent.advise_plain_in(
            &repo,
            "Updates were rejected because the tip of your current branch is behind\n\
             its remote counterpart. If you want to integrate the remote changes,\n\
             use 'git pull' before pushing again.\n\
             See the 'Note about fast-forwards' in 'git push --help' for details.",
        );
    } else if non_ff_other {
        Advice::PushNonFFMatching.advise_plain_in(
            &repo,
            "Updates were rejected because a pushed branch tip is behind its remote\n\
             counterpart. If you want to integrate the remote changes, use 'git pull'\n\
             before pushing again.\n\
             See the 'Note about fast-forwards' in 'git push --help' for details.",
        );
    } else if already_exists {
        Advice::PushAlreadyExists.advise_plain_in(
            &repo,
            "Updates were rejected because the tag already exists in the remote.",
        );
    } else if fetch_first {
        Advice::PushFetchFirst.advise_plain_in(
            &repo,
            "Updates were rejected because the remote contains work that you do not\n\
             have locally. This is usually caused by another repository pushing to\n\
             the same ref. If you want to integrate the remote changes, use\n\
             'git pull' before pushing again.\n\
             See the 'Note about fast-forwards' in 'git push --help' for details.",
        );
    } else if needs_force {
        // `message_advice_ref_needs_force` is the one that ends with a newline;
        // `vadvise()` stops at the terminator, so it prints no extra blank line.
        Advice::PushNeedsForce.advise_plain_in(
            &repo,
            "You cannot update a remote ref that points at a non-commit object,\n\
             or update a remote ref to make it point at a non-commit object,\n\
             without using the '--force' option.\n",
        );
    } else if needs_update {
        Advice::PushRefNeedsUpdate.advise_plain_in(
            &repo,
            "Updates were rejected because the tip of the remote-tracking branch has\n\
             been updated since the last checkout. If you want to integrate the\n\
             remote changes, use 'git pull' before pushing again.\n\
             See the 'Note about fast-forwards' in 'git push --help' for details.",
        );
    }
}

/// `--porcelain`: machine-readable output — `<flag>\t<ref>\t<summary>` per ref,
/// framed by `To <url>` and a trailing `Done`, on stdout.
fn report_porcelain(outcome: &push_proto::Outcome) -> Result<ExitCode> {
    let mut any_failed = outcome.unpack.is_err();
    println!("To {}", outcome.url);
    for s in &outcome.statuses {
        let short = |oid: &ObjectId| oid.to_hex_with_len(7).to_string();
        // `fprintf(stdout, "%c\t%s:%s\t", flag, from->name, to_name)` — the local
        // ref and the ref the server says it updated, both unshortened.
        let dst = s.report_name.as_deref().unwrap_or(&s.name);
        let refpair = format!("{}:{}", s.src.as_deref().unwrap_or(&s.name), dst);
        match &s.result {
            Ok(()) if s.up_to_date => println!("=\t{refpair}\t[up to date]"),
            Ok(()) if s.new.is_null() => println!("-\t:{dst}\t[deleted]"),
            Ok(()) if s.old.is_null() => {
                let kind = if dst.starts_with("refs/tags/") {
                    "[new tag]"
                } else {
                    "[new branch]"
                };
                println!("*\t{refpair}\t{kind}");
            }
            Ok(()) => {
                let flag = if s.forced { "+" } else { " " };
                let sep = if s.forced { "..." } else { ".." };
                println!("{flag}\t{refpair}\t{}{sep}{}", short(&s.old), short(&s.new));
            }
            Err(reason) => {
                any_failed = true;
                let label = if s.remote_rejected { "[remote rejected]" } else { "[rejected]" };
                println!("!\t{refpair}\t{label} ({reason})");
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
                    .filter_map(|mut r| r.peel_to_id().ok().map(|id| id.detach()))
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
        crate::git_fatal!("invalid refspec '{spec}': a pattern needs exactly one '*' on each side");
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
            src: Some(name.clone()),
            new: id,
            force,
            expected: None,
            only_if_absent: false, check_reachable: None, explicit_delete: false,
        });
    }
    Ok(out)
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
            src: Some("refs/heads/main".into()),
            name: "refs/heads/main".into(),
            new: tip,
            force: false,
            expected: None,
            only_if_absent: false, check_reachable: None, explicit_delete: false,
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
            src: Some("refs/heads/main".into()),
            name: "refs/heads/main".into(),
            new: tip,
            // Even under `--force`, a followed tag must not inherit the force bit.
            force: true,
            expected: None,
            only_if_absent: false, check_reachable: None, explicit_delete: false,
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
            src: Some("refs/heads/gone".into()),
            name: "refs/heads/gone".into(),
            new: gix::hash::ObjectId::null(repo.object_hash()),
            force: false,
            expected: None,
            only_if_absent: false, check_reachable: None, explicit_delete: false,
        }];
        append_followed_tags(&repo, &mut requests).expect("append");
        assert_eq!(requests.len(), 1, "no tips to be reachable from, so no tags");
    }
}
