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
//!
//! ## The safety property `remove` has to hold
//!
//! `remove` is the only verb here that deletes, and the two things it deletes —
//! the worktree tree and each repository's `<gitdir>/worktrees/<name>/` — are
//! reached by *reading a path out of a file inside the tree it is deleting*. That
//! file, a linked worktree's `.git` pointer, is plain text in a directory an agent
//! has been handed to work in; whoever can write in the worktree chooses the path.
//!
//! So the rule is: **`remove` deletes a metadata directory only when that
//! directory proves it is the one `provision` wrote for this worktree.** The proof
//! is the round trip [`provision`] creates at step 3 — `<wt>/.git` says
//! `gitdir: <M>`, and `<M>/gitdir` says `<wt>/.git` — plus the shape and kind of
//! `<M>` itself. [`classify_metadata`] is that check, and it is the only way to a
//! `remove_dir_all` on a path this command read out of a file. Every other shape
//! (unreadable, not a `gitdir:` line, relative or absolute but leading somewhere
//! else, a symlink, a directory whose `gitdir` names a different `.git`) is
//! **refused by name on stderr and left on disk** — never deleted, never silently
//! skipped.
//!
//! A `.git` pointer that resolves *inside* the tree being removed is the one
//! exception, and it is not a deletion at all: nested clones and `git submodule
//! update` checkouts an agent made inside its worktree keep their object store
//! there, so that metadata goes away with the tree and needs no separate — or
//! validated — `remove_dir_all`.

use anyhow::{anyhow, bail, Result};
use std::path::{Component, Path, PathBuf};
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

/// A worktree name must be a simple path segment — no separators, no `..`, not
/// empty. Both `add` and `remove` gate on this: `remove` joins the name onto the
/// base dir and `remove_dir_all`s the result, so an unvalidated `../../x` would
/// delete an arbitrary directory outside the worktree base.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." || name == "." {
        bail!("worktree name must be a simple identifier (no path separators or `..`)");
    }
    Ok(())
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
    validate_name(name)?;
    // Absolutize `dest`: git records an absolute path in each linked worktree's
    // `gitdir` bookkeeping, so a cwd-relative `<dest>` would make `git worktree
    // list`/`prune`/repair resolve it wrong from any other directory.
    let dest = match positional.get(1) {
        Some(d) => PathBuf::from(d),
        None => base_dir().join(name),
    };
    let dest = if dest.is_absolute() {
        dest
    } else {
        std::env::current_dir()?.join(dest)
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
    validate_name(name)?;
    let path = match crate::db::open_ro().ok().and_then(|c| crate::db::worktree_path(&c, name).ok().flatten()) {
        Some(p) => PathBuf::from(p),
        None => base_dir().join(name),
    };
    // `symlink_metadata` rather than `exists`: a *symlink* named `<base>/<name>`
    // is not a worktree this command provisioned, and `remove_dir_all` through one
    // would work on whatever it points at.
    match std::fs::symlink_metadata(&path) {
        Ok(md) if md.is_dir() => {}
        Ok(_) => bail!("'{name}' at {} is not a directory", path.display()),
        Err(_) => bail!("no worktree '{name}' at {}", path.display()),
    }

    // Prune each linked-worktree's metadata + its zwt/<name> branch, then delete.
    // Nothing outside this tree is deleted without [`classify_metadata`]'s proof
    // that it is metadata `provision` wrote for this worktree — see the module
    // docs for why the pointer's own word is not enough.
    let mut dotgits = Vec::new();
    find_dotgit_files(&path, &mut dotgits);
    let mut problems = 0usize;

    // The walk below can only report on pointers it *found*, and it deliberately
    // does not follow a `.git` symlink or descend a `.git` directory — so a
    // worktree root whose own pointer is gone or is not a file would otherwise go
    // through this command in silence, leaving `<gitdir>/worktrees/<name>` and the
    // `zwt/<name>` branch behind with nothing said. Name that here instead: there
    // is no pointer to identify the metadata with, so none is deleted.
    let root_dotgit = path.join(".git");
    match std::fs::symlink_metadata(&root_dotgit) {
        Ok(md) if md.file_type().is_symlink() => {
            eprintln!(
                "error: refusing to remove metadata for '{name}': {} is a symlink, not a `gitdir:` pointer",
                root_dotgit.display()
            );
            problems += 1;
        }
        // The pointer file itself — [`classify_metadata`] decides in the loop.
        Ok(md) if md.is_file() => {}
        Ok(_) => {
            eprintln!(
                "error: refusing to remove metadata for '{name}': {} is a directory, so this is not a linked worktree",
                root_dotgit.display()
            );
            problems += 1;
        }
        Err(err) => {
            eprintln!(
                "error: refusing to remove metadata for '{name}': {} is missing ({err}), so its metadata cannot be identified",
                root_dotgit.display()
            );
            problems += 1;
        }
    }

    for dotgit in &dotgits {
        match classify_metadata(dotgit, &path, name) {
            Metadata::Ours(meta) => {
                // meta = <G>/worktrees/<name>  ->  G = meta/../.., and the shape
                // check already established both components.
                if let Some(g) = meta.parent().and_then(|p| p.parent()) {
                    delete_branch(g, name);
                }
                // Not `let _ =`: the path is now known to be this worktree's own
                // metadata, so a failed delete leaves a directory `git worktree
                // list` keeps reporting and `add` would refuse to recreate. It is
                // reported and the run keeps going, so one stuck repository does
                // not strand the rest of the tree.
                if let Err(err) = std::fs::remove_dir_all(&meta) {
                    eprintln!("error: could not remove {}: {err}", meta.display());
                    problems += 1;
                }
            }
            // Deleted with the tree below; see the module docs.
            Metadata::WithinWorktree => {}
            Metadata::Refused(why) => {
                eprintln!("error: refusing to remove {}: {why}", dotgit.display());
                problems += 1;
            }
        }
    }
    // Same reasoning as above: this path is the one recorded for `<name>`, so a
    // failure to delete it is a fact the caller needs, not a `.ok()`.
    if let Err(err) = std::fs::remove_dir_all(&path) {
        eprintln!("error: could not remove {}: {err}", path.display());
        problems += 1;
    }
    // The row is dropped whichever way the deletions went: leaving it would make
    // `list` name a worktree that is gone and `remove` unable to reach the ones it
    // refused, since it resolves `<name>` through this same row.
    if let Ok(conn) = crate::db::open_rw() {
        let _ = crate::db::remove_worktree(&conn, name);
    }
    if problems > 0 {
        return Ok(ExitCode::FAILURE);
    }
    println!("removed worktree '{name}'");
    Ok(ExitCode::SUCCESS)
}

