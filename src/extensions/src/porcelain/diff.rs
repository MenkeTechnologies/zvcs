//! `git diff` — show changes between HEAD/index/worktree as a unified patch.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). Supported invocations:
//!
//! * `git diff`                       — index vs. worktree (unstaged changes)
//! * `git diff --cached [<rev>]`      — `<rev>`-tree (default `HEAD`) vs. the index (staged)
//! * `git diff --staged [<rev>]`      — alias of `--cached`
//! * `git diff <revA> <revB>`         — tree vs. tree (also `<revA>..<revB>`)
//!
//! Output formats: unified patch (default), `--name-only`, `--name-status`.
//! Context lines are controllable with `-U<n>` / `--unified=<n>`.
//!
//! ### Honest limitations (bailed on with a precise message, never faked)
//!
//! * Diffing a single revision against the worktree (`git diff HEAD`, `git diff <rev>`)
//!   is not supported — gitoxide has no tree-vs-worktree status, only tree-vs-tree,
//!   tree-vs-index and index-vs-worktree. Use `--cached <rev>` or two revisions.
//! * Merge-base ranges (`<revA>...<revB>`) are not supported.
//! * Rename/copy detection is disabled (equivalent to `git diff --no-renames`); renamed
//!   files render as a deletion plus an addition. `-M`/`-C`/`--find-renames` bail.
//! * Submodule/gitlink (`160000`) changes are not diffable through the blob pipeline and bail.
//! * Unmerged (conflicted) paths and type changes bail.
//! * Hunk *section headings* (the text after the second `@@`, i.e. the enclosing function)
//!   are not emitted — gitoxide's unified-diff writer does not compute them.
//! * Magic pathspecs (`:(...)`) and glob pathspecs bail; literal path / directory-prefix
//!   filtering is supported.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::pipeline::{Mode, WorktreeRoots};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, InternedInput, ResourceKind, UnifiedDiff};
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;

/// How the change list should be rendered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Patch,
    NameOnly,
    NameStatus,
}

/// The "new" side of a change.
enum NewSide {
    /// The path no longer exists (a deletion).
    Absent,
    /// A concrete object in the database (tree/index diffs).
    Blob(ObjectId, EntryKind),
    /// Content that must be read from the worktree at this path (index-vs-worktree diffs).
    Worktree(EntryKind),
}

/// A single file-level change, normalized across all diff sources.
struct Delta {
    path: BString,
    /// `None` means the path did not exist before (an addition).
    old: Option<(ObjectId, EntryKind)>,
    new: NewSide,
}

impl Delta {
    fn new_kind(&self) -> Option<EntryKind> {
        match self.new {
            NewSide::Absent => None,
            NewSide::Blob(_, k) | NewSide::Worktree(k) => Some(k),
        }
    }
}

