use anyhow::Result;
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};
use gix::refs::{Category, FullName, Target, TargetRef};

use gix::remote::fetch::refs::update::Mode;
use gix::remote::fetch::{RefLogMessage, Shallow, Status, Tags};

/// `git fetch [<options>] [<remote> [<refspec>...]]` — download objects and
/// update the remote-tracking refs, backed by gitoxide's blocking fetch.
///
/// Supported forms:
///   * `git fetch`                    → fetch the branch's remote, else the default remote
///   * `git fetch <remote>`           → fetch a named remote (or a bare URL)
///   * `git fetch <remote> <refspec>…`→ fetch explicit refspecs (override configured)
///   * `--all`                        → fetch every configured remote
///   * `-m`/`--multiple`              → treat all positionals as remotes and fetch each
///   * `-t`/`--tags`                  → also fetch all tags (`refs/tags/*:refs/tags/*`)
///   * `-n`/`--no-tags`               → disable automatic tag following
///   * `-p`/`--prune`                 → delete tracking refs no longer on the remote
///   * `-P`/`--prune-tags`            → add the tags refspec and (with `-p`) prune stale tags
///   * `-f`/`--force`                 → force updates (treat every refspec as `+`)
///   * `--depth <n>`/`--deepen <n>`/`--unshallow` → shallow-clone history controls
///   * `-v`/`--verbose`, `-q`/`--quiet`, `--dry-run`
///
/// The per-ref summary is written to stderr in `git fetch` layout (`From <url>`
/// header plus one aligned line per changed or pruned ref). Options that require
/// substrate gitoxide's high-level fetch does not expose (`--filter`,
/// `--append`/FETCH_HEAD, `--set-upstream`, `--refmap`) are rejected with a
/// precise message rather than silently ignored.
pub fn fetch(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // --- argument parsing -------------------------------------------------
    let mut opts = FetchOpts::default();
    let mut all = false;
    let mut multiple = false;
    let mut positionals: Vec<&str> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        // Split `--opt=value` for the value-taking long options.
        let (key, inline_val) = match (a.starts_with("--"), a.split_once('=')) {
            (true, Some((k, v))) => (k, Some(v.to_string())),
            _ => (a, None),
        };

        // Fetch the value for a value-taking option (inline `=v` or next arg).
        // Kept as a plain expression (not a closure) so the `i` cursor stays
        // freely borrowable in the other match arms.
        macro_rules! take_value {
            ($name:literal) => {
                match inline_val.clone() {
                    Some(v) => v,
                    None => {
                        let v = args
                            .get(i)
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!(concat!($name, " requires a value")))?;
                        i += 1;
                        v
                    }
                }
            };
        }

        match key {
            "-v" | "--verbose" => opts.verbose = true,
            "-q" | "--quiet" => opts.quiet = true,
            "--dry-run" => opts.dry_run = true,
            "--all" => all = true,
            "-m" | "--multiple" => multiple = true,
            "-t" | "--tags" => opts.tags = Some(Tags::All),
            // git: `-n` is the short form of `--no-tags`, not `--dry-run`.
            "-n" | "--no-tags" => opts.tags = Some(Tags::None),
            "-p" | "--prune" => opts.prune = true,
            "-P" | "--prune-tags" => opts.prune_tags = true,
            "-f" | "--force" => opts.force = true,
            "--unshallow" => opts.shallow = Some(Shallow::undo()),
            "--depth" => {
                let v = take_value!("--depth");
                let n: u32 = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--depth expects a positive integer, got {v:?}"))?;
                let n = NonZeroU32::new(n)
                    .ok_or_else(|| anyhow::anyhow!("--depth expects a positive integer"))?;
                opts.shallow = Some(Shallow::DepthAtRemote(n));
            }
            "--deepen" => {
                let v = take_value!("--deepen");
                let n: u32 = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--deepen expects an integer, got {v:?}"))?;
                opts.shallow = Some(Shallow::Deepen(n));
            }
            // Options requiring substrate the high-level fetch does not expose.
            "--filter" => {
                let _ = take_value!("--filter");
                anyhow::bail!("--filter (partial clone) is not supported");
            }
            "--refmap" => {
                let _ = take_value!("--refmap");
                anyhow::bail!("--refmap is not supported");
            }
            "-a" | "--append" => {
                anyhow::bail!("--append is not supported (FETCH_HEAD is not written)");
            }
            "--set-upstream" => {
                anyhow::bail!("--set-upstream is not supported");
            }
            "--" => {
                positionals.extend(args[i..].iter().map(String::as_str));
                break;
            }
            s if s.starts_with('-') && s.len() > 1 => anyhow::bail!("unsupported option {s:?}"),
            s => positionals.push(s),
        }
    }

    // Serialize ref mutations through the repo coordinator, as the write
    // commands do; a no-op guard if no daemon is running.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // --- dispatch by mode -------------------------------------------------
    let mut failure = false;

    if all {
        if !positionals.is_empty() {
            anyhow::bail!("fetch --all does not take a repository argument");
        }
        for name in repo.remote_names() {
            let n = name.as_bstr();
            match fetch_one(&repo, Some(n), &[], &opts) {
                Ok(true) => failure = true,
                Ok(false) => {}
                Err(e) => {
                    eprintln!("error: could not fetch {n}: {e}");
                    failure = true;
                }
            }
        }
    } else if multiple {
        for name in &positionals {
            match fetch_one(&repo, Some(BStr::new(*name)), &[], &opts) {
                Ok(true) => failure = true,
                Ok(false) => {}
                Err(e) => {
                    eprintln!("error: could not fetch {name}: {e}");
                    failure = true;
                }
            }
        }
    } else {
        let name = positionals.first().map(|s| BStr::new(*s));
        let refspecs: Vec<&str> = positionals.iter().skip(1).copied().collect();
        if fetch_one(&repo, name, &refspecs, &opts)? {
            failure = true;
        }
    }

    if failure {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// Parsed command-line options shared across every remote a single invocation
/// touches (`--all`/`--multiple` fan out but carry the same flags).
#[derive(Default)]
struct FetchOpts {
    dry_run: bool,
    verbose: bool,
    quiet: bool,
    force: bool,
    prune: bool,
    prune_tags: bool,
    tags: Option<Tags>,
    shallow: Option<Shallow>,
}

/// One line of the git-style per-ref summary.
struct Line {
    flag: char,
    summary: String,
    from: String,
    to: String,
    reason: &'static str,
}

/// Prepend `+` (force) to a refspec string unless it is already forced or a
/// negative/exclude spec (`^`).
fn forced(spec: BString) -> BString {
    match spec.first() {
        Some(b'+') | Some(b'^') => spec,
        _ => {
            let mut out = BString::from("+");
            out.extend_from_slice(&spec);
            out
        }
    }
}

/// Run the fetch pipeline for a single remote and print its summary. Returns
/// `true` if any ref update was rejected (drives the non-zero exit code).
fn fetch_one(
    repo: &gix::Repository,
    name_or_url: Option<&BStr>,
    refspecs: &[&str],
    opts: &FetchOpts,
) -> Result<bool> {
    let mut remote = repo.find_fetch_remote(name_or_url)?;

    // Tag handling: `-t` → all tags, `-n` → none. Injected as an implicit
    // `refs/tags/*:refs/tags/*` refspec by the ref-map builder.
    if let Some(tags) = opts.tags {
        remote = remote.with_fetch_tags(tags);
    }

    // Refspec selection: explicit args replace the configured set; `--force`
    // rewrites whichever set is active so every spec is forced.
    if !refspecs.is_empty() {
        let specs: Vec<BString> = refspecs
            .iter()
            .map(|r| {
                let s = BString::from(*r);
                if opts.force {
                    forced(s)
                } else {
                    s
                }
            })
            .collect();
        remote.replace_refspecs(specs, gix::remote::Direction::Fetch)?;
    } else if opts.force {
        let specs: Vec<BString> = remote
            .refspecs(gix::remote::Direction::Fetch)
            .iter()
            .map(|s| forced(s.to_ref().to_bstring()))
            .collect();
        remote.replace_refspecs(specs, gix::remote::Direction::Fetch)?;
    }

    // Destination prefixes to prune (glob refspec destinations only), captured
    // before the remote is consumed by `connect`.
    let mut prune_prefixes: Vec<Vec<u8>> = Vec::new();
    if opts.prune {
        for s in remote.refspecs(gix::remote::Direction::Fetch) {
            if let Some(dst) = s.to_ref().destination() {
                let dst: &[u8] = dst.as_ref();
                if let Some(star) = dst.iter().position(|&b| b == b'*') {
                    prune_prefixes.push(dst[..star].to_vec());
                }
            }
        }
        // `-P` adds the tags refspec, so its destination joins the prune set.
        if opts.prune_tags {
            prune_prefixes.push(b"refs/tags/".to_vec());
        }
        prune_prefixes.sort();
        prune_prefixes.dedup();
    }

    // `-P` fetches all tags via an implicit refspec so pruning has the full
    // remote tag set to diff against, without persisting the spec to config.
    let mut extra_refspecs = Vec::new();
    if opts.prune_tags {
        extra_refspecs.push(
            gix::refspec::parse(
                "refs/tags/*:refs/tags/*".into(),
                gix::refspec::parse::Operation::Fetch,
            )?
            .to_owned(),
        );
    }
    let map_options = gix::remote::ref_map::Options {
        extra_refspecs,
        ..Default::default()
    };

    let url = remote
        .url(gix::remote::Direction::Fetch)
        .map(ToString::to_string)
        .or_else(|| remote.name().map(|n| n.as_bstr().to_string()))
        .unwrap_or_default();

    let should_interrupt = AtomicBool::new(false);
    let outcome = remote
        .connect(gix::remote::Direction::Fetch)?
        .prepare_fetch(gix::progress::Discard, map_options)?
        .with_dry_run(opts.dry_run)
        .with_shallow(opts.shallow.clone().unwrap_or_default())
        .with_reflog_message(RefLogMessage::Prefixed {
            action: "fetch".into(),
        })
        .receive(gix::progress::Discard, &should_interrupt)?;

    // Both status variants carry the ref-update outcome; the ref_map ties each
    // update back to its remote/local mapping.
    let ref_map = &outcome.ref_map;
    let update_refs = match &outcome.status {
        Status::NoPackReceived { update_refs, .. } => update_refs,
        Status::Change { update_refs, .. } => update_refs,
    };

    // --- build the git-style per-ref summary ------------------------------
    let mut update_lines: Vec<Line> = Vec::new();
    let mut rejected = false;

    for (update, mapping, _spec, edit) in update_refs.iter_mapping_updates(
        &ref_map.mappings,
        &ref_map.refspecs,
        &ref_map.extra_refspecs,
    ) {
        // No local tracking ref → nothing to display (e.g. bare FETCH_HEAD-only).
        let local_full = match mapping.local.as_ref() {
            Some(name) => match FullName::try_from(BStr::new(name)) {
                Ok(f) => f,
                Err(_) => continue,
            },
            None => continue,
        };
        let to = local_full.shorten().to_string();
        let is_tag = matches!(local_full.category(), Some(Category::Tag));

        let from = mapping
            .remote
            .as_name()
            .and_then(|n| FullName::try_from(n).ok())
            .map(|f| f.shorten().to_string())
            .or_else(|| mapping.remote.as_id().map(|id| id.to_hex_with_len(7).to_string()))
            .unwrap_or_default();

        // Old/new ids for range summaries, extracted from the applied edit.
        let (old_id, new_id) = match edit.map(|e| &e.change) {
            Some(Change::Update { expected, new, .. }) => {
                let old = match expected {
                    PreviousValue::MustExistAndMatch(Target::Object(id)) => Some(*id),
                    _ => None,
                };
                let new = match new {
                    Target::Object(id) => Some(*id),
                    _ => None,
                };
                (old, new)
            }
            _ => (None, None),
        };
        let range = |sep: &str| match (old_id, new_id) {
            (Some(o), Some(n)) => {
                format!("{}{sep}{}", o.to_hex_with_len(7), n.to_hex_with_len(7))
            }
            _ => String::new(),
        };

        let (flag, summary, reason): (char, String, &'static str) = match &update.mode {
            Mode::New => {
                let s = if is_tag { "[new tag]" } else { "[new branch]" };
                ('*', s.to_string(), "")
            }
            Mode::FastForward => (' ', range(".."), ""),
            Mode::Forced => ('+', range("..."), " (forced update)"),
            Mode::NoChangeNeeded => {
                if !opts.verbose {
                    continue;
                }
                ('=', "[up to date]".to_string(), "")
            }
            Mode::ImplicitTagNotSentByRemote => continue,
            Mode::RejectedNonFastForward => {
                rejected = true;
                ('!', "[rejected]".to_string(), " (non-fast-forward)")
            }
            Mode::RejectedTagUpdate => {
                rejected = true;
                ('!', "[rejected]".to_string(), " (would clobber existing tag)")
            }
            Mode::RejectedCurrentlyCheckedOut { .. } => {
                rejected = true;
                ('!', "[rejected]".to_string(), " (branch is currently checked out)")
            }
            Mode::RejectedToReplaceWithUnborn => {
                rejected = true;
                ('!', "[rejected]".to_string(), " (would replace with unborn)")
            }
            Mode::RejectedSourceObjectNotFound { .. } => {
                rejected = true;
                ('!', "[rejected]".to_string(), " (source object not found)")
            }
        };
        update_lines.push(Line {
            flag,
            summary,
            from,
            to,
            reason,
        });
    }

    // --- prune stale tracking refs ----------------------------------------
    let mut prune_lines: Vec<Line> = Vec::new();
    if !prune_prefixes.is_empty() {
        // Every local ref the remote still advertises is kept; the rest under a
        // pruned prefix are deleted (git's `prune_refs`).
        let kept: HashSet<BString> = ref_map
            .mappings
            .iter()
            .filter_map(|m| m.local.clone())
            .collect();
        let mut pruned: HashSet<BString> = HashSet::new();

        // Collect candidates first, then delete: mutating refs while the ref
        // iterator still borrows the store would be unsound.
        let mut to_delete: Vec<(FullName, String)> = Vec::new();
        for prefix in &prune_prefixes {
            for r in repo.references()?.prefixed(&prefix[..])? {
                let r = r.map_err(anyhow::Error::msg)?;
                // Never prune symbolic tracking refs like `refs/remotes/*/HEAD`.
                if matches!(r.target(), TargetRef::Symbolic(_)) {
                    continue;
                }
                let full = r.name().as_bstr().to_owned();
                if kept.contains(&full) || !pruned.insert(full.clone()) {
                    continue;
                }
                to_delete.push((FullName::try_from(full.as_bstr())?, r.name().shorten().to_string()));
            }
        }

        for (name, short) in to_delete {
            if !opts.dry_run {
                repo.edit_reference(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::Any,
                        log: RefLog::AndReference,
                    },
                    name: name.clone(),
                    deref: false,
                })?;
            }
            prune_lines.push(Line {
                flag: '-',
                summary: "[deleted]".to_string(),
                from: "(none)".to_string(),
                to: short,
                reason: "",
            });
        }
    }

    // --- print the summary (git writes it to stderr) ----------------------
    // Pruned refs are reported first, mirroring git's prune-before-fetch order.
    let mut lines = prune_lines;
    lines.extend(update_lines);

    if !opts.quiet && !lines.is_empty() {
        let sw = lines.iter().map(|l| l.summary.chars().count()).max().unwrap_or(0);
        let fw = lines.iter().map(|l| l.from.chars().count()).max().unwrap_or(0);
        eprintln!("From {url}");
        for l in &lines {
            eprintln!(
                " {} {:<sw$} {:<fw$} -> {}{}",
                l.flag, l.summary, l.from, l.to, l.reason,
            );
        }
    }

    Ok(rejected)
}
