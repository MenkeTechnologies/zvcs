//! `git merge-tree` — perform a merge without touching the index or worktree.
//!
//! Only the modern `--write-tree` mode is served. The merge itself is done by
//! the vendored `gix-merge` tree/commit merge, which performs the same class of
//! work git's `merge-ort` does: three-way content merges, rename detection and
//! recursive merge-base consolidation.
//!
//! Covered, byte-for-byte against stock git:
//!   * clean merges — the merged tree id and nothing else, exit 0
//!   * conflicted merges — tree id, the `<mode> <object> <stage>\t<path>` stage
//!     lines (or `--name-only` paths), and the informational-message block,
//!     exit 1
//!   * `-z`, `--name-only`, `--messages`/`--no-messages`, `--quiet`,
//!     `--allow-unrelated-histories`, `--merge-base=<tree-ish>`,
//!     `--write-tree` (the default mode), `--`
//!
//! Not covered, and refused rather than approximated:
//!   * `--stdin` (multi-merge batch protocol) and the deprecated
//!     `--trivial-merge` mode
//!   * `-X`/`--strategy-option`
//!   * message rendering for conflict classes outside the content family —
//!     rename/rename, rename/delete, modify/delete, directory/file, submodule
//!     and binary conflicts. `gix-merge` reports these as structured
//!     resolutions, not as git's message strings, so rendering them would mean
//!     inventing text. Those merges still work under `--no-messages` and
//!     `--quiet`, where no message text is emitted at all.