pub fn diff(args: &[String]) -> Result<ExitCode> {
    // ---- argument parsing -------------------------------------------------
    let mut cached = false;
    let mut ctx: u32 = 3;
    let mut format = Format::Patch;
    let mut raw_positional: Vec<String> = Vec::new();
    let mut trailing_paths: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    for a in args {
        if after_dashdash {
            trailing_paths.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => after_dashdash = true,
            "--cached" | "--staged" => cached = true,
            "--name-only" => format = Format::NameOnly,
            "--name-status" => format = Format::NameStatus,
            // Accepted no-ops: these describe behavior zvcs already produces.
            "-p" | "-u" | "--patch" | "--no-renames" | "--no-color" | "--color=never" => {}
            s if s.starts_with("-U") => ctx = parse_context(&s[2..])?,
            s if s.starts_with("--unified=") => ctx = parse_context(&s["--unified=".len()..])?,
            s if s.starts_with('-') => bail!("unsupported option {s:?}"),
            s => raw_positional.push(s.to_string()),
        }
    }

    let repo = gix::discover(".")?;

    // ---- classify positionals into revisions and pathspecs ----------------
    // Leading positionals that resolve as revisions (up to two) are revisions;
    // the first that doesn't, plus everything after it, are pathspecs.
    let mut revs: Vec<String> = Vec::new();
    let mut paths: Vec<String> = trailing_paths;
    let mut in_rev_region = true;
    for tok in raw_positional {
        if in_rev_region && tok.contains("...") && looks_like_range(&tok) {
            bail!("merge-base ranges (<a>...<b>) are not supported");
        }
        if in_rev_region && tok.contains("..") && looks_like_range(&tok) {
            let (a, b) = tok.split_once("..").expect("checked contains");
            revs.push(if a.is_empty() { "HEAD".into() } else { a.into() });
            revs.push(if b.is_empty() { "HEAD".into() } else { b.into() });
            continue;
        }
        if in_rev_region && revs.len() < 2 && repo.rev_parse_single(tok.as_str()).is_ok() {
            revs.push(tok);
        } else {
            in_rev_region = false;
            paths.push(tok);
        }
    }

    for p in &paths {
        if p.starts_with(':') || p.bytes().any(|b| matches!(b, b'*' | b'?' | b'[')) {
            bail!("magic/glob pathspecs are not supported, got {p:?}");
        }
    }

    if revs.len() > 2 {
        bail!("at most two revisions may be given, got {}", revs.len());
    }

    // ---- collect the normalized change list -------------------------------
    let hash_kind = repo.object_hash();
    let mut deltas: Vec<Delta> = Vec::new();
    let mut worktree_mode = false;
    let mut cache;

    if cached {
        if revs.len() == 2 {
            bail!("--cached with two revisions is not supported");
        }
        let tree_id = tree_id_for(&repo, revs.first())?;
        let index = repo.index_or_load_from_head()?;
        let mut gitlink: Option<BString> = None;
        repo.tree_index_status(
            &tree_id,
            &index,
            None,
            gix::status::tree_index::TrackRenames::Disabled,
            |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
                collect_index_change(change, &mut deltas, &mut gitlink);
                Ok(gix::diff::index::Action::Continue(()))
            },
        )?;
        if let Some(p) = gitlink {
            bail!("submodule/gitlink change at {p:?} is not supported");
        }
        cache = repo.diff_resource_cache_for_tree_diff()?;
    } else if revs.len() == 2 {
        let old_tree = repo.rev_parse_single(revs[0].as_str())?.object()?.peel_to_tree()?;
        let new_tree = repo.rev_parse_single(revs[1].as_str())?.object()?.peel_to_tree()?;
        let changes =
            repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(gix::diff::Options::default()))?;
        for change in changes {
            collect_tree_change(change, &mut deltas)?;
        }
        cache = repo.diff_resource_cache_for_tree_diff()?;
    } else if revs.len() == 1 {
        bail!(
            "diffing a single revision against the worktree is not supported; use --cached {:?}, two revisions, or no revision",
            revs[0]
        );
    } else {
        // Default: index vs. worktree.
        let workdir = repo
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("this operation must be run in a work tree"))?
            .to_owned();
        let patterns: Vec<BString> = paths.iter().map(|p| BString::from(p.as_str())).collect();
        let iter = repo
            .status(gix::progress::Discard)?
            .index_worktree_options_mut(|o| {
                o.dirwalk_options = None; // exclude untracked files, matching `git diff`
                o.rewrites = None; // no rename detection
            })
            .into_index_worktree_iter(patterns)?;
        for item in iter {
            collect_worktree_item(item?, &mut deltas)?;
        }
        cache = repo.diff_resource_cache(
            Mode::ToGit,
            WorktreeRoots {
                old_root: None,
                new_root: Some(workdir.clone()),
            },
        )?;
        worktree_mode = true;
    }

    // For tree/index sources, apply literal pathspec filtering here (the worktree
    // iterator already filtered via `patterns`).
    if !worktree_mode && !paths.is_empty() {
        deltas.retain(|d| paths.iter().any(|p| path_matches(&d.path, p)));
    }

    deltas.sort_by(|a, b| a.path.cmp(&b.path));

    // ---- render -----------------------------------------------------------
    let workdir = repo.workdir().map(|p| p.to_owned());
    let mut out: Vec<u8> = Vec::new();
    for delta in &deltas {
        match format {
            Format::NameOnly => {
                out.extend_from_slice(&delta.path);
                out.push(b'\n');
            }
            Format::NameStatus => {
                out.push(status_char(delta));
                out.push(b'\t');
                out.extend_from_slice(&delta.path);
                out.push(b'\n');
            }
            Format::Patch => {
                render_patch(
                    &mut out,
                    &mut cache,
                    &repo.objects,
                    delta,
                    ctx,
                    hash_kind,
                    workdir.as_deref(),
                )?;
            }
        }
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// A single revision spec into a tree id, defaulting to `HEAD^{tree}` (or the empty
/// tree if `HEAD` is unborn) when no spec is given.
fn tree_id_for(repo: &gix::Repository, spec: Option<&String>) -> Result<ObjectId> {
    Ok(match spec {
        Some(s) => repo.rev_parse_single(s.as_str())?.object()?.peel_to_tree()?.id,
        None => repo.head_tree_id_or_empty()?.detach(),
    })
}

/// `true` if a token looks like a revision range rather than a filename that merely
/// contains `..` (e.g. `../foo`). Ranges don't contain `/` and don't start with `.`.
fn looks_like_range(tok: &str) -> bool {
    !tok.starts_with('.') && !tok.contains('/')
}

fn parse_context(s: &str) -> Result<u32> {
    s.parse::<u32>().map_err(|_| anyhow::anyhow!("invalid context line count {s:?}"))
}

/// Convert an index-entry mode into an [`EntryKind`], or `None` for tree entries.
fn index_mode_kind(mode: gix::index::entry::Mode) -> Option<EntryKind> {
    mode.to_tree_entry_mode().map(|m| m.kind())
}

/// Record a change from a `HEAD`-tree-vs-index diff, flagging gitlinks for the caller
/// to reject (the blob pipeline cannot diff `160000` entries).
fn collect_index_change(
    change: gix::diff::index::ChangeRef<'_, '_>,
    deltas: &mut Vec<Delta>,
    gitlink: &mut Option<BString>,
) {
    use gix::diff::index::ChangeRef;
    let is_gitlink = |k: Option<EntryKind>| matches!(k, Some(EntryKind::Commit));
    match change {
        ChangeRef::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let k = index_mode_kind(entry_mode);
            if is_gitlink(k) {
                *gitlink = Some(location.into_owned());
                return;
            }
            if let Some(k) = k {
                deltas.push(Delta {
                    path: location.into_owned(),
                    old: None,
                    new: NewSide::Blob(id.into_owned(), k),
                });
            }
        }
        ChangeRef::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let k = index_mode_kind(entry_mode);
            if is_gitlink(k) {
                *gitlink = Some(location.into_owned());
                return;
            }
            if let Some(k) = k {
                deltas.push(Delta {
                    path: location.into_owned(),
                    old: Some((id.into_owned(), k)),
                    new: NewSide::Absent,
                });
            }
        }
        ChangeRef::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => {
            let ok = index_mode_kind(previous_entry_mode);
            let nk = index_mode_kind(entry_mode);
            if is_gitlink(ok) || is_gitlink(nk) {
                *gitlink = Some(location.into_owned());
                return;
            }
            if let (Some(ok), Some(nk)) = (ok, nk) {
                deltas.push(Delta {
                    path: location.into_owned(),
                    old: Some((previous_id.into_owned(), ok)),
                    new: NewSide::Blob(id.into_owned(), nk),
                });
            }
        }
        // Rewrites are disabled, so this never fires; ignore defensively.
        ChangeRef::Rewrite { .. } => {}
    }
}

