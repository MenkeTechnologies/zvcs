use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::index::entry::{Flags, Mode, Stage, Stat};
use gix::objs::tree::EntryMode;
use gix::prelude::ObjectIdExt;
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
/// With no `-m`, the message is captured from an editor exactly as git does:
/// a template (`commit.template` plus a status header, unless `commit.status` is
/// false) is opened with the `GIT_EDITOR` → `core.editor` → `$VISUAL` →
/// `$EDITOR` editor, then cleaned up per `commit.cleanup` (default: strip
/// comment/blank lines) with the comment prefix taken from `core.commentString`
/// or `core.commentChar`.
///
/// Options that change staging or history semantics (`--amend`, `-F`, `-C`,
/// `--author`, `-p`, `-S`, pathspec-limited commits, …) are not backed by this
/// port and fail with a precise message rather than silently doing the wrong
/// thing.
pub fn commit(args: &[String]) -> Result<ExitCode> {
    // --- argument parsing ------------------------------------------------
    let mut messages: Vec<String> = Vec::new();
    let mut allow_empty = false;
    let mut allow_empty_message = false;
    let mut quiet = false;
    let mut all = false;
    let mut no_verify = false;
    let mut amend = false;
    let mut no_edit = false;
    let mut reset_author = false;

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
            "-n" | "--no-verify" => no_verify = true,
            "--" => {
                if i + 1 < args.len() {
                    anyhow::bail!("pathspec-limited commits are not supported");
                }
            }
            "--amend" => amend = true,
            "--no-edit" => no_edit = true,
            "--reset-author" => reset_author = true,
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
                        'n' => no_verify = true,
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

    // A `-m`/`--message` value is validated now; without one, the message is
    // captured from the editor below, but only once we know there is something
    // to commit (git opens the editor only then).
    let from_flags = !messages.is_empty();
    let mut message = messages.join("\n\n");
    if from_flags {
        if message.trim().is_empty() && !allow_empty_message {
            anyhow::bail!("empty commit message (use --allow-empty-message to override)");
        }
        // Match git's on-disk message, which is newline-terminated.
        if !message.ends_with('\n') {
            message.push('\n');
        }
    }

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
    // `pre-commit` runs before the commit is built; a non-zero exit aborts it
    // (the hook prints its own diagnostics, so we exit quietly). `--no-verify`
    // skips it, as it does `commit-msg`.
    if !no_verify && !crate::hooks::run(&repo, "pre-commit", &[], None)? {
        return Ok(ExitCode::from(1));
    }

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
    // `--amend` replaces HEAD: the new commit takes HEAD's *parents*, and the
    // summary/nothing-to-commit checks compare against HEAD's first parent tree,
    // not HEAD itself.
    let mut head = repo.head()?;
    let head_tip = head.try_peel_to_id()?.map(|id| id.detach());
    let amend_head = if amend {
        let hid = head_tip.ok_or_else(|| anyhow::anyhow!("You have nothing to amend."))?;
        Some(repo.find_commit(hid)?)
    } else {
        None
    };
    let parents: Vec<ObjectId> = match &amend_head {
        Some(hc) => hc.parent_ids().map(|id| id.detach()).collect(),
        None => head_tip.into_iter().collect(),
    };
    let is_root = parents.is_empty();

    let parent_tree_id = match parents.first() {
        Some(p) => Some(repo.find_commit(*p)?.tree_id()?.detach()),
        None => None,
    };

    // --- nothing-to-commit guard -----------------------------------------
    let unchanged = match parent_tree_id {
        Some(pt) => pt == tree_id,
        None => tree_id == ObjectId::empty_tree(hash),
    };
    // `--amend` always produces a new commit (a message- or author-only amend is
    // valid), so it is exempt from the empty-change guard.
    if unchanged && !allow_empty && !amend {
        anyhow::bail!("nothing to commit (no changes staged)");
    }

    // --- message when none was given on the CLI --------------------------
    // `--amend --no-edit` reuses HEAD's message verbatim. Otherwise git opens
    // the editor (seeded with HEAD's message for a plain `--amend`), or, for a
    // normal commit, on the status template.
    if !from_flags {
        if amend && no_edit {
            message = amend_head
                .as_ref()
                .expect("amend implies HEAD")
                .message_raw()?
                .to_string();
            if !message.ends_with('\n') {
                message.push('\n');
            }
        } else {
            let seed = if amend {
                Some(
                    amend_head
                        .as_ref()
                        .expect("amend implies HEAD")
                        .message_raw()?
                        .to_string(),
                )
            } else {
                None
            };
            message = obtain_message_via_editor(&repo, is_root, seed.as_deref())?;
            if message.trim().is_empty() && !allow_empty_message {
                anyhow::bail!("Aborting commit due to empty commit message.");
            }
            if !message.ends_with('\n') {
                message.push('\n');
            }
        }
    }
    // `commit-msg` gets the message file and may rewrite it (e.g. add a trailer);
    // a non-zero exit aborts. Re-read afterward to pick up any edits.
    if !no_verify {
        let msg_path = repo.git_dir().join("COMMIT_EDITMSG");
        std::fs::write(&msg_path, &message)?;
        let arg = msg_path.to_string_lossy().into_owned();
        if !crate::hooks::run(&repo, "commit-msg", &[&arg], None)? {
            return Ok(ExitCode::from(1));
        }
        message = std::fs::read_to_string(&msg_path)?;
    }
    let subject = message.lines().next().unwrap_or("").to_string();

    // --- write the commit and advance HEAD -------------------------------
    let commit_id = if amend {
        // `--amend`: the new commit keeps HEAD's author (unless `--reset-author`)
        // and takes a fresh committer. `Repository::commit`'s ref update requires
        // the ref to equal the new commit's first parent, which is false for an
        // amend (HEAD points at the commit being replaced, not its parent), so
        // write the object with `new_commit_as` and move HEAD ourselves, gating
        // on HEAD's current tip and writing git's `commit (amend):` reflog line.
        let hc = amend_head.as_ref().expect("amend implies HEAD");
        let committer = repo
            .committer()
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("unable to determine committer identity"))?;
        let author = if reset_author {
            repo.author()
                .transpose()?
                .ok_or_else(|| anyhow::anyhow!("unable to determine author identity"))?
        } else {
            hc.author()?
        };
        let new: ObjectId = repo
            .new_commit_as(committer, author, &message, tree_id, parents)?
            .id;
        let prev = head_tip.expect("amend implies HEAD");
        repo.edit_reference(gix::refs::transaction::RefEdit {
            change: gix::refs::transaction::Change::Update {
                log: gix::refs::transaction::LogChange {
                    mode: gix::refs::transaction::RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("commit (amend): {subject}").into(),
                },
                expected: gix::refs::transaction::PreviousValue::MustExistAndMatch(
                    gix::refs::Target::Object(prev),
                ),
                new: gix::refs::Target::Object(new),
            },
            name: "HEAD"
                .try_into()
                .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
            deref: true,
        })?;
        new.attach(&repo)
    } else {
        // `Repository::commit` writes the commit object, then updates `HEAD`
        // (write-through to its branch, or the detached ref) with the canonical
        // `commit`/`commit (initial)` reflog message, requiring the first parent
        // to be the current tip — the same ref-safety check git performs.
        repo.commit("HEAD", &message, tree_id, parents)?
    };

    // `post-commit` is a notification hook: it runs after the commit regardless of
    // `--no-verify`, and its exit status is ignored.
    let _ = crate::hooks::run(&repo, "post-commit", &[], None);

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

    // git prints a ` Date:` line in the summary when the author date is
    // "interesting" — i.e. it differs from the committer date, as `--amend`
    // (preserved author, fresh committer) and an explicit GIT_AUTHOR_DATE do.
    let written = repo.find_commit(commit_id.detach())?;
    let a_time = written.author()?.time()?;
    let c_time = written.committer()?.time()?;
    if a_time.seconds != c_time.seconds || a_time.offset != c_time.offset {
        let dt = a_time
            .format(gix::date::time::format::DEFAULT)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(" Date: {dt}");
    }

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

