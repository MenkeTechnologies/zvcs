use anyhow::Result;
use std::collections::BTreeMap;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::hash::ObjectId;

/// `git status` — working-tree status vs the index and `HEAD`.
///
/// Backed entirely by gitoxide's `Repository::status()` platform, which fans a
/// tree↔index diff (the staged changes) and an index↔worktree diff (the
/// unstaged changes plus the directory walk for untracked files) into a single
/// iterator. From those items we reconstruct git's own output.
///
/// Supported invocations (output byte-for-byte matches stock `git status`):
///   * `git status`                — default long format.
///   * `git status -s|--short`     — short format.
///   * `git status --porcelain`    — porcelain v1 (identical to short here as we
///                                    never colorize and resolve paths repo-root
///                                    relative).
///
/// Faithfully unsupported cases `bail!` with a precise reason rather than
/// emitting wrong output: merge/unmerged (conflicted) paths, intent-to-add
/// entries, `--porcelain=v2`, `-z`, the `-b`/`--branch` short header,
/// `--ignored`, non-default `--untracked-files` modes, and pathspec-limited
/// status.
pub fn status(args: &[String]) -> Result<ExitCode> {
    let mut short = false;
    for a in args {
        match a.as_str() {
            "-s" | "--short" => short = true,
            "--porcelain" | "--porcelain=v1" => short = true,
            "--long" => short = false,
            "--porcelain=v2" => anyhow::bail!("porcelain v2 format is not supported"),
            "-z" => anyhow::bail!("NUL-terminated output (-z) is not supported"),
            "-b" | "--branch" => {
                anyhow::bail!("the -b/--branch short-format header is not supported")
            }
            "--ignored" | "--ignored=traditional" | "--ignored=matching" | "--ignored=no" => {
                anyhow::bail!("listing ignored files (--ignored) is not supported")
            }
            "-u" | "-uall" | "-uno" | "-unormal" => {
                anyhow::bail!("non-default --untracked-files mode is not supported")
            }
            _ if a.starts_with("--untracked-files") => {
                anyhow::bail!("non-default --untracked-files mode is not supported")
            }
            _ if a.starts_with('-') => anyhow::bail!("unsupported option {a:?}"),
            _ => anyhow::bail!("pathspec-limited status ({a:?}) is not supported"),
        }
    }

    let repo = gix::discover(".")?;

    // Resolve the head into an owned description so the borrow ends before we
    // re-open references for the tracking computation.
    let head = repo.head()?;
    let unborn = head.is_unborn();
    let head_state = if unborn {
        HeadState::Unborn(referent_short(head.referent_name(), "main"))
    } else if head.is_detached() {
        let short_id = head
            .id()
            .map(|id| id.shorten_or_id().to_string())
            .unwrap_or_default();
        HeadState::Detached(short_id)
    } else {
        HeadState::Branch(referent_short(head.referent_name(), "HEAD"))
    };
    drop(head);

    // Collect the three change classes from the unified status iterator.
    let mut staged: Vec<(StageKind, BString, Option<BString>)> = Vec::new();
    let mut unstaged: Vec<(WorkKind, BString)> = Vec::new();
    let mut untracked: Vec<BString> = Vec::new();

    let patterns: Vec<BString> = Vec::new();
    for item in repo.status(gix::progress::Discard)?.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                match change {
                    ChangeRef::Addition { location, .. } => {
                        staged.push((StageKind::New, location.into_owned(), None));
                    }
                    ChangeRef::Deletion { location, .. } => {
                        staged.push((StageKind::Deleted, location.into_owned(), None));
                    }
                    ChangeRef::Modification {
                        location,
                        previous_entry_mode,
                        entry_mode,
                        ..
                    } => {
                        let kind = if type_class(previous_entry_mode) != type_class(entry_mode) {
                            StageKind::TypeChange
                        } else {
                            StageKind::Modified
                        };
                        staged.push((kind, location.into_owned(), None));
                    }
                    ChangeRef::Rewrite {
                        source_location,
                        location,
                        copy,
                        ..
                    } => {
                        let kind = if copy {
                            StageKind::Copied
                        } else {
                            StageKind::Renamed
                        };
                        staged.push((kind, location.into_owned(), Some(source_location.into_owned())));
                    }
                }
            }
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
                match iw {
                    Item::Modification { rela_path, status, .. } => match status {
                        EntryStatus::Conflict { .. } => {
                            anyhow::bail!("unmerged (conflicted) paths are not supported")
                        }
                        EntryStatus::IntentToAdd => {
                            anyhow::bail!("intent-to-add entries (git add -N) are not supported")
                        }
                        EntryStatus::NeedsUpdate(_) => {}
                        EntryStatus::Change(change) => match change {
                            Change::Removed => unstaged.push((WorkKind::Deleted, rela_path)),
                            Change::Type { .. } => unstaged.push((WorkKind::TypeChange, rela_path)),
                            Change::Modification { .. } | Change::SubmoduleModification(_) => {
                                unstaged.push((WorkKind::Modified, rela_path))
                            }
                        },
                    },
                    Item::DirectoryContents { entry, .. } => {
                        if matches!(entry.status, gix::dir::entry::Status::Untracked) {
                            untracked.push(entry.rela_path);
                        }
                    }
                    // Rename tracking is disabled for the index↔worktree pass in the
                    // default status platform, so this never fires; ignore defensively.
                    Item::Rewrite { .. } => {}
                }
            }
        }
    }

    // git orders each section (and the short listing) by path.
    staged.sort_by(|a, b| a.1.cmp(&b.1));
    unstaged.sort_by(|a, b| a.1.cmp(&b.1));
    untracked.sort();

    if short {
        print!("{}", render_short(staged, unstaged, untracked));
    } else {
        let tracking = if unborn {
            String::new()
        } else {
            tracking_lines(&repo)?
        };
        print!(
            "{}",
            render_long(&head_state, &tracking, unborn, &staged, &unstaged, &untracked)
        );
    }

    Ok(ExitCode::SUCCESS)
}

