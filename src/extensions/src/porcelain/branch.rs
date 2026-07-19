use anyhow::{anyhow, bail, Result};
use std::process::ExitCode;

use gix::bstr::{BStr, BString};
use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};
use gix::refs::FullName;

/// `git branch` — list, create, and delete local branches, backed by the
/// vendored gitoxide ref store.
///
/// Supported invocations (the common forms):
///   * `git branch`                       → list local branches, current marked `*`
///   * `git branch <name>`                → create `refs/heads/<name>` at HEAD
///   * `git branch -d|--delete <name>...` → delete, refusing an unmerged branch
///   * `git branch -D <name>...`          → force delete (skips the merge check)
///
/// Deferred: creating a branch at an explicit start-point, listing remote
/// (`-r`) or all (`-a`) refs, verbose (`-v`) columns, rename (`-m`), and
/// upstream configuration. The merge check for `-d` uses reachability from
/// HEAD only (not a configured upstream), which is git's default when no
/// upstream is set.
pub fn branch(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // Classify arguments into flags and positional names.
    let mut force = false;
    let mut delete = false;
    let mut names: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-d" | "--delete" => delete = true,
            "-D" => {
                delete = true;
                force = true;
            }
            "-f" | "--force" => force = true,
            "--list" => {} // explicit list mode; stays list unless a name is also given
            "--" => {
                for rest in &args[i + 1..] {
                    names.push(rest.as_str());
                }
                break;
            }
            _ if a.starts_with('-') => bail!("unsupported flag {a:?}"),
            _ => names.push(a),
        }
        i += 1;
    }

    if delete {
        return delete_branches(&repo, &names, force);
    }
    if names.is_empty() {
        return list_branches(&repo);
    }
    create_branch(&repo, &names)
}

/// List local branches, one per line, sorted by full ref name, marking the
/// currently checked-out branch with `* ` (others `  `). A detached HEAD is
/// reported as `* (HEAD detached at <abbrev>)` before the branch list.
fn list_branches(repo: &gix::Repository) -> Result<ExitCode> {
    let head = repo.head()?;

    if head.is_detached() {
        if let Some(id) = head.id() {
            println!("* (HEAD detached at {})", id.shorten_or_id());
        }
    }

    // Full ref name HEAD points at (symbolic or unborn), for the `*` marker.
    let current: Option<BString> = head.referent_name().map(|n| n.as_bstr().to_owned());

    let mut items: Vec<(BString, String)> = Vec::new();
    for r in repo.references()?.local_branches()? {
        let r = r.map_err(|e| anyhow::anyhow!("{e}"))?;
        let full = r.name().as_bstr().to_owned();
        let short = r.name().shorten().to_string();
        items.push((full, short));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));

    for (full, short) in items {
        let marker = if current.as_ref() == Some(&full) {
            "* "
        } else {
            "  "
        };
        println!("{marker}{short}");
    }

    Ok(ExitCode::SUCCESS)
}

/// Create a single local branch at the current HEAD commit. A second positional
/// (start-point) is not supported.
fn create_branch(repo: &gix::Repository, names: &[&str]) -> Result<ExitCode> {
    if names.len() > 1 {
        bail!("creating a branch at an explicit start-point is not supported");
    }
    let name = names[0];
    let full = format!("refs/heads/{name}");

    // Validate as a local branch name (rejects `..`, spaces, leading `-`,
    // `refs/heads/HEAD`, etc.) before touching the ref store.
    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_err() {
        bail!("'{name}' is not a valid branch name");
    }

    // Resolve the target commit before locking so the error path is cheap.
    let head = repo.head()?;
    if head.is_unborn() {
        bail!("not a valid object name: 'HEAD'");
    }
    let target = head
        .id()
        .ok_or_else(|| anyhow!("HEAD does not point to a commit"))?
        .detach();

    // Serialize the ref read-modify-write through the repo coordinator.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if repo.try_find_reference(full.as_str())?.is_some() {
        bail!("a branch named '{name}' already exists");
    }

    repo.reference(
        full,
        target,
        PreviousValue::MustNotExist,
        "branch: Created from HEAD",
    )?;

    Ok(ExitCode::SUCCESS)
}

/// Delete one or more local branches. Without `force`, a branch that is not
/// reachable from HEAD (not fully merged) is refused. The currently checked-out
/// branch cannot be deleted. Successfully deleted branches are reported as
/// `Deleted branch <name> (was <abbrev>).` Prior successes are committed before
/// bailing on the first failure.
fn delete_branches(repo: &gix::Repository, names: &[&str], force: bool) -> Result<ExitCode> {
    if names.is_empty() {
        bail!("branch name required");
    }

    // Full ref name of the current branch (None if detached/unborn).
    let current: Option<BString> = repo.head_name()?.map(|n| n.as_bstr().to_owned());

    // Serialize all deletions through the repo coordinator, held across the loop.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    for name in names {
        let full = format!("refs/heads/{name}");

        if current.as_ref().map(|c| c.as_slice()) == Some(full.as_bytes()) {
            bail!("cannot delete branch '{name}' checked out at HEAD");
        }

        let mut reference = match repo.try_find_reference(full.as_str())? {
            Some(r) => r,
            None => bail!("branch '{name}' not found"),
        };

        let tip_id = reference.peel_to_id_in_place()?;
        let abbrev = tip_id.shorten_or_id();
        let tip = tip_id.detach();

        if !force {
            let merged = match repo.head_id() {
                Ok(head_id) => match repo.merge_base(tip, head_id.detach()) {
                    Ok(base) => base.detach() == tip,
                    Err(_) => false, // no common ancestor → not merged
                },
                Err(_) => false, // unborn HEAD → nothing merged into
            };
            if !merged {
                bail!("the branch '{name}' is not fully merged");
            }
        }

        let name_full: FullName = full
            .as_str()
            .try_into()
            .map_err(|e| anyhow!("invalid branch name '{name}': {e}"))?;
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
            },
            name: name_full,
            deref: false,
        })?;

        println!("Deleted branch {name} (was {abbrev}).");
    }

    Ok(ExitCode::SUCCESS)
}
