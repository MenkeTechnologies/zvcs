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
            // `--help-all` reaches the same renderer with USAGE_FULL, which this
            // table renders identically: it has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => return Ok(super::show_usage(USAGE)),
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
            // `handle_is_ancestor()` resolves argv[0] and then argv[1]
            // (builtin/merge-base.c:117-118), so the *first* bad rev is named.
            let one = match get_commit_reference(&repo, revs[0]) {
                Ok(id) => id,
                Err(code) => return Ok(code),
            };
            let two = match get_commit_reference(&repo, revs[1]) {
                Ok(id) => id,
                Err(code) => return Ok(code),
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
            // `handle_independent()` walks its args back to front
            // (builtin/merge-base.c:65-66).
            let commits = match resolve_all(&repo, &revs, true) {
                Ok(commits) => commits,
                Err(code) => return Ok(code),
            };
            let heads = reduce_heads(&repo, &commits)?;
            if heads.is_empty() {
                return Ok(ExitCode::from(1));
            }
            // `--independent` always lists every remaining head.
            Ok(print_bases(&heads, true))
        }
        Mode::Octopus => {
            // `handle_octopus()` walks its args back to front
            // (builtin/merge-base.c:86-87).
            let commits = match resolve_all(&repo, &revs, true) {
                Ok(commits) => commits,
                Err(code) => return Ok(code),
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
            // The default mode consumes argv forward
            // (builtin/merge-base.c:205-206).
            let commits = match resolve_all(&repo, &revs, false) {
                Ok(commits) => commits,
                Err(code) => return Ok(code),
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

/// git's `get_commit_reference()` (builtin/merge-base.c:46-58), which every
/// operand of every mode is funnelled through:
///
/// ```c
/// if (repo_get_oid(the_repository, arg, &revkey))
///         die("Not a valid object name %s", arg);
/// r = lookup_commit_reference(the_repository, &revkey);
/// if (!r)
///         die("Not a valid commit name %s", arg);
/// ```
///
/// It dies *twice over*, and which of the two messages is reached is decided by
/// `repo_get_oid()` alone. A full-length hex id always gets past the first check
/// even when the object is absent, because `get_oid_basic()` decodes it without
/// consulting the object database — [`crate::objname::resolve`] is that rule.
/// `lookup_commit_reference` then fails to peel it (`peel_object_ext` cannot
/// read a missing object) and the *second* message is the one printed. Resolving
/// only through `rev_parse_single` collapsed the two and always printed the first.
///
/// There is a third outcome the two `die`s hide: `lookup_commit_reference()` is
/// `lookup_commit_reference_gently(r, oid, 0)`, so `quiet` is 0 and an object
/// that is *present* but is not a commit gets a diagnostic of its own first
/// (commit.c:61-67):
///
/// ```c
/// if (type != OBJ_COMMIT) {
///         if (!quiet)
///                 error(_("object %s is a %s, not a %s"),
///                       oid_to_hex(oid), type_name(type), type_name(OBJ_COMMIT));
/// ```
///
/// That line mixes the two halves of the lookup — `oid` is the **operand's** id
/// while `type` is what the peel *arrived at* — so an annotated tag pointing at a
/// tree reports the tag's id and the word `tree`, and the `fatal:` under it still
/// names the spec as typed. [`crate::objname::CommitRef`] models exactly that
/// split, which is why the wording is taken from it rather than rebuilt here.
fn get_commit_reference(repo: &Repository, arg: &str) -> Result<ObjectId, ExitCode> {
    let Some(oid) = crate::objname::resolve(repo, arg) else {
        return Err(fatal(&format!("Not a valid object name {arg}")));
    };
    let found = crate::objname::lookup_commit_reference(repo, oid);
    if let crate::objname::CommitRef::Commit(id) = found {
        return Ok(id);
    }
    if let Some(note) = found.type_error() {
        eprintln!("error: {note}");
    }
    Err(fatal(&format!("Not a valid commit name {arg}")))
}

/// Resolve every rev through [`get_commit_reference`], stopping at the first one
/// that fails — in the order the *mode's own loop* visits them.
///
/// `--octopus` and `--independent` build their `commit_list` back to front
/// (builtin/merge-base.c:65-66 and :86-87):
///
/// ```c
/// for (i = count - 1; i >= 0; i--)
///         commit_list_insert(get_commit_reference(args[i]), &revs);
/// ```
///
/// `commit_list_insert()` prepends, so the assembled list still comes out in
/// input order and the *result* is unaffected — but the resolution runs last rev
/// first, and it is therefore the **last** bad rev that gets named, not the
/// first. Every other mode walks argv forward, so `last_first` is what tells the
/// two apart; scanning forward for all of them named the wrong rev whenever more
/// than one was bad.
fn resolve_all(
    repo: &Repository,
    revs: &[&str],
    last_first: bool,
) -> Result<Vec<ObjectId>, ExitCode> {
    let mut ids = Vec::with_capacity(revs.len());
    if last_first {
        for rev in revs.iter().rev() {
            ids.push(get_commit_reference(repo, rev)?);
        }
        ids.reverse();
    } else {
        for rev in revs {
            ids.push(get_commit_reference(repo, rev)?);
        }
    }
    Ok(ids)
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
///
/// Two things are easy to get backwards here, and both are visible in
/// `handle_fork_point()` (builtin/merge-base.c:128-147):
///
/// ```c
/// if (repo_get_oid(the_repository, commitname, &oid))
///         die("Not a valid object name: '%s'", commitname);
/// derived = lookup_commit_reference(the_repository, &oid);
/// fork_point = get_fork_point(argv[0], derived);
/// if (!fork_point)
///         return 1;
/// ```
///
/// The rev is checked *before* `get_fork_point()` looks the ref up, so a bad rev
/// outranks a bad ref. And only `repo_get_oid()` is fatal: `lookup_commit_reference`
/// failing leaves `derived` NULL, which `get_fork_point()` walks from without
/// finding anything — exit 1, silently. A full-length hex id of an absent object
/// resolves (it never reaches the object database), so `--fork-point <ref> <absent>`
/// is that silent exit 1 and not a fatal.
///
/// "Silently" only covers the *absent* object. `lookup_commit_reference()` is not
/// quiet (see [`get_commit_reference`]), so a rev naming a present non-commit
/// prints `error: object %s is a %s, not a %s` here — and prints it on line 138,
/// **ahead of** the `No such ref:` that `get_fork_point()` dies with on line 140.
/// Looking the ref up first would put the two lines in the wrong order.
fn fork_point(repo: &Repository, refname: &str, commitname: &str) -> Result<ExitCode> {
    let Some(oid) = crate::objname::resolve(repo, commitname) else {
        return Ok(fatal(&format!("Not a valid object name: '{commitname}'")));
    };
    let found = crate::objname::lookup_commit_reference(repo, oid);
    if let Some(note) = found.type_error() {
        eprintln!("error: {note}");
    }
    let derived = match found {
        crate::objname::CommitRef::Commit(id) => Some(id),
        _ => None,
    };

    let Ok(reference) = repo.find_reference(refname) else {
        return Ok(fatal(&format!("No such ref: '{refname}'")));
    };
    let Some(derived) = derived else {
        // `derived == NULL`: nothing to take a merge base against.
        return Ok(ExitCode::from(1));
    };

    let mut candidates: Vec<ObjectId> = Vec::new();
    // `add_one_commit()` (commit.c:1061-1076):
    //
    // ```c
    // if (is_null_oid(oid)) return;
    // commit = lookup_commit(the_repository, oid);
    // if (!commit || (commit->object.flags & TMP_MARK) ||
    //     repo_parse_commit(the_repository, commit))
    //         return;
    // ```
    //
    // The null id and an id already taken (`TMP_MARK`, which only a commit ever
    // gets) are dropped in silence — but `repo_parse_commit()` is
    // `repo_parse_commit_gently(r, item, 0)`, i.e. **not** quiet, so anything it
    // turns away is reported before the `return` (commit.c:641-650):
    //
    // ```c
    // if (odb_read_object_info_extended(...) < 0)
    //         return quiet_on_missing ? -1 : error("Could not read %s", ...);
    // if (type != OBJ_COMMIT)
    //         return error("Object %s not a commit", ...);
    // ```
    //
    // Both name `oid_to_hex()`, neither is translated, and neither has ever been
    // fatal: `get_fork_point()` simply ends up with one candidate fewer.
    //
    // Known gap. `lookup_commit()` reaches `object_as_type(obj, OBJ_COMMIT, 0)`
    // instead — `error("object %s is a %s, not a %s")` — when the id is *already
    // interned* in git's in-memory object table, and only then; an id git has not
    // seen yet gets a fresh commit node and falls through to the wording above.
    // Which of the two a non-commit id gets therefore depends on what has been
    // parsed so far in the process (parsing `derived` interns its own tree and
    // parents), not on anything in the repository. Reproducing that would mean
    // modelling git's object table, so the parse-time wording is used for every
    // non-commit: right for the reachable case — a ref that names a tag object,
    // which nothing has interned — and wrong only for an id some earlier parse
    // happened to touch, e.g. a ref pointed straight at the tip commit's tree.
    let push = |id: ObjectId, candidates: &mut Vec<ObjectId>| {
        if id.is_null() || candidates.contains(&id) {
            return;
        }
        match repo.find_header(id) {
            Ok(header) if header.kind() == gix::object::Kind::Commit => candidates.push(id),
            Ok(_) => eprintln!("error: Object {id} not a commit"),
            Err(_) => eprintln!("error: Could not read {id}"),
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
        // No reflog: `add_one_commit(&oid, &revs)` on the id `repo_dwim_ref()`
        // returned — the ref's own target, *not* peeled, so an annotated tag ref
        // contributes nothing (`push` drops it for the same reason
        // `add_one_commit`'s `repo_parse_commit()` does).
        if let Some(id) = crate::objname::resolve(repo, refname) {
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
