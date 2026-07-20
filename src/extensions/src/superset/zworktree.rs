//! `zworktree` — one-command isolated worktree of the whole submodule tree.
//!
//! `git zworktree add <name>` provisions a *complete* private checkout of the
//! current repo AND every nested submodule at `<base>/<name>/` (base =
//! `zvcs.worktreebase`, default `~/.zvcs/worktrees`), so each agent gets a tree
//! that cannot collide with any other. Each repo becomes a **linked git worktree**
//! (separate index + HEAD + working dir, on a fresh `zwt/<name>` branch) that
//! **shares the object store** — no re-clone, and stock git recognizes it
//! (`git worktree list`/`fsck`). Doing this by hand is `git worktree add` for the
//! parent plus one per submodule; here it is one command over the whole tree.
//!
//! The linked-worktree bookkeeping is written directly (gix has no create API):
//! `<gitdir>/worktrees/<name>/{HEAD,commondir,gitdir,index}` and the worktree's
//! `.git` file — exactly git's format.

use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

pub fn zworktree(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        Some("add") => add(&args[1..]),
        Some("list") => list(),
        Some("remove") | Some("rm") => remove(&args[1..]),
        _ => bail!("usage: git zworktree <add <name> [<dest>] | list | remove <name>>"),
    }
}

/// Worktree base dir: `zvcs.worktreebase` else `~/.zvcs/worktrees`.
fn base_dir() -> PathBuf {
    if let Ok(repo) = gix::discover(".") {
        if let Some(b) = repo.config_snapshot().string("zvcs.worktreebase") {
            let s = b.to_string();
            if !s.trim().is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    crate::superset::zdaemon::zvcs_home().join("worktrees")
}

fn add(args: &[String]) -> Result<ExitCode> {
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let name = positional
        .first()
        .ok_or_else(|| anyhow!("usage: git zworktree add <name> [<dest>]"))?;
    if name.contains('/') || name.is_empty() {
        bail!("worktree name must be a simple identifier");
    }
    let dest = match positional.get(1) {
        Some(d) => PathBuf::from(d),
        None => base_dir().join(name),
    };
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }

    let repo = gix::discover(".")?;
    let mut count = 0usize;
    provision(&repo, &dest, name, &mut count)?;

    if let Ok(conn) = crate::db::open_rw() {
        let _ = crate::db::add_worktree(&conn, name, &dest.to_string_lossy());
    }
    println!("worktree '{name}' at {} ({count} repo(s))", dest.display());
    Ok(ExitCode::SUCCESS)
}

/// Provision `repo` as a linked worktree at `wt_path`, then recurse into submodules.
fn provision(repo: &gix::Repository, wt_path: &Path, name: &str, count: &mut usize) -> Result<()> {
    let git_dir = repo
        .git_dir()
        .canonicalize()
        .unwrap_or_else(|_| repo.git_dir().to_path_buf());
    let mut head = repo.head()?;
    let head_id = head
        .try_peel_to_id()?
        .ok_or_else(|| anyhow!("unborn HEAD in {}", git_dir.display()))?
        .detach();

    // 1. Fresh branch `zwt/<name>` at HEAD, in the common gitdir (shared refs).
    let branch_name = format!("refs/heads/zwt/{name}");
    let branch: FullName = branch_name
        .clone()
        .try_into()
        .map_err(|e| anyhow!("invalid branch {branch_name}: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("zworktree {name}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(head_id),
        },
        name: branch,
        deref: false,
    })?;

    // 2. Linked-worktree metadata `<gitdir>/worktrees/<name>/`.
    let meta = git_dir.join("worktrees").join(name);
    std::fs::create_dir_all(&meta)?;
    std::fs::write(meta.join("HEAD"), format!("ref: {branch_name}\n"))?;
    std::fs::write(meta.join("commondir"), "../..\n")?;

    // 3. The worktree's `.git` file <-> metadata gitdir pointer.
    std::fs::create_dir_all(wt_path)?;
    let dotgit = wt_path.join(".git");
    std::fs::write(meta.join("gitdir"), format!("{}\n", dotgit.display()))?;
    std::fs::write(&dotgit, format!("gitdir: {}\n", meta.display()))?;

    // 4. Check out the tree and write the per-worktree index.
    checkout_tree(repo, head_id, wt_path, &meta.join("index"))?;
    *count += 1;

    // 5. Recurse into initialized submodules.
    if let Ok(Some(subs)) = repo.submodules() {
        for sm in subs {
            if let Ok(Some(sub)) = sm.open() {
                let subpath = sm.path()?.to_string();
                provision(&sub, &wt_path.join(&subpath), name, count)?;
            }
        }
    }
    Ok(())
}

