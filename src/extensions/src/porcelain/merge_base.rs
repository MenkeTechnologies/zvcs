//! `git merge-base` — find as good common ancestors as possible for a merge.
//!
//! All five operation modes of stock `git merge-base` are covered, driven by
//! gitoxide's port of git's `paint_down_to_common` + `remove_redundant`
//! (`gix_revision::merge_base`), so the selected bases are the same commits git
//! picks, printed one full hex id per line:
//!
//!   * `merge-base <commit> <commit>...`   — bases of the first commit against
//!                                           the rest taken together
//!   * `merge-base --octopus <commit>...`  — best common ancestors of all
//!   * `merge-base --independent <commit>...` — the input commits that are not
//!                                           reachable from another input
//!   * `merge-base --is-ancestor <a> <b>`  — no output, exit 0 (yes) / 1 (no)
//!   * `merge-base --fork-point <ref> [<commit>]` — walks the reflog of `<ref>`
//!   * `-a`/`--all`/`--no-all`
//!
//! Exit codes follow git: 1 when no merge base exists (or `--is-ancestor` is
//! false), 128 for a bad object name or a mode/`--all` conflict, 129 for a
//! usage error (unknown option, wrong argument count, conflicting modes).
//!
//! Not covered: option abbreviation (`--oct` for `--octopus`) — git's
//! `parse_options` accepts unique prefixes, this rejects them as unknown
//! options, which is the only intentional divergence.

use anyhow::Result;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::Repository;

/// The operation mode selected by the (mutually exclusive) mode flags.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Default: bases between the first commit and all the others.
    Bases,
    /// `--octopus`
    Octopus,
    /// `--independent`
    Independent,
    /// `--is-ancestor`
    IsAncestor,
    /// `--fork-point`
    ForkPoint,
}