/// git's editor path for `git commit` without `-m`: build a template from
/// `commit.template` and a commented status header, open it in the configured
/// editor, and return the cleaned-up message per `commit.cleanup`.
fn obtain_message_via_editor(
    repo: &gix::Repository,
    is_root: bool,
    seed: Option<&str>,
) -> Result<String> {
    let snap = repo.config_snapshot();
    let comment = comment_prefix(&snap);

    let mut buf = String::new();

    // `--amend` seeds the buffer with HEAD's existing message so the editor opens
    // pre-filled, exactly as git does.
    if let Some(seed) = seed {
        buf.push_str(seed);
        if !buf.is_empty() && !buf.ends_with('\n') {
            buf.push('\n');
        }
    }

    // `commit.template`, if configured, seeds the buffer (git reads it verbatim).
    if let Some(path) = snap.string("commit.template") {
        let path = expand_tilde(&path.to_string());
        match std::fs::read_to_string(&path) {
            Ok(text) => buf.push_str(&text),
            Err(e) => anyhow::bail!("could not read commit.template '{}': {e}", path.display()),
        }
        if !buf.is_empty() && !buf.ends_with('\n') {
            buf.push('\n');
        }
    }

    // Commented help + a minimal status header, mirroring git's wt-status block.
    // Omitted entirely when `commit.status=false`, exactly as git does.
    if snap.boolean("commit.status") != Some(false) {
        let branch = repo.head_name()?.map(|n| n.shorten().to_string());
        buf.push('\n');
        buf.push_str(&format!(
            "{comment} Please enter the commit message for your changes. Lines starting\n"
        ));
        buf.push_str(&format!(
            "{comment} with '{comment}' will be ignored, and an empty message aborts the commit.\n"
        ));
        buf.push_str(&format!("{comment}\n"));
        match &branch {
            Some(b) => buf.push_str(&format!("{comment} On branch {b}\n")),
            None => buf.push_str(&format!("{comment} HEAD detached\n")),
        }
        if is_root {
            buf.push_str(&format!("{comment}\n{comment} Initial commit\n"));
        }
        buf.push_str(&format!("{comment}\n"));
    }

    // Write the template to COMMIT_EDITMSG, edit in place, read it back.
    let path = repo.git_dir().join("COMMIT_EDITMSG");
    std::fs::write(&path, &buf)?;
    launch_editor(&snap, &path)?;
    let edited = std::fs::read_to_string(&path)?;

    Ok(cleanup_message(&edited, &comment, cleanup_mode(&snap)))
}

