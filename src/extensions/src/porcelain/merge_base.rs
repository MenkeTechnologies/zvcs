//! `git merge-base` — find as good common ancestors as possible for a merge.
//!
//! All five operation modes of stock `git merge-base` are covered, driven by
//! gitoxide's port of git's `paint_down_to_common` + `remove_redundant`
//! (`gix_revision::merge_base`), so the selected bases are the same commits git
//! picks, printed one full hex id per line:
//!
//! ```text
//!   * `merge-base <commit> <commit>...`   — bases of the first commit against
//!                                           the rest taken together
//!   * `merge-base --octopus <commit>...`  — best common ancestors of all
//!   * `merge-base --independent <commit>...` — the input commits that are not
//!                                           reachable from another input
//!   * `merge-base --is-ancestor <a> <b>`  — no output, exit 0 (yes) / 1 (no)
//!   * `merge-base --fork-point <ref> [<commit>]` — walks the reflog of `<ref>`
//!   * `-a`/`--all`/`--no-all`
//! ```
//!
//! Exit codes follow git: 1 when no merge base exists (or `--is-ancestor` is
//! false), 128 for a bad object name or a mode/`--all` conflict, 129 for a
//! usage error (unknown option, wrong argument count, conflicting modes).
//!
//! Option abbreviation (`--oct` for `--octopus`) resolves the way `parse_long_opt()`
//! resolves it, against [`LONG_OPTS`].

use anyhow::Result;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::Repository;

use super::{Arg, LongOpt};

/// `cmd_merge_base()`'s `struct option options[]` (builtin/merge-base.c), in
/// table order, as [`super::resolve_long`] reads it. The four mode flags are
/// `OPT_CMDMODE`, which carries `PARSE_OPT_NONEG`, so only `--all` negates.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "all", neg: true, arg: Arg::None },
    LongOpt { name: "octopus", neg: false, arg: Arg::None },
    LongOpt { name: "independent", neg: false, arg: Arg::None },
    LongOpt { name: "is-ancestor", neg: false, arg: Arg::None },
    LongOpt { name: "fork-point", neg: false, arg: Arg::None },
];

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
    // The spelling `get_value()` would report for the mode flag already seen:
    // `optnamearg()` renders the *resolved* long name, so an abbreviation is
    // reported by the option it named.
    let mut mode_flag = String::new();
    let mut show_all = false;
    let mut revs: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    for arg in args.iter() {
        let raw = arg.as_str();
        if no_more_opts || !raw.starts_with('-') || raw == "-" {
            revs.push(raw);
            continue;
        }
        let resolved = match super::canonical_long(raw, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(raw, &first, &second, USAGE))
            }
        };
        let a = resolved.as_ref();
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
            // parse_options_step() answers `-h` on stdout at 129, with no
            // `error:` line — a help request is not a rejection.
            "-h" => return Ok(super::show_usage(USAGE)),
            // `PARSE_OPT_UNKNOWN` names a *switch* for a short argument
            // (parse-options.c:889-898).
            _ => {
                let _ = match a.strip_prefix("--") {
                    Some(body) => eprintln!("error: unknown option `{body}'"),
                    None => {
                        let c = a[1..].chars().next().unwrap_or_default();
                        match c.is_ascii() {
                            true => eprintln!("error: unknown switch `{c}'"),
                            false => eprintln!(
                                "error: unknown non-ascii option in string: `{a}'"
                            ),
                        }
                    }
                };
                return Ok(usage());
            }
        };
        // git's `OPT_CMDMODE`: a second, different mode flag is refused by
        // `get_value()` (parse-options.c:394-423), which names the option being
        // parsed *first* and the one already recorded second, and then
        // `return -1`.
        //
        // That -1 is `PARSE_OPT_ERROR` (`parse-options.h:62`: "must be the same
        // as error()"), which `parse_options()` answers with a bare
        // `exit(129)` — **no usage block**, unlike the `PARSE_OPT_UNKNOWN` arm
        // three lines below it that does call `usage_with_options()`. Verified
        // against stock 2.55.0: `merge-base --octopus --independent HEAD HEAD`
        // writes 71 bytes to stderr, `merge-base -Z` writes 662.
        if mode != Mode::Bases && mode != next_mode {
            eprintln!("error: options '{a}' and '{mode_flag}' cannot be used together");
            return Ok(ExitCode::from(129));
        }
        mode = next_mode;
        mode_flag = a.to_string();
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

/// `usage_with_options()` over `builtin/merge-base.c`'s option table: the five
/// synopsis lines, a blank line, then the options — the synopsis alone was only
/// half of what git prints, on `-h` and on a rejection alike.
const USAGE: &str = r"usage: git merge-base [-a | --all] <commit> <commit>...
   or: git merge-base [-a | --all] --octopus <commit>...
   or: git merge-base --is-ancestor <commit> <commit>
   or: git merge-base --independent <commit>...
   or: git merge-base --fork-point <ref> [<commit>]

    -a, --[no-]all        output all common ancestors
    --octopus             find ancestors for a single n-way merge
    --independent         list revs not reachable from others
    --is-ancestor         is the first one ancestor of the other?
    --fork-point          find where <commit> forked from reflog of <ref>

";

/// Print the usage block on stderr and return git's usage status.
fn usage() -> ExitCode {
    eprint!("{USAGE}");
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
