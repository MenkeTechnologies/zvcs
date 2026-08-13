//! `git rev-parse` — argument-order-sensitive revision and flag translation.
//!
//! The shape of stock `rev-parse` is a single left-to-right scan over `argv`.
//! Display options (`--short`, `--symbolic`, `--abbrev-ref`, …) mutate state as
//! they are encountered, so an argument is rendered with whatever options were
//! in effect *at its position*: `--branches --symbolic` prints object ids while
//! `--symbolic --branches` prints names. Repository queries (`--git-dir`,
//! `--show-toplevel`, …) print at their position. This module keeps that scan.
//!
//! `--verify` (which `--short` turns on implicitly) changes the flow: revisions
//! are counted rather than printed, a non-revision argument aborts immediately,
//! and the single surviving revision is printed *after* the scan using the final
//! option state. That is why `--verify HEAD --symbolic` prints `HEAD`.
//!
//! `--` is the end-of-options separator: every following token is a pathspec,
//! echoed verbatim under `DO_NONFLAGS` with no worktree existence check and never
//! interpreted as a flag or counted as a revision (git sets `as_is = 2`). The
//! `--` token itself is echoed when `DO_FLAGS`/`DO_REVS` are still in effect.
//!
//! Range revspecs are expanded at their position: `a..b` prints `b` then `^a`;
//! `a...b` prints `b`, `a`, then `^<merge-base>` for each merge base — matching
//! stock git's left-to-right emission.
//!
//! Implemented: `--verify`, `-q`/`--quiet`, `--short[=n]`, `--abbrev-ref`,
//! `--symbolic`, `--symbolic-full-name`, `--git-dir`, `--absolute-git-dir`,
//! `--git-common-dir`, `--show-toplevel`, `--is-inside-work-tree`,
//! `--is-inside-git-dir`, `--is-bare-repository`, `--is-shallow-repository`,
//! `--show-cdup`, `--show-prefix`, `--show-object-format`, `--show-ref-format`,
//! `--all`, `--branches`, `--tags`, `--remotes[=<pattern>]`, `--glob=<pattern>`,
//! `--exclude=<pattern>`, `--disambiguate=<prefix>`, `--sq-quote`, plus revision
//! and path arguments. Every other option stock git recognizes is rejected
//! rather than ignored; options git does *not* recognize are echoed, which is
//! what git itself does with them.

use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

use crate::advice::Advice;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;
use gix::refs::TargetRef;

/// How a revision's *name* is rendered, when it is rendered instead of its id.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sym {
    /// Render the object id.
    No,
    /// `--symbolic`: echo the argument as close to the input as possible.
    AsIs,
    /// `--symbolic-full-name`: render the full ref name, or nothing if not a ref.
    Full,
}

/// Which ref namespace a bulk-listing option walks.
#[derive(Clone, Copy)]
enum RefSet {
    All,
    Branches,
    Tags,
}

/// What the full revspec grammar (`rev_parse`) made of an argument the
/// single-object parser could not resolve.
#[derive(Clone, Copy)]
enum Parsed {
    /// A range/merge revspec, expanded to both endpoints at this position.
    Range(RangeSpec),
    /// A single object: `reversed` marks a `^rev` exclude, which prints `^<id>`.
    Single { id: ObjectId, reversed: bool },
}

/// A resolved range revspec, ready to emit at its position in the scan.
#[derive(Clone, Copy)]
enum RangeSpec {
    /// `from..to`: prints `to`, then `^from`.
    Range { from: ObjectId, to: ObjectId },
    /// `theirs...ours`: prints `ours`, `theirs`, then `^<merge-base>` per base.
    Merge { theirs: ObjectId, ours: ObjectId },
}

struct Opts {
    verify: bool,
    quiet: bool,
    /// `None` = full hex, `Some(0)` = `core.abbrev`/auto length, `Some(n)` = `n` hex chars.
    abbrev: Option<usize>,
    sym: Sym,
    abbrev_ref: bool,
    /// git's `DO_FLAGS`: echo unrecognized options. Cleared by `--verify`/`--short`.
    echo_flags: bool,
    /// git's `DO_NONFLAGS`: echo path arguments. Cleared by `--verify`/`--short`.
    echo_paths: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            verify: false,
            quiet: false,
            abbrev: None,
            sym: Sym::No,
            abbrev_ref: false,
            echo_flags: true,
            echo_paths: true,
        }
    }
}

/// Options stock git recognizes that this port does not implement. Echoing them
/// the way unknown options are echoed would silently produce a wrong answer, so
/// they are rejected instead.
const UNIMPLEMENTED_EXACT: &[&str] = &[
    "-h",
    "--help",
    "--parseopt",
    "--sq-quote",
    "--keep-dashdash",
    "--stop-at-non-option",
    "--stuck-long",
    "--sq",
    "--not",
    "--default",
    "--prefix",
    "--revs-only",
    "--no-revs",
    "--flags",
    "--no-flags",
    "--local-env-vars",
    "--output-object-format",
    "--resolve-git-dir",
    "--git-path",
    "--shared-index-path",
    "--is-shallow-repository",
    "--show-superproject-working-tree",
    "--remotes",
    "--bisect",
    "--end-of-options",
    "--all-objects",
];