/// What a `.git` pointer found inside the tree being removed turned out to name.
enum Metadata {
    /// Provably the `<gitdir>/worktrees/<name>` directory [`provision`] wrote for
    /// this worktree: safe to `remove_dir_all`.
    Ours(PathBuf),
    /// Resolves inside the tree being removed — a nested clone or submodule
    /// checkout an agent made in its own worktree. Deleted with the tree; nothing
    /// separate to do, and nothing to refuse.
    WithinWorktree,
    /// Anything else, with the reason, for stderr. Nothing is deleted for it.
    Refused(String),
}

/// Decide whether `dotgit`'s `gitdir:` target may be deleted, and say why not when
/// it may not.
///
/// Every check here exists because the target is *text an agent can write*: the
/// only thing that makes a path outside `worktree_root` fair game is that it
/// carries the other half of the round trip [`provision`] writes at step 3.
fn classify_metadata(dotgit: &Path, worktree_root: &Path, name: &str) -> Metadata {
    let content = match std::fs::read_to_string(dotgit) {
        Ok(c) => c,
        Err(err) => return Metadata::Refused(format!("its .git file cannot be read: {err}")),
    };
    // git's `.git` file is exactly one `gitdir: <path>` line. More than one line,
    // or any other first word, is not a pointer this command wrote.
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());
    let target = match (lines.next(), lines.next()) {
        (Some(first), None) => match first.trim().strip_prefix("gitdir:") {
            Some(rest) if !rest.trim().is_empty() => rest.trim().to_owned(),
            _ => return Metadata::Refused("its .git file is not a `gitdir:` pointer".into()),
        },
        _ => return Metadata::Refused("its .git file is not a single `gitdir:` line".into()),
    };
    let target = PathBuf::from(target);

    // Resolve relative pointers the way git does — against the directory holding
    // the `.git` file — so the "does it stay inside the tree" question is asked of
    // the path that would actually be opened.
    let parent = dotgit.parent().unwrap_or(Path::new("."));
    let resolved = lexical_join(parent, &target);
    let root = lexical_join(Path::new("."), worktree_root);
    if resolved.starts_with(&root) {
        return Metadata::WithinWorktree;
    }
    // Left the tree. `provision` always writes an absolute path here (step 3), so
    // a relative one that climbs out is not ours by construction.
    if !target.is_absolute() {
        return Metadata::Refused(format!(
            "its .git file names a relative gitdir `{}` that leaves the worktree",
            target.display()
        ));
    }
    let meta = target;

    // Shape: the last two components must be `worktrees/<name>`. `validate_name`
    // has already established that `<name>` is a single path segment, so this is
    // an equality test, not another traversal question.
    let tail: Vec<_> = meta.components().rev().take(2).collect();
    let shaped = matches!(
        tail.as_slice(),
        [Component::Normal(last), Component::Normal(dir)]
            if last.to_str() == Some(name) && dir.to_str() == Some("worktrees")
    );
    if !shaped {
        return Metadata::Refused(format!(
            "its .git file names {}, which is not a `worktrees/{name}` metadata directory",
            meta.display()
        ));
    }

    // Kind: a real directory, not a symlink standing in for one. `remove_dir_all`
    // would refuse the symlink itself, but the check is here so the refusal says
    // what is wrong instead of surfacing an `ENOTDIR` after the branch delete.
    match std::fs::symlink_metadata(&meta) {
        Ok(md) if md.is_dir() => {}
        Ok(_) => {
            return Metadata::Refused(format!("{} is not a directory", meta.display()));
        }
        Err(err) => {
            return Metadata::Refused(format!("{} cannot be read: {err}", meta.display()));
        }
    }
    // `commondir` is one of the two files `provision` writes at step 2; without it
    // the directory is not linked-worktree metadata at all.
    if !meta.join("commondir").is_file() {
        return Metadata::Refused(format!(
            "{} has no `commondir`, so it is not linked-worktree metadata",
            meta.display()
        ));
    }
    // The round trip. This is the check that actually confines the deletion: the
    // metadata has to name *this* `.git` file back.
    match std::fs::read_to_string(meta.join("gitdir")) {
        Ok(back) if same_file_path(Path::new(back.trim()), dotgit) => Metadata::Ours(meta),
        Ok(back) => Metadata::Refused(format!(
            "{} points back at {} rather than at itself",
            meta.display(),
            back.trim()
        )),
        Err(err) => Metadata::Refused(format!("{}/gitdir cannot be read: {err}", meta.display())),
    }
}