/// Record a change from a tree-vs-tree diff.
fn collect_tree_change(
    change: gix::object::tree::diff::ChangeDetached,
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    use gix::object::tree::diff::ChangeDetached;
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let k = entry_mode.kind();
            reject_gitlink(k, &location)?;
            if !entry_mode.is_tree() {
                deltas.push(Delta {
                    path: location,
                    old: None,
                    new: NewSide::Blob(id, k),
                });
            }
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let k = entry_mode.kind();
            reject_gitlink(k, &location)?;
            if !entry_mode.is_tree() {
                deltas.push(Delta {
                    path: location,
                    old: Some((id, k)),
                    new: NewSide::Absent,
                });
            }
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            reject_gitlink(entry_mode.kind(), &location)?;
            reject_gitlink(previous_entry_mode.kind(), &location)?;
            if !entry_mode.is_tree() {
                deltas.push(Delta {
                    path: location,
                    old: Some((previous_id, previous_entry_mode.kind())),
                    new: NewSide::Blob(id, entry_mode.kind()),
                });
            }
        }
        // Rewrites are disabled, so this never fires; ignore defensively.
        ChangeDetached::Rewrite { .. } => {}
    }
    Ok(())
}

fn reject_gitlink(k: EntryKind, location: &BString) -> Result<()> {
    if matches!(k, EntryKind::Commit) {
        bail!("submodule/gitlink change at {location:?} is not supported");
    }
    Ok(())
}