const UNIMPLEMENTED_PREFIX: &[&str] = &[
    "--abbrev-ref=",
    "--path-format=",
    "--disambiguate=",
    "--glob=",
    "--exclude=",
    "--exclude-hidden=",
    "--branches=",
    "--tags=",
    "--remotes=",
    "--since=",
    "--after=",
    "--until=",
    "--before=",
    "--default=",
    "--prefix=",
    "--git-path=",
];

pub fn rev_parse(args: &[String]) -> Result<ExitCode> {
    // `setup_git_directory()` looks at `$GIT_DIR` before it walks upwards, so
    // `git --git-dir=<path> rev-parse <rev>` resolves against THAT repository.
    // Plain discovery ignores the variable and silently answers about whatever
    // repository the current directory happens to sit in — a wrong object id
    // rather than an error.
    let repo = match gix::discover_with_environment_overrides(".") {
        Ok(repo) => repo,
        Err(_) => {
            eprintln!("fatal: not a git repository (or any of the parent directories): .git");
            return Ok(ExitCode::from(128));
        }
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut o = Opts::default();
    // The single revision `--verify` mode holds back until the scan finishes.
    // The `bool` is git's "reversed" flag: a `^rev` exclude prints `^<id>`.
    let mut held: Option<(ObjectId, BString, bool)> = None;
    let mut revs = 0usize;
    // Once a path argument is seen, every later argument is a path too.
    let mut as_is = false;
    // Set by an explicit `--`: git's `as_is = 2`. Every later token is a pathspec
    // echoed verbatim with no existence check and no flag interpretation.
    let mut dashdash = false;

    for arg in args {
        // After an explicit `--`, everything is a pathspec: echo it (when paths
        // are being echoed) and move on. No existence check, no flag parsing.
        if dashdash {
            if o.echo_paths {
                emit(&mut out, arg.as_bytes())?;
            }
            continue;
        }

        // `--` terminates options. git echoes the separator itself while flags or
        // revs are still being echoed (`DO_FLAGS`/`DO_REVS`), i.e. not under
        // `--verify`/`--short`.
        if !as_is && arg == "--" {
            if o.echo_flags {
                emit(&mut out, arg.as_bytes())?;
            }
            dashdash = true;
            continue;
        }

        if !as_is && arg.len() > 1 && arg.starts_with('-') {
            match option(&mut o, arg)? {
                Opt::Consumed => {}
                Opt::Query(q) => {
                    if let Some(code) = query(&mut out, &repo, q)? {
                        out.flush()?;
                        return Ok(code);
                    }
                }
                Opt::Refs(which) => {
                    for (echo, full, id) in collect_refs(&repo, which)? {
                        show_rev(&mut out, &repo, &o, &id, Some(echo.as_bstr()), Some(full.as_bstr()), false)?;
                    }
                }
                Opt::Unknown => {
                    if o.echo_flags {
                        emit(&mut out, arg.as_bytes())?;
                    }
                }
                Opt::Fatal => {
                    out.flush()?;
                    return Ok(ExitCode::from(128));
                }
            }
            continue;
        }

        if as_is {
            if o.echo_paths {
                emit(&mut out, arg.as_bytes())?;
            }
            if !is_worktree_path(&repo, arg) {
                out.flush()?;
                eprintln!(
                    "fatal: {arg}: no such path in the working tree.\n\
                     Use 'git <command> -- <path>...' to specify paths that do not exist locally."
                );
                return Ok(ExitCode::from(128));
            }
            continue;
        }

        // An empty argument is never a revision and never a path, even though
        // joining it onto the worktree root would name the root itself.
        let resolved = if arg.is_empty() {
            None
        } else {
            warn_ambiguous_refname(&repo, arg, o.quiet);
            repo.rev_parse_single(arg.as_str()).ok().map(|id| id.detach())
        };

        // A reflog ordinal past the end of an existing ref's log is its own
        // `die()` inside `get_oid()`, ahead of any path interpretation — nothing
        // has been echoed yet at this point, which is what keeps stdout empty.
        if resolved.is_none() {
            if let Some((name, count)) = reflog_overflow(&repo, arg) {
                out.flush()?;
                eprintln!("fatal: log for '{name}' only has {count} entries");
                return Ok(ExitCode::from(128));
            }
        }

        match resolved {
            Some(id) => {
                if o.verify {
                    revs += 1;
                    held = Some((id, BString::from(arg.as_bytes()), false));
                } else {
                    show_rev(&mut out, &repo, &o, &id, Some(arg.as_bytes().as_bstr()), None, false)?;
                }
            }
            None => {
                // A multi-object revspec that `rev_parse_single` cannot resolve
                // lands here: a range (`a..b`, `a...b`), an exclude (`^rev`), or a
                // single revision that only the full grammar accepts (`Include`).
                // The full parser (`rev_parse`) classifies it; expand at this
                // position before falling through to path handling.
                let parsed = if arg.is_empty() {
                    None
                } else {
                    repo.rev_parse(arg.as_str()).ok().and_then(|s| match s.detach() {
                        gix::revision::plumbing::Spec::Range { from, to } => {
                            Some(Parsed::Range(RangeSpec::Range { from, to }))
                        }
                        gix::revision::plumbing::Spec::Merge { theirs, ours } => {
                            Some(Parsed::Range(RangeSpec::Merge { theirs, ours }))
                        }
                        // `^rev`: git's "reversed" single revision, prints `^<id>`.
                        gix::revision::plumbing::Spec::Exclude(id) => {
                            Some(Parsed::Single { id, reversed: true })
                        }
                        // A single revision the single-object parser missed.
                        gix::revision::plumbing::Spec::Include(id) => {
                            Some(Parsed::Single { id, reversed: false })
                        }
                        // `a^@`/`a^!` expand to a variable number of parents; not
                        // implemented, so fall through to path handling as before.
                        _ => None,
                    })
                };
                match parsed {
                    Some(Parsed::Range(range)) => {
                        emit_range(&mut out, &repo, &o, range)?;
                        // A range is never a single revision. Under
                        // `--verify`/`--short` the endpoints still print, but the
                        // scan then fails afterward with "Needed a single revision".
                        if o.verify {
                            revs += 2;
                        }
                        continue;
                    }
                    Some(Parsed::Single { id, reversed }) => {
                        // The name for `--symbolic` echo is the text after any `^`.
                        let name = if reversed {
                            arg.strip_prefix('^').unwrap_or(arg.as_str())
                        } else {
                            arg.as_str()
                        };
                        if o.verify {
                            revs += 1;
                            held = Some((id, BString::from(name.as_bytes()), reversed));
                        } else {
                            show_rev(
                                &mut out,
                                &repo,
                                &o,
                                &id,
                                Some(name.as_bytes().as_bstr()),
                                None,
                                reversed,
                            )?;
                        }
                        continue;
                    }
                    None => {}
                }

                if o.verify {
                    out.flush()?;
                    return Ok(die_single(o.quiet));
                }
                as_is = true;
                if o.echo_paths {
                    emit(&mut out, arg.as_bytes())?;
                }
                if !is_worktree_path(&repo, arg) {
                    out.flush()?;
                    eprintln!(
                        "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
                         Use '--' to separate paths from revisions, like this:\n\
                         'git <command> [<revision>...] -- [<file>...]'"
                    );
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    if o.verify {
        match held {
            Some((id, name, reversed)) if revs == 1 => {
                show_rev(&mut out, &repo, &o, &id, Some(name.as_bstr()), None, reversed)?;
            }
            _ => {
                out.flush()?;
                return Ok(die_single(o.quiet));
            }
        }
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Port of `ref_rev_parse_rules` + `repo_dwim_ref()`: every full ref name `name`
/// resolves to, in git's rule order, with symbolic refs already followed (which
/// is what `refs_resolve_ref_unsafe()` returns). The first element is the ref git
/// treats as *the* match; the length is git's `refs_found`.
pub(crate) fn dwim_ref_matches(repo: &gix::Repository, name: &str) -> Vec<String> {
    let mut found = Vec::new();
    for rule in [
        name.to_owned(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ] {
        let Ok(Some(r)) = repo.try_find_reference(rule.as_str()) else {
            continue;
        };
        // git resolves each rule with `refs_resolve_ref_unsafe()`, an *exact*
        // lookup, so `refs_found` counts distinct existing refnames. gitoxide's
        // `try_find` instead applies a DWIM of its own over `refs/`,
        // `refs/tags/`, `refs/heads/` and `refs/remotes/`, so several rules would
        // otherwise report the same underlying ref and inflate the count — which
        // would make a plain `git rev-parse main` look ambiguous. Keep only the
        // candidates that named the very ref they found.
        if r.name().as_bstr() != rule.as_bytes() {
            continue;
        }
        // `repo_dwim_ref` records what `resolve_ref_unsafe` resolved *to*, so a
        // symbolic `HEAD` contributes the branch it points at.
        found.push(match r.target() {
            TargetRef::Symbolic(full) => full.as_bstr().to_string(),
            TargetRef::Object(_) => r.name().as_bstr().to_string(),
        });
    }
    found
}

/// `get_oid_basic`'s ambiguity warning (`object-name.c`): once a plain name has
/// resolved as a ref, warn when `core.warnAmbiguousRefs` (default true) is on and
/// either more than one of the rev-parse rules matched, or the name *also* reads
/// as an unambiguous abbreviated object id — the `refs_found > 1 ||
/// !get_short_oid(…)` disjunction. `--quiet` is git's `GET_OID_QUIETLY`, which
/// suppresses it.
///
/// Only a bare name goes through `repo_dwim_ref`, so anything carrying revision
/// grammar (`~`, `^`, `:`, `@{`) is left alone here, as in git.
pub(crate) fn warn_ambiguous_refname(repo: &gix::Repository, arg: &str, quiet: bool) {
    if arg.contains(['~', '^', ':']) || arg.contains("@{") {
        return;
    }
    if repo.config_snapshot().boolean("core.warnAmbiguousRefs") == Some(false) {
        return;
    }

    // `get_oid_basic` splits in two. A *full-length* hex name resolves as the
    // object and returns immediately, but first checks whether a ref answers to
    // the same 40 (or 64) characters — which only happens when a ref was created
    // by accident, hence the paragraph explaining how.
    let hexsz = repo.object_hash().len_in_hex();
    if arg.len() == hexsz && arg.bytes().all(|b| b.is_ascii_hexdigit()) {
        if !dwim_ref_matches(repo, arg).is_empty() {
            eprintln!("warning: refname '{arg}' is ambiguous.");
            // git prints this one with a bare `fprintf(stderr, "%s\n", …)`, not
            // through `advise()`, so it carries no `hint: ` prefix and no color.
            if Advice::ObjectNameWarning.enabled_in(repo) {
                eprintln!("{OBJECT_NAME_MSG}");
            }
        }
        return;
    }

    if quiet {
        return;
    }
    let refs_found = dwim_ref_matches(repo, arg).len();
    if refs_found == 0 {
        return;
    }
    if refs_found > 1 || resolves_as_short_oid(repo, arg) {
        eprintln!("warning: refname '{arg}' is ambiguous.");
    }
}

/// `object_name_msg` in `object-name.c`, verbatim (git 2.55.0).
const OBJECT_NAME_MSG: &str = "\
Git normally never creates a ref that ends with 40 hex characters
because it will be ignored when you just specify 40-hex. These refs
may be created by mistake. For example,

  git switch -c $br $(git rev-parse ...)

where \"$br\" is somehow empty and a 40-hex ref is created. Please
examine these refs and maybe delete them. Turn this message off by
running \"git config set advice.objectNameWarning false\"";

/// `get_short_oid(…, GET_OID_QUIETLY) == 0`: whether `arg` is a hex prefix that
/// names exactly one object. git's minimum abbreviation is four hex digits.
fn resolves_as_short_oid(repo: &gix::Repository, arg: &str) -> bool {
    if arg.len() < 4 || !arg.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let Ok(prefix) = gix::hash::Prefix::from_hex(arg) else {
        return false;
    };
    // No candidate set: git only needs "does this name exactly one object", and
    // the early-abort form answers that — `Ok(Some(Err(())))` is the ambiguous
    // case, which `get_short_oid` also treats as a failure to resolve.
    matches!(repo.objects.lookup_prefix(prefix, None), Ok(Some(Ok(_))))
}

/// `die_no_single_rev` in stock git: silent exit 1 under `--quiet`, else fatal.
fn die_single(quiet: bool) -> ExitCode {
    if quiet {
        ExitCode::from(1)
    } else {
        eprintln!("fatal: Needed a single revision");
        ExitCode::from(128)
    }
}

/// What a recognized option asks the scan to do next.
enum Opt {
    /// Pure state change.
    Consumed,
    Query(Query),
    Refs(RefSet),
    /// Not an option stock git knows; git echoes these.
    Unknown,
    /// git `die()`d on the option's value: the message is already on stderr and the
    /// scan stops with git's fatal exit code.
    Fatal,
}

#[derive(Clone, Copy)]
enum Query {
    GitDir,
    /// `--absolute-git-dir`: the same directory, always symlink-resolved and absolute.
    AbsoluteGitDir,
    /// `--git-common-dir`: `$GIT_COMMON_DIR` — the git directory a linked worktree shares with
    /// the main one, which is the git directory itself when there is no `commondir` file.
    GitCommonDir,
    ShowToplevel,
    /// `--show-prefix`: the path from the top of the work tree down to the cwd, slash-terminated.
    ShowPrefix,
    /// `--show-cdup`: the `../` sequence that climbs from the cwd back to the top of the work tree.
    ShowCdup,
    IsInsideWorkTree,
    /// `--is-inside-git-dir`: whether the cwd is the git directory or below it.
    IsInsideGitDir,
    IsBareRepository,
    /// `--show-object-format[=(storage|input|output)]`: the hash algorithm's name.
    /// All three modes read the same algorithm here — this port stores, reads and
    /// writes one hash, so there is no compatibility algorithm to differ from.
    ObjectFormat,
    /// `--show-ref-format`: how refs are stored, which is always the loose-plus-
    /// packed `files` backend; `reftable` is not implemented.
    RefFormat,
}

fn option(o: &mut Opts, arg: &str) -> Result<Opt> {
    if UNIMPLEMENTED_EXACT.contains(&arg) || UNIMPLEMENTED_PREFIX.iter().any(|p| arg.starts_with(p)) {
        anyhow::bail!("{arg} is not ported yet");
    }

    match arg {
        "--verify" => {
            o.verify = true;
            o.echo_flags = false;
            o.echo_paths = false;
        }
        "-q" | "--quiet" => o.quiet = true,
        "--short" => {
            // `--short` implies `--verify` in stock git; that is where the
            // otherwise surprising `fatal: Needed a single revision` comes from
            // for invocations like `rev-parse --short --git-dir`.
            o.verify = true;
            o.echo_flags = false;
            o.echo_paths = false;
            o.abbrev = Some(0);
        }
        "--symbolic" => o.sym = Sym::AsIs,
        "--symbolic-full-name" => o.sym = Sym::Full,
        "--abbrev-ref" => o.abbrev_ref = true,
        "--git-dir" => return Ok(Opt::Query(Query::GitDir)),
        "--absolute-git-dir" => return Ok(Opt::Query(Query::AbsoluteGitDir)),
        "--git-common-dir" => return Ok(Opt::Query(Query::GitCommonDir)),
        "--show-toplevel" => return Ok(Opt::Query(Query::ShowToplevel)),
        "--show-prefix" => return Ok(Opt::Query(Query::ShowPrefix)),
        "--show-cdup" => return Ok(Opt::Query(Query::ShowCdup)),
        "--is-inside-work-tree" => return Ok(Opt::Query(Query::IsInsideWorkTree)),
        "--is-inside-git-dir" => return Ok(Opt::Query(Query::IsInsideGitDir)),
        "--is-bare-repository" => return Ok(Opt::Query(Query::IsBareRepository)),
        "--show-object-format" => return Ok(Opt::Query(Query::ObjectFormat)),
        "--show-ref-format" => return Ok(Opt::Query(Query::RefFormat)),
        "--all" => return Ok(Opt::Refs(RefSet::All)),
        "--branches" => return Ok(Opt::Refs(RefSet::Branches)),
        "--tags" => return Ok(Opt::Refs(RefSet::Tags)),
        _ => {
            // `--show-object-format=<mode>`: git names three, and rejects anything
            // else before it looks at the repository.
            if let Some(mode) = arg.strip_prefix("--show-object-format=") {
                if !matches!(mode, "storage" | "input" | "output") {
                    eprintln!("fatal: unknown mode for --show-object-format: {mode}");
                    return Ok(Opt::Fatal);
                }
                return Ok(Opt::Query(Query::ObjectFormat));
            }
            if let Some(n) = arg.strip_prefix("--short=") {
                let n: usize = n
                    .parse()
                    .map_err(|_| anyhow::anyhow!("{arg} is not a valid abbreviation length"))?;
                o.verify = true;
                o.echo_flags = false;
                o.echo_paths = false;
                o.abbrev = Some(n.max(1));
            } else {
                return Ok(Opt::Unknown);
            }
        }
    }
    Ok(Opt::Consumed)
}

/// Repository queries that print at their position in the scan. Returns an exit
/// code when the query cannot be answered the way git would answer it.
fn query(out: &mut impl Write, repo: &gix::Repository, q: Query) -> Result<Option<ExitCode>> {
    match q {
        Query::GitDir => {
            // git prints `$GIT_DIR` verbatim when set. Otherwise the value is whatever
            // `setup.c` left in it:
            //
            // * `.git`, its default, which `setup_discovered_git_dir()` leaves alone when the
            //   `.git` it found is a plain directory at the top of the work tree we stand in.
            //   A `.git` *file* — a linked worktree or a submodule checkout — is resolved and
            //   set outright, so those print the absolute private git dir even at the top.
            // * `.` from `setup_bare_git_dir()` when the cwd *is* the git directory.
            // * the absolute path in every other case.
            emit(out, git_dir_display(repo).as_os_str().as_encoded_bytes())?;
        }
        Query::AbsoluteGitDir => emit(out, absolute(repo.git_dir()).as_os_str().as_encoded_bytes())?,
        Query::GitCommonDir => {
            // `print_path(…, DEFAULT_RELATIVE_IF_SHARED)`: relative to the prefix when there is
            // one, and otherwise the stored value as-is — which is the same string `--git-dir`
            // prints whenever there is no separate common directory.
            let common = absolute(repo.common_dir());
            match prefix(repo) {
                Some(pfx) => {
                    let up: std::path::PathBuf = pfx.components().map(|_| "..").collect();
                    emit(out, up.join(&common).as_os_str().as_encoded_bytes())?;
                }
                None if common == absolute(repo.git_dir()) => {
                    emit(out, git_dir_display(repo).as_os_str().as_encoded_bytes())?;
                }
                None => emit(out, common.as_os_str().as_encoded_bytes())?,
            }
        }
        Query::ShowToplevel => match toplevel(repo) {
            Some(top) => emit(out, top.as_os_str().as_encoded_bytes())?,
            None => {
                out.flush()?;
                eprintln!("fatal: this operation must be run in a work tree");
                return Ok(Some(ExitCode::from(128)));
            }
        },
        Query::ShowPrefix => match prefix(repo) {
            // git's prefix is slash-terminated; without one it still prints the empty line.
            Some(pfx) => emit(out, format!("{}/", pfx.display()).as_bytes())?,
            None => emit(out, b"")?,
        },
        Query::ShowCdup => {
            // Outside the work tree git prints the work tree itself rather than a `../` climb —
            // and where there is no work tree at all it prints *nothing*, not even the newline
            // every other query ends with. `builtin/rev-parse.c` reaches its `putchar('\n')`
            // only through the `is_inside_work_tree()` branch; the other one is
            //
            //     if (!is_inside_work_tree(the_repository)) {
            //             const char *work_tree = repo_get_work_tree(the_repository);
            //             if (work_tree)
            //                     printf("%s\n", work_tree);
            //             continue;
            //     }
            //
            // so a missing work tree writes zero bytes and still exits 0. That is what happens
            // inside a `.git` directory, inside a linked worktree's administrative directory and
            // in a bare repository.
            if !is_inside_work_tree(repo) {
                if let Some(top) = toplevel(repo) {
                    emit(out, top.as_os_str().as_encoded_bytes())?;
                }
            } else {
                let up: String = prefix(repo).map_or_else(String::new, |pfx| {
                    pfx.components().map(|_| "../").collect()
                });
                emit(out, up.as_bytes())?;
            }
        }
        Query::IsInsideWorkTree => emit(out, yes_no(is_inside_work_tree(repo)))?,
        Query::IsInsideGitDir => emit(out, yes_no(is_inside_git_dir(repo)))?,
        Query::IsBareRepository => emit(out, yes_no(repo.is_bare()))?,
        Query::ObjectFormat => emit(out, repo.object_hash().to_string().as_bytes())?,
        Query::RefFormat => emit(out, b"files")?,
    }
    Ok(None)
}

fn yes_no(b: bool) -> &'static [u8] {
    if b {
        b"true"
    } else {
        b"false"
    }
}

/// The worktree root as git reports it: symlink-resolved, absolute.
fn toplevel(repo: &gix::Repository) -> Option<std::path::PathBuf> {
    let wd = repo.workdir()?;
    std::fs::canonicalize(wd).ok()
}

/// `path` symlink-resolved and absolute, or unchanged when it cannot be resolved.
fn absolute(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// The git directory the way `--git-dir` prints it, which is the string `setup.c` left in
/// `$GIT_DIR`: the environment value verbatim when it was given, `.git` when discovery found a
/// plain `.git` directory at the top of the work tree we stand in, `.` when the cwd *is* the git
/// directory, and the absolute path otherwise.
fn git_dir_display(repo: &gix::Repository) -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("GIT_DIR") {
        return dir.into();
    }
    let git_dir = absolute(repo.git_dir());
    match toplevel(repo) {
        Some(top) if is_cwd(&top) && git_dir == top.join(".git") => ".git".into(),
        _ if is_cwd(&git_dir) => ".".into(),
        _ => git_dir,
    }
}

/// git's `prefix`: the path from the top of the work tree down to the cwd, or `None` when there is
/// no work tree or the cwd sits outside of it.
fn prefix(repo: &gix::Repository) -> Option<std::path::PathBuf> {
    let top = toplevel(repo)?;
    let cwd = std::env::current_dir().ok().and_then(|c| std::fs::canonicalize(c).ok())?;
    let rel = cwd.strip_prefix(&top).ok()?;
    (!rel.as_os_str().is_empty()).then(|| rel.to_owned())
}

/// git's `is_inside_git_dir()`: whether the cwd is the git directory or below it.
fn is_inside_git_dir(repo: &gix::Repository) -> bool {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| std::fs::canonicalize(cwd).ok())
        .is_some_and(|cwd| cwd.starts_with(absolute(repo.git_dir())))
}

/// git's `is_inside_work_tree()`: `is_inside_dir(repo_get_work_tree())` — purely whether the cwd
/// sits in the work tree, which says nothing about the git directory. Both this and
/// [`is_inside_git_dir`] are true at once for `GIT_DIR=.` inside a `.git` directory, because that
/// setup makes the git directory its own work tree.
fn is_inside_work_tree(repo: &gix::Repository) -> bool {
    let Some(top) = toplevel(repo) else { return false };
    std::env::current_dir()
        .ok()
        .and_then(|cwd| std::fs::canonicalize(cwd).ok())
        .is_some_and(|cwd| cwd.starts_with(top))
}

fn is_cwd(dir: &std::path::Path) -> bool {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| std::fs::canonicalize(cwd).ok())
        .is_some_and(|cwd| cwd == dir)
}

fn is_worktree_path(repo: &gix::Repository, arg: &str) -> bool {
    if arg.is_empty() {
        return false;
    }
    repo.workdir()
        .map(|wd| wd.join(arg))
        .is_some_and(|p| p.symlink_metadata().is_ok())
}

/// Render one resolved revision.
///
/// `name` is the text the revision came in as — the command-line argument for a
/// positional, or the ref name the listing walk produced. `known_full` is the
/// full ref name when the caller already knows it (the listing walk does), which
/// spares a lookup and is the only way a listing entry can be shortened.
fn show_rev(
    out: &mut impl Write,
    repo: &gix::Repository,
    o: &Opts,
    id: &ObjectId,
    name: Option<&BStr>,
    known_full: Option<&BStr>,
    reversed: bool,
) -> Result<()> {
    // Build the rendered text without the newline first. `None` means "print
    // nothing" — and for a `^rev` exclude the `^` is suppressed along with it,
    // which is why `rev-parse --abbrev-ref ^HEAD~1` prints an empty result
    // rather than a bare `^`.
    let payload: Option<Vec<u8>> = if o.abbrev_ref || o.sym == Sym::Full {
        let full = match known_full {
            Some(f) => Some(f.to_owned()),
            None => name.and_then(|n| dwim_full_name(repo, n)),
        };
        // A revision that names no ref prints nothing at all in these modes.
        full.map(|full| {
            if o.abbrev_ref {
                <&gix::refs::FullNameRef>::try_from(full.as_bstr())
                    .map(|f| f.shorten().to_owned())
                    .unwrap_or_else(|_| full.clone())
                    .into()
            } else {
                full.into()
            }
        })
    } else if o.sym == Sym::AsIs {
        // `--symbolic` echoes the input name; with no name, fall back to the id.
        Some(match name {
            Some(n) => n.to_vec(),
            None => render_id(repo, o, id)?,
        })
    } else {
        Some(render_id(repo, o, id)?)
    };

    if let Some(p) = payload {
        if reversed {
            let mut buf = Vec::with_capacity(p.len() + 1);
            buf.push(b'^');
            buf.extend_from_slice(&p);
            emit(out, &buf)?;
        } else {
            emit(out, &p)?;
        }
    }
    Ok(())
}

/// Render an object id to the hex bytes that current option state calls for:
/// full hex, `core.abbrev`/auto-length (`Some(0)`), or an `n`-char disambiguated
/// prefix. Shared by positional revisions and range endpoints.
fn render_id(repo: &gix::Repository, o: &Opts, id: &ObjectId) -> Result<Vec<u8>> {
    Ok(match o.abbrev {
        None => id.to_string().into_bytes(),
        Some(0) => id.attach(repo).shorten()?.to_string().into_bytes(),
        Some(n) => {
            let n = n.clamp(4, id.kind().len_in_hex());
            let candidate = gix::odb::store::prefix::disambiguate::Candidate::new(*id, n)?;
            match repo.objects.disambiguate_prefix(candidate)? {
                Some(prefix) => prefix.to_string().into_bytes(),
                None => id.to_string().into_bytes(),
            }
        }
    })
}

/// Emit `^<id>` for the excluded side of a range.
fn emit_exclude(out: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
    out.write_all(b"^")?;
    out.write_all(bytes)?;
    out.write_all(b"\n")
}

/// Expand a range revspec at its position, matching stock git's line order.
///
/// `a..b` prints `b` then `^a`; `a...b` prints `b`, `a`, then `^<merge-base>` for
/// each merge base between the two sides (none is printed when the histories are
/// unrelated). Endpoints honor the current abbreviation state.
fn emit_range(
    out: &mut impl Write,
    repo: &gix::Repository,
    o: &Opts,
    range: RangeSpec,
) -> Result<()> {
    match range {
        RangeSpec::Range { from, to } => {
            emit(out, render_id(repo, o, &to)?)?;
            emit_exclude(out, &render_id(repo, o, &from)?)?;
        }
        RangeSpec::Merge { theirs, ours } => {
            emit(out, render_id(repo, o, &ours)?)?;
            emit(out, render_id(repo, o, &theirs)?)?;
            let bases = repo
                .merge_bases_many(theirs, &[ours])
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            for base in bases {
                emit_exclude(out, &render_id(repo, o, &base.detach())?)?;
            }
        }
    }
    Ok(())
}

/// `get_oid_basic()`'s reflog branch: `<ref>@{<n>}` whose `<n>` is past the end
/// of that ref's reflog is `die("log for '%s' only has %d entries")` (exit 128),
/// not an unknown revision — the ref resolved fine, only the ordinal did not.
///
/// Returns the name git puts in the message and the entry count it reports.
/// `None` for anything that is not that shape: a non-numeric `@{...}` is a date
/// or tracking spec, and a `<ref>` that does not exist or has no reflog falls
/// through to the ordinary `ambiguous argument` path.
///
/// An empty `<ref>` (`@{2}`) means the current branch, which is why git answers
/// `git rev-parse @{999}` with `log for 'main' …` rather than `log for '@' …`.
fn reflog_overflow(repo: &gix::Repository, spec: &str) -> Option<(String, usize)> {
    let rest = spec.strip_suffix('}')?;
    let (name, ordinal) = rest.rsplit_once("@{")?;
    if ordinal.is_empty() || !ordinal.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let nth: usize = ordinal.parse().ok()?;
    let name = if name.is_empty() || name == "@" {
        repo.head_name().ok()??.shorten().to_string()
    } else {
        name.to_string()
    };
    let mut reference = repo.find_reference(name.as_str()).ok()?;
    let mut platform = reference.log_iter();
    let count = platform.all().ok()??.count();
    (nth >= count).then_some((name, count))
}

/// git's `dwim_ref`: resolve a bare name to the full ref it designates, then
/// follow symbolic refs so `HEAD` reports the branch it is on. `None` when the
/// name is not a ref at all — which is how `HEAD^` prints nothing under
/// `--symbolic-full-name`.
fn dwim_full_name(repo: &gix::Repository, name: &BStr) -> Option<BString> {
    let name = name.to_str().ok()?;
    // `@{-N}`: `interpret_nth_prior_checkout()` rewrites the spec to the branch
    // that many checkouts ago, and everything after it applies to that name.
    if let Some((nth, used)) = super::check_ref_format::parse_nth_prior(name.as_bytes()) {
        let mut branch = super::check_ref_format::nth_branch_switch(repo, nth)?;
        branch.extend_from_slice(&name.as_bytes()[used..]);
        return dwim_full_name(repo, BStr::new(&branch));
    }
    // `@{u}`/`@{upstream}`/`@{push}`: git's `interpret_branch_name()` resolves
    // these through the branch's configured remote and records the ref it landed
    // on, which is what `--symbolic-full-name` reports. Without this the whole
    // spec looks like "not a ref" and prints nothing at all.
    if let Some((base, direction)) = split_tracking_suffix(name) {
        let branch = if base.is_empty() {
            repo.head_name().ok()??
        } else {
            let full = repo.find_reference(base).ok()?.name().to_owned();
            full
        };
        return repo
            .branch_remote_tracking_ref_name(branch.as_ref(), direction)
            .and_then(std::result::Result::ok)
            .map(|r| r.as_bstr().to_owned());
    }
    if name == "HEAD" || name == "@" {
        return Some(match repo.head_name().ok()? {
            Some(referent) => referent.as_bstr().to_owned(),
            // Detached: HEAD designates itself.
            None => BString::from("HEAD"),
        });
    }

    let mut current = repo.find_reference(name).ok()?;
    for _ in 0..16 {
        let next = match current.target() {
            TargetRef::Object(_) => return Some(current.name().as_bstr().to_owned()),
            TargetRef::Symbolic(target) => target.as_bstr().to_owned(),
        };
        current = repo.find_reference(next.as_bstr()).ok()?;
    }
    None
}

/// Split `<branch>@{upstream}` / `@{push}` into the branch part (empty for the
/// current one) and the direction the suffix asks about.
///
/// git accepts `@{u}` and `@{upstream}` case-insensitively, and `@{push}` the
/// same way; anything else after `@{` is a reflog or date spec, which names no
/// ref of its own.
fn split_tracking_suffix(name: &str) -> Option<(&str, gix::remote::Direction)> {
    let at = name.rfind("@{")?;
    if !name.ends_with('}') {
        return None;
    }
    let suffix = &name[at + 2..name.len() - 1];
    let direction = if suffix.eq_ignore_ascii_case("u") || suffix.eq_ignore_ascii_case("upstream") {
        gix::remote::Direction::Fetch
    } else if suffix.eq_ignore_ascii_case("push") {
        gix::remote::Direction::Push
    } else {
        return None;
    };
    Some((&name[..at], direction))
}

/// Walk a ref namespace the way `--all`/`--branches`/`--tags` do.
///
/// Entries are ordered by full ref name and are *not* peeled: `--tags` reports
/// an annotated tag's own object id, matching stock git.
fn collect_refs(repo: &gix::Repository, which: RefSet) -> Result<Vec<(BString, BString, ObjectId)>> {
    let platform = repo.references()?;
    let iter = match which {
        RefSet::All => platform.all()?,
        RefSet::Branches => platform.local_branches()?,
        RefSet::Tags => platform.tags()?,
    };

    let mut refs = Vec::new();
    for reference in iter {
        let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
        let full = reference.name().as_bstr().to_owned();
        // `--all` hands the callback a full name; the narrowed walks hand it the
        // name with its namespace stripped. `--symbolic` echoes exactly that.
        let echo = match which {
            RefSet::All => full.clone(),
            RefSet::Branches | RefSet::Tags => reference.name().shorten().to_owned(),
        };
        let Some(id) = ref_target(repo, &reference) else {
            continue;
        };
        refs.push((echo, full, id));
    }
    refs.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(refs)
}

/// The object a ref points at, following symbolic refs but never peeling tags.
///
/// Deliberately avoids `Reference::id()`, which panics on a symbolic target.
fn ref_target(repo: &gix::Repository, reference: &gix::Reference<'_>) -> Option<ObjectId> {
    let mut current = match reference.target() {
        TargetRef::Object(id) => return Some(id.to_owned()),
        TargetRef::Symbolic(target) => target.as_bstr().to_owned(),
    };
    for _ in 0..16 {
        let next = repo.find_reference(current.as_bstr()).ok()?;
        match next.target() {
            TargetRef::Object(id) => return Some(id.to_owned()),
            TargetRef::Symbolic(target) => current = target.as_bstr().to_owned(),
        }
    }
    None
}

/// Ref names and paths are bytes, not necessarily UTF-8, so output goes out raw.
fn emit(out: &mut impl Write, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    out.write_all(bytes.as_ref())?;
    out.write_all(b"\n")
}
