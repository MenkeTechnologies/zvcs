use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::index::entry::{Flags, Mode, Stage, Stat};
use gix::objs::tree::EntryMode;
use gix::ObjectId;

/// `git commit` — record a commit from the staged index.
///
/// Supported invocation forms (the ones the meta workflow relies on):
///   * `git commit -m <msg>` (repeatable; paragraphs joined by a blank line)
///   * `--message=<msg>` / `-m<msg>` (attached value)
///   * `--allow-empty`, `--allow-empty-message`, `-q`/`--quiet`
///   * `-a`/`--all` (auto-stage tracked modifications and deletions)
///   * bundled short flags, e.g. `-am <msg>` / `-qam <msg>`
///
/// The tree is built from the current index (staging area), the commit is
/// written with `author`/`committer` from configuration, and `HEAD` is advanced
/// exactly like `git`: write-through to the branch it points at, or the detached
/// `HEAD` directly, with a matching reflog entry.
///
/// The summary line and short-stat output match stock `git commit` for the
/// common add/modify/delete/mode-change cases. Rename detection is NOT performed
/// (a rename is reported as a delete plus a create), and binary blobs contribute
/// `0` insertions/deletions to the short-stat, just as `git` does.
///
/// Options that change staging or history semantics (`--amend`, `-F`, `-C`,
/// `--author`, `-p`, `-S`, pathspec-limited commits, editor mode, …) are not
/// backed by this port and fail with a precise message rather than silently
/// doing the wrong thing.
pub fn commit(args: &[String]) -> Result<ExitCode> {
    // --- argument parsing ------------------------------------------------
    let mut messages: Vec<String> = Vec::new();
    let mut allow_empty = false;
    let mut allow_empty_message = false;
    let mut quiet = false;
    let mut all = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-m" | "--message" => {
                i += 1;
                let m = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("option `{a}` requires a value"))?;
                messages.push(m.clone());
            }
            "--allow-empty" => allow_empty = true,
            "--allow-empty-message" => allow_empty_message = true,
            "-q" | "--quiet" => quiet = true,
            "-a" | "--all" => all = true,
            "--" => {
                if i + 1 < args.len() {
                    anyhow::bail!("pathspec-limited commits are not supported");
                }
            }
            "--amend" => anyhow::bail!("`--amend` is not supported"),
            s if s.starts_with("--message=") => messages.push(s["--message=".len()..].to_string()),
            s if s.starts_with("--") => anyhow::bail!("unsupported option `{s}`"),
            // A bundled short-flag cluster, e.g. `-am <msg>`, `-qam <msg>`,
            // `-amMSG`. git's parse-options treats every char as its own option;
            // the first one that takes a value consumes the rest of the cluster,
            // or the next argv element when the cluster ends there.
            s if s.len() > 1 && s.starts_with('-') => {
                let cluster = &s[1..];
                for (at, c) in cluster.char_indices() {
                    match c {
                        'a' => all = true,
                        'q' => quiet = true,
                        'm' => {
                            let rest = &cluster[at + c.len_utf8()..];
                            if rest.is_empty() {
                                i += 1;
                                let m = args.get(i).ok_or_else(|| {
                                    anyhow::anyhow!("option `-m` requires a value")
                                })?;
                                messages.push(m.clone());
                            } else {
                                messages.push(rest.to_string());
                            }
                            break;
                        }
                        _ => anyhow::bail!("unsupported option `-{c}`"),
                    }
                }
            }
            _ => anyhow::bail!("pathspec-limited commits are not supported"),
        }
        i += 1;
    }

    if messages.is_empty() {
        anyhow::bail!("no commit message provided (editor mode is unsupported; use -m)");
    }
    let mut message = messages.join("\n\n");
    if message.trim().is_empty() && !allow_empty_message {
        anyhow::bail!("empty commit message (use --allow-empty-message to override)");
    }
    // Match git's on-disk message, which is newline-terminated.
    if !message.ends_with('\n') {
        message.push('\n');
    }
    let subject = message.lines().next().unwrap_or("").to_string();

    // --- repository + serialized read-modify-write -----------------------
    let repo = gix::discover(".")?;
    // Serialize tree build + commit + HEAD update through the repo coordinator so
    // concurrent zvcs writers queue instead of racing. Held across the whole op.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let hash = repo.object_hash();

    // --- `-a`/`--all`: auto-stage tracked modifications and deletions -----
    // Runs under the same lock, and writes the index through before the tree is
    // built so the on-disk index and the commit agree even if we bail later.
    if all {
        stage_tracked_changes(&repo)?;
    }

    // --- build a tree object from the index ------------------------------
    let index = repo.open_index()?;
    let backing = index.path_backing();

    // Refuse while conflicts are staged, exactly as git does.
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            anyhow::bail!("committing is not possible because you have unmerged files");
        }
    }

    // Feed every index entry into the plumbing tree editor, which builds the
    // nested trees in canonical (git) order and writes them to the odb. The
    // high-level `Repository::edit_tree` wrapper is gated behind the
    // `tree-editor` feature, so the editor is constructed directly over the
    // public object database handle instead.
    let mut editor = gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, hash);
    // Snapshot (path, mode, id) per staged file for the summary/short-stat below.
    let mut new_entries: Vec<(BString, EntryMode, ObjectId)> =
        Vec::with_capacity(index.entries().len());
    for entry in index.entries() {
        let path = entry.path_in(backing);
        let mode = entry
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("index entry `{path}` has an unrepresentable mode"))?;
        editor.upsert(
            path.split(|&b| b == b'/').map(|c| c.as_bstr()),
            mode.kind(),
            entry.id,
        )?;
        new_entries.push((path.to_owned(), mode, entry.id));
    }
    let tree_id = editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?;

    // --- parents ---------------------------------------------------------
    let mut head = repo.head()?;
    let parent = head.try_peel_to_id()?.map(|id| id.detach());
    let is_root = parent.is_none();
    let parents: Vec<ObjectId> = parent.into_iter().collect();

    let parent_tree_id = match parent {
        Some(p) => Some(repo.find_commit(p)?.tree_id()?.detach()),
        None => None,
    };

    // --- nothing-to-commit guard -----------------------------------------
    let unchanged = match parent_tree_id {
        Some(pt) => pt == tree_id,
        None => tree_id == ObjectId::empty_tree(hash),
    };
    if unchanged && !allow_empty {
        anyhow::bail!("nothing to commit (no changes staged)");
    }

    // --- write the commit and advance HEAD -------------------------------
    // `Repository::commit` writes the commit object, then updates `HEAD`
    // (write-through to its branch, or the detached ref) with the canonical
    // `commit`/`commit (initial)` reflog message, requiring the first parent to
    // be the current tip — the same ref-safety check git performs.
    let commit_id = repo.commit("HEAD", &message, tree_id, parents)?;

    if quiet {
        return Ok(ExitCode::SUCCESS);
    }

    // --- summary line ----------------------------------------------------
    let short = commit_id.shorten_or_id();
    let branch_label = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "detached HEAD".to_string(),
    };
    let root_marker = if is_root { " (root-commit)" } else { "" };
    println!("[{branch_label}{root_marker} {short}] {subject}");

    // --- short-stat + create/delete/mode-change summary ------------------
    // Old file set (path -> mode, id) flattened from the parent tree; empty for
    // the root commit.
    let mut old_entries: HashMap<BString, (EntryMode, ObjectId)> = HashMap::new();
    if let Some(pt) = parent_tree_id {
        let old_index = repo.index_from_tree(&pt)?;
        let old_backing = old_index.path_backing();
        for e in old_index.entries() {
            if let Some(m) = e.mode.to_tree_entry_mode() {
                old_entries.insert(e.path_in(old_backing).to_owned(), (m, e.id));
            }
        }
    }
    let new_paths: HashSet<&BString> = new_entries.iter().map(|(p, _, _)| p).collect();

    // File-level change count (git's "N files changed"), including binaries and
    // pure mode changes; renames are counted as a delete plus a create.
    let mut files_changed: u64 = 0;
    let mut summary: Vec<(BString, String)> = Vec::new();
    for (path, mode, id) in &new_entries {
        match old_entries.get(path) {
            None => {
                files_changed += 1;
                summary.push((path.clone(), format!("create mode {} {path}", octal(*mode))));
            }
            Some((old_mode, old_id)) => {
                if old_id != id || old_mode != mode {
                    files_changed += 1;
                }
                if old_mode != mode {
                    summary.push((
                        path.clone(),
                        format!("mode change {} => {} {path}", octal(*old_mode), octal(*mode)),
                    ));
                }
            }
        }
    }
    for (path, (mode, _)) in &old_entries {
        if !new_paths.contains(path) {
            files_changed += 1;
            summary.push((path.clone(), format!("delete mode {} {path}", octal(*mode))));
        }
    }

    // Line counts from a real tree-to-tree blob diff (rename detection off, to
    // keep the file accounting consistent with the count above).
    let new_tree = repo.find_tree(tree_id)?;
    let old_tree = match parent_tree_id {
        Some(pt) => repo.find_tree(pt)?,
        None => repo.empty_tree(),
    };
    let mut platform = old_tree.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });
    let stats = platform.stats(&new_tree)?;

    // git prints the diff block only when something actually changed.
    if files_changed > 0 {
        let ins = stats.lines_added;
        let del = stats.lines_removed;
        let mut line = format!(" {files_changed} file{} changed", plural(files_changed));
        // git shows the insertion clause unless there are only deletions, and the
        // deletion clause unless there are only insertions.
        if ins > 0 || del == 0 {
            line.push_str(&format!(", {ins} insertion{}(+)", plural(ins)));
        }
        if del > 0 || ins == 0 {
            line.push_str(&format!(", {del} deletion{}(-)", plural(del)));
        }
        println!("{line}");

        summary.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, l) in &summary {
            println!(" {l}");
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Stage every *tracked* path whose worktree state diverges from the index —
/// `git commit -a`, which is `git add -u` over the whole worktree.
///
/// Only stage-0 entries participate: conflicted stages are left for the caller's
/// unmerged-files check to reject, and submodule gitlinks are never re-read from
/// the worktree here. Untracked files are deliberately not added, which is the
/// whole distinction between `-a` and `git add -A`.
///
/// Content filters (`autocrlf`, `clean`/`smudge`) are not applied, matching the
/// same deviation `git add` carries in this port.
fn stage_tracked_changes(repo: &gix::Repository) -> Result<()> {
    if repo.workdir().is_none() {
        anyhow::bail!("this operation must be run in a work tree");
    }
    if !repo.index_path().exists() {
        return Ok(());
    }
    let index = repo.open_index()?;

    /// A tracked path whose worktree content or mode moved.
    struct Staged {
        path: BString,
        id: ObjectId,
        mode: Mode,
        stat: Stat,
    }
    let mut staged: Vec<Staged> = Vec::new();
    let mut deletions: Vec<BString> = Vec::new();

    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted || e.mode == Mode::COMMIT {
                continue;
            }
            let path = e.path_in(backing).to_owned();
            let Some(abs) = repo.workdir_path(&path) else {
                continue;
            };
            // A vanished (or unreadable) tracked path stages as a deletion.
            let Ok(md) = gix::index::fs::Metadata::from_path_no_follow(&abs) else {
                deletions.push(path);
                continue;
            };
            // A tracked file replaced by a directory is not stageable content;
            // leave the index entry untouched rather than guessing.
            if md.is_dir() {
                continue;
            }

            let (bytes, mode) = if md.is_symlink() {
                let target = std::fs::read_link(&abs)?;
                #[cfg(unix)]
                let bytes = {
                    use std::os::unix::ffi::OsStrExt;
                    target.as_os_str().as_bytes().to_vec()
                };
                #[cfg(not(unix))]
                let bytes = target.to_string_lossy().into_owned().into_bytes();
                (bytes, Mode::SYMLINK)
            } else {
                let bytes = std::fs::read(&abs)?;
                let mode = if md.is_executable() {
                    Mode::FILE_EXECUTABLE
                } else {
                    Mode::FILE
                };
                (bytes, mode)
            };

            // Hash first, write only on a real change: an unmodified worktree
            // must not churn the index or touch the object database.
            let id = gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Blob, &bytes)?;
            if id == e.id && mode == e.mode {
                continue;
            }
            let id = repo.write_blob(&bytes)?.detach();
            staged.push(Staged {
                path,
                id,
                mode,
                stat: Stat::from_fs(&md)?,
            });
        }
    }

    if staged.is_empty() && deletions.is_empty() {
        return Ok(());
    }

    // Replace every touched path wholesale, then restore sort order. The
    // tree-cache extension is dropped so the tree build below cannot pick up a
    // stale subtree for a path that just moved.
    let mut index = repo.open_index()?;
    let remove: HashSet<BString> = staged
        .iter()
        .map(|s| s.path.clone())
        .chain(deletions.iter().cloned())
        .collect();
    index.remove_entries(|_, path, _| remove.contains(&path.to_owned()));
    for s in &staged {
        index.dangerously_push_entry(s.stat, s.id, Flags::empty(), s.mode, s.path.as_ref());
    }
    index.sort_entries();
    index.remove_tree();
    index.write(gix::index::write::Options::default())?;

    Ok(())
}

/// The git-internal octal representation of a tree entry mode, e.g. `100644`.
fn octal(mode: EntryMode) -> String {
    let mut buf = [0u8; 6];
    mode.as_bytes(&mut buf).to_string()
}

/// `""` for a count of 1, `"s"` otherwise — for git's `file`/`files` etc.
fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