/// Record a change from an index-vs-worktree status item.
fn collect_worktree_item(item: gix::status::index_worktree::Item, deltas: &mut Vec<Delta>) -> Result<()> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    match item {
        Item::Modification {
            entry,
            rela_path,
            status,
            ..
        } => {
            let old_kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
            if matches!(old_kind, EntryKind::Commit) {
                // Submodule content change; `git diff` renders this specially. Skip.
                return Ok(());
            }
            match status {
                EntryStatus::Change(Change::Modification {
                    executable_bit_changed,
                    ..
                }) => {
                    let new_kind = if executable_bit_changed {
                        toggle_exec(old_kind)
                    } else {
                        old_kind
                    };
                    deltas.push(Delta {
                        path: rela_path,
                        old: Some((entry.id, old_kind)),
                        new: NewSide::Worktree(new_kind),
                    });
                }
                EntryStatus::Change(Change::Removed) => {
                    deltas.push(Delta {
                        path: rela_path,
                        old: Some((entry.id, old_kind)),
                        new: NewSide::Absent,
                    });
                }
                EntryStatus::Change(Change::Type { .. }) => {
                    bail!("type change at {rela_path:?} is not supported");
                }
                EntryStatus::Conflict { .. } => {
                    bail!("unmerged path {rela_path:?} is not supported");
                }
                // Submodule content modification, intent-to-add, and stat-only refreshes
                // produce no textual diff.
                EntryStatus::Change(Change::SubmoduleModification(_))
                | EntryStatus::IntentToAdd
                | EntryStatus::NeedsUpdate(_) => {}
            }
        }
        // Untracked/ignored entries never appear in `git diff`; the dirwalk is disabled
        // so these shouldn't be emitted, but ignore them if they are.
        Item::DirectoryContents { .. } => {}
        // Rename tracking is disabled.
        Item::Rewrite { .. } => {}
    }
    Ok(())
}

fn toggle_exec(k: EntryKind) -> EntryKind {
    match k {
        EntryKind::Blob => EntryKind::BlobExecutable,
        EntryKind::BlobExecutable => EntryKind::Blob,
        other => other,
    }
}

/// `--name-status` letter for a delta.
fn status_char(d: &Delta) -> u8 {
    match (&d.old, &d.new) {
        (None, _) => b'A',
        (_, NewSide::Absent) => b'D',
        _ => b'M',
    }
}

