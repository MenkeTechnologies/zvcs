use anyhow::Result;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::refs::transaction::{Change, PreviousValue};
use gix::refs::{Category, FullName, Target};

use gix::remote::fetch::refs::update::Mode;
use gix::remote::fetch::{RefLogMessage, Status};

/// `git fetch [<options>] [<remote>]` — download objects and update the
/// remote-tracking refs, backed by gitoxide's blocking fetch.
///
/// Supported forms (the ones the meta workflow leans on):
///   * `git fetch`            → fetch the branch's remote, else the sole/default remote
///   * `git fetch <remote>`   → fetch a named remote (or a bare URL)
///   * `-n`/`--dry-run`, `-v`/`--verbose`, `-q`/`--quiet`
///
/// The remote's configured refspecs drive which tracking refs are updated; the
/// per-ref summary is written to stderr in `git fetch` layout (`From <url>`
/// header plus one aligned line per changed ref). Unimplemented flags
/// (`--all`, `--prune`, `--tags`, `--depth`, explicit refspec arguments, …) are
/// rejected with a precise message rather than silently ignored.
pub fn fetch(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // --- argument parsing -------------------------------------------------
    let mut dry_run = false;
    let mut verbose = false;
    let mut quiet = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" | "--dry-run" => dry_run = true,
            "-v" | "--verbose" => verbose = true,
            "-q" | "--quiet" => quiet = true,
            "--" => positionals.extend(it.by_ref().map(String::as_str)),
            s if s.starts_with('-') => anyhow::bail!("unsupported option {s:?}"),
            s => positionals.push(s),
        }
    }
    if positionals.len() > 1 {
        anyhow::bail!("explicit refspec arguments are not supported (only `fetch [<remote>]`)");
    }
    let name_or_url = positionals.first().map(|s| gix::bstr::BStr::new(*s));

    // Serialize the ref mutation through the repo coordinator, exactly as the
    // write commands do; a no-op guard if no daemon is running.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // --- resolve the remote and run the blocking fetch --------------------
    let remote = repo.find_fetch_remote(name_or_url)?;
    let url = remote
        .url(gix::remote::Direction::Fetch)
        .map(ToString::to_string)
        .or_else(|| remote.name().map(|n| n.as_bstr().to_string()))
        .unwrap_or_default();

    let should_interrupt = AtomicBool::new(false);
    let outcome = remote
        .connect(gix::remote::Direction::Fetch)?
        .prepare_fetch(gix::progress::Discard, gix::remote::ref_map::Options::default())?
        .with_dry_run(dry_run)
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
    struct Line {
        flag: char,
        summary: String,
        from: String,
        to: String,
        reason: &'static str,
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut rejected = false;

    for (update, mapping, _spec, edit) in update_refs.iter_mapping_updates(
        &ref_map.mappings,
        &ref_map.refspecs,
        &ref_map.extra_refspecs,
    ) {
        // No local tracking ref → nothing to display (e.g. bare FETCH_HEAD-only).
        let local_full = match mapping.local.as_ref() {
            Some(name) => match FullName::try_from(gix::bstr::BStr::new(name)) {
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
                if !verbose {
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
        lines.push(Line {
            flag,
            summary,
            from,
            to,
            reason,
        });
    }

    // --- print the summary (git writes it to stderr) ----------------------
    if !quiet && !lines.is_empty() {
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

    if rejected {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}
