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
//! The parent shorthands expand at their position too, through
//! `try_parent_shorthands()` (`builtin/rev-parse.c:328-390`) rather than the
//! object-name parser: `<rev>^!` prints the rev and `^<parent>` per parent,
//! `<rev>^@` prints the parents alone, and `<rev>^-[<n>]` prints the rev and the
//! one selected parent. They run *after* the range split and before the single
//! name is resolved, they are not gated on `--verify`, and the `<n>` is
//! `strtoul` here rather than the walk's `strtol_i`. An operand that still
//! carries one of the three marks when the ordinary resolution is reached cannot
//! resolve at all — `get_oid_1()` has no case for them.
//!
//! Implemented: `--verify`, `-q`/`--quiet`, `--short[=n]`,
//! `--abbrev-ref[=(strict|loose)]`, `--symbolic`, `--symbolic-full-name`,
//! `--git-dir`, `--absolute-git-dir`, `--git-common-dir`, `--show-toplevel`,
//! `--is-inside-work-tree`, `--is-inside-git-dir`, `--is-bare-repository`,
//! `--show-cdup`, `--show-prefix`, `--show-object-format[=<mode>]`,
//! `--show-ref-format`, `--all`, `--branches[=<glob>]`, `--tags[=<glob>]`,
//! `--remotes[=<glob>]`, `--glob=<glob>`, `--exclude=<pattern>`, plus revision
//! and path arguments.
//!
//! The ref-set family goes through the same
//! [`crate::porcelain::log::RefSelection`] the revision walkers use, which is
//! `refs_for_each_ref_ext()`'s rule: the pattern is matched against the *whole*
//! refname with `wildmatch(…, 0)`, the namespace is trimmed afterwards, and
//! `--exclude` is tested against the trimmed name. `handle_ref_opt()` clears the
//! exclusion list once the walk that consumed it is done.
//!
//! Rejected with an explicit refusal rather than silently ignored — the list is
//! [`UNIMPLEMENTED_EXACT`] and [`UNIMPLEMENTED_PREFIX`], and it includes
//! `--is-shallow-repository`, `--disambiguate=<prefix>` and `--sq-quote`.
//! Options git does *not* recognize are echoed, which is what git itself does
//! with them.

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

/// The `--exclude=<pattern>` list `handle_ref_opt()` consumes and then clears.

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
    /// `abbrev_ref_strict` (`builtin/rev-parse.c:54`). `None` is git's default,
    /// `repo_settings_get_warn_ambiguous_refs()`; `--abbrev-ref=strict` and
    /// `--abbrev-ref=loose` pin it (`builtin/rev-parse.c:917-930`).
    abbrev_ref_strict: Option<bool>,
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
            abbrev_ref_strict: None,
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
    "--bisect",
    "--end-of-options",
    "--all-objects",
];

const UNIMPLEMENTED_PREFIX: &[&str] = &[
    "--path-format=",
    "--disambiguate=",
    "--exclude-hidden=",
    "--since=",
    "--after=",
    "--until=",
    "--before=",
    "--default=",
    "--prefix=",
    "--git-path=",
];