/// Join `p` onto `base` when relative and resolve `.`/`..` textually, without
/// touching the filesystem — the target of a `gitdir:` line may not exist, and a
/// `canonicalize` that followed symlinks would answer a different question than
/// "where does this path lead".
fn lexical_join(base: &Path, p: &Path) -> PathBuf {
    let joined = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether two paths name the same `.git` file. `provision` writes the pointer and
/// the back-pointer from the same `PathBuf`, so the textual test normally decides
/// it; `canonicalize` is the fallback for a base directory reached through a
/// symlink (`/tmp` -> `/private/tmp` on macOS being the everyday case).
fn same_file_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Delete the `zwt/<name>` branch from the common git dir at `g`, via a ref
/// transaction so a *packed* ref is removed too (unlinking the loose file leaves
/// a packed ref behind, leaking the branch and its reflog).
fn delete_branch(g: &Path, name: &str) {
    let full = format!("refs/heads/zwt/{name}");
    if let Ok(repo) = gix::open(g) {
        if let Ok(fname) = TryInto::<FullName>::try_into(full.clone()) {
            let _ = repo.edit_reference(RefEdit {
                change: Change::Delete {
                    expected: PreviousValue::Any,
                    log: RefLog::AndReference,
                    message: Default::default(),
                },
                name: fname,
                deref: false,
            });
            return;
        }
    }
    // Fallback: best-effort loose unlink if the repo won't open.
    let _ = std::fs::remove_file(g.join("refs/heads/zwt").join(name));
}

/// Collect `.git` *files* (linked-worktree pointers) under `dir`, not descending
/// into any `.git`.
///
/// `DirEntry::file_type()` does not follow symlinks, which is load-bearing for
/// `remove`: a `.git` *symlink* is neither collected as a pointer nor descended
/// into as a directory, so a link planted in the worktree cannot steer the walk
/// out of the tree or hand [`classify_metadata`] a body from elsewhere.
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