/// `true` if `path` equals `pat` or lives under the directory `pat`.
fn path_matches(path: &BString, pat: &str) -> bool {
    let pat = pat.trim_end_matches('/').as_bytes();
    let path = path.as_slice();
    path == pat || (path.len() > pat.len() && path.starts_with(pat) && path[pat.len()] == b'/')
}

/// The body of a blob diff: either textual hunks, a binary marker, or nothing (identical
/// content, e.g. a pure mode change).
enum Body {
    Text(Vec<u8>),
    Binary,
    Empty,
}

/// Render one delta as a `git diff` file section into `out`.
fn render_patch(
    out: &mut Vec<u8>,
    cache: &mut gix::diff::blob::Platform,
    objects: &gix::OdbHandle,
    delta: &Delta,
    ctx: u32,
    hash_kind: gix::hash::Kind,
    workdir: Option<&std::path::Path>,
) -> Result<()> {
    let path = delta.path.as_bstr();
    let null = hash_kind.null();

    // Prime the diff resources on both sides.
    let old_kind = delta.old.map(|(_, k)| k).unwrap_or(EntryKind::Blob);
    match delta.old {
        Some((id, k)) => {
            cache.set_resource(id, k, path, ResourceKind::OldOrSource, objects)?;
        }
        None => {
            cache.set_resource(null, old_kind, path, ResourceKind::OldOrSource, objects)?;
        }
    }
    match &delta.new {
        NewSide::Blob(id, k) => {
            cache.set_resource(*id, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        NewSide::Worktree(k) => {
            // With `new_root` set on the cache, a null id reads from the worktree by path.
            cache.set_resource(null, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        NewSide::Absent => {
            let k = delta.old.map(|(_, k)| k).unwrap_or(EntryKind::Blob);
            cache.set_resource(null, k, path, ResourceKind::NewOrDestination, objects)?;
        }
    }

    // Compute the new-side hash and the diff body within the prepare scope.
    let (new_id, body) = {
        let prep = cache.prepare_diff()?;

        let new_id: ObjectId = match &delta.new {
            NewSide::Absent => null,
            NewSide::Blob(id, _) => *id,
            NewSide::Worktree(_) => {
                if !prep.new.id.is_null() {
                    prep.new.id.to_owned()
                } else if let Some(buf) = prep.new.data.as_slice() {
                    gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, buf)?
                } else {
                    // Binary worktree content: hash the raw file (filters not applied).
                    let base = workdir.ok_or_else(|| anyhow::anyhow!("missing work tree"))?;
                    let full = base.join(gix::path::from_bstr(path));
                    let bytes = std::fs::read(&full)?;
                    gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &bytes)?
                }
            }
        };

        let body = match prep.operation {
            Operation::InternalDiff { algorithm } => {
                let input = InternedInput::new(prep.old.intern_source(), prep.new.intern_source());
                let diff = diff_with_slider_heuristics(algorithm, &input);
                let bytes = UnifiedDiff::new(&diff, &input, PatchSink::default(), ContextSize::symmetrical(ctx))
                    .consume()?;
                if bytes.is_empty() {
                    Body::Empty
                } else {
                    Body::Text(bytes)
                }
            }
            Operation::SourceOrDestinationIsBinary => Body::Binary,
            Operation::ExternalCommand { .. } => {
                bail!("external diff drivers are not supported for {path:?}")
            }
        };
        (new_id, body)
    };

    // ---- assemble the file header + body ----------------------------------
    let old_hash = delta
        .old
        .map(|(id, _)| id.to_hex_with_len(7).to_string())
        .unwrap_or_else(|| "0000000".to_string());
    let new_hash = if matches!(delta.new, NewSide::Absent) {
        "0000000".to_string()
    } else {
        new_id.to_hex_with_len(7).to_string()
    };
    let content_differs = old_hash != new_hash;

    let new_kind = delta.new_kind();

    push_str(out, "diff --git a/");
    out.extend_from_slice(&delta.path);
    push_str(out, " b/");
    out.extend_from_slice(&delta.path);
    out.push(b'\n');

    // File-creation / deletion / mode-change lines.
    match (delta.old, new_kind) {
        (None, Some(nk)) => {
            push_str(out, "new file mode ");
            push_str(out, mode_str(nk));
            out.push(b'\n');
        }
        (Some((_, ok)), None) => {
            push_str(out, "deleted file mode ");
            push_str(out, mode_str(ok));
            out.push(b'\n');
        }
        (Some((_, ok)), Some(nk)) if ok != nk => {
            push_str(out, "old mode ");
            push_str(out, mode_str(ok));
            push_str(out, "\nnew mode ");
            push_str(out, mode_str(nk));
            out.push(b'\n');
        }
        _ => {}
    }

    // The `index <old>..<new>[ <mode>]` line only appears when content differs.
    if content_differs {
        push_str(out, "index ");
        push_str(out, &old_hash);
        push_str(out, "..");
        push_str(out, &new_hash);
        // Trailing mode only for an unchanged-mode modification (not add/delete/mode-change).
        if let (Some((_, ok)), Some(nk)) = (delta.old, new_kind) {
            if ok == nk {
                out.push(b' ');
                push_str(out, mode_str(nk));
            }
        }
        out.push(b'\n');
    }

    let old_label = if delta.old.is_some() {
        let mut s = b"a/".to_vec();
        s.extend_from_slice(&delta.path);
        s
    } else {
        b"/dev/null".to_vec()
    };
    let new_label = if matches!(delta.new, NewSide::Absent) {
        b"/dev/null".to_vec()
    } else {
        let mut s = b"b/".to_vec();
        s.extend_from_slice(&delta.path);
        s
    };

    match body {
        Body::Binary => {
            push_str(out, "Binary files ");
            out.extend_from_slice(&old_label);
            push_str(out, " and ");
            out.extend_from_slice(&new_label);
            push_str(out, " differ\n");
        }
        Body::Text(bytes) => {
            push_str(out, "--- ");
            out.extend_from_slice(&old_label);
            out.push(b'\n');
            push_str(out, "+++ ");
            out.extend_from_slice(&new_label);
            out.push(b'\n');
            out.extend_from_slice(&bytes);
        }
        // Empty body (e.g. pure mode change): only the header lines above.
        Body::Empty => {}
    }

    Ok(())
}

fn mode_str(k: EntryKind) -> &'static str {
    std::str::from_utf8(k.as_octal_str()).unwrap_or("100644")
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

/// Format one side of a hunk header (`@@ -<here> +<here> @@`), omitting the length when
/// it is 1 and using the pre-hunk line number when it is 0, exactly like `git diff`.
fn fmt_range(start: u32, len: u32) -> String {
    match len {
        1 => format!("{start}"),
        0 => format!("{},0", start.saturating_sub(1)),
        _ => format!("{start},{len}"),
    }
}

/// A [`ConsumeHunk`] sink that renders unified-diff hunks (with `git`-style
/// `\ No newline at end of file` markers) into a byte buffer.
#[derive(Default)]
struct PatchSink {
    buf: Vec<u8>,
}

impl ConsumeHunk for PatchSink {
    type Out = Vec<u8>;

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        self.buf.extend_from_slice(b"@@ -");
        self.buf.extend_from_slice(fmt_range(header.before_hunk_start, header.before_hunk_len).as_bytes());
        self.buf.extend_from_slice(b" +");
        self.buf.extend_from_slice(fmt_range(header.after_hunk_start, header.after_hunk_len).as_bytes());
        self.buf.extend_from_slice(b" @@\n");
        for (kind, content) in lines {
            self.buf.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            self.buf.extend_from_slice(content);
            // Tokens keep their line terminator; a token without one is the last line
            // of a file that lacks a trailing newline.
            if content.last() != Some(&b'\n') {
                self.buf.push(b'\n');
                self.buf.extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}