pub fn rev_parse(args: &[String]) -> Result<ExitCode> {
    // `show_usage_if_asked(argc, argv, builtin_rev_parse_usage)`
    // (builtin/rev-parse.c:723) is the first statement of `cmd_rev_parse`: a
    // lone `-h` goes to stdout at 129, before the repository is opened. `-h`
    // anywhere else is not help — rev-parse has no parse-options table, so it
    // falls through to the ordinary argument handling.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }

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
    // `ref_excludes` in `builtin/rev-parse.c`: `--exclude=<pattern>` accumulates
    // here and the next ref walk both applies and clears it.
    let mut ref_excludes: Vec<String> = Vec::new();
    // git's `has_dashdash`, decided by a scan of the whole argument vector before
    // the loop starts (`builtin/rev-parse.c:717-722`):
    //
    // ```c
    // for (i = 1; i < argc; i++) {
    //         if (!strcmp(argv[i], "--")) {
    //                 has_dashdash = 1;
    //                 break;
    //         }
    // }
    // ```
    //
    // It is not positional: a separator anywhere makes an operand *in front of
    // it* revision-only, so `git rev-parse main^{blob} --` dies with
    // `bad revision` before echoing anything, where the same operand without a
    // separator is echoed and then diagnosed as a path.
    let has_dashdash = args.iter().any(|a| a == "--");

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
                Opt::Exclude(pattern) => ref_excludes.push(pattern),
                Opt::Refs(kind, pattern) => {
                    // `handle_ref_opt()` ends in `clear_ref_exclusions()`, and so
                    // does the `--all` branch: the exclusion list lives only until
                    // the next ref walk. `git rev-parse --exclude=side --branches
                    // --branches` therefore prints `main main side`.
                    let selection = crate::porcelain::log::RefSelection::new(
                        0,
                        kind,
                        pattern.as_deref(),
                        std::mem::take(&mut ref_excludes),
                        false,
                    );
                    for (echo, full, id) in collect_refs(&repo, &selection)? {
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

        // ```c
        // /* Not a flag argument */
        // if (try_difference(arg))
        //         continue;
        // if (try_parent_shorthands(arg))
        //         continue;
        // ```
        //
        // (`builtin/rev-parse.c:1158-1162`.) The parent shorthands are decided
        // *before* the operand is resolved as a single name, so a name they claim
        // never reaches `repo_get_oid_with_flags()` at all.
        if try_parent_shorthands(&mut out, &repo, &o, arg)? {
            continue;
        }

        // An empty argument is never a revision and never a path, even though
        // joining it onto the worktree root would name the root itself.
        let resolved = if arg.is_empty() {
            None
        } else {
            // `try_difference()` runs ahead of the plain resolution and resolves
            // each endpoint with `repo_get_oid_committish()`, joined by `&&` — so
            // a range warns once per endpoint, and not at all for the second one
            // when the first failed to resolve.
            match crate::objname::split_range(arg) {
                Some(range) => {
                    warn_ambiguous_refname(&repo, range.a, o.quiet);
                    warn_reflog_reach(&mut out, &repo, range.a, o.quiet)?;
                    // The endpoint has already been warned about on the line
                    // above; this is the same resolution, not a second operand.
                    let a_resolved = crate::objname::resolve_quiet(&repo, range.a).is_some();
                    if a_resolved {
                        warn_ambiguous_refname(&repo, range.b, o.quiet);
                        warn_reflog_reach(&mut out, &repo, range.b, o.quiet)?;
                    }
                }
                None => {
                    // `cmd_rev_parse()` advances past the exclusion mark before
                    // it resolves (`builtin/rev-parse.c:1165-1169`):
                    //
                    // ```c
                    // name = arg;
                    // type = NORMAL;
                    // if (*arg == '^') { name++; type = REVERSED; }
                    // if (!repo_get_oid_with_flags(the_repository, name, &oid, flags)) {
                    // ```
                    //
                    // so `get_oid_basic()` measures the name without the caret
                    // and both of its warnings are due for `^<40-hex-ref>` and
                    // `^<ref>@{<date>}`. The range endpoints above keep theirs:
                    // `try_difference()` cuts at the `..` and hands
                    // `repo_get_oid_committish()` the endpoint as written.
                    let name = crate::objname::uninteresting_mark(arg).0;
                    // `interpret_branch_mark()`'s `die()` happens inside
                    // `get_oid()`, before rev-parse has a failed operand to
                    // report, so it replaces the "ambiguous argument" block rather
                    // than preceding it (`refs.c`, via `substitute_branch_name()`).
                    if let Some(message) = crate::objname::upstream_mark_fatal(&repo, name) {
                        out.flush()?;
                        eprintln!("fatal: {message}");
                        return Ok(ExitCode::from(128));
                    }
                    warn_ambiguous_refname(&repo, name, o.quiet);
                    warn_reflog_reach(&mut out, &repo, name, o.quiet)?;
                }
            }
            // A full-length hex name *is* the object id and short-circuits ahead
            // of every database lookup, so it answers even for an object that is
            // not present — see [`crate::objname::full_hex`].
            crate::objname::full_hex(&repo, arg).or_else(|| {
                // `get_oid_basic()` resolves `<ref>@{<n>}` itself, through
                // `repo_dwim_log()` rather than gitoxide's ref lookup — which
                // answers for names git rejects and rejects names git answers.
                if crate::objname::is_reflog_operand(arg) {
                    crate::objname::reflog_oid(&repo, arg)
                } else if carries_walk_mark(arg) {
                    None
                } else {
                    repo
                        .rev_parse_single(crate::objname::canonical_spec(&repo, arg).as_ref())
                        .ok()
                        .map(|id| id.detach())
                }
            })
        };

        // A reflog ordinal past the end of an existing ref's log is its own
        // `die()` inside `get_oid()`, ahead of any path interpretation — nothing
        // has been echoed yet at this point, which is what keeps stdout empty.
        if resolved.is_none() {
            // `peel_onion()` reports a type it cannot reach through `error()`, not
            // through the caller's `die()`, so the line comes out once per
            // resolution attempt — here for the one that just failed, and again
            // below if `die_verify_filename()` resolves the operand a second time.
            // The name is measured after the exclusion mark, matching
            // `cmd_rev_parse()`'s `if (*arg == '^') name++;`.
            if let Some(message) =
                crate::objname::peel_type_error(&repo, crate::objname::uninteresting_mark(arg).0)
            {
                out.flush()?;
                eprintln!("error: {message}");
            }
            // Same class: `prefix_path()` dies while `get_oid_with_context_1()` is
            // still rewriting the path arm, so nothing has been echoed yet.
            if let Some(message) = crate::objpath::relative_path_fatal(&repo, arg) {
                out.flush()?;
                eprintln!("fatal: {message}");
                return Ok(ExitCode::from(128));
            }
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
                // A reflog operand has already had git's own answer — and its
                // own refusal — from `get_oid_basic()`'s `repo_dwim_log()` branch
                // above. Re-asking gitoxide's full grammar would resolve names git
                // rejects (`HEAD@{0}` off a stale log under an unborn HEAD).
                let parsed = if arg.is_empty()
                    || crate::objname::is_reflog_operand(arg)
                    || carries_walk_mark(arg)
                {
                    None
                } else {
                    let full = crate::objname::canonical_spec(&repo, arg);
                    repo.rev_parse(full.as_ref()).ok().and_then(|s| match s.detach() {
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
                // gitoxide resolves every endpoint through the object database, so
                // a revspec built out of a full-length hex naming an object that is
                // not present fails to parse at all. git reaches the same specs
                // through `get_oid()`, which takes full hex at face value.
                let parsed = parsed.or_else(|| full_hex_spec(&repo, arg));
                match parsed {
                    Some(Parsed::Range(range)) => {
                        emit_range(&mut out, &repo, &o, range, arg)?;
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
                // `if (has_dashdash) die(_("bad revision '%s'"), arg);` — ahead of
                // `show_file()`, so the operand is never echoed.
                if has_dashdash {
                    out.flush()?;
                    eprintln!("fatal: bad revision '{arg}'");
                    return Ok(ExitCode::from(128));
                }
                as_is = true;
                if o.echo_paths {
                    emit(&mut out, arg.as_bytes())?;
                }
                if !is_worktree_path(&repo, arg) {
                    out.flush()?;
                    // `verify_filename(prefix, arg, 1)` → `die_verify_filename()`:
                    // the operand gets one more resolution, with
                    // `GET_OID_ONLY_TO_DIE`, and a `<rev>:<path>` / `:<n>:<path>`
                    // failure has a message of its own there.
                    if let Some(message) = crate::objname::peel_type_error(&repo, arg) {
                        eprintln!("error: {message}");
                    }
                    match crate::objpath::verify_filename_diagnosis(&repo, arg) {
                        Some(diagnosis) => eprintln!("fatal: {diagnosis}"),
                        None => eprintln!(
                            "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
                             Use '--' to separate paths from revisions, like this:\n\
                             'git <command> [<revision>...] -- [<file>...]'"
                        ),
                    }
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
    // `expand_ref()` stops at the first match when `core.warnAmbiguousRefs` is
    // off, so `refs_found` never exceeds 1 there and nothing downstream can call
    // the name ambiguous:
    //
    // ```c
    // if (r) {
    //         if (!refs_found++)
    //                 *ref = xstrdup(r);
    //         if (!repo_settings_get_warn_ambiguous_refs(repo))
    //                 break;
    // }
    // ```
    //
    // Observable through the callers that turn the count into a `die()` rather
    // than a warning: stock `git -c core.warnAmbiguousRefs=false branch nb dup`
    // creates the branch, and `merge-base --fork-point dup main` answers instead
    // of reporting `Ambiguous refname`.
    // ```c
    // int repo_dwim_ref(struct repository *r, const char *str, int len, …)
    // {
    //         char *last_branch = substitute_branch_name(r, &str, &len, 0);
    //         int   refs_found  = expand_ref(r, str, len, oid, ref, …);
    // ```
    //
    // `substitute_branch_name()` is `repo_interpret_branch_name()`, so the rules
    // below are applied to the *rewritten* name: `git rev-parse --abbrev-ref
    // main@{u}` shortens the upstream ref, not the operand. Without it the scan
    // finds nothing and `show_rev()`'s `case 0` prints nothing at all.
    let rewritten = match crate::objname::interpret_branch_name(repo, name) {
        Some(Ok(full)) => std::borrow::Cow::Owned(full),
        _ => std::borrow::Cow::Borrowed(name),
    };
    let name: &str = rewritten.as_ref();
    let stop_at_first = repo.config_snapshot().boolean("core.warnAmbiguousRefs") == Some(false);
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
        if stop_at_first {
            break;
        }
    }
    found
}

/// `get_oid_basic`'s *other* warning, a little further down the same function
/// (`object-name.c:1006-1011`): a `<ref>@{<date>}` operand whose date is older
/// than every entry in that ref's log resolves to the oldest entry and says so.
///
/// It shares `--quiet`'s `GET_OID_QUIETLY` gate with the ambiguity warning, and
/// nothing else — in particular `core.warnAmbiguousRefs` does not silence it. The
/// message itself is [`crate::objname::reflog_reach_warning`]'s, so this is only
/// the placement: stdout is flushed first, because the operands before this one
/// have already printed and git's warning lands after them.
fn warn_reflog_reach(
    out: &mut impl Write,
    repo: &gix::Repository,
    arg: &str,
    quiet: bool,
) -> std::io::Result<()> {
    if quiet {
        return Ok(());
    }
    if let Some(warning) = crate::objname::reflog_reach_warning(repo, arg) {
        out.flush()?;
        eprint!("{warning}");
    }
    Ok(())
}

/// `get_oid_basic`'s ambiguity warning (`object-name.c`): once a plain name has
/// resolved as a ref, warn when `core.warnAmbiguousRefs` (default true) is on and
/// either more than one of the rev-parse rules matched, or the name *also* reads
/// as an unambiguous abbreviated object id — the `refs_found > 1 ||
/// !get_short_oid(…)` disjunction. `--quiet` is git's `GET_OID_QUIETLY`, which
/// suppresses it.
///
/// Both halves live in [`crate::objname::warn_ambiguous_operand`], because every
/// other command that takes an object name needs the same two warnings and a
/// second copy of the rule here would be a second thing to keep in step. `quiet`
/// is the only thing rev-parse adds: it is the one builtin with a `--quiet` that
/// reaches `get_oid()` with `GET_OID_QUIETLY`.
pub(crate) fn warn_ambiguous_refname(repo: &gix::Repository, arg: &str, quiet: bool) {
    crate::objname::warn_ambiguous_operand(
        repo,
        arg,
        crate::objname::OidFlags { quiet, ..Default::default() },
    );
}

/// The revspecs [`crate::objname::full_hex`] rescues once an endpoint is a full-length hex
/// whose object is absent, which gitoxide's revspec parser rejects outright:
///
/// * `^<full-hex>` — git's `REVERSED` single revision, `^<id>`.
/// * `<a>..<b>` — `try_difference()` resolves both sides with `get_oid()`, so
///   `git rev-parse 0{40}..HEAD` prints HEAD's id then `^0{40}` and exits 0.
///
/// `<a>...<b>` is deliberately absent: git prints both endpoints and then dies
/// looking for merge bases against the object it does not have, leaving output
/// half-written. That is a failure path, not a result worth reproducing here.
fn full_hex_spec(repo: &gix::Repository, arg: &str) -> Option<Parsed> {
    if let Some(rest) = arg.strip_prefix('^') {
        return crate::objname::full_hex(repo, rest).map(|id| Parsed::Single { id, reversed: true });
    }
    let at = arg.find("..")?;
    if arg[at + 2..].starts_with('.') {
        return None;
    }
    let (left, right) = endpoint_names(arg);
    Some(Parsed::Range(RangeSpec::Range {
        from: endpoint(repo, left)?,
        to: endpoint(repo, right)?,
    }))
}

/// One side of a range, resolved the way `get_oid()` resolves it: a full-length
/// hex is its own answer, everything else goes to the ordinary parser.
fn endpoint(repo: &gix::Repository, name: &str) -> Option<ObjectId> {
    // `repo_get_oid_committish()` and nothing wider: an endpoint still carrying a
    // `^!`/`^@`/`^-<n>` mark has no case in `get_oid_1()`, so `try_difference()`
    // declines the whole token rather than resolving half of it through
    // gitoxide's `Spec::ExcludeParents`.
    crate::objname::resolve_quiet(repo, name)
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
    /// One of `--all`, `--branches[=<glob>]`, `--tags[=<glob>]`,
    /// `--remotes[=<glob>]`, `--glob=<glob>` — a ref walk that also consumes the
    /// pending `--exclude` list.
    Refs(crate::porcelain::log::RefSelector, Option<String>),
    /// `--exclude=<pattern>`: accumulated until the next ref walk consumes it.
    Exclude(String),
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
    // A `-h` that was not the sole argument (the lone-`-h` case is answered at
    // the entry point) is `usage(builtin_rev_parse_usage)`: the same block, but
    // on stderr and with no `error:` line, exit 129.
    if arg == "-h" {
        eprint!("{USAGE}");
        return Err(crate::fatal::Silent(129).into());
    }
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
        _ if crate::porcelain::log::ref_selector(arg).is_some() => {
            let (kind, pattern) = crate::porcelain::log::ref_selector(arg).expect("checked above");
            return Ok(Opt::Refs(kind, pattern.map(str::to_string)));
        }
        _ if arg.starts_with("--exclude=") => {
            return Ok(Opt::Exclude(arg["--exclude=".len()..].to_string()))
        }
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
            // ```c
            // if (opt_with_value(arg, "--abbrev-ref", &arg)) {
            //         abbrev_ref = 1;
            //         abbrev_ref_strict = repo_settings_get_warn_ambiguous_refs(the_repository);
            //         if (arg) {
            //                 if (!strcmp(arg, "strict"))       abbrev_ref_strict = 1;
            //                 else if (!strcmp(arg, "loose"))   abbrev_ref_strict = 0;
            //                 else die(_("unknown mode for --abbrev-ref: %s"), arg);
            //         }
            // ```
            // (`builtin/rev-parse.c:917-930`)
            if let Some(mode) = arg.strip_prefix("--abbrev-ref=") {
                o.abbrev_ref = true;
                o.abbrev_ref_strict = match mode {
                    "strict" => Some(true),
                    "loose" => Some(false),
                    _ => {
                        eprintln!("fatal: unknown mode for --abbrev-ref: {mode}");
                        return Ok(Opt::Fatal);
                    }
                };
                return Ok(Opt::Consumed);
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

// `is_inside_git_dir()` and `is_inside_work_tree()` are shared with the other commands that ask
// setup the same questions — see [`crate::setup`].
use crate::setup::{is_inside_git_dir, is_inside_work_tree};

/// `builtin_rev_parse_usage` — the bare synopsis `show_usage_if_asked()`
/// and `usage()` both print. rev-parse has no parse-options table of its own.
const USAGE: &str = r#"usage: git rev-parse --parseopt [<options>] -- [<args>...]
   or: git rev-parse --sq-quote [<arg>...]
   or: git rev-parse [<options>] [<arg>...]

Run "git rev-parse --parseopt -h" for more information on the first usage.
"#;

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

/// Whether `arg` still carries a revision-walk mark (`^!`, `^@`, `^-<n>`) where
/// `cmd_rev_parse()` would hand it to the object-name parser.
///
/// `get_oid_1()` has no case for any of the three: they are
/// `handle_revision_arg_1()`'s grammar, and `try_parent_shorthands()` is the only
/// thing in `rev-parse` that reads them. So an operand that reaches
/// `repo_get_oid_with_flags()` — or `try_difference()`'s
/// `repo_get_oid_committish()`, one endpoint at a time — while still carrying one
/// cannot resolve, however much wider gitoxide's own revspec grammar is.
///
/// The leading `^` is stripped first because `cmd_rev_parse()` strips it before
/// resolving (`builtin/rev-parse.c:1163-1167`), which is why `^main^!` is refused
/// while `^main` is an ordinary exclude.
fn carries_walk_mark(arg: &str) -> bool {
    let name = crate::objname::uninteresting_mark(arg).0;
    match crate::objname::split_range(name) {
        Some(range) => {
            crate::objname::has_walk_mark(range.a) || crate::objname::has_walk_mark(range.b)
        }
        None => crate::objname::has_walk_mark(name),
    }
}

/// `try_parent_shorthands()` (`builtin/rev-parse.c:328-390`):
///
/// ```c
/// if ((mark = strstr(arg, "^!"))) {
///         include_rev = 1;
///         if (mark[2])
///                 return 0;
/// } else if ((mark = strstr(arg, "^@"))) {
///         include_parents = 1;
///         if (mark[2])
///                 return 0;
/// } else if ((mark = strstr(arg, "^-"))) {
///         include_rev = 1;
///         exclude_parent = 1;
///         if (mark[2]) {
///                 char *end;
///                 exclude_parent = strtoul(mark + 2, &end, 10);
///                 if (*end != '\0' || !exclude_parent)
///                         return 0;
///         }
/// } else
///         return 0;
///
/// arg = to_free = xmemdupz(arg, mark - arg);
/// if (repo_get_oid_committish(the_repository, arg, &oid) ||
///     !(commit = lookup_commit_reference(the_repository, &oid))) { … return 0; }
/// if (exclude_parent &&
///     exclude_parent > commit_list_count(commit->parents)) { … return 0; }
/// if (include_rev)
///         show_rev(NORMAL, &oid, arg);
/// for (parents = commit->parents, parent_number = 1; parents;
///      parents = parents->next, parent_number++) {
///         char *name = NULL;
///         if (exclude_parent && parent_number != exclude_parent)
///                 continue;
///         if (symbolic)
///                 name = xstrfmt("%s^%d", arg, parent_number);
///         show_rev(include_parents ? NORMAL : REVERSED, &parents->item->object.oid, name);
///         free(name);
/// }
/// return 1;
/// ```
///
/// Four details a re-derivation gets wrong:
///
///   * The mark is found with `strstr`, i.e. anywhere in the operand, and the
///     three tests are an `else if` chain — so `main^!x` is refused outright
///     rather than falling through to the `^-` case.
///   * `show_rev(NORMAL, &oid, arg)` prints the id `repo_get_oid_committish()`
///     produced, *not* the peeled commit: `<annotated-tag>^!` leads with the tag
///     object's own id and follows it with the tagged commit's parents.
///   * The `--symbolic` name is `<base>^<n>` and is built only when `symbolic` is
///     set. `--abbrev-ref` alone leaves it `NULL`, and `show_rev()` then prints
///     the parent's hex rather than trying to name it.
///   * `verify` is not consulted at all, so `rev-parse --verify <rev>^!` prints
///     every line and *then* fails `Needed a single revision` with `revs_count`
///     still zero.
fn try_parent_shorthands(
    out: &mut impl Write,
    repo: &gix::Repository,
    o: &Opts,
    arg: &str,
) -> Result<bool> {
    let (at, include_rev, include_parents, exclude_parent) = if let Some(at) = arg.find("^!") {
        if !arg[at + 2..].is_empty() {
            return Ok(false);
        }
        (at, true, false, 0i32)
    } else if let Some(at) = arg.find("^@") {
        if !arg[at + 2..].is_empty() {
            return Ok(false);
        }
        (at, false, true, 0i32)
    } else if let Some(at) = arg.find("^-") {
        let tail = &arg[at + 2..];
        let n = if tail.is_empty() { 1 } else { strtoul_int(tail).unwrap_or(0) };
        // `if (*end != '\0' || !exclude_parent) return 0;`
        if n == 0 {
            return Ok(false);
        }
        (at, true, false, n)
    } else {
        return Ok(false);
    };

    let base = &arg[..at];
    // `repo_get_oid_committish()`, which is where an ambiguous base earns its
    // `warning: refname '<name>' is ambiguous.` — the operand never reaches the
    // scan's own warning because a claimed one `continue`s.
    let Some(oid) = crate::objname::resolve(repo, base) else {
        return Ok(false);
    };
    // `lookup_commit_reference()` peels tags and reports the object's own type
    // when nothing commit-ish is behind it, on stderr and without failing.
    let commit = match crate::sequencer::peel_id(repo, oid) {
        crate::sequencer::Side::Commit(id) => id,
        crate::sequencer::Side::NotACommit(kind) => {
            out.flush()?;
            eprintln!("error: object {oid} is a {kind}, not a commit");
            return Ok(false);
        }
        crate::sequencer::Side::Unresolved => return Ok(false),
    };
    let Ok(commit) = repo.find_commit(commit) else {
        return Ok(false);
    };
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    // `exclude_parent > commit_list_count(commit->parents)`, where the count is
    // unsigned: a `strtoul` result that wrapped an `int` negative compares as a
    // huge number and is refused here rather than silently selecting no parent.
    if exclude_parent != 0 && exclude_parent as u32 > parents.len() as u32 {
        return Ok(false);
    }

    if include_rev {
        show_rev(out, repo, o, &oid, Some(base.as_bytes().as_bstr()), None, false)?;
    }
    for (n, parent) in parents.iter().enumerate() {
        let number = n as i32 + 1;
        if exclude_parent != 0 && number != exclude_parent {
            continue;
        }
        let name = matches!(o.sym, Sym::AsIs | Sym::Full).then(|| format!("{base}^{number}"));
        show_rev(
            out,
            repo,
            o,
            parent,
            name.as_deref().map(|n| n.as_bytes().as_bstr()),
            None,
            !include_parents,
        )?;
    }
    Ok(true)
}

/// `strtoul(mark + 2, &end, 10)` with the whole tail consumed, reduced to the
/// `int` `try_parent_shorthands()` stores it in.
///
/// `strtoul` skips leading whitespace and takes an optional sign, negating on
/// `-`, so `^- 1` and `^-+1` are parent 1 while `^--1` becomes `ULONG_MAX` and
/// then `-1` — non-zero, so it survives the `!exclude_parent` test and is caught
/// by the unsigned bounds comparison instead. Overflow saturates the same way.
/// `None` is `*end != '\0'`: a tail with anything left over.
fn strtoul_int(tail: &str) -> Option<i32> {
    let body = tail.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (negate, digits) = match body.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, body.strip_prefix('+').unwrap_or(body)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let magnitude = digits.parse::<u64>().unwrap_or(u64::MAX);
    let value = if negate { 0u64.wrapping_sub(magnitude) } else { magnitude };
    Some(value as u32 as i32)
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
    // ```c
    // if ((symbolic || abbrev_ref) && name) { … }
    // else if (abbrev) …
    // else show_with_type(type, oid_to_hex(oid));
    // ```
    //
    // (`builtin/rev-parse.c:150-186`.) The naming modes are gated on there being
    // a name at all: `try_parent_shorthands()` passes `NULL` for a parent unless
    // `--symbolic` asked for `<base>^<n>`, and `--abbrev-ref <rev>^!` therefore
    // prints the parents as plain hex rather than dropping them.
    let payload: Option<Vec<u8>> = if (o.abbrev_ref || o.sym == Sym::Full)
        && (name.is_some() || known_full.is_some())
    {
        let full = match known_full {
            Some(f) => Some(f.to_owned()),
            None => name.and_then(|n| dwim_full_name(repo, n)),
        };
        // ```c
        // switch (repo_dwim_ref(the_repository, name, strlen(name), &discard, &full, 0)) {
        // case 0:  /* Not found -- not a ref. */          break;
        // case 1:  /* happy */                            … show_with_type(type, full); break;
        // default: /* ambiguous */
        //         error("refname '%s' is ambiguous", name);
        //         break;
        // }
        // ```
        // (`builtin/rev-parse.c:155-180`). More than one `ref_rev_parse_rules`
        // spelling exists, so there is no single full name to report: git prints
        // an `error:` and *nothing* on stdout, at exit 0. It is not the ambiguity
        // *warning* — `-q` silences that one and leaves this.
        let full = match (&full, name, known_full) {
            (Some(_), Some(n), None)
                if n.to_str()
                    .ok()
                    .is_some_and(|n| dwim_ref_matches(repo, n).len() > 1) =>
            {
                out.flush()?;
                eprintln!("error: refname '{}' is ambiguous", n);
                None
            }
            _ => full,
        };
        // A revision that names no ref prints nothing at all in these modes.
        full.map(|full| {
            if o.abbrev_ref {
                // `refs_shorten_unambiguous_ref(…, full, abbrev_ref_strict)`
                // (`builtin/rev-parse.c:170-172`). Not a category-prefix strip:
                // `refs/remotes/origin/HEAD` shortens to `origin`, and an
                // ambiguous name keeps a component (`refs/tags/dup` → `tags/dup`).
                let strict = o
                    .abbrev_ref_strict
                    .unwrap_or_else(|| crate::refname::warn_ambiguous_refs(repo));
                crate::refname::shorten_unambiguous(repo, &full, strict).into()
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
        Some(0) => match id.attach(repo).shorten() {
            Ok(prefix) => prefix.to_string().into_bytes(),
            Err(_) => truncate_hex(id, crate::abbrev::configured_abbrev(repo, id.kind().len_in_hex())),
        },
        Some(n) => {
            let n = n.clamp(4, id.kind().len_in_hex());
            let candidate = gix::odb::store::prefix::disambiguate::Candidate::new(*id, n)?;
            match repo.objects.disambiguate_prefix(candidate)? {
                Some(prefix) => prefix.to_string().into_bytes(),
                None => truncate_hex(id, n),
            }
        }
    })
}

/// Cut a full hex id to `len` without consulting the object database.
///
/// Abbreviation normally asks the odb for the shortest unambiguous prefix, which
/// needs the object to be present. A gitlink names a commit that lives in the
/// submodule's odb, not the parent's, so `HEAD:<submodule>` has nothing local to
/// disambiguate against. git does not fail there and does not widen back to the
/// full hash — `find_unique_abbrev_r()` (object-name.c:900-916) returns early with
/// `len` when `repo_find_cmp_by_hash()` misses, so the id is simply cut at the
/// requested width. Both callers above reproduce that: `--short` cuts at
/// `core.abbrev`, `--short=n` cuts at `n`.
fn truncate_hex(id: &ObjectId, len: usize) -> Vec<u8> {
    let mut hex = id.to_string().into_bytes();
    hex.truncate(len.clamp(4, hex.len()));
    hex
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
/// unrelated).
///
/// `spec` is the argument as typed. `try_difference()` hands each endpoint to
/// `show_rev` with its *name*, so the endpoints of a range answer to
/// `--symbolic`, `--abbrev-ref` and `--symbolic-full-name` exactly as a bare
/// revision does. A merge base has no name and is always an object id.
fn emit_range(
    out: &mut impl Write,
    repo: &gix::Repository,
    o: &Opts,
    range: RangeSpec,
    spec: &str,
) -> Result<()> {
    let (left, right) = endpoint_names(spec);
    match range {
        RangeSpec::Range { from, to } => {
            show_rev(out, repo, o, &to, Some(right.as_bytes().as_bstr()), None, false)?;
            show_rev(out, repo, o, &from, Some(left.as_bytes().as_bstr()), None, true)?;
        }
        RangeSpec::Merge { theirs, ours } => {
            show_rev(out, repo, o, &ours, Some(right.as_bytes().as_bstr()), None, false)?;
            show_rev(out, repo, o, &theirs, Some(left.as_bytes().as_bstr()), None, false)?;
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

/// Split `a..b` / `a...b` into the two endpoint names `try_difference()` builds,
/// including its defaults: an omitted side is `HEAD`.
fn endpoint_names(spec: &str) -> (&str, &str) {
    let Some(at) = spec.find("..") else {
        return ("HEAD", "HEAD");
    };
    let left = &spec[..at];
    let rest = &spec[at + 2..];
    // `symmetric = (*next == '.')`, and the extra dot belongs to the separator.
    let right = rest.strip_prefix('.').unwrap_or(rest);
    (
        if left.is_empty() { "HEAD" } else { left },
        if right.is_empty() { "HEAD" } else { right },
    )
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
        // `branch_get_upstream()` reads `branch.<name>.merge` directly when
        // `branch.<name>.remote` is `.`: the upstream is a *local* ref and there
        // is no remote-tracking name to map to. Stock 2.55.0 answers
        // `git rev-parse --symbolic-full-name main@{u}` with `refs/heads/side`
        // there; `branch_remote_tracking_ref_name()` has nothing to return.
        if direction == gix::remote::Direction::Fetch {
            return super::branch::upstream_ref(repo, branch.as_bstr())
                .map(|r| r.as_bstr().to_owned());
        }
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

/// Walk a ref namespace the way `--all`, `--branches[=<glob>]`,
/// `--tags[=<glob>]`, `--remotes[=<glob>]` and `--glob=<glob>` do.
///
/// The selection rule — pattern construction, `wildmatch(…, 0)` against the
/// *whole* refname, trimming after the match, then `ref_excluded()` against the
/// *trimmed* name — is `refs_for_each_ref_ext()` and is shared with the revision
/// walkers through [`crate::porcelain::log::RefSelection`]. The trim is why
/// `git rev-parse --exclude=side --branches` drops `side` while
/// `--exclude=refs/heads/side --branches` drops nothing: the callback never sees
/// the `refs/heads/` half.
///
/// Entries are ordered by full ref name and are *not* peeled: `--tags` reports an
/// annotated tag's own object id, matching stock git. `--all` here is
/// `refs_for_each_ref()`, which — unlike revision.c's `--all` — does not add
/// `HEAD`.
fn collect_refs(
    repo: &gix::Repository,
    selection: &crate::porcelain::log::RefSelection,
) -> Result<Vec<(BString, BString, ObjectId)>> {
    let mut refs = Vec::new();
    for reference in repo.references()?.all()? {
        let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
        let full = reference.name().as_bstr().to_owned();
        let Some(full_str) = full.to_str().ok() else { continue };
        let Some(echo) = selection.selects(full_str) else { continue };
        let echo = BString::from(echo.as_bytes());
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