enum HeadState {
    Branch(String),
    Detached(String),
    Unborn(String),
}

#[derive(Clone, Copy)]
enum StageKind {
    New,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
}

#[derive(Clone, Copy)]
enum WorkKind {
    Modified,
    Deleted,
    TypeChange,
}

/// Shorten a `HEAD` referent name (`refs/heads/main` → `main`), or fall back.
fn referent_short(name: Option<&gix::refs::FullNameRef>, fallback: &str) -> String {
    use gix::bstr::ByteSlice;
    name.map(|n| n.shorten().to_str_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_owned())
}

/// Map an index-entry mode to a coarse type class, ignoring the executable bit
/// (git treats a permission-only change as `modified`, not `typechange`).
/// 0 = regular blob, 1 = symlink, 2 = gitlink/commit, 3 = tree.
fn type_class(mode: gix::index::entry::Mode) -> u8 {
    match mode.to_tree_entry_mode() {
        Some(m) if m.is_link() => 1,
        Some(m) if m.is_commit() => 2,
        Some(m) if m.is_tree() => 3,
        _ => 0,
    }
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.push_str(&format!("\\{b:03o}"));
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// Build the tracking header line(s) for a born, attached branch, matching git's
/// `format_tracking_info` output including advice hints. Returns an empty string
/// when there is no upstream configured.
fn tracking_lines(repo: &gix::Repository) -> Result<String> {
    use gix::bstr::ByteSlice;

    let Some(branch_ref) = repo.head_ref()? else {
        return Ok(String::new());
    };
    let Some(Ok(upstream_name)) = branch_ref.remote_tracking_ref_name(gix::remote::Direction::Fetch)
    else {
        return Ok(String::new());
    };
    let upstream_short = upstream_name.shorten().to_str_lossy().into_owned();
    let upstream_full = upstream_name.as_bstr().to_str_lossy().into_owned();

    let upstream_ref = match repo.try_find_reference(upstream_full.as_str())? {
        Some(r) => r,
        None => {
            return Ok(format!(
                "Your branch is based on '{upstream_short}', but the upstream is gone.\n  (use \"git branch --unset-upstream\" to fixup)\n"
            ));
        }
    };

    let upstream_id = upstream_ref.into_fully_peeled_id()?.detach();
    let local_id = repo.head_id()?.detach();

    let ahead = count_commits(repo, local_id, upstream_id)?;
    let behind = count_commits(repo, upstream_id, local_id)?;

    let line = if ahead == 0 && behind == 0 {
        format!("Your branch is up to date with '{upstream_short}'.\n")
    } else if behind == 0 {
        let noun = if ahead == 1 { "commit" } else { "commits" };
        format!(
            "Your branch is ahead of '{upstream_short}' by {ahead} {noun}.\n  (use \"git push\" to publish your local commits)\n"
        )
    } else if ahead == 0 {
        let noun = if behind == 1 { "commit" } else { "commits" };
        format!(
            "Your branch is behind '{upstream_short}' by {behind} {noun}, and can be fast-forwarded.\n  (use \"git pull\" to update your local branch)\n"
        )
    } else {
        format!(
            "Your branch and '{upstream_short}' have diverged,\nand have {ahead} and {behind} different commits each, respectively.\n  (use \"git pull\" if you want to integrate the remote branch with yours)\n"
        )
    };
    Ok(line)
}

/// Count commits reachable from `tip` but not from `hidden` — i.e. the ahead/
/// behind count, exactly as git derives it from the merge base.
fn count_commits(repo: &gix::Repository, tip: ObjectId, hidden: ObjectId) -> Result<usize> {
    let walk = repo
        .rev_walk(Some(tip))
        .with_hidden(Some(hidden))
        .all()?;
    Ok(walk.take_while(Result::is_ok).count())
}

fn render_long(
    head_state: &HeadState,
    tracking: &str,
    unborn: bool,
    staged: &[(StageKind, BString, Option<BString>)],
    unstaged: &[(WorkKind, BString)],
    untracked: &[BString],
) -> String {
    let mut out = String::new();

    match head_state {
        HeadState::Branch(name) => out.push_str(&format!("On branch {name}\n")),
        HeadState::Detached(short) => out.push_str(&format!("HEAD detached at {short}\n")),
        HeadState::Unborn(name) => out.push_str(&format!("On branch {name}\n\nNo commits yet\n")),
    }

    // git prints a trailing blank line after the header block only when tracking
    // info or the "No commits yet" note was emitted; a plain branch/detached
    // header runs straight into the first section.
    out.push_str(tracking);
    if !tracking.is_empty() || unborn {
        out.push('\n');
    }

    if !staged.is_empty() {
        out.push_str("Changes to be committed:\n");
        if unborn {
            out.push_str("  (use \"git rm --cached <file>...\" to unstage)\n");
        } else {
            out.push_str("  (use \"git restore --staged <file>...\" to unstage)\n");
        }
        for (kind, path, orig) in staged {
            let label = stage_label(*kind);
            match orig {
                Some(o) => out.push_str(&format!(
                    "\t{label:<12}{} -> {}\n",
                    quote_path(o),
                    quote_path(path)
                )),
                None => out.push_str(&format!("\t{label:<12}{}\n", quote_path(path))),
            }
        }
        out.push('\n');
    }

    if !unstaged.is_empty() {
        let any_deleted = unstaged.iter().any(|(k, _)| matches!(k, WorkKind::Deleted));
        let add_hint = if any_deleted { "git add/rm" } else { "git add" };
        out.push_str("Changes not staged for commit:\n");
        out.push_str(&format!(
            "  (use \"{add_hint} <file>...\" to update what will be committed)\n"
        ));
        out.push_str("  (use \"git restore <file>...\" to discard changes in working directory)\n");
        for (kind, path) in unstaged {
            let label = work_label(*kind);
            out.push_str(&format!("\t{label:<12}{}\n", quote_path(path)));
        }
        out.push('\n');
    }

    if !untracked.is_empty() {
        out.push_str("Untracked files:\n");
        out.push_str("  (use \"git add <file>...\" to include in what will be committed)\n");
        for path in untracked {
            out.push_str(&format!("\t{}\n", quote_path(path)));
        }
        out.push('\n');
    }

    // Trailing summary — omitted entirely when there is anything staged
    // (git's "committable" state), matching stock output.
    if staged.is_empty() {
        let summary = if unstaged.is_empty() && untracked.is_empty() {
            if unborn {
                "nothing to commit (create/copy files and use \"git add\" to track)"
            } else {
                "nothing to commit, working tree clean"
            }
        } else if !unstaged.is_empty() {
            "no changes added to commit (use \"git add\" and/or \"git commit -a\")"
        } else {
            "nothing added to commit but untracked files present (use \"git add\" to track)"
        };
        out.push_str(summary);
        out.push('\n');
    }

    out
}

fn render_short(
    staged: Vec<(StageKind, BString, Option<BString>)>,
    unstaged: Vec<(WorkKind, BString)>,
    untracked: Vec<BString>,
) -> String {
    struct Short {
        x: u8,
        y: u8,
        orig: Option<BString>,
    }

    // Merge both change streams per path: X is the staged (index) column, Y the
    // worktree column; a file can carry both (e.g. "MM").
    let mut map: BTreeMap<BString, Short> = BTreeMap::new();
    for (kind, path, orig) in staged {
        let e = map.entry(path).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
        });
        e.x = stage_char(kind);
        if orig.is_some() {
            e.orig = orig;
        }
    }
    for (kind, path) in unstaged {
        let e = map.entry(path).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
        });
        e.y = work_char(kind);
    }
    for path in untracked {
        map.entry(path).or_insert(Short {
            x: b'?',
            y: b'?',
            orig: None,
        });
    }

    let mut out = String::new();
    for (path, e) in &map {
        let (x, y) = (e.x as char, e.y as char);
        match &e.orig {
            Some(o) => {
                out.push_str(&format!("{x}{y} {} -> {}\n", quote_path(o), quote_path(path)))
            }
            None => out.push_str(&format!("{x}{y} {}\n", quote_path(path))),
        }
    }
    out
}

fn stage_label(kind: StageKind) -> &'static str {
    match kind {
        StageKind::New => "new file:",
        StageKind::Modified => "modified:",
        StageKind::Deleted => "deleted:",
        StageKind::Renamed => "renamed:",
        StageKind::Copied => "copied:",
        StageKind::TypeChange => "typechange:",
    }
}

fn work_label(kind: WorkKind) -> &'static str {
    match kind {
        WorkKind::Modified => "modified:",
        WorkKind::Deleted => "deleted:",
        WorkKind::TypeChange => "typechange:",
    }
}

fn stage_char(kind: StageKind) -> u8 {
    match kind {
        StageKind::New => b'A',
        StageKind::Modified => b'M',
        StageKind::Deleted => b'D',
        StageKind::Renamed => b'R',
        StageKind::Copied => b'C',
        StageKind::TypeChange => b'T',
    }
}

fn work_char(kind: WorkKind) -> u8 {
    match kind {
        WorkKind::Modified => b'M',
        WorkKind::Deleted => b'D',
        WorkKind::TypeChange => b'T',
    }
}