use anyhow::{anyhow, bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::tree_with_rewrites::Change;
use gix::hash::ObjectId;
use gix::merge::blob::builtin_driver::text::Labels;
use gix::merge::tree::apply_index_entries::RemovalMode;
use gix::merge::tree::{Conflict, Resolution, TreatAsUnresolved};

/// Verbatim `git merge-tree` usage text, printed to stderr when the two
/// revisions are missing (git exits 129 in that case).
const USAGE: &str = "\
usage: git merge-tree [--write-tree] [<options>] <branch1> <branch2>
   or: git merge-tree [--trivial-merge] <base-tree> <branch1> <branch2>

    --write-tree          do a real merge instead of a trivial merge
    --trivial-merge       do a trivial merge only
    --[no-]messages       also show informational/conflict messages
    --quiet               suppress all output; only exit status wanted
    -z                    separate paths with the NUL character
    --name-only           list filenames without modes/oids/stages
    --allow-unrelated-histories
                          allow merging unrelated histories
    --stdin               perform multiple merges, one per line of input
    --[no-]merge-base <tree-ish>
                          specify a merge-base for the merge
    -X, --[no-]strategy-option <option=value>
                          option for selected merge strategy
";

/// One informational message, in both the human and the `-z` shape.
///
/// `ctype` is git's stable short conflict type (the `-z` field); `text` is the
/// free-form line, which always carries its own trailing newline exactly as git
/// emits it.
struct Message {
    path: BString,
    ctype: &'static str,
    text: String,
}

/// `git merge-tree --write-tree <branch1> <branch2>`.
pub fn merge_tree(args: &[String]) -> Result<ExitCode> {
    let mut nul = false;
    let mut name_only = false;
    let mut quiet = false;
    let mut allow_unrelated = false;
    // `None` = git's default (show messages iff the merge is conflicted).
    let mut show_messages: Option<bool> = None;
    let mut merge_base: Option<String> = None;
    let mut revs: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    let mut i = 1; // args[0] is the subcommand name
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            revs.push(a);
            i += 1;
            continue;
        }
        match a {
            "--" => no_more_opts = true,
            "--write-tree" => {} // the only mode we implement; already the default
            "-z" => nul = true,
            "--name-only" => name_only = true,
            "--quiet" => quiet = true,
            "--messages" => show_messages = Some(true),
            "--no-messages" => show_messages = Some(false),
            "--allow-unrelated-histories" => allow_unrelated = true,
            "--no-merge-base" => merge_base = None,
            "--merge-base" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("option `--merge-base` requires a value"))?;
                merge_base = Some(v.clone());
            }
            "--stdin" => bail!("unsupported flag \"--stdin\" (ported: --write-tree, -z, --name-only, --messages, --no-messages, --quiet, --allow-unrelated-histories, --merge-base)"),
            "--trivial-merge" => bail!("unsupported flag \"--trivial-merge\" (deprecated mode; ported: --write-tree)"),
            _ if a.starts_with("--merge-base=") => {
                merge_base = Some(a["--merge-base=".len()..].to_string());
            }
            _ if a == "-X" || a.starts_with("-X") || a.starts_with("--strategy-option") => {
                bail!("unsupported flag {a:?} (merge strategy options are not ported)")
            }
            _ => bail!("unsupported flag {a:?} (ported: --write-tree, -z, --name-only, --messages, --no-messages, --quiet, --allow-unrelated-histories, --merge-base)"),
        }
        i += 1;
    }

    if revs.len() != 2 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    let (spec1, spec2) = (revs[0], revs[1]);

    let repo = gix::discover(".")?;
    let labels = Labels {
        ancestor: None,
        current: Some(BStr::new(spec1)),
        other: Some(BStr::new(spec2)),
    };
    let tree_options = repo.tree_merge_options()?;

    // Both branches below produce the same tree-merge outcome; only how the
    // ancestor is chosen differs.
    let mut outcome: gix::merge::tree::Outcome<'_> = if let Some(base_spec) = &merge_base {
        // With an explicit base, git accepts plain trees for all three sides.
        let base = peel_tree(&repo, base_spec)?;
        let ours = peel_tree(&repo, spec1)?;
        let theirs = peel_tree(&repo, spec2)?;
        repo.merge_trees(base, ours, theirs, labels, tree_options)?
    } else {
        let Some(ours) = peel_commit(&repo, spec1) else {
            eprintln!("merge-tree: {spec1} - not something we can merge");
            return Ok(ExitCode::FAILURE);
        };
        let Some(theirs) = peel_commit(&repo, spec2) else {
            eprintln!("merge-tree: {spec2} - not something we can merge");
            return Ok(ExitCode::FAILURE);
        };
        if !allow_unrelated && repo.merge_bases_many(ours, &[theirs])?.is_empty() {
            eprintln!("fatal: refusing to merge unrelated histories");
            return Ok(ExitCode::from(128));
        }
        let commit_options = gix::merge::commit::Options::from(tree_options)
            .with_allow_missing_merge_base(allow_unrelated);
        repo.merge_commits(ours, theirs, labels, commit_options)?
            .tree_merge
    };

    let how = TreatAsUnresolved::git();
    let conflicted = outcome.has_unresolved_conflicts(how);

    if quiet {
        // git suppresses all output here and only reports through the status.
        return Ok(exit_code(conflicted));
    }

    // Render everything up front so an unrenderable conflict class fails before
    // a single byte reaches stdout.
    let mut buf: Vec<u8> = Vec::new();
    let sep = if nul { b'\0' } else { b'\n' };

    let tree_id = outcome.tree.write()?.detach();
    buf.extend_from_slice(tree_id.to_string().as_bytes());
    buf.push(sep);

    if conflicted {
        let mut index = repo.index_from_tree(&tree_id)?;
        outcome.index_changed_after_applying_conflicts(&mut index, how, RemovalMode::Prune);
        let mut last_path: Option<BString> = None;
        for entry in index.entries() {
            let stage = entry.stage_raw();
            if stage == 0 {
                continue;
            }
            let path = entry.path(&index);
            if name_only {
                // One line per path, however many stages it has.
                if last_path.as_ref().map(|p| p.as_bstr()) == Some(path) {
                    continue;
                }
                last_path = Some(path.to_owned());
                buf.extend_from_slice(&render_path(path, nul));
            } else {
                let line = format!(
                    "{:06o} {} {stage}\t",
                    entry.mode.bits(),
                    entry.id.to_hex()
                );
                buf.extend_from_slice(line.as_bytes());
                buf.extend_from_slice(&render_path(path, nul));
            }
            buf.push(sep);
        }
    }

    if show_messages.unwrap_or(conflicted) {
        let messages = render_messages(&repo, &outcome.conflicts)?;
        if nul {
            // The `-z` messages section opens with its own NUL separator, then
            // carries one `<count> <path>... <type> <message>` record per entry.
            buf.push(b'\0');
            for m in &messages {
                buf.extend_from_slice(b"1\0");
                buf.extend_from_slice(m.path.as_slice());
                buf.push(b'\0');
                buf.extend_from_slice(m.ctype.as_bytes());
                buf.push(b'\0');
                buf.extend_from_slice(m.text.as_bytes());
                buf.push(b'\0');
            }
        } else {
            buf.push(b'\n');
            for m in &messages {
                buf.extend_from_slice(m.text.as_bytes());
            }
        }
    }

    std::io::stdout().lock().write_all(&buf)?;
    Ok(exit_code(conflicted))
}