/// Check out `commit`'s tree into `wt_path` and persist the index at `index_path`.
fn checkout_tree(
    repo: &gix::Repository,
    commit: gix::hash::ObjectId,
    wt_path: &Path,
    index_path: &Path,
) -> Result<()> {
    let tree_id = repo.find_commit(commit)?.tree_id()?.detach();
    let mut index = repo.index_from_tree(&tree_id)?;
    // Only check out when there are entries: `gix_worktree_state::checkout` panics
    // on an empty entry list with a non-empty path backing (see `worktree.rs`).
    if !index.entries().is_empty() {
        let odb = repo.objects.clone().into_arc()?;
        let should_interrupt = AtomicBool::new(false);
        let mut opts =
            repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
        opts.destination_is_initially_empty = true;
        opts.overwrite_existing = false;
        gix::worktree::state::checkout(
            &mut index,
            wt_path,
            odb,
            &gix::progress::Discard,
            &gix::progress::Discard,
            &should_interrupt,
            opts,
        )?;
    }
    index.remove_tree();
    let mut f = std::fs::File::create(index_path)?;
    index.write_to(&mut f, gix::index::write::Options::default())?;
    Ok(())
}

fn list() -> Result<ExitCode> {
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => return Ok(ExitCode::SUCCESS),
    };
    for (name, path) in crate::db::list_worktrees(&conn)? {
        println!("{name}\t{path}");
    }
    Ok(ExitCode::SUCCESS)
}

fn remove(args: &[String]) -> Result<ExitCode> {
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("usage: git zworktree remove <name>"))?;
    let path = match crate::db::open_ro().ok().and_then(|c| crate::db::worktree_path(&c, name).ok().flatten()) {
        Some(p) => PathBuf::from(p),
        None => base_dir().join(name),
    };
    if !path.exists() {
        bail!("no worktree '{name}' at {}", path.display());
    }

    // Prune each linked-worktree's metadata + its zwt/<name> branch, then delete.
    let mut dotgits = Vec::new();
    find_dotgit_files(&path, &mut dotgits);
    for dotgit in &dotgits {
        if let Ok(content) = std::fs::read_to_string(dotgit) {
            if let Some(rest) = content.trim().strip_prefix("gitdir:") {
                let meta = PathBuf::from(rest.trim());
                // meta = <G>/worktrees/<name>  ->  G = meta/../..
                if let Some(g) = meta.parent().and_then(|p| p.parent()) {
                    let _ = std::fs::remove_file(g.join("refs/heads/zwt").join(name));
                }
                let _ = std::fs::remove_dir_all(&meta);
            }
        }
    }
    std::fs::remove_dir_all(&path).ok();
    if let Ok(conn) = crate::db::open_rw() {
        let _ = crate::db::remove_worktree(&conn, name);
    }
    println!("removed worktree '{name}'");
    Ok(ExitCode::SUCCESS)
}

/// Collect `.git` *files* (linked-worktree pointers) under `dir`, not descending
/// into any `.git`.
fn find_dotgit_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name() == ".git" {
            if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                out.push(p);
            }
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            find_dotgit_files(&p, out);
        }
    }
}
