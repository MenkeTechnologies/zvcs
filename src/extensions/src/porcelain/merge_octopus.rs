//! `git merge-octopus` — resolve two or more trees (the octopus merge strategy).
//!
//! Stock `git-merge-octopus` is a POSIX shell driver (`git-merge-octopus.sh`)
//! that sources `git-sh-setup` and then orchestrates four plumbing commands per
//! head being merged: `git merge-base --all`, `git read-tree -u -m`,
//! `git write-tree`, and `git merge-index -o git-merge-one-file -a`. Two of
//! those have no substrate here:
//!
//! * `read-tree -u -m <a> <b>` and `read-tree -u -m --aggressive <base> <ours>
//!   <theirs>` — the two- and three-way index merge with worktree update. The
//!   ported `read-tree` rejects both (`read_tree.rs:120` for `--aggressive`,
//!   `read_tree.rs:183` for more than one tree-ish with `-m`), because gitoxide
//!   has no `unpack_trees` equivalent.
//! * `git-merge-one-file` — the per-path shell resolver `merge-index` spawns.
//!   The ported `merge-index` execs whatever program it is handed; there is no
//!   `git-merge-one-file` binary in this tree to hand it.
//!
//! Everything the script does *before* it touches the index is reproduced
//! natively and byte-for-byte; every path that would mutate the index or the
//! worktree bails with a message naming the missing substrate rather than
//! writing an approximation of a merge.
//!
//! ### Covered (verified against git 2.55.0: stdout, stderr, exit code)
//!
//! * `-h` as the first argument — `git-sh-setup`'s `$LONG_USAGE` path with an
//!   empty `USAGE`, i.e. the single line `usage: git merge-octopus ` (note the
//!   trailing space) on **stdout**, exit 0, and no repository required.
//! * `git_dir_init` running before any argument is looked at: outside a
//!   repository, `fatal: not a git repository (or any of the parent
//!   directories): .git` on stderr, exit 128.
//! * The argument split: everything before the first `--` is a merge base and
//!   is discarded, the first argument after it is `$head`, the rest are the
//!   heads to merge.
//! * The "this is not an octopus" guard — fewer than two heads to merge exits 2
//!   silently, so `git merge` can fall back to another strategy.
//! * The `git diff-index --quiet --cached HEAD --` pre-flight: on any
//!   tree↔index difference, `Error: Your local changes to the following files
//!   would be overwritten by merge` followed by the changed paths each indented
//!   by four spaces — both on **stdout**, as `gettextln` and the script's `sed`
//!   pipeline emit them — then exit 2. Paths are quoted per `core.quotePath`.
//! * The merge-base pass over every head: `$GITHEAD_<sha1>` (then the
//!   uppercased `$GITHEAD_<SHA1>`) as the pretty name, `Already up to date with
//!   <name>` on stdout for a head already reachable, and
//!   `Unable to find common commit with <name>` on stderr with exit 1 (the
//!   script's `die`, which prints no `fatal:` prefix) when `merge-base --all`
//!   fails or finds nothing.
//! * The all-heads-already-up-to-date run therefore completes exactly as git
//!   does: those lines on stdout, exit 0, and the repository untouched.
//!
//! ### Not covered
//!
//! The moment a head actually needs merging — the fast-forward branch
//! (`Fast-forwarding to: <name>`) or the three-way branch (`Trying simple merge
//! with <name>`, `Simple merge did not work, trying automatic merge.`,
//! `Automated merge did not work.` / `Should not be doing an octopus.`) — this
//! bails. No index or worktree write happens, and no `MRT`/`MRC` bookkeeping is
//! faked. `git write-tree` is likewise never run: the script's only unchecked
//! `write-tree` is unreachable here, since an index that would fail it is
//! already rejected by the `diff-index` pre-flight above.
//!
//! An unborn `HEAD` also bails: stock git runs `diff-index` against it twice
//! and lets the resulting `fatal: ambiguous argument 'HEAD'` through, which is
//! not reproduced. So does an unmerged index, whose `U` records the ported
//! `diff-index` does not emit either.

use anyhow::{bail, Result};
use std::collections::BTreeSet;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::hash::ObjectId;
use gix::Repository;

/// `git-sh-setup`'s `$LONG_USAGE` for a script that sets neither `USAGE` nor
/// `OPTIONS_SPEC`: `usage: $dashless $USAGE` with `$USAGE` empty, so the line
/// ends in a space. `echo` supplies the newline.
const LONG_USAGE: &str = "usage: git merge-octopus \n";