/// The comment prefix for message templates: `core.commentString` (a multi-byte
/// prefix, git 2.45+) if set, else `core.commentChar` (a single character),
/// defaulting to `#`. `auto` is treated as the default here.
fn comment_prefix(snap: &gix::config::Snapshot<'_>) -> String {
    if let Some(v) = snap.string("core.commentString") {
        let v = v.to_string();
        if !v.is_empty() && v != "auto" {
            return v;
        }
    }
    match snap.string("core.commentChar") {
        None => "#".to_string(),
        Some(v) => {
            let s = v.to_string();
            if s.is_empty() || s == "auto" {
                "#".to_string()
            } else {
                // core.commentChar is a single character.
                s.chars().next().unwrap_or('#').to_string()
            }
        }
    }
}

/// Resolve the editor command git would use: `GIT_EDITOR` → `core.editor` →
/// `$VISUAL` → `$EDITOR`, else `vi`. On a dumb/non-interactive terminal with no
/// editor configured, git refuses rather than launching a broken editor.
fn resolve_editor(snap: &gix::config::Snapshot<'_>) -> Result<String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(e) = env("GIT_EDITOR") {
        return Ok(e);
    }
    if let Some(e) = snap.string("core.editor") {
        return Ok(e.to_string());
    }
    if let Some(e) = env("VISUAL") {
        return Ok(e);
    }
    if let Some(e) = env("EDITOR") {
        return Ok(e);
    }
    let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(true);
    if dumb || !std::io::stdin().is_terminal() {
        anyhow::bail!("Terminal is dumb, but EDITOR unset. Please supply the message using -m.");
    }
    Ok("vi".to_string())
}

/// Open `path` in the configured editor and wait, git-style: the editor string
/// runs through the shell so `core.editor = "code -w"` and other argument-bearing
/// commands work, and stdio is inherited so the interactive editor owns the tty.
fn launch_editor(snap: &gix::config::Snapshot<'_>, path: &std::path::Path) -> Result<()> {
    let editor = resolve_editor(snap)?;
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor) // $0
        .arg(path) // $1
        .status()
        .map_err(|e| anyhow::anyhow!("cannot run editor '{editor}': {e}"))?;
    if !status.success() {
        anyhow::bail!("there was a problem with the editor '{editor}'");
    }
    Ok(())
}

/// The `commit.cleanup` modes this port implements; `scissors` degrades to
/// `strip` (safe — it only removes more), and unknown values likewise.
enum Cleanup {
    Strip,
    Whitespace,
    Verbatim,
}

/// `commit.cleanup`, defaulting to `strip` (git's default when a message is read
/// from an editor).
fn cleanup_mode(snap: &gix::config::Snapshot<'_>) -> Cleanup {
    match snap.string("commit.cleanup").map(|v| v.to_string()).as_deref() {
        Some("verbatim") => Cleanup::Verbatim,
        Some("whitespace") => Cleanup::Whitespace,
        _ => Cleanup::Strip,
    }
}

/// Apply git's message cleanup: `verbatim` leaves the text untouched; otherwise
/// trailing whitespace is trimmed, runs of blank lines are collapsed, and
/// leading/trailing blank lines are dropped. `strip` additionally removes lines
/// beginning with the comment prefix.
fn cleanup_message(raw: &str, comment: &str, mode: Cleanup) -> String {
    if let Cleanup::Verbatim = mode {
        return raw.to_string();
    }
    let strip_comments = matches!(mode, Cleanup::Strip);

    let mut out: Vec<&str> = Vec::new();
    let mut prev_blank = true; // drop leading blank lines
    for line in raw.lines() {
        if strip_comments && line.starts_with(comment) {
            continue;
        }
        let line = line.trim_end();
        let blank = line.is_empty();
        if blank && prev_blank {
            continue;
        }
        out.push(line);
        prev_blank = blank;
    }
    while out.last() == Some(&"") {
        out.pop();
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Expand a leading `~`/`~/` to `$HOME`, as git does for path-valued config.
fn expand_tilde(tok: &str) -> std::path::PathBuf {
    if tok == "~" {
        if let Some(h) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(h);
        }
    } else if let Some(rest) = tok.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(h).join(rest);
        }
    }
    std::path::PathBuf::from(tok)
}
