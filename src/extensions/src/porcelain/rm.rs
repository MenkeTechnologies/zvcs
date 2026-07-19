//! `git rm [<options>] <pathspec>...` — remove tracked paths from the index and
//! (unless `--cached`) from the working tree.
//!
//! Served natively through the vendored gitoxide crates so tools on PATH observe
//! the same staged index. Supported flags mirror stock `git rm`:
//!
//!   * `--cached`            — remove from the index only, leave the worktree file
//!   * `-f`, `--force`       — skip the up-to-date safety check
//!   * `-r`                  — allow recursive removal of a directory pathspec
//!   * `-n`, `--dry-run`     — report what would be removed, change nothing
//!   * `--ignore-unmatch`    — exit 0 even if a pathspec matched nothing
//!   * `-q`, `--quiet`       — suppress the `rm '<path>'` lines
//!   * `--`                  — end option parsing
//!
//! Faithfully reproduced: literal-file and directory pathspecs, the index-vs-HEAD
//! and worktree-vs-index safety check (raw blob hashing; conservative — a
//! filtered worktree that differs at the byte level is reported as modified, so
//! `-f` is required, never silently discarded), worktree file removal with empty
//! leading-directory pruning, and the `rm '<path>'` output in index order.
//!
//! Deliberately unsupported (each bails with a precise reason rather than faking
//! success): glob/magic pathspecs, unmerged (conflicted) paths without `-f`, and
//! submodule (gitlink) removal, which would require rewriting `.gitmodules`.

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::Mode;

/// A tracked path selected for removal, captured before the index is mutated.
struct Target {
    path: BString,
    id: ObjectId,
    mode: Mode,
    stage: u32,
}