/// The script's argument loop: merge bases, then `--`, then `$head`, then the
/// heads to merge. Bases are collected but unused, exactly as in the script.
struct Args {
    head: Option<String>,
    remotes: Vec<String>,
}

/// Reproduce the `case ",$sep_seen,$head,$arg," in` dispatch verbatim: `--`
/// flips the separator (every time it appears), the first argument after it
/// becomes `$head`, later ones accumulate into `$remotes`, and anything before
/// it is a merge base.
fn parse(args: &[String]) -> Args {
    let mut sep_seen = false;
    let mut head: Option<String> = None;
    let mut remotes = Vec::new();

    for arg in args {
        if arg == "--" {
            sep_seen = true;
        } else if !sep_seen {
            // A merge base; the script keeps these in `$bases` and never reads it.
        } else if head.is_none() {
            head = Some(arg.clone());
        } else {
            remotes.push(arg.clone());
        }
    }

    Args { head, remotes }
}

/// `git merge-octopus` — see the module docs for what is and is not covered.
pub fn merge_octopus(args: &[String]) -> Result<ExitCode> {
    // `git-sh-setup` inspects only `$1`, and does so before `git_dir_init`.
    if args.first().map(String::as_str) == Some("-h") {
        print!("{LONG_USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    // `git_dir_init`, which every non-`-h` invocation reaches first.
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    let parsed = parse(args);

    // `case "$remotes" in ?*' '?*)` — anything but two or more heads to merge
    // is not an octopus, and exits 2 without a word so `git merge` can pick
    // another strategy.
    if parsed.remotes.len() < 2 {
        return Ok(ExitCode::from(2));
    }

    // `if ! git diff-index --quiet --cached HEAD --`
    let dirty = dirty_paths(&repo)?;
    if !dirty.is_empty() {
        println!("Error: Your local changes to the following files would be overwritten by merge");
        for path in &dirty {
            println!("    {}", quote_path(path));
        }
        return Ok(ExitCode::from(2));
    }

    // `MRC=$(git rev-parse --verify -q $head)` — unpeeled, and its failure is
    // not checked by the script, which leaves `$MRC` empty and lets the first
    // `merge-base` call fail instead.
    let head_spec = parsed.head.as_deref().unwrap_or("");
    let mrc: Option<ObjectId> = repo
        .rev_parse_single(head_spec)
        .ok()
        .map(|id| id.detach());

    for sha1 in &parsed.remotes {
        let pretty = pretty_name(sha1);

        // `common=$(git merge-base --all $SHA1 $MRC) || die ...`
        let common = match mrc.and_then(|mrc| merge_bases(&repo, sha1, mrc).transpose()) {
            Some(bases) => bases?,
            None => Vec::new(),
        };
        if common.is_empty() {
            eprintln!("Unable to find common commit with {pretty}");
            return Ok(ExitCode::from(1));
        }

        // `case "$LF$common$LF" in *"$LF$SHA1$LF"*)` — a literal line-wise
        // comparison against the argument as spelled, so only a full object id
        // can match. `git merge` always passes full ids.
        if common.iter().any(|id| id.to_string() == *sha1) {
            println!("Already up to date with {pretty}");
            continue;
        }

        // Past this point the script reads trees into the index and updates the
        // worktree. `NON_FF_MERGE` is still 0 here (a non-fast-forward merge
        // would have bailed on the previous iteration), so the branch taken is
        // decided by `test "$common,$NON_FF_MERGE" = "$MRC,0"`.
        let fast_forward = common.len() == 1 && Some(common[0]) == mrc;
        if fast_forward {
            bail!(
                "unsupported: fast-forwarding to {pretty} needs `read-tree -u -m <head> <commit>`, \
                 the two-tree index merge with worktree update, which is not ported \
                 (ported: argument split, the not-an-octopus and dirty-index pre-flights, \
                 merge-base scan, already-up-to-date)"
            );
        }
        bail!(
            "unsupported: merging {pretty} needs `read-tree -u -m --aggressive` (three-tree index \
             merge with worktree update) and `merge-index -o git-merge-one-file -a`, neither of \
             which has a substrate here (ported: argument split, the not-an-octopus and \
             dirty-index pre-flights, merge-base scan, already-up-to-date)"
        );
    }

    // Every head was already up to date, so `OCTOPUS_FAILURE` is still 0.
    Ok(ExitCode::SUCCESS)
}

/// `eval pretty_name=\${GITHEAD_$SHA1:-$SHA1}`, then the uppercased retry.
/// `${x:-y}` treats an empty value as unset, hence the `filter`.
fn pretty_name(sha1: &str) -> String {
    let lookup = |key: String| std::env::var(key).ok().filter(|v| !v.is_empty());
    if let Some(name) = lookup(format!("GITHEAD_{sha1}")) {
        return name;
    }
    let upper: String = sha1
        .chars()
        .map(|c| if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c })
        .collect();
    lookup(format!("GITHEAD_{upper}")).unwrap_or_else(|| sha1.to_string())
}

/// `git merge-base --all <sha1> <mrc>`: peel both to commits as git's
/// `get_commit_reference` does, then take every best common ancestor. `None`
/// when either argument does not name a commit, which is git's exit-128 path
/// and the script's `die` either way.
fn merge_bases(repo: &Repository, sha1: &str, mrc: ObjectId) -> Result<Option<Vec<ObjectId>>> {
    let Some(one) = commit_reference(repo, sha1) else {
        return Ok(None);
    };
    let Some(two) = repo
        .find_object(mrc)
        .ok()
        .and_then(|o| o.peel_to_commit().ok())
        .map(|c| c.id)
    else {
        return Ok(None);
    };
    Ok(Some(
        repo.merge_bases_many(one, &[two])?
            .into_iter()
            .map(|id| id.detach())
            .collect(),
    ))
}

/// Resolve `spec` and peel it to the commit it names, or `None`.
fn commit_reference(repo: &Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    object.peel_to_commit().ok().map(|c| c.id)
}

/// The paths `git diff-index --cached --name-only HEAD --` would print, sorted
/// bytewise as the index — and therefore git's diff queue — orders them.
fn dirty_paths(repo: &Repository) -> Result<Vec<BString>> {
    use gix::diff::index::ChangeRef;
    use gix::status::tree_index::TrackRenames;

    let head_tree = match repo.head_commit().ok().and_then(|c| c.tree_id().ok()) {
        Some(id) => id.detach(),
        None => bail!(
            "unsupported: merge-octopus against an unborn HEAD (git lets diff-index's \
             `fatal: ambiguous argument 'HEAD'` through, which is not reproduced)"
        ),
    };

    let index = repo.index_or_empty()?;
    let index_state: &gix::index::State = &index;
    if index_state.entries().iter().any(|e| e.stage_raw() != 0) {
        bail!("unsupported: unmerged (conflicted) index entries — diff-index's U records are not ported");
    }

    let mut paths: BTreeSet<BString> = BTreeSet::new();
    repo.tree_index_status(
        &head_tree,
        index_state,
        None,
        TrackRenames::Disabled,
        |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
            match change {
                ChangeRef::Addition { location, .. } => {
                    paths.insert(location.into_owned());
                }
                ChangeRef::Deletion { location, .. } => {
                    paths.insert(location.into_owned());
                }
                ChangeRef::Modification { location, .. } => {
                    paths.insert(location.into_owned());
                }
                // Rename tracking is disabled above, so this never fires.
                ChangeRef::Rewrite { .. } => {}
            }
            Ok(gix::diff::index::Action::Continue(()))
        },
    )?;

    Ok(paths.into_iter().collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// The `case ",$sep_seen,$head,$arg," in` dispatch: bases are dropped, the
    /// first argument after `--` is the head, the rest are merged.
    #[test]
    fn splits_bases_head_and_remotes() {
        let a = parse(&v(&["base1", "base2", "--", "head", "r1", "r2"]));
        assert_eq!(a.head.as_deref(), Some("head"));
        assert_eq!(a.remotes, v(&["r1", "r2"]));

        // No separator at all: everything is a merge base, so there is nothing
        // to merge and the caller exits 2.
        let a = parse(&v(&["head", "r1", "r2"]));
        assert_eq!(a.head, None);
        assert!(a.remotes.is_empty());

        // A second `--` re-sets `sep_seen`, which is already `yes`, so it is
        // consumed rather than becoming a head — as in the script.
        let a = parse(&v(&["--", "head", "--", "r1"]));
        assert_eq!(a.head.as_deref(), Some("head"));
        assert_eq!(a.remotes, v(&["r1"]));
    }

    /// `${GITHEAD_$SHA1:-$SHA1}` falls back to the id itself when no
    /// `GITHEAD_<id>` is exported, which is the case for this synthetic id.
    #[test]
    fn pretty_name_falls_back_to_the_id() {
        let id = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(pretty_name(id), id);
    }
}
