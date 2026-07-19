//! `git tag` — list, create (lightweight) and delete tags.
//!
//! Served natively via the vendored gitoxide crates so tools on PATH observe
//! the same ref store. Implemented forms (matching stock `git tag`):
//!
//!   * `git tag`                       → list every tag, one short name per line,
//!                                       sorted ascending by refname.
//!   * `git tag -l` / `--list`         → same as the bare list form.
//!   * `git tag <name> [<commit>]`     → create a lightweight tag at `<commit>`
//!                                       (default `HEAD`); errors if it exists.
//!   * `git tag -f <name> [<commit>]`  → force, overwriting an existing tag and
//!                                       printing `Updated tag '<name>' (was …)`.
//!   * `git tag -d <name>...`          → delete each tag, printing
//!                                       `Deleted tag '<name>' (was …)`.
//!
//! Annotated/signed tags (`-a`, `-s`, `-m`, `-F`) and pattern-filtered listing
//! are not backed here and bail with a precise message rather than faking it.

use anyhow::{anyhow, bail, Result};
use std::process::ExitCode;

use gix::bstr::BStr;
use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};
use gix::refs::FullName;

pub fn tag(args: &[String]) -> Result<ExitCode> {
    // Partition into flags and positional operands.
    let mut delete = false;
    let mut list = false;
    let mut force = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut operands_only = false;

    for a in args {
        if operands_only || !a.starts_with('-') || a == "-" {
            positionals.push(a.as_str());
            continue;
        }
        match a.as_str() {
            "--" => operands_only = true,
            "-d" | "--delete" => delete = true,
            "-l" | "--list" => list = true,
            "-f" | "--force" => force = true,
            // Object of an annotated/signed tag or an explicit message: gix's
            // high-level tag API only writes lightweight refs, so refuse rather
            // than silently downgrade to a lightweight tag.
            "-a" | "--annotate" => bail!("annotated tags (-a) are not supported"),
            "-s" | "--sign" => bail!("signed tags (-s) are not supported"),
            "-m" | "--message" | "-F" | "--file" => {
                bail!("tag messages ({a}) are not supported")
            }
            "-n" => bail!("annotation listing (-n) is not supported"),
            other => bail!("unsupported option {other:?}"),
        }
    }

    let repo = gix::discover(".")?;

    if delete {
        return delete_tags(&repo, &positionals);
    }

    // A positional operand while not deleting means "create"; `-l` forces list
    // mode. Bare invocation (no operands) lists.
    if !list && !positionals.is_empty() {
        return create_tag(&repo, &positionals, force);
    }

    // Listing. A pattern operand under `-l` would need fnmatch semantics we
    // don't back yet — refuse precisely instead of returning a wrong subset.
    if !positionals.is_empty() {
        bail!("listing with a match pattern is not supported");
    }
    list_tags(&repo)
}

/// List every tag as its short name, sorted ascending by refname (git's default).
fn list_tags(repo: &gix::Repository) -> Result<ExitCode> {
    let mut names: Vec<String> = Vec::new();
    for r in repo.references()?.tags()? {
        let r = r.map_err(|e| anyhow!("failed to read a tag reference: {e}"))?;
        names.push(r.name().shorten().to_string());
    }
    names.sort();
    for name in names {
        println!("{name}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Create a lightweight tag `<name>` pointing at `[<commit>]` (default `HEAD`).
fn create_tag(repo: &gix::Repository, positionals: &[&str], force: bool) -> Result<ExitCode> {
    let name = positionals[0];
    if positionals.len() > 2 {
        bail!("too many arguments; expected <name> [<commit>]");
    }
    let spec = positionals.get(1).copied().unwrap_or("HEAD");

    // Resolve the target object before taking the write lock.
    let target = repo
        .rev_parse_single(BStr::new(spec))
        .map_err(|e| anyhow!("{e}"))?
        .detach();

    let ref_name = format!("refs/tags/{name}");

    // Serialize the ref read-modify-write through the repo coordinator so
    // concurrent zvcs writers queue instead of racing the ref lock.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Detect an existing tag to reproduce git's messaging/exit semantics.
    let existing = repo
        .try_find_reference(ref_name.as_str())?
        .and_then(|r| r.try_id().map(|id| id.detach()));

    if let Some(old) = existing {
        if !force {
            bail!("tag '{name}' already exists");
        }
        // Force overwrite: git prints `Updated tag '<name>' (was <short>)`.
        repo.tag_reference(name, target, PreviousValue::Any)?;
        println!("Updated tag '{name}' (was {})", short_hex(repo, old));
    } else {
        repo.tag_reference(name, target, PreviousValue::MustNotExist)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Delete each named tag, printing `Deleted tag '<name>' (was <short>)`.
///
/// Mirrors git: a missing tag is reported on stderr and does not abort the
/// remaining deletions; the command exits non-zero if any tag was missing.
fn delete_tags(repo: &gix::Repository, positionals: &[&str]) -> Result<ExitCode> {
    if positionals.is_empty() {
        bail!("option 'd' requires a value");
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut had_failure = false;
    for name in positionals {
        let ref_name = format!("refs/tags/{name}");
        let old = match repo.try_find_reference(ref_name.as_str())? {
            Some(r) => r.try_id().map(|id| id.detach()),
            None => {
                eprintln!("error: tag '{name}' not found.");
                had_failure = true;
                continue;
            }
        };

        let full: FullName = ref_name
            .as_str()
            .try_into()
            .map_err(|e| anyhow!("invalid tag name {name:?}: {e}"))?;
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::MustExist,
                log: RefLog::AndReference,
            },
            name: full,
            deref: false,
        })?;

        match old {
            Some(id) => println!("Deleted tag '{name}' (was {})", short_hex(repo, id)),
            None => println!("Deleted tag '{name}'"),
        }
    }

    Ok(if had_failure {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Abbreviated hex for `id`, honoring the repo's shortening rules (falls back to
/// the full id when the object isn't present to disambiguate against).
fn short_hex(repo: &gix::Repository, id: gix::hash::ObjectId) -> String {
    use gix::prelude::ObjectIdExt;
    id.attach(repo).shorten_or_id().to_string()
}