/// `git merge-base` — see the module docs for the covered forms.
pub fn merge_base(args: &[String]) -> Result<ExitCode> {
    let mut mode = Mode::Bases;
    let mut mode_flag = "";
    let mut show_all = false;
    let mut revs: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    for a in args.iter().skip(1) {
        let a = a.as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            revs.push(a);
            continue;
        }
        let next_mode = match a {
            "--" => {
                no_more_opts = true;
                continue;
            }
            "-a" | "--all" => {
                show_all = true;
                continue;
            }
            "--no-all" => {
                show_all = false;
                continue;
            }
            "--octopus" => Mode::Octopus,
            "--independent" => Mode::Independent,
            "--is-ancestor" => Mode::IsAncestor,
            "--fork-point" => Mode::ForkPoint,
            _ => {
                eprintln!("error: unknown option `{}'", a.trim_start_matches('-'));
                return Ok(usage());
            }
        };
        // git's OPT_CMDMODE: a second, different mode flag is a usage error.
        if mode != Mode::Bases && mode != next_mode {
            eprintln!("error: options '{mode_flag}' and '{a}' cannot be used together");
            return Ok(usage());
        }
        mode = next_mode;
        mode_flag = a;
    }

    let repo = gix::discover(".")?;

    match mode {
        Mode::IsAncestor => {
            if revs.len() < 2 {
                return Ok(usage());
            }
            if show_all {
                return Ok(fatal("options '--is-ancestor' and '--all' cannot be used together"));
            }
            if revs.len() != 2 {
                return Ok(fatal("--is-ancestor takes exactly two commits"));
            }
            let (Some(one), Some(two)) = (
                commit_reference(&repo, revs[0]),
                commit_reference(&repo, revs[1]),
            ) else {
                return Ok(not_a_commit(&repo, &revs));
            };
            Ok(if is_ancestor(&repo, one, two)? {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Mode::ForkPoint => {
            if revs.is_empty() || revs.len() > 2 {
                return Ok(usage());
            }
            fork_point(&repo, revs[0], revs.get(1).copied().unwrap_or("HEAD"))
        }
        Mode::Independent => {
            if show_all {
                return Ok(fatal("options '--independent' and '--all' cannot be used together"));
            }
            let Some(commits) = resolve_all(&repo, &revs) else {
                return Ok(not_a_commit(&repo, &revs));
            };
            let heads = reduce_heads(&repo, &commits)?;
            if heads.is_empty() {
                return Ok(ExitCode::from(1));
            }
            // `--independent` always lists every remaining head.
            Ok(print_bases(&heads, true))
        }
        Mode::Octopus => {
            let Some(commits) = resolve_all(&repo, &revs) else {
                return Ok(not_a_commit(&repo, &revs));
            };
            let Some(bases) = octopus_bases(&repo, &commits)? else {
                return Ok(ExitCode::from(1));
            };
            let heads = reduce_heads(&repo, &bases)?;
            if heads.is_empty() {
                return Ok(ExitCode::from(1));
            }
            Ok(print_bases(&heads, show_all))
        }
        Mode::Bases => {
            if revs.len() < 2 {
                return Ok(usage());
            }
            let Some(commits) = resolve_all(&repo, &revs) else {
                return Ok(not_a_commit(&repo, &revs));
            };
            let bases: Vec<ObjectId> = repo
                .merge_bases_many(commits[0], &commits[1..])?
                .into_iter()
                .map(|id| id.detach())
                .collect();
            if bases.is_empty() {
                return Ok(ExitCode::from(1));
            }
            Ok(print_bases(&bases, show_all))
        }
    }
}

/// git's `get_commit_reference`: resolve `spec` and peel it to the commit it
/// names (so tags and refs work), or `None` if it names no commit.
fn commit_reference(repo: &Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    object.peel_to_commit().ok().map(|c| c.id)
}

/// Resolve every rev, or `None` if any of them fails to name a commit.
fn resolve_all(repo: &Repository, revs: &[&str]) -> Option<Vec<ObjectId>> {
    revs.iter().map(|r| commit_reference(repo, r)).collect()
}

/// Report the first rev that doesn't name a commit, exactly as git's
/// `get_commit_reference` dies (exit 128).
fn not_a_commit(repo: &Repository, revs: &[&str]) -> ExitCode {
    let bad = revs
        .iter()
        .find(|r| commit_reference(repo, r).is_none())
        .copied()
        .unwrap_or("");
    fatal(&format!("Not a valid object name {bad}"))
}

/// Print a `fatal:` line and return git's die status.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// Print the usage synopsis and return git's usage status.
fn usage() -> ExitCode {
    eprintln!("usage: git merge-base [-a | --all] <commit> <commit>...");
    eprintln!("   or: git merge-base [-a | --all] --octopus <commit>...");
    eprintln!("   or: git merge-base --is-ancestor <commit> <commit>");
    eprintln!("   or: git merge-base --independent <commit>...");
    eprintln!("   or: git merge-base --fork-point <ref> [<commit>]");
    ExitCode::from(129)
}

/// Print the bases, one hex id per line — only the first unless `show_all`.
fn print_bases(bases: &[ObjectId], show_all: bool) -> ExitCode {
    for id in bases {
        println!("{id}");
        if !show_all {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// git's `in_merge_bases`: is `one` reachable from `two`? True exactly when
/// `one` is itself a merge base of the two.
fn is_ancestor(repo: &Repository, one: ObjectId, two: ObjectId) -> Result<bool> {
    Ok(repo
        .merge_bases_many(one, &[two])?
        .into_iter()
        .any(|id| id.detach() == one))
}

/// git's `get_octopus_merge_bases`: fold the commit list into the accumulated
/// bases, taking every pairwise merge base at each step. `None` when the
/// commits don't all share history (git returns an empty list there).
fn octopus_bases(repo: &Repository, commits: &[ObjectId]) -> Result<Option<Vec<ObjectId>>> {
    let Some((first, rest)) = commits.split_first() else {
        return Ok(None);
    };
    let mut acc = vec![*first];
    for commit in rest {
        let mut next = Vec::new();
        for base in &acc {
            next.extend(
                repo.merge_bases_many(*commit, std::slice::from_ref(base))?
                    .into_iter()
                    .map(|id| id.detach()),
            );
        }
        if next.is_empty() {
            return Ok(None);
        }
        acc = next;
    }
    Ok(Some(acc))
}

/// git's `reduce_heads`: de-duplicate `commits` (keeping first occurrence, so
/// input order is preserved) and drop every commit that is reachable from
/// another one in the list.
fn reduce_heads(repo: &Repository, commits: &[ObjectId]) -> Result<Vec<ObjectId>> {
    let mut unique: Vec<ObjectId> = Vec::with_capacity(commits.len());
    for id in commits {
        if !unique.contains(id) {
            unique.push(*id);
        }
    }

    let mut out = Vec::with_capacity(unique.len());
    for (i, candidate) in unique.iter().enumerate() {
        let mut redundant = false;
        for (j, other) in unique.iter().enumerate() {
            if i != j && is_ancestor(repo, *candidate, *other)? {
                redundant = true;
                break;
            }
        }
        if !redundant {
            out.push(*candidate);
        }
    }
    Ok(out)
}

/// git's `handle_fork_point`: find where the history leading to `commitname`
/// forked from any incarnation of `refname`, using that ref's reflog.
///
/// The candidate set is every commit the reflog ever pointed at (plus the old
/// id of its first entry), or the ref tip when there is no reflog. There must
/// be exactly one merge base between `commitname` and that set, and it must be
/// one of the candidates; otherwise git prints nothing and exits 1.
fn fork_point(repo: &Repository, refname: &str, commitname: &str) -> Result<ExitCode> {
    let Ok(reference) = repo.find_reference(refname) else {
        return Ok(fatal(&format!("No such ref: '{refname}'")));
    };
    let Some(derived) = commit_reference(repo, commitname) else {
        return Ok(fatal(&format!("Not a valid object name: '{commitname}'")));
    };

    let mut candidates: Vec<ObjectId> = Vec::new();
    let push = |id: ObjectId, candidates: &mut Vec<ObjectId>| {
        // Skip the null id, non-commits, and repeats — as `add_one_commit` does.
        if id.is_null() || candidates.contains(&id) {
            return;
        }
        if repo
            .find_header(id)
            .is_ok_and(|h| h.kind() == gix::object::Kind::Commit)
        {
            candidates.push(id);
        }
    };

    let mut log = reference.log_iter();
    if let Some(entries) = log.all()? {
        let mut first = true;
        for entry in entries {
            let entry = entry?;
            if first {
                first = false;
                push(entry.previous_oid(), &mut candidates);
            }
            push(entry.new_oid(), &mut candidates);
        }
    }
    if candidates.is_empty() {
        // No reflog: fall back to what the ref points at right now.
        if let Some(id) = commit_reference(repo, refname) {
            push(id, &mut candidates);
        }
    }

    let bases: Vec<ObjectId> = repo
        .merge_bases_many(derived, &candidates)?
        .into_iter()
        .map(|id| id.detach())
        .collect();

    // Exactly one base, and it has to be one of the reflog entries.
    if bases.len() != 1 || !candidates.contains(&bases[0]) {
        return Ok(ExitCode::from(1));
    }
    println!("{}", bases[0]);
    Ok(ExitCode::SUCCESS)
}