pub fn rm(args: &[String]) -> Result<ExitCode> {
    // 1. Parse flags and collect the pathspecs. Short flags may be clustered
    //    (e.g. `-rf`); `--` terminates option parsing.
    let mut cached = false;
    let mut force = false;
    let mut recursive = false;
    let mut dry_run = false;
    let mut ignore_unmatch = false;
    let mut quiet = false;
    let mut pathspecs: Vec<String> = Vec::new();
    let mut opts_done = false;

    for a in args {
        if opts_done {
            pathspecs.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => opts_done = true,
            "--cached" => cached = true,
            "--force" => force = true,
            "--dry-run" => dry_run = true,
            "--ignore-unmatch" => ignore_unmatch = true,
            "--quiet" => quiet = true,
            "-r" => recursive = true,
            s if s.starts_with("--") => bail!("unknown option {s:?}"),
            s if s.starts_with('-') && s.len() > 1 => {
                for c in s[1..].chars() {
                    match c {
                        'f' => force = true,
                        'r' => recursive = true,
                        'n' => dry_run = true,
                        'q' => quiet = true,
                        _ => bail!("unknown switch `{c}`"),
                    }
                }
            }
            _ => pathspecs.push(a.clone()),
        }
    }

    if pathspecs.is_empty() {
        bail!("no pathspec given (use --cached to keep the file, or -f to force removal)");
    }

    // 2. Open the repository and require a working tree.
    let repo = gix::discover(".")?;
    let workdir = match repo.workdir() {
        Some(w) => w.to_owned(),
        None => bail!("this operation must be run in a work tree"),
    };

    // Serialize the whole read-modify-write of the index through the repo
    // coordinator so concurrent zvcs writers queue FCFS instead of racing
    // `index.lock`. Held for the rest of the function; a no-op with no daemon.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Pathspecs are relative to the current directory; translate them to the
    // repository-root-relative, slash-separated form the index stores.
    let prefix: String = match repo.prefix()? {
        Some(p) if !p.as_os_str().is_empty() => {
            let s = p.to_str().unwrap_or_default().replace('\\', "/");
            format!("{}/", s.trim_end_matches('/'))
        }
        _ => String::new(),
    };

    // Normalize each pathspec to an index-relative directory-or-file selector.
    // Reject glob magic up front rather than silently mismatching.
    let mut specs: Vec<(String, String)> = Vec::new(); // (original, normalized)
    for raw in &pathspecs {
        if raw.contains(|c| matches!(c, '*' | '?' | '[')) {
            bail!("glob/magic pathspec not supported: {raw:?}");
        }
        let trimmed = raw.strip_prefix("./").unwrap_or(raw);
        let norm = if trimmed == "." || trimmed.is_empty() {
            // "." selects everything under the current prefix.
            prefix.trim_end_matches('/').to_string()
        } else {
            format!("{prefix}{trimmed}").trim_end_matches('/').to_string()
        };
        specs.push((raw.clone(), norm));
    }

    // 3. Snapshot the index entries (owned) so matching/safety reads don't hold a
    //    borrow across the later mutation.
    let index = repo.open_index()?;
    let targets_all: Vec<Target> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .map(|e| Target {
                path: e.path_in(backing).to_owned(),
                id: e.id,
                mode: e.mode,
                stage: e.stage_raw(),
            })
            .collect()
    };

    // 4. Match pathspecs against the index. A directory selector (matches only
    //    via a `dir/` prefix, never as an exact path) requires `-r`.
    let mut selected: Vec<&Target> = Vec::new();
    let mut selected_paths: HashSet<BString> = HashSet::new();
    for (orig, norm) in &specs {
        let mut exact = false;
        let mut dir = false;
        let norm_bytes = norm.as_bytes();
        let dir_prefix: BString = if norm.is_empty() {
            BString::from("") // whole repository under an empty prefix
        } else {
            BString::from(format!("{norm}/"))
        };
        for t in &targets_all {
            let is_exact = !norm.is_empty() && t.path.as_slice() == norm_bytes;
            let is_under = norm.is_empty() || t.path.starts_with(dir_prefix.as_slice());
            if is_exact {
                exact = true;
            } else if is_under {
                dir = true;
            } else {
                continue;
            }
            if selected_paths.insert(t.path.clone()) {
                selected.push(t);
            }
        }

        if !exact && !dir {
            if ignore_unmatch {
                continue;
            }
            bail!("pathspec {orig:?} did not match any files");
        }
        if dir && !exact && !recursive {
            bail!("not removing {orig:?} recursively without -r");
        }
    }

    if selected.is_empty() {
        // Only reachable when every unmatched spec was ignored.
        return Ok(ExitCode::SUCCESS);
    }

    // 5. Reject cases gitoxide-backed removal cannot faithfully perform.
    for t in &selected {
        if t.mode == Mode::COMMIT {
            bail!(
                "removing submodule {:?} is not supported (would require rewriting .gitmodules)",
                t.path.to_str_lossy()
            );
        }
        if t.stage != 0 && !force {
            bail!(
                "cannot remove unmerged path {:?} without -f",
                t.path.to_str_lossy()
            );
        }
    }

    // 6. Up-to-date safety check (skipped with -f). Compare, per stage-0 path:
    //      staged = index blob differs from HEAD blob
    //      local  = worktree content differs from index blob (missing == no change)
    //    Full removal refuses on staged OR local; --cached refuses only when the
    //    staged content matches neither HEAD nor the worktree (staged AND local).
    if !force {
        let hash_kind = repo.object_hash();
        let head_tree = repo.head_tree().ok();

        let mut both: Vec<String> = Vec::new();
        let mut staged_only: Vec<String> = Vec::new();
        let mut local_only: Vec<String> = Vec::new();

        for t in &selected {
            if t.stage != 0 {
                continue; // forced path already validated above
            }
            let path_str = t.path.to_str_lossy().into_owned();

            let head_id: Option<ObjectId> = match &head_tree {
                Some(tree) => tree
                    .lookup_entry_by_path(std::path::Path::new(&path_str))?
                    .map(|e| e.id().detach()),
                None => None,
            };
            let staged = head_id.map(|h| h != t.id).unwrap_or(true);

            let local = match worktree_blob(&repo, &t.path, t.mode, hash_kind)? {
                Some(wt_id) => wt_id != t.id,
                None => false, // already gone from the worktree
            };

            match (staged, local) {
                (true, true) => both.push(path_str),
                (true, false) => staged_only.push(path_str),
                (false, true) => local_only.push(path_str),
                (false, false) => {}
            }
        }

        // Assemble the refusal exactly along git's categories.
        let mut blocks: Vec<String> = Vec::new();
        let plural = |v: &[String]| if v.len() == 1 { ("file", "has") } else { ("files", "have") };
        if !both.is_empty() {
            let (f, h) = plural(&both);
            blocks.push(format!(
                "the following {f} {h} staged content different from both the file and the HEAD:\n    {}",
                both.join("\n    ")
            ));
        }
        if !cached && !staged_only.is_empty() {
            let (f, h) = plural(&staged_only);
            blocks.push(format!(
                "the following {f} {h} changes staged in the index:\n    {}",
                staged_only.join("\n    ")
            ));
        }
        if !cached && !local_only.is_empty() {
            let (f, h) = plural(&local_only);
            blocks.push(format!(
                "the following {f} {h} local modifications:\n    {}",
                local_only.join("\n    ")
            ));
        }
        if !blocks.is_empty() {
            let hint = if cached {
                "(use -f to force removal)"
            } else {
                "(use --cached to keep the file, or -f to force removal)"
            };
            bail!("{}\n{hint}", blocks.join("\n"));
        }
    }

    // 7. Print the removals (index order) unless quiet. Done before mutating so
    //    dry-run and real runs report identically.
    if !quiet {
        for t in &selected {
            println!("rm '{}'", t.path.to_str_lossy());
        }
    }

    if dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // 8. Remove the selected worktree files first (unless --cached), pruning any
    //    leading directories left empty, then drop the entries from the index.
    if !cached {
        for t in &selected {
            let Some(abs) = repo.workdir_path(t.path.as_bstr()) else {
                continue;
            };
            match std::fs::remove_file(&abs) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => bail!("failed to remove {}: {e}", t.path.to_str_lossy()),
            }
            // Prune now-empty parent directories up to (never including) workdir.
            let mut cur = abs.parent().map(|p| p.to_owned());
            while let Some(dir) = cur {
                if dir == workdir || std::fs::remove_dir(&dir).is_err() {
                    break;
                }
                cur = dir.parent().map(|p| p.to_owned());
            }
        }
    }

    // 9. Drop every selected path (all stages) from the owned index and persist.
    let mut index = index;
    index.remove_entries(|_, path, _| selected_paths.contains(&path.to_owned()));
    // The cache-tree extension is written as-is, so drop it after mutating
    // entries or a later commit could capture a stale subtree.
    index.remove_tree();
    index.write(gix::index::write::Options::default())?;

    Ok(ExitCode::SUCCESS)
}

/// Hash the working-tree content at `path` into its git blob id, or `None` if the
/// file is absent. Symlinks hash their target string (as git stores them); an
/// unreadable file is treated as changed (conservative — forces `-f`).
fn worktree_blob(
    repo: &gix::Repository,
    path: &BString,
    mode: Mode,
    hash_kind: gix::hash::Kind,
) -> Result<Option<ObjectId>> {
    let Some(abs) = repo.workdir_path(path.as_bstr()) else {
        return Ok(None);
    };
    let meta = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => bail!("failed to stat {}: {e}", path.to_str_lossy()),
    };

    let content: Vec<u8> = if mode == Mode::SYMLINK || meta.is_symlink() {
        use std::os::unix::ffi::OsStrExt;
        std::fs::read_link(&abs)
            .map_err(|e| anyhow::anyhow!("failed to read symlink {}: {e}", path.to_str_lossy()))?
            .as_os_str()
            .as_bytes()
            .to_vec()
    } else {
        std::fs::read(&abs)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.to_str_lossy()))?
    };

    let id = gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &content)?;
    Ok(Some(id))
}
