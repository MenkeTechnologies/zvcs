//! `git zbump [<submodule-path>...]` — forward-only submodule gitlink bumps.
//!
//! For each target submodule this advances the parent's recorded gitlink to the
//! submodule worktree's current HEAD, but ONLY when that HEAD is a descendant of
//! the pointer already recorded in the parent (a fast-forward). It never
//! regresses or diverges a pointer. Served natively via the vendored gitoxide
//! crates so tools on PATH see the same staged index.

use anyhow::Result;
use std::process::ExitCode;

use gix::bstr::BStr;

pub fn zbump(args: &[String]) -> Result<ExitCode> {
    // 1. Parent repo.
    let repo = gix::discover(".")?;

    // 2. Target submodules: all of them, or the ones named on the command line.
    let submodules: Vec<_> = match repo.submodules()? {
        Some(iter) => iter.collect(),
        None => anyhow::bail!("no submodules configured in this repository"),
    };

    // Requested paths, normalized (trailing slash stripped). `None` == all.
    let wanted: Option<Vec<String>> = if args.is_empty() {
        None
    } else {
        Some(
            args.iter()
                .map(|a| a.trim_end_matches('/').to_string())
                .collect(),
        )
    };

    // Owned, mutable copy of the parent index; staged once at the end.
    let mut index = repo.open_index()?;
    let mut staged = false;
    let mut had_failure = false;
    let mut seen: Vec<String> = Vec::new();

    for sub in submodules {
        let path = sub.path()?; // repo-relative, slash-separated (BString)
        let path_str = path.to_string();

        if let Some(w) = &wanted {
            if !w.iter().any(|x| *x == path_str) {
                continue;
            }
        }
        seen.push(path_str.clone());

        // 3a. `old` = gitlink recorded in the parent HEAD tree for this path.
        let old = match sub.head_id()? {
            Some(id) => id,
            None => {
                println!("{path_str}: refused (not recorded in parent HEAD)");
                had_failure = true;
                continue;
            }
        };

        // 3b. `new` = the submodule worktree's current HEAD commit.
        let subrepo = match sub.open()? {
            Some(r) => r,
            None => {
                println!("{path_str}: refused (submodule not initialized)");
                had_failure = true;
                continue;
            }
        };
        let new = subrepo.head_id()?.detach();

        if new == old {
            println!("{path_str}: already up to date");
            continue;
        }

        // 3c. Ancestry gate: fast-forward only. The merge-base is computed in
        // the submodule's object database, which holds both commits. `old` must
        // be the merge-base (i.e. an ancestor of `new`) for the bump to proceed.
        let base = match subrepo.merge_base(old, new) {
            Ok(id) => id.detach(),
            Err(err) => {
                println!("{path_str}: refused (cannot compute merge-base: {err})");
                had_failure = true;
                continue;
            }
        };
        if base != old {
            println!(
                "{path_str}: refused (not a fast-forward: {} is not an ancestor of {})",
                old.to_hex_with_len(12),
                new.to_hex_with_len(12)
            );
            had_failure = true;
            continue;
        }

        // 3d. Stage the new gitlink into the parent index at `path`.
        let idx = match index.entry_index_by_path(BStr::new(&path)) {
            Ok(idx) => idx,
            Err(_) => {
                println!("{path_str}: refused (no index entry at path)");
                had_failure = true;
                continue;
            }
        };
        let entry = &mut index.entries_mut()[idx];
        if entry.mode != gix::index::entry::Mode::COMMIT {
            println!("{path_str}: refused (index entry is not a gitlink)");
            had_failure = true;
            continue;
        }
        entry.id = new;
        staged = true;
        println!(
            "bumped {path_str}: {}..{}",
            old.to_hex_with_len(12),
            new.to_hex_with_len(12)
        );
    }

    // Report any requested path that matched no submodule.
    if let Some(w) = &wanted {
        for a in w {
            if !seen.contains(a) {
                println!("{a}: refused (no such submodule)");
                had_failure = true;
            }
        }
    }

    // 4. Persist the index once if anything was staged. The tree-cache extension
    // is written as-is by `File::write`, so drop it after mutating entries or a
    // later commit could capture the stale subtree (see gix File::write docs).
    if staged {
        index.remove_tree();
        index.write(gix::index::write::Options::default())?;
    }

    Ok(if had_failure {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