/// `1` when the merge had unresolved conflicts, `0` otherwise — git's contract.
fn exit_code(conflicted: bool) -> ExitCode {
    if conflicted {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Resolve `spec` to the tree it names (commits and tags peel through).
fn peel_tree(repo: &gix::Repository, spec: &str) -> Result<ObjectId> {
    Ok(repo.rev_parse_single(spec)?.object()?.peel_to_tree()?.id)
}

/// Resolve `spec` to a commit id, or `None` when it is not something git would
/// accept as a side of the merge.
fn peel_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    Some(object.peel_to_commit().ok()?.id)
}

/// Turn the structured conflict records into git's informational messages.
///
/// Only the content-merge family is rendered: git's text for those is
/// reproduced exactly. Any other resolution class — and any content merge over
/// binary data or symlinks, where git prepends a `warning:` line we cannot
/// reconstruct — errors out instead of guessing.
fn render_messages(repo: &gix::Repository, conflicts: &[Conflict]) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    for conflict in conflicts {
        let (ours, theirs) = conflict.changes_in_resolution();
        let path = ours.location().to_owned();
        let merged_blob = match &conflict.resolution {
            Ok(Resolution::OursModifiedTheirsModifiedThenBlobContentMerge { merged_blob }) => {
                merged_blob
            }
            _ => bail!(
                "conflict at {path} is not a content merge; message rendering for this conflict class is not ported (retry with --no-messages or --quiet)"
            ),
        };

        for change in [ours, theirs] {
            let (mode, id) = change_state(change);
            if !mode.is_blob() {
                bail!(
                    "conflict at {path} involves a symlink or submodule; message rendering is not ported (retry with --no-messages or --quiet)"
                );
            }
            if is_binary(repo, &id)? {
                bail!(
                    "conflict at {path} is a binary content merge; git's `warning: Cannot merge binary files` line is not ported (retry with --no-messages or --quiet)"
                );
            }
        }

        out.push(Message {
            path: path.clone(),
            ctype: "Auto-merging",
            text: format!("Auto-merging {path}\n"),
        });
        if merged_blob.resolution == gix::merge::blob::Resolution::Conflict {
            // Both sides adding the same path is reported as `add/add` in the
            // human message, but shares the `contents` short type with a plain
            // content conflict.
            let kind = if matches!(ours, Change::Addition { .. })
                && matches!(theirs, Change::Addition { .. })
            {
                "add/add"
            } else {
                "content"
            };
            out.push(Message {
                text: format!("CONFLICT ({kind}): Merge conflict in {path}\n"),
                path,
                ctype: "CONFLICT (contents)",
            });
        }
    }
    Ok(out)
}

/// The post-change mode and id of `change` (the rename destination for rewrites).
fn change_state(change: &Change) -> (gix::object::tree::EntryMode, ObjectId) {
    match change {
        Change::Addition { entry_mode, id, .. }
        | Change::Deletion { entry_mode, id, .. }
        | Change::Modification { entry_mode, id, .. }
        | Change::Rewrite { entry_mode, id, .. } => (*entry_mode, *id),
    }
}

/// git's binary heuristic: a NUL byte within the first 8000 bytes of the blob.
fn is_binary(repo: &gix::Repository, id: &ObjectId) -> Result<bool> {
    let data = repo.find_object(*id)?.data.clone();
    let head = &data[..data.len().min(8000)];
    Ok(head.contains(&0))
}

/// A path as it appears in the conflicted-file-info section: raw under `-z`,
/// otherwise C-quoted the way git's `core.quotePath` default does.
fn render_path(path: &BStr, nul: bool) -> Vec<u8> {
    if nul {
        path.to_vec()
    } else {
        quote_path(path).into_bytes()
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
