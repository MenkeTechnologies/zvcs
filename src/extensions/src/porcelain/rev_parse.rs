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
//! Also implemented, and none of them a revision query: the whole `--parseopt`
//! mode (with `--keep-dashdash`, `--stop-at-non-option` and `--stuck-long`) over a
//! port of `parse-options.c`, `--sq-quote`, `--local-env-vars`,
//! `--resolve-git-dir <path>`, `--git-path <name>`, `--shared-index-path`,
//! `--path-format=(absolute|relative)`, `--disambiguate=<prefix>` and the four
//! date rewrites `--since=`/`--after=`/`--before=`/`--until=`.
//!
//! `--path-format=` is scan state like every display option, so it governs only the
//! path queries written *after* it; the rendering it selects is
//! [`print_path`], a port of `builtin/rev-parse.c:656-703`.
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
//! `--is-shallow-repository`, `--show-superproject-working-tree`, `--bisect`,
//! `--default`, `--prefix`, `--revs-only`/`--no-revs`/`--flags`/`--no-flags`,
//! `--end-of-options`, `--all-objects` and `--exclude-hidden=`. Options git does
//! *not* recognize are echoed, which is what git itself does with them.

use anyhow::Result;
use std::io::Write;
use std::ops::ControlFlow;
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
    /// `--path-format=(absolute|relative)`, git's `enum format_type format`
    /// (`builtin/rev-parse.c:721`). It is plain scan state, so it governs only the
    /// path-printing options that come *after* it on the command line.
    format: Format,
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
            format: Format::Default,
        }
    }
}

/// Options stock git recognizes that this port does not implement. Echoing them
/// the way unknown options are echoed would silently produce a wrong answer, so
/// they are rejected instead.
const UNIMPLEMENTED_EXACT: &[&str] = &[
    "-h",
    "--help",
    "--sq",
    "--not",
    "--default",
    "--prefix",
    "--revs-only",
    "--no-revs",
    "--flags",
    "--no-flags",
    "--output-object-format",
    "--is-shallow-repository",
    "--show-superproject-working-tree",
    "--bisect",
    "--end-of-options",
    "--all-objects",
];

const UNIMPLEMENTED_PREFIX: &[&str] = &[
    "--exclude-hidden=",
    "--default=",
    "--prefix=",
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

    // ```c
    // if (argc > 1 && !strcmp("--parseopt", argv[1]))
    //         return cmd_parseopt(argc - 1, argv + 1, prefix);
    //
    // if (argc > 1 && !strcmp("--sq-quote", argv[1]))
    //         return cmd_sq_quote(argc - 2, argv + 2);
    // ```
    //
    // (`builtin/rev-parse.c:725-729`.) Both are whole *modes*, recognized only in
    // the first argument slot and never entered from anywhere else in the scan:
    // `git rev-parse HEAD --sq-quote` echoes the flag instead. Neither opens a
    // repository.
    match args.first().map(String::as_str) {
        Some("--parseopt") => return parseopt(&args[1..]),
        Some("--sq-quote") => {
            let mut buf = Vec::new();
            sq_quote_argv(&mut buf, &args[1..]);
            buf.push(b'\n');
            std::io::stdout().write_all(&buf)?;
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    // ```c
    // if (!seen_end_of_options) {
    //         if (!strcmp(arg, "--local-env-vars")) { … continue; }
    //         if (!strcmp(arg, "--resolve-git-dir")) { … continue; }
    // }
    //
    // /* The rest of the options require a git repository. */
    // if (!did_repo_setup) {
    //         prefix = setup_git_directory(the_repository);
    // ```
    //
    // (`builtin/rev-parse.c:757-780`.) Repository setup is lazy and happens on the
    // first argument that is *not* one of those two, so a command made only of them
    // answers outside a repository. This port opens the repository up front, so the
    // leading run of pre-setup options is drained here first; any later occurrence
    // is answered by the scan below, where the repository is already open — which is
    // the same order git prints them in.
    let mut start = 0usize;
    while start < args.len() {
        match args[start].as_str() {
            "--local-env-vars" => {
                print_local_env_vars(&mut std::io::stdout())?;
                start += 1;
            }
            "--resolve-git-dir" => {
                let Some(dir) = args.get(start + 1) else {
                    eprintln!("fatal: --resolve-git-dir requires an argument");
                    return Ok(ExitCode::from(128));
                };
                if !print_resolved_git_dir(&mut std::io::stdout(), dir)? {
                    return Ok(ExitCode::from(128));
                }
                start += 2;
            }
            _ => break,
        }
    }
    let args = &args[start..];
    // With nothing left, `did_repo_setup` never becomes 1 and no repository is ever
    // opened — which is what lets `git rev-parse --local-env-vars` answer outside one.
    if args.is_empty() {
        return Ok(ExitCode::SUCCESS);
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

    // Everything `print_path()` needs out of a setup this port does not perform.
    let paths = PathCtx::new(&repo);

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

    // An index loop rather than a `for`: `--resolve-git-dir` and `--git-path` take
    // the *next* argv entry as their value (`argv[++i]`), which the scan has to be
    // able to skip.
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
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

        // The options that print at their position and need more than the option
        // table: the two pre-setup ones (reached here when they were not part of the
        // leading run), the two that consume `argv[++i]`, and the ones that read the
        // repository or the clock.
        if !as_is && arg.starts_with('-') && arg.len() > 1 {
            match positional_option(&mut out, &repo, &paths, &mut o, arg, args.get(i))? {
                Positional::NotMine => {}
                Positional::Consumed => continue,
                Positional::ConsumedValue => {
                    i += 1;
                    continue;
                }
                Positional::Fatal => {
                    out.flush()?;
                    return Ok(ExitCode::from(128));
                }
            }
        }

        if !as_is && arg.len() > 1 && arg.starts_with('-') {
            match option(&mut o, arg)? {
                Opt::Consumed => {}
                Opt::Query(q) => {
                    if let Some(code) = query(&mut out, &repo, &paths, &o, q)? {
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
                    if let ControlFlow::Break(code) =
                        warn_reflog_reach(&mut out, &repo, range.a, o.quiet)?
                    {
                        return Ok(code);
                    }
                    // The endpoint has already been warned about on the line
                    // above; this is the same resolution, not a second operand.
                    let a_resolved = crate::objname::resolve_quiet(&repo, range.a).is_some();
                    if a_resolved {
                        warn_ambiguous_refname(&repo, range.b, o.quiet);
                        if let ControlFlow::Break(code) =
                            warn_reflog_reach(&mut out, &repo, range.b, o.quiet)?
                        {
                            return Ok(code);
                        }
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
                    if let ControlFlow::Break(code) =
                        warn_reflog_reach(&mut out, &repo, name, o.quiet)?
                    {
                        return Ok(code);
                    }
                }
            }
            // A full-length hex name *is* the object id and short-circuits ahead
            // of every database lookup, so it answers even for an object that is
            // not present — see [`crate::objname::full_hex`].
            crate::objname::full_hex(&repo, arg).or_else(|| {
                // `get_oid_basic()` resolves `<ref>@{<n>}` itself, through
                // `repo_dwim_log()` rather than gitoxide's ref lookup — which
                // answers for names git rejects and rejects names git answers.
                // The test is on the *reduced* name, not on the operand: a
                // `^{…}`, `~<n>` or `:<path>` suffix is applied to whatever the
                // reader answered, never folded into the selector.
                if crate::objname::resolves_through_reflog(arg) {
                    crate::objname::reflog_spec_oid(&repo, arg)
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
            // `read_ref_at()`'s two `die()`s — the ordinal past the end of the log
            // and `!cb.reccnt`, a log file that exists but holds no entries — are
            // *not* raised here. They fire inside `get_oid_basic()`, i.e. during
            // the resolution, which is `warn_reflog_reach()` above: raising them
            // from this block instead let `HEAD@{99}^{commit}` resolve through
            // `get_oid_1()`'s fallback first and answer where stock dies, and put
            // the operand on stdout via `show_file()` before the message.
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
                //
                // A range endpoint is resolved the same way and so is subject to
                // the same rule: `try_difference()` calls
                // `repo_get_oid_committish()` on each side, never the range
                // grammar, so `HEAD@{1}..HEAD` is two ordinary resolutions and
                // gitoxide must not be asked about either of them.
                let parsed = if arg.is_empty()
                    || reflog_operand_anywhere(arg)
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
                let parsed = parsed
                    .or_else(|| reflog_range(&repo, arg))
                    .or_else(|| full_hex_spec(&repo, arg));
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
                    warn_reflog_reach_again(&repo, arg);
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

/// Everything `get_oid_basic()`'s reflog branch has to say about one operand
/// (`object-name.c:787-820`), for the one command that holds its `flags`.
///
/// `builtin/rev-parse.c:862-863` is the only place that sets `GET_OID_QUIETLY`
/// from the command line:
///
/// ```c
/// quiet = 1;
/// flags |= GET_OID_QUIETLY;
/// ```
///
/// so this is the only caller that cannot go through
/// [`crate::objname::resolve`], which is `repo_get_oid()` with git's defaults.
///
/// Three diagnostics, three different gates:
///
/// * `read_ref_at()`'s own `warning()` has **no** gate — `refs.c:1135` and
///   `refs.c:1141` call `warning()` outright — so `--quiet` does not silence it;
/// * `warning: log for '<ref>' only goes back to …` is gated by
///   `GET_OID_QUIETLY`;
/// * the `die()` is gated by it too, but only for the *message*:
///   `if (flags & GET_OID_QUIETLY) exit(128); else die(…)` — stock
///   `git rev-parse --quiet 'HEAD@{99}'` still exits 128, in silence.
///
/// And the gate is not simply `--quiet`, because `get_parent()` and
/// `get_nth_ancestor()` hand the recursion a literal `GET_OID_COMMITTISH` and so
/// drop the flag before `get_oid_basic()` is reached — see
/// [`crate::objname::quiet_lost_in_navigation`]. Stock
/// `git rev-parse --quiet --verify 'HEAD@{99}^'` therefore dies with the message
/// and exit 128 where `… --quiet --verify 'HEAD@{99}'` exits 128 saying nothing.
///
/// The `die()` fires *inside* the resolution, so it is raised here rather than
/// from the failed-operand block below: stdout is still empty at this point,
/// which is what keeps `show_file()` from echoing the operand first.
fn warn_reflog_reach(
    out: &mut impl Write,
    repo: &gix::Repository,
    arg: &str,
    quiet: bool,
) -> std::io::Result<ControlFlow<ExitCode>> {
    // `read_ref_at()`'s own warning comes first, because it is raised from inside
    // the call (`refs.c:1135` and `refs.c:1141`) that `object-name.c:787` makes.
    if let Some(message) = crate::objname::read_ref_at_warning(repo, arg) {
        out.flush()?;
        eprintln!("warning: {message}");
    }
    let quiet = quiet && !crate::objname::quiet_lost_in_navigation(arg);
    match crate::objname::reflog_reach(repo, arg) {
        Some(crate::objname::ReflogReach::Warning(message)) if !quiet => {
            out.flush()?;
            eprintln!("warning: {message}");
        }
        Some(crate::objname::ReflogReach::Fatal(message)) => {
            out.flush()?;
            if !quiet {
                eprintln!("fatal: {message}");
            }
            return Ok(ControlFlow::Break(ExitCode::from(128)));
        }
        _ => {}
    }
    Ok(ControlFlow::Continue(()))
}

/// Whether [`crate::objname::resolves_through_reflog`] claims `arg` or, when
/// `arg` is a range, either of its endpoints.
///
/// `try_difference()` (`builtin/rev-parse.c:269-326`) never resolves the range as
/// a unit — it cuts at the `..` and calls `repo_get_oid_committish()` on each
/// side — so an endpoint the reflog reader owns makes the whole spelling
/// gitoxide's parser's business no longer.
fn reflog_operand_anywhere(arg: &str) -> bool {
    match crate::objname::split_range(arg) {
        Some(range) => {
            crate::objname::resolves_through_reflog(range.a)
                || crate::objname::resolves_through_reflog(range.b)
        }
        None => crate::objname::resolves_through_reflog(arg),
    }
}

/// `try_difference()` for a range [`reflog_operand_anywhere`] took away from
/// gitoxide: both endpoints through the shared resolver, joined by `&&`.
///
/// ```c
/// if (!repo_get_oid_committish(the_repository, this, &start_oid) &&
///     !repo_get_oid_committish(the_repository, next, &end_oid)) {
/// ```
///
/// Without this a `git branch -m` round trip made `git rev-parse 'HEAD@{1}..HEAD'`
/// print `^0000000000000000000000000000000000000000` — gitoxide's range grammar
/// resolves `<ref>@{<n>}` with its own reflog reader, which hands back the
/// selected entry's raw new id where `read_ref_at()` answers with the ref's own
/// value.
///
/// The endpoints have already been warned about above, so this is
/// `resolve_quiet`: the same resolution, not a second pair of operands.
fn reflog_range(repo: &gix::Repository, arg: &str) -> Option<Parsed> {
    let range = crate::objname::split_range(arg)?;
    if !crate::objname::resolves_through_reflog(range.a)
        && !crate::objname::resolves_through_reflog(range.b)
    {
        return None;
    }
    let from = crate::objname::resolve_quiet(repo, range.a)?;
    let to = crate::objname::resolve_quiet(repo, range.b)?;
    Some(Parsed::Range(if range.symmetric {
        RangeSpec::Merge { theirs: from, ours: to }
    } else {
        RangeSpec::Range { from, to }
    }))
}

/// [`warn_reflog_reach`] for the *second* resolution — the one
/// `die_verify_filename()` performs before it dies.
///
/// ```c
/// void maybe_die_on_misspelt_object_name(struct repository *r,
///                                        const char *name,
///                                        const char *prefix)
/// {
///         struct object_context oc;
///         struct object_id oid;
///         get_oid_with_context_1(r, name, GET_OID_ONLY_TO_DIE | GET_OID_QUIETLY,
///                                prefix, &oid, &oc);
///         object_context_release(&oc);
/// }
/// ```
///
/// (`object-name.c:1880-1889`.) So `get_oid_basic()` is reached a second time for
/// every operand that failed and is then diagnosed, and everything it is not
/// gated out of saying it says again — which is why stock 2.55.0 prints
///
/// ```text
/// warning: log for ref HEAD unexpectedly ended on Thu, 7 Apr 2005 22:13:13 +0200
/// warning: log for ref HEAD unexpectedly ended on Thu, 7 Apr 2005 22:13:13 +0200
/// fatal: ambiguous argument 'HEAD@{1}~99': ...
/// ```
///
/// `GET_OID_QUIETLY` is set here whatever the command line said, so the second
/// pass is quiet by default — and the exception is the same one as everywhere
/// else: `get_parent()`/`get_nth_ancestor()` drop the flag, so
/// `HEAD@{<old date>}^` and `HEAD@{<old date>}~99` print `only goes back to`
/// twice while `HEAD@{<old date>}:nosuch` prints it once.
///
/// The name is `arg`, not the caret-stripped `name`: `die_verify_filename()` is
/// handed `builtin/rev-parse.c`'s `arg` and passes it straight through.
///
/// No `die()` can come out of this pass — an out-of-range selector ended the
/// command during the first resolution — so there is nothing to propagate.
fn warn_reflog_reach_again(repo: &gix::Repository, arg: &str) {
    // `get_oid_basic()` says all of this in one call, and in this order: the
    // full-hex ambiguity warning, then the plain-name one, then `read_ref_at()`'s.
    // `GET_OID_QUIETLY` gates the middle one and the reflog reach warning below;
    // the full-hex one answers to `GET_OID_SKIP_AMBIGUITY_CHECK` instead and so
    // comes out on both passes regardless — stock 2.55.0 prints
    // `refname '<40-hex>' is ambiguous.` twice for `<40-hex-ref>:nosuch`.
    warn_ambiguous_refname(repo, arg, true);
    if let Some(message) = crate::objname::read_ref_at_warning(repo, arg) {
        eprintln!("warning: {message}");
    }
    if !crate::objname::quiet_lost_in_navigation(arg) {
        return;
    }
    if let Some(warning) = crate::objname::reflog_reach_warning(repo, arg) {
        eprint!("{warning}");
    }
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
///
/// `quiet` is `--quiet`'s `GET_OID_QUIETLY` *as `get_oid_basic()` finally sees
/// it*, so the caller has to account for the reduction dropping it — see
/// [`crate::objname::quiet_lost_in_navigation`]. Stock
/// `git rev-parse --quiet --verify 'dup~99'` prints `refname 'dup' is
/// ambiguous.` although `--quiet` was given, because `get_nth_ancestor()` handed
/// the recursion a bare `GET_OID_COMMITTISH`.
pub(crate) fn warn_ambiguous_refname(repo: &gix::Repository, arg: &str, quiet: bool) {
    crate::objname::warn_ambiguous_operand(
        repo,
        arg,
        crate::objname::OidFlags {
            quiet: quiet && !crate::objname::quiet_lost_in_navigation(arg),
            ..Default::default()
        },
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
fn query(
    out: &mut impl Write,
    repo: &gix::Repository,
    paths: &PathCtx,
    o: &Opts,
    q: Query,
) -> Result<Option<ExitCode>> {
    // `--path-format=` overrides the `default_type` each of these options would
    // otherwise print with, and it is scan state — so it only reaches the queries
    // written after it on the command line. Under `FORMAT_DEFAULT` the existing
    // per-query rendering below is what git's `DEFAULT_*` arms already produce.
    if o.format != Format::Default {
        if let Some(path) = match q {
            Query::GitDir | Query::AbsoluteGitDir => Some(gitdir_string(repo, paths)),
            Query::GitCommonDir => Some(gitdir_common_string(repo, paths)),
            Query::ShowToplevel => match toplevel(repo) {
                Some(top) => Some(top),
                None => {
                    out.flush()?;
                    eprintln!("fatal: this operation must be run in a work tree");
                    return Ok(Some(ExitCode::from(128)));
                }
            },
            _ => None,
        } {
            // `--absolute-git-dir` pins `wanted = FORMAT_CANONICAL` regardless
            // (`builtin/rev-parse.c:1053`).
            let format =
                if matches!(q, Query::AbsoluteGitDir) { Format::Canonical } else { o.format };
            print_path(out, paths, &path, format, DefaultType::Unmodified)?;
            return Ok(None);
        }
    }
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
            // `print_path(repo_get_common_dir(the_repository), prefix, format,
            //  DEFAULT_RELATIVE_IF_SHARED)` (`builtin/rev-parse.c:1073`): the stored
            // string, made relative to the directory the command was run in when the
            // two share a root. A `.git` one directory down is `../.git`; an absolute
            // one (a linked worktree's common directory) shares no root with the
            // relative prefix and is printed whole.
            let common = gitdir_common_string(repo, paths);
            print_path(out, paths, &common, o.format, DefaultType::RelativeIfShared)?;
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

// ---------------------------------------------------------------------------
// Options that print at their position (builtin/rev-parse.c:757-1122)
// ---------------------------------------------------------------------------

/// What [`positional_option`] made of an argument.
enum Positional {
    /// Not one of the options this function answers; the option table gets it.
    NotMine,
    /// Answered; the scan moves to the next argument.
    Consumed,
    /// Answered, and the option also ate `argv[++i]`.
    ConsumedValue,
    /// git `die()`d; the message is on stderr and the scan stops at 128.
    Fatal,
}

/// The `builtin/rev-parse.c` options that print (or die) at their position and
/// need more than the flag table to do it: the two that run before repository
/// setup, the two that consume `argv[++i]`, `--path-format=`, `--disambiguate=`,
/// the four date rewrites, and `--shared-index-path`.
fn positional_option(
    out: &mut impl Write,
    repo: &gix::Repository,
    paths: &PathCtx,
    o: &mut Opts,
    arg: &str,
    next: Option<&String>,
) -> Result<Positional> {
    // ```c
    // if (!strcmp(arg, "--local-env-vars")) {
    //         int i;
    //         for (i = 0; local_repo_env[i]; i++)
    //                 printf("%s\n", local_repo_env[i]);
    //         continue;
    // }
    // ```
    // (`builtin/rev-parse.c:758-763`.)
    if arg == "--local-env-vars" {
        print_local_env_vars(out)?;
        return Ok(Positional::Consumed);
    }
    // ```c
    // if (!strcmp(arg, "--resolve-git-dir")) {
    //         const char *gitdir = argv[++i];
    //         if (!gitdir)
    //                 die(_("--resolve-git-dir requires an argument"));
    //         gitdir = resolve_gitdir(gitdir);
    //         if (!gitdir)
    //                 die(_("not a gitdir '%s'"), argv[i]);
    //         puts(gitdir);
    //         continue;
    // }
    // ```
    // (`builtin/rev-parse.c:764-773`.)
    if arg == "--resolve-git-dir" {
        let Some(dir) = next else {
            out.flush()?;
            eprintln!("fatal: --resolve-git-dir requires an argument");
            return Ok(Positional::Fatal);
        };
        out.flush()?;
        if !print_resolved_git_dir(out, dir)? {
            return Ok(Positional::Fatal);
        }
        return Ok(Positional::ConsumedValue);
    }
    // ```c
    // if (!strcmp(arg, "--git-path")) {
    //         if (!argv[i + 1])
    //                 die(_("--git-path requires an argument"));
    //         print_path(repo_git_path_replace(the_repository, &buf, "%s", argv[i + 1]),
    //                    prefix, format, DEFAULT_RELATIVE_IF_SHARED);
    //         i++;
    //         continue;
    // }
    // ```
    // (`builtin/rev-parse.c:796-805`.) `--git-path=<name>` is *not* this option —
    // the test is a whole-string compare — so it falls through and is echoed.
    if arg == "--git-path" {
        let Some(name) = next else {
            out.flush()?;
            eprintln!("fatal: --git-path requires an argument");
            return Ok(Positional::Fatal);
        };
        let path = git_path(repo, paths, name);
        print_path(out, paths, &path, o.format, DefaultType::RelativeIfShared)?;
        return Ok(Positional::ConsumedValue);
    }
    // ```c
    // if (opt_with_value(arg, "--path-format", &arg)) {
    //         if (!arg)
    //                 die(_("--path-format requires an argument"));
    //         if (!strcmp(arg, "absolute")) {
    //                 format = FORMAT_CANONICAL;
    //         } else if (!strcmp(arg, "relative")) {
    //                 format = FORMAT_RELATIVE;
    //         } else {
    //                 die(_("unknown argument to --path-format: %s"), arg);
    //         }
    //         continue;
    // }
    // ```
    // (`builtin/rev-parse.c:820-831`.) `opt_with_value()` accepts the bare spelling
    // with a NULL value, which is the `requires an argument` arm; `--path-format=`
    // with an empty value is the `unknown argument` one.
    if arg == "--path-format" {
        out.flush()?;
        eprintln!("fatal: --path-format requires an argument");
        return Ok(Positional::Fatal);
    }
    if let Some(mode) = arg.strip_prefix("--path-format=") {
        o.format = match mode {
            "absolute" => Format::Canonical,
            "relative" => Format::Relative,
            _ => {
                out.flush()?;
                eprintln!("fatal: unknown argument to --path-format: {mode}");
                return Ok(Positional::Fatal);
            }
        };
        return Ok(Positional::Consumed);
    }
    // ```c
    // if (skip_prefix(arg, "--disambiguate=", &arg)) {
    //         repo_for_each_abbrev(the_repository, arg, the_hash_algo, show_abbrev, NULL);
    //         continue;
    // }
    // ```
    // (`builtin/rev-parse.c:938-942`.) `show_abbrev()` is `show_rev(NORMAL, oid, NULL)`,
    // so the listing obeys whatever display options are in effect at this position —
    // and `--verify` counts every match as a revision, which is what makes
    // `git rev-parse --short --disambiguate=<prefix>` print one and then die.
    if let Some(prefix) = arg.strip_prefix("--disambiguate=") {
        for id in for_each_abbrev(repo, prefix) {
            show_rev(out, repo, o, &id, None, None, false)?;
        }
        return Ok(Positional::Consumed);
    }
    // ```c
    // if (!strcmp(arg, "--shared-index-path")) {
    //         if (repo_read_index(the_repository) < 0)
    //                 die(_("Could not read the index"));
    //         if (the_repository->index->split_index) {
    //                 const struct object_id *oid = &the_repository->index->split_index->base_oid;
    //                 const char *path = repo_git_path_replace(the_repository, &buf,
    //                                                          "sharedindex.%s", oid_to_hex(oid));
    //                 print_path(path, prefix, format, DEFAULT_RELATIVE);
    //         }
    //         continue;
    // }
    // ```
    // (`builtin/rev-parse.c:1097-1106`.) An index that is not a split index prints
    // *nothing at all* — not an empty line — and still exits 0.
    if arg == "--shared-index-path" {
        if let Some(base) = shared_index_base(repo) {
            let path = git_path(repo, paths, &format!("sharedindex.{base}"));
            print_path(out, paths, &path, o.format, DefaultType::Relative)?;
        }
        return Ok(Positional::Consumed);
    }
    // ```c
    // if (skip_prefix(arg, "--since=", &arg)) { show_datestring("--max-age=", arg); continue; }
    // if (skip_prefix(arg, "--after=", &arg)) { show_datestring("--max-age=", arg); continue; }
    // if (skip_prefix(arg, "--before=", &arg)) { show_datestring("--min-age=", arg); continue; }
    // if (skip_prefix(arg, "--until=", &arg)) { show_datestring("--min-age=", arg); continue; }
    // ```
    // (`builtin/rev-parse.c:1107-1122`.)
    for (opt, flag) in [
        ("--since=", "--max-age="),
        ("--after=", "--max-age="),
        ("--before=", "--min-age="),
        ("--until=", "--min-age="),
    ] {
        if let Some(value) = arg.strip_prefix(opt) {
            show_datestring(out, o, flag, value)?;
            return Ok(Positional::Consumed);
        }
    }
    Ok(Positional::NotMine)
}

/// ```c
/// static void show_datestring(const char *flag, const char *datestr)
/// {
///         char *buffer;
///
///         /* date handling requires both flags and revs */
///         if ((filter & (DO_FLAGS | DO_REVS)) != (DO_FLAGS | DO_REVS))
///                 return;
///         buffer = xstrfmt("%s%"PRItime, flag, approxidate(datestr));
///         show(buffer);
///         free(buffer);
/// }
/// ```
///
/// (`builtin/rev-parse.c:241-251`.) `approxidate()` — not `approxidate_careful()` —
/// so a string it cannot read is silently "now" rather than an error:
/// `--since=bogusdate` prints the current epoch second. `DO_REVS` is only ever
/// cleared by `--no-revs`, which this port still refuses, so the gate reduces to
/// `DO_FLAGS`: `--verify` and `--short` clear it and the rewrite disappears.
fn show_datestring(out: &mut impl Write, o: &Opts, flag: &str, datestr: &str) -> Result<()> {
    if !o.echo_flags {
        return Ok(());
    }
    let when = crate::date::approxidate(datestr);
    emit(out, format!("{flag}{when}").as_bytes())?;
    Ok(())
}

/// `local_repo_env[]` (`environment.c:101-118`) — the repository-local environment
/// variables `git` clears before it recurses into a submodule, printed one per line.
/// The order is the array's, not alphabetical.
fn print_local_env_vars(out: &mut impl Write) -> Result<()> {
    const LOCAL_REPO_ENV: &[&str] = &[
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_OBJECT_DIRECTORY",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_GRAFT_FILE",
        "GIT_INDEX_FILE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_REPLACE_REF_BASE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_COMMON_DIR",
    ];
    for name in LOCAL_REPO_ENV {
        writeln!(out, "{name}")?;
    }
    Ok(())
}

/// ```c
/// const char *resolve_gitdir_gently(const char *suspect, int *return_error_code)
/// {
///         if (is_git_directory(suspect))
///                 return suspect;
///         return read_gitfile_gently(suspect, return_error_code);
/// }
/// ```
///
/// (`setup.c:2169-2174`.) A real git directory answers with the string it was
/// *given*, unchanged — no realpath, no absolutization. A `.git` file answers with
/// the `gitdir: <path>` it points at. Anything else is
/// `fatal: not a gitdir '<path>'`; the `bool` is `false` there.
fn print_resolved_git_dir(out: &mut impl Write, suspect: &str) -> Result<bool> {
    let path = std::path::Path::new(suspect);
    if is_git_directory(path) {
        writeln!(out, "{suspect}")?;
        return Ok(true);
    }
    match read_gitfile(path) {
        Some(target) => {
            out.write_all(target.as_os_str().as_encoded_bytes())?;
            out.write_all(b"\n")?;
            Ok(true)
        }
        None => {
            out.flush()?;
            eprintln!("fatal: not a gitdir '{suspect}'");
            Ok(false)
        }
    }
}

/// ```c
/// int is_git_directory(const char *suspect)
/// {
///         /* Check worktree-related signatures */
///         … "%s/HEAD" …
///         if (validate_headref(path.buf))
///                 goto done;
///
///         strbuf_reset(&path);
///         get_common_dir(&path, suspect);
///         len = path.len;
///
///         /* Check non-worktree-related signatures */
///         if (getenv(DB_ENVIRONMENT)) {
///                 if (access(getenv(DB_ENVIRONMENT), X_OK)) goto done;
///         } else {
///                 strbuf_setlen(&path, len);
///                 strbuf_addstr(&path, "/objects");
///                 if (access(path.buf, X_OK)) goto done;
///         }
///
///         strbuf_setlen(&path, len);
///         strbuf_addstr(&path, "/refs");
///         if (access(path.buf, X_OK)) goto done;
///
///         ret = 1;
/// ```
///
/// (`setup.c:415-453`.) `HEAD` is looked for in `suspect` itself, but `objects` and
/// `refs` in the *common* directory — which is what makes a linked worktree's
/// private administrative directory (`.git/worktrees/<id>`, which has neither) a
/// git directory all the same.
fn is_git_directory(dir: &std::path::Path) -> bool {
    if !validate_headref(&dir.join("HEAD")) {
        return false;
    }
    let common = get_common_dir(dir);
    let objects = match std::env::var_os("GIT_OBJECT_DIRECTORY") {
        Some(v) => std::path::PathBuf::from(v),
        None => common.join("objects"),
    };
    objects.is_dir() && common.join("refs").is_dir()
}

/// ```c
/// int get_common_dir_noenv(struct strbuf *sb, const char *gitdir)
/// {
///         strbuf_addf(&path, "%s/commondir", gitdir);
///         if (file_exists(path.buf)) {
///                 … read it, strip trailing CR/LF …
///                 if (!is_absolute_path(data.buf))
///                         strbuf_addf(&path, "%s/", gitdir);
///                 strbuf_addbuf(&path, &data);
///                 strbuf_add_real_path(sb, path.buf);
///         } else {
///                 strbuf_addstr(sb, gitdir);
///         }
/// }
/// ```
///
/// (`setup.c:312-350`.) `$GIT_COMMON_DIR` wins outright when it is set.
fn get_common_dir(gitdir: &std::path::Path) -> std::path::PathBuf {
    if let Some(v) = std::env::var_os("GIT_COMMON_DIR") {
        return std::path::PathBuf::from(v);
    }
    let Ok(text) = std::fs::read_to_string(gitdir.join("commondir")) else {
        return gitdir.to_path_buf();
    };
    let target = text.trim_end_matches(['\n', '\r']);
    let path = std::path::Path::new(target);
    let joined = if path.is_absolute() { path.to_path_buf() } else { gitdir.join(path) };
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

/// `validate_headref()` (`setup.c:352-402`): a `refs/…` symlink, a file whose text
/// is `ref: refs/…`, or a file holding a bare object name.
fn validate_headref(path: &std::path::Path) -> bool {
    if let Ok(target) = std::fs::read_link(path) {
        return target.to_string_lossy().starts_with("refs/");
    }
    let Ok(data) = std::fs::read(path) else { return false };
    // `read_in_full(fd, buffer, sizeof(buffer)-1)` with a 256-byte buffer.
    let text = String::from_utf8_lossy(&data[..data.len().min(255)]).into_owned();
    if let Some(refname) = text.strip_prefix("ref:") {
        if refname.trim_start().starts_with("refs/") {
            return true;
        }
    }
    // `get_oid_hex_any()`: any of the hash algorithms git knows, so SHA-1's 40 and
    // SHA-256's 64 hex characters both answer.
    let head = text.trim_end_matches(['\n', '\r']);
    matches!(head.len(), 40 | 64) && head.bytes().all(|b| b.is_ascii_hexdigit())
}

/// ```c
/// if (!starts_with(buf, "gitdir: "))
///         error_code = READ_GITFILE_ERR_INVALID_FORMAT;
/// …
/// if (!is_absolute_path(dir) && (slash = strrchr(path, '/')))
///         dir = xstrfmt("%.*s%.*s", …);   /* relative to the gitfile's directory */
/// if (!is_git_directory(dir))
///         error_code = READ_GITFILE_ERR_NOT_A_REPO;
/// strbuf_realpath(&realpath, dir, 1);
/// ```
///
/// (`setup.c:956-1035`.) The prefix is `gitdir: ` *with* the space, the target is
/// resolved against the gitfile's own directory when it is relative, and the answer
/// is the symlink-resolved absolute path rather than the stored text.
fn read_gitfile(file: &std::path::Path) -> Option<std::path::PathBuf> {
    if !file.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(file).ok()?;
    let target = text.strip_prefix("gitdir: ")?.trim_end_matches(['\n', '\r']);
    if target.is_empty() {
        return None;
    }
    let mut path = std::path::PathBuf::from(target);
    if path.is_relative() {
        path = file.parent()?.join(path);
    }
    is_git_directory(&path).then(|| std::fs::canonicalize(&path).unwrap_or(path))
}

// ---------------------------------------------------------------------------
// `repo_git_path()` (path.c:418-465) and `print_path()` (builtin/rev-parse.c:656)
// ---------------------------------------------------------------------------

/// git's `enum format_type` (`builtin/rev-parse.c:636-643`), set by `--path-format`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    /// `FORMAT_DEFAULT`: whatever the individual option's `default_type` asks for.
    Default,
    /// `FORMAT_RELATIVE`: relative to the directory the command was run in.
    Relative,
    /// `FORMAT_CANONICAL`: symlink-resolved and absolute.
    Canonical,
}

/// git's `enum default_type` (`builtin/rev-parse.c:645-654`): what an option asks
/// for when `--path-format` has not overridden it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DefaultType {
    /// `DEFAULT_RELATIVE`.
    Relative,
    /// `DEFAULT_RELATIVE_IF_SHARED`: relative only when the two paths share a root,
    /// which for a *relative* stored path means "print it as it stands".
    RelativeIfShared,
    /// `DEFAULT_CANONICAL`. Reached in git only from `--git-dir`'s "build
    /// `<cwd>/.git` and canonicalize it" fallback, which [`git_dir_display`]
    /// answers directly — so no caller here selects it.
    #[allow(dead_code)]
    Canonical,
    /// `DEFAULT_UNMODIFIED`: `puts(path)`.
    Unmodified,
}

/// git's `prefix` and the directory it is measured from: everything `print_path()`
/// reads out of the setup that this port does not perform.
///
/// git has chdir'd to [`PathCtx::root`] by the time any of these options print, and
/// `prefix` is the slash-terminated path from there back down to the directory the
/// user typed the command in — `NULL` when those are the same directory. A relative
/// path (`.git`, `.git/HEAD`) is therefore resolved against `root`, never against
/// the process's own working directory.
struct PathCtx {
    /// git's working directory after `setup_git_directory()`: the top of the work
    /// tree, or the git directory itself when there is no work tree to stand in.
    root: std::path::PathBuf,
    /// git's `prefix`, slash-terminated, or `None` when the command was already run
    /// at `root`.
    prefix: Option<String>,
}

impl PathCtx {
    fn new(repo: &gix::Repository) -> PathCtx {
        // `setup_git_directory()` only climbs to the top of the work tree when the
        // cwd is *inside* it; standing in the git directory of a non-bare repository
        // leaves it there, which is why `--git-dir` answers `.` from inside `.git`.
        let root = match repo.workdir() {
            Some(wd) if !is_inside_git_dir(repo) => absolute(wd),
            _ => absolute(repo.git_dir()),
        };
        // git's `prefix` is the path down from the top of the WORK TREE, and
        // nothing else: `setup_git_directory_gently_1()` sets the work tree to
        // NULL when the cwd is inside the git directory and returns no prefix at
        // all, which is why `--show-prefix` is empty from inside `.git`. Measuring
        // the cwd against `root` here instead would hand `print_path` a prefix
        // like `refs/heads/` in exactly that case, and `--git-common-dir` — the
        // one query rendered with `DEFAULT_RELATIVE_IF_SHARED` — would answer
        // `../../.` where git answers the absolute common directory.
        let in_work_tree = repo.workdir().is_some() && !is_inside_git_dir(repo);
        let prefix = in_work_tree
            .then(|| std::env::current_dir().ok())
            .flatten()
            .and_then(|c| std::fs::canonicalize(c).ok())
            .and_then(|cwd| cwd.strip_prefix(&root).ok().map(std::path::Path::to_path_buf))
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(|rel| format!("{}/", rel.display()));
        PathCtx { root, prefix }
    }

    /// `strbuf_realpath_forgiving()`: resolve as much of `path` as exists and keep
    /// the rest verbatim. A relative path is joined onto [`PathCtx::root`] first,
    /// which is git's cwd.
    fn realpath(&self, path: &std::path::Path) -> std::path::PathBuf {
        let absolute =
            if path.is_absolute() { path.to_path_buf() } else { self.root.join(path) };
        if let Ok(p) = std::fs::canonicalize(&absolute) {
            return p;
        }
        // Peel components off the tail until something resolves, then put them back.
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        let mut head = absolute.clone();
        while let Some(parent) = head.parent().map(std::path::Path::to_path_buf) {
            let Some(name) = head.file_name().map(std::ffi::OsString::from) else { break };
            head = parent;
            tail.push(name);
            if let Ok(resolved) = std::fs::canonicalize(&head) {
                let mut out = resolved;
                for name in tail.iter().rev() {
                    out.push(name);
                }
                return out;
            }
        }
        absolute
    }
}

/// ```c
/// static void print_path(const char *path, const char *prefix, enum format_type format, enum default_type def)
/// {
///         char *cwd = NULL;
///         if (!prefix && (format != FORMAT_DEFAULT || def != DEFAULT_RELATIVE_IF_SHARED))
///                 prefix = cwd = xgetcwd();
///         if (format == FORMAT_DEFAULT && def == DEFAULT_UNMODIFIED) {
///                 puts(path);
///         } else if (format == FORMAT_RELATIVE ||
///                   (format == FORMAT_DEFAULT && def == DEFAULT_RELATIVE)) {
///                 /* both sides are made absolute first */
///                 puts(relative_path(path, prefix, &buf));
///         } else if (format == FORMAT_DEFAULT && def == DEFAULT_RELATIVE_IF_SHARED) {
///                 puts(relative_path(path, prefix, &buf));
///         } else {
///                 strbuf_realpath_forgiving(&buf, path, 1);
///                 puts(buf.buf);
///         }
/// }
/// ```
///
/// (`builtin/rev-parse.c:656-703`.) The `RELATIVE_IF_SHARED` arm is the only one
/// that can see a NULL `prefix`, and there a NULL prefix makes `relative_path()`
/// return its input unchanged — which is how a stored `.git` prints as `.git`.
fn print_path(
    out: &mut impl Write,
    ctx: &PathCtx,
    path: &std::path::Path,
    format: Format,
    def: DefaultType,
) -> Result<()> {
    let text = path.as_os_str().as_encoded_bytes().to_vec();
    let shared_default = format == Format::Default && def == DefaultType::RelativeIfShared;

    let rendered: Vec<u8> = if format == Format::Default && def == DefaultType::Unmodified {
        text
    } else if format == Format::Relative
        || (format == Format::Default && def == DefaultType::Relative)
    {
        // `relative_path()` compares text, so both sides are absolutized first or a
        // relative path measured against an absolute one is simply handed back.
        let abs = ctx.realpath(path);
        let base = ctx.realpath(std::path::Path::new(ctx.prefix.as_deref().unwrap_or("")));
        relative_path(
            abs.as_os_str().as_encoded_bytes(),
            Some(base.as_os_str().as_encoded_bytes()),
        )
    } else if shared_default {
        // git compares the *stored* strings here: a relative `prefix` against a
        // relative git-directory path is what turns `.git/HEAD` into `../.git/HEAD`
        // one directory down, and an absolute git directory (a linked worktree)
        // shares no root with it and is printed whole.
        relative_path(&text, ctx.prefix.as_deref().map(str::as_bytes))
    } else {
        ctx.realpath(path).as_os_str().as_encoded_bytes().to_vec()
    };
    out.write_all(&rendered)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// ```c
/// const char *relative_path(const char *in, const char *prefix, struct strbuf *sb)
/// ```
///
/// (`path.c:942-1037`), byte for byte: an empty `in` is `./`, an empty (or NULL)
/// `prefix` returns `in` unchanged, and paths that do not share a root are also
/// returned unchanged. Otherwise the shared directory components are dropped and one
/// `../` is emitted per component of `prefix` that is left over.
fn relative_path(input: &[u8], prefix: Option<&[u8]>) -> Vec<u8> {
    let is_sep = |b: u8| b == b'/';
    let in_len = input.len();
    let prefix = prefix.unwrap_or(b"");
    let prefix_len = prefix.len();
    if in_len == 0 {
        return b"./".to_vec();
    }
    if prefix_len == 0 {
        return input.to_vec();
    }
    // `have_same_root()`: on a POSIX filesystem that is "both absolute or both
    // relative", since there is no drive prefix to compare.
    if input.starts_with(b"/") != prefix.starts_with(b"/") {
        return input.to_vec();
    }

    let (mut i, mut j) = (0usize, 0usize);
    let (mut prefix_off, mut in_off) = (0usize, 0usize);
    while i < prefix_len && j < in_len && prefix[i] == input[j] {
        if is_sep(prefix[i]) {
            while i < prefix_len && is_sep(prefix[i]) {
                i += 1;
            }
            while j < in_len && is_sep(input[j]) {
                j += 1;
            }
            prefix_off = i;
            in_off = j;
        } else {
            i += 1;
            j += 1;
        }
    }

    if i >= prefix_len && prefix_off < prefix_len {
        if j >= in_len {
            in_off = in_len;
        } else if is_sep(input[j]) {
            while j < in_len && is_sep(input[j]) {
                j += 1;
            }
            in_off = j;
        } else {
            i = prefix_off;
        }
    } else if j >= in_len && in_off < in_len && i < prefix_len && is_sep(prefix[i]) {
        while i < prefix_len && is_sep(prefix[i]) {
            i += 1;
        }
        in_off = in_len;
    }

    let rest = &input[in_off..];
    if i >= prefix_len {
        return if rest.is_empty() { b"./".to_vec() } else { rest.to_vec() };
    }

    let mut sb: Vec<u8> = Vec::with_capacity(rest.len());
    while i < prefix_len {
        if is_sep(prefix[i]) {
            sb.extend_from_slice(b"../");
            while i < prefix_len && is_sep(prefix[i]) {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    if !is_sep(prefix[prefix_len - 1]) {
        sb.extend_from_slice(b"../");
    }
    sb.extend_from_slice(rest);
    sb
}

/// ```c
/// static void repo_git_pathv(struct repository *repo, const struct worktree *wt,
///                            struct strbuf *buf, const char *fmt, va_list args)
/// {
///         int gitdir_len;
///         strbuf_worktree_gitdir(buf, repo, wt);
///         if (buf->len && !is_dir_sep(buf->buf[buf->len - 1]))
///                 strbuf_addch(buf, '/');
///         gitdir_len = buf->len;
///         strbuf_vaddf(buf, fmt, args);
///         if (!wt)
///                 adjust_git_path(repo, buf, gitdir_len);
///         strbuf_cleanup_path(buf);
/// }
/// ```
///
/// (`path.c:418-431`.) The base is `repo->gitdir` — the *string* setup left there,
/// not a canonical path, which is why `--git-path HEAD` answers `.git/HEAD` in a
/// plain checkout, `HEAD` in a bare repository (where the gitdir is `.` and
/// `cleanup_path()` strips the `./`), and an absolute path in a linked worktree.
fn git_path(repo: &gix::Repository, ctx: &PathCtx, name: &str) -> std::path::PathBuf {
    let gitdir = gitdir_string(repo, ctx);
    let mut buf = format!("{}", gitdir.display());
    if !buf.is_empty() && !buf.ends_with('/') {
        buf.push('/');
    }
    let gitdir_len = buf.len();
    buf.push_str(name);
    adjust_git_path(repo, ctx, &mut buf, gitdir_len);
    // `cleanup_path()` (path.c:42-50): a leading `./` and the slashes right behind
    // it are dropped, which is what turns the bare repository's `./HEAD` into `HEAD`.
    if let Some(rest) = buf.strip_prefix("./") {
        buf = rest.trim_start_matches('/').to_string();
    }
    std::path::PathBuf::from(buf)
}

/// The string `setup_git_directory()` leaves in `repo->gitdir`: `$GIT_DIR` verbatim
/// when it was given, `.` when git's own working directory *is* the git directory,
/// `.git` for a plain checkout, and the absolute path in every other case (a linked
/// worktree, a submodule, a `--git-dir` elsewhere).
fn gitdir_string(repo: &gix::Repository, ctx: &PathCtx) -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("GIT_DIR") {
        return dir.into();
    }
    let git_dir = absolute(repo.git_dir());
    if git_dir == ctx.root {
        return ".".into();
    }
    if git_dir == ctx.root.join(".git") {
        return ".git".into();
    }
    git_dir
}

/// ```c
/// static void adjust_git_path(struct repository *repo, struct strbuf *buf, int git_dir_len)
/// {
///         const char *base = buf->buf + git_dir_len;
///
///         if (is_dir_file(base, "info", "grafts"))
///                 strbuf_splice(buf, 0, buf->len, repo->graft_file, strlen(repo->graft_file));
///         else if (!strcmp(base, "index"))
///                 strbuf_splice(buf, 0, buf->len, repo->index_file, strlen(repo->index_file));
///         else if (dir_prefix(base, "objects"))
///                 replace_dir(buf, git_dir_len + 7, repo->objects->sources->path);
///         else if (repo_settings_get_hooks_path(repo) && dir_prefix(base, "hooks"))
///                 replace_dir(buf, git_dir_len + 5, repo_settings_get_hooks_path(repo));
///         else if (repo->different_commondir)
///                 update_common_dir(buf, git_dir_len, repo->commondir);
/// }
/// ```
///
/// (`path.c:387-404`.) The four relocations are `$GIT_GRAFT_FILE`, `$GIT_INDEX_FILE`,
/// `$GIT_OBJECT_DIRECTORY` and `core.hooksPath`; the fifth arm sends the paths a
/// linked worktree *shares* with the main checkout back to the common directory.
fn adjust_git_path(repo: &gix::Repository, ctx: &PathCtx, buf: &mut String, git_dir_len: usize) {
    let base = buf[git_dir_len..].to_string();
    let replace_whole = |buf: &mut String, with: std::ffi::OsString| {
        *buf = std::path::PathBuf::from(with).display().to_string();
    };
    if base == "info/grafts" {
        if let Some(v) = std::env::var_os("GIT_GRAFT_FILE") {
            replace_whole(buf, v);
        }
        return;
    }
    if base == "index" {
        if let Some(v) = std::env::var_os("GIT_INDEX_FILE") {
            replace_whole(buf, v);
        }
        return;
    }
    // `dir_prefix(base, "objects")`: the name itself or anything under it.
    if base == "objects" || base.starts_with("objects/") {
        if let Some(v) = std::env::var_os("GIT_OBJECT_DIRECTORY") {
            let rest = &base["objects".len()..];
            *buf = format!("{}{}", std::path::PathBuf::from(v).display(), rest);
        }
        return;
    }
    if base == "hooks" || base.starts_with("hooks/") {
        if let Ok(Some(hooks)) = repo.config_snapshot().trusted_path("core.hooksPath") {
            let rest = &base["hooks".len()..];
            *buf = format!("{}{}", hooks.display(), rest);
            return;
        }
    }
    // `repo->different_commondir`: only a linked worktree has one.
    let common = absolute(repo.common_dir());
    if common != absolute(repo.git_dir()) && is_common_path(&base) {
        let common = gitdir_common_string(repo, ctx);
        let mut replaced = format!("{}", common.display());
        if !replaced.is_empty() && !replaced.ends_with('/') {
            replaced.push('/');
        }
        replaced.push_str(&base);
        *buf = replaced;
    }
}

/// The string git holds in `repo->commondir`, mirroring [`gitdir_string`]: `.git`
/// for the main checkout of a plain repository, otherwise the absolute path.
fn gitdir_common_string(repo: &gix::Repository, ctx: &PathCtx) -> std::path::PathBuf {
    let common = absolute(repo.common_dir());
    // The discriminator is the CWD, not [`PathCtx::root`]. Standing in the git
    // directory itself, git's stored common-dir string is `.` — but one directory
    // deeper (`.git/refs`) setup has absolutized it and git prints the whole path,
    // while `root` is the git directory in both places. Comparing against `root`
    // answered `.` for every directory below `.git`, where stock answers the
    // absolute path.
    let cwd = std::env::current_dir().ok().and_then(|c| std::fs::canonicalize(c).ok());
    if cwd.is_some_and(|c| c == common) {
        return ".".into();
    }
    // `.git` relative to the top of the work tree, which is the string git keeps
    // while the cwd is anywhere inside that work tree; `print_path`'s
    // `DEFAULT_RELATIVE_IF_SHARED` turns it into `../.git` one directory down.
    if ctx.prefix.is_some() || !is_inside_git_dir(repo) {
        if common == ctx.root.join(".git") {
            return ".git".into();
        }
    }
    common
}

/// `common_list[]` (`path.c:98-124`) through `check_common()`: whether a path below
/// the git directory belongs to the *common* directory a linked worktree shares with
/// the main checkout, rather than to the worktree's own private directory.
///
/// ```c
/// static struct common_dir common_list[] = {
///         { 0, 1, 1, "branches" },
///         { 0, 1, 1, "common" },
///         { 0, 1, 1, "hooks" },
///         { 0, 1, 1, "info" },
///         { 0, 0, 0, "info/sparse-checkout" },
///         { 1, 1, 1, "logs" },
///         { 1, 0, 0, "logs/HEAD" },
///         { 0, 1, 0, "logs/refs/bisect" },
///         { 0, 1, 0, "logs/refs/rewritten" },
///         { 0, 1, 0, "logs/refs/worktree" },
///         { 0, 1, 1, "lost-found" },
///         { 0, 1, 1, "objects" },
///         { 0, 1, 1, "refs" },
///         { 0, 1, 0, "refs/bisect" },
///         { 0, 1, 0, "refs/rewritten" },
///         { 0, 1, 0, "refs/worktree" },
///         { 0, 1, 1, "remotes" },
///         { 0, 1, 1, "worktrees" },
///         { 0, 1, 1, "rr-cache" },
///         { 0, 1, 1, "svn" },
///         { 0, 0, 1, "config" },
///         { 1, 0, 1, "gc.pid" },
///         { 0, 0, 1, "packed-refs" },
///         { 0, 0, 1, "shallow" },
///         { 0, 0, 0, NULL }
/// };
/// ```
///
/// The trie matches the *longest* entry that is a path prefix of the query, and the
/// third column (`is_common`) then decides. A directory entry matches the name
/// itself or anything under it; a file entry only the exact name. The `.lock`
/// suffix is stripped before the lookup and put back afterwards
/// (`update_common_dir()`, `path.c:351-363`).
fn is_common_path(path: &str) -> bool {
    // (path, is_dir, is_common)
    const COMMON_LIST: &[(&str, bool, bool)] = &[
        ("branches", true, true),
        ("common", true, true),
        ("hooks", true, true),
        ("info", true, true),
        ("info/sparse-checkout", false, false),
        ("logs", true, true),
        ("logs/HEAD", false, false),
        ("logs/refs/bisect", true, false),
        ("logs/refs/rewritten", true, false),
        ("logs/refs/worktree", true, false),
        ("lost-found", true, true),
        ("objects", true, true),
        ("refs", true, true),
        ("refs/bisect", true, false),
        ("refs/rewritten", true, false),
        ("refs/worktree", true, false),
        ("remotes", true, true),
        ("worktrees", true, true),
        ("rr-cache", true, true),
        ("svn", true, true),
        ("config", false, true),
        ("gc.pid", false, true),
        ("packed-refs", false, true),
        ("shallow", false, true),
    ];
    let path = path.strip_suffix(".lock").unwrap_or(path);
    let mut best: Option<(usize, bool)> = None;
    for (name, is_dir, is_common) in COMMON_LIST {
        let matches = if *is_dir {
            path == *name || path.strip_prefix(name).is_some_and(|r| r.starts_with('/'))
        } else {
            path == *name
        };
        if matches && best.is_none_or(|(len, _)| name.len() > len) {
            best = Some((name.len(), *is_common));
        }
    }
    best.is_some_and(|(_, is_common)| is_common)
}

/// `repo_for_each_abbrev()` (`object-name.c:548-567`): every object whose name
/// starts with `prefix`, in ascending object-id order and without repeats.
///
/// A prefix shorter than two hex characters selects nothing — the object database
/// is enumerated through the two-character fanout of the loose object directories
/// and of the pack index, so there is no bucket to open — and a prefix carrying a
/// non-hex character is `parse_oid_prefix()`'s `-1`, which is also nothing. Both
/// were verified against stock 2.55.0 on a loose and on a packed repository.
fn for_each_abbrev(repo: &gix::Repository, prefix: &str) -> Vec<ObjectId> {
    if prefix.len() < 2 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Vec::new();
    }
    let hex_len = prefix.len().min(repo.object_hash().len_in_hex());
    // `from_hex_nonempty()` rather than `from_hex()`: the latter refuses anything
    // shorter than `MIN_HEX_LEN`, and git's own minimum here is the two-character
    // fanout the guard above already applies.
    let Ok(prefix) = gix::hash::Prefix::from_hex_nonempty(&prefix[..hex_len]) else {
        return Vec::new();
    };
    let mut found: Vec<ObjectId> = Vec::new();
    let Ok(iter) = repo.objects.iter() else { return found };
    for id in iter.flatten() {
        if prefix.cmp_oid(&id) == std::cmp::Ordering::Equal {
            found.push(id);
        }
    }
    found.sort();
    found.dedup();
    found
}

/// `the_repository->index->split_index->base_oid`: the shared index a split index
/// points at, read out of the `link` extension of `$GIT_DIR/index`. `None` when the
/// index is an ordinary one, which is what makes `--shared-index-path` print nothing.
fn shared_index_base(repo: &gix::Repository) -> Option<ObjectId> {
    let path = match std::env::var_os("GIT_INDEX_FILE") {
        Some(v) => std::path::PathBuf::from(v),
        None => repo.git_dir().join("index"),
    };
    let data = std::fs::read(path).ok()?;
    let (state, _) = gix::index::State::from_bytes(
        &data,
        std::time::SystemTime::UNIX_EPOCH.into(),
        repo.object_hash(),
        gix::index::decode::Options::default(),
    )
    .ok()?;
    state.shared_index_checksum()
}

// ---------------------------------------------------------------------------
// `--sq-quote` (builtin/rev-parse.c:569-579) and `sq_quote_buf` (quote.c:28-48)
// ---------------------------------------------------------------------------

/// ```c
/// void sq_quote_buf(struct strbuf *dst, const char *src)
/// {
///         strbuf_addch(dst, '\'');
///         while (*src) {
///                 size_t len = strcspn(src, "'!");
///                 strbuf_add(dst, src, len);
///                 src += len;
///                 while (need_bs_quote(*src)) {
///                         strbuf_addstr(dst, "'\\");
///                         strbuf_addch(dst, *src++);
///                         strbuf_addch(dst, '\'');
///                 }
///         }
///         strbuf_addch(dst, '\'');
/// }
/// ```
///
/// (`quote.c:28-48`.) The whole string is wrapped in single quotes and the two
/// characters `need_bs_quote()` names — `'` and `!` — leave the quotes, are
/// backslash-escaped, and the quotes reopen: `a'b` becomes `'a'\''b'`.
pub(crate) fn sq_quote_buf(dst: &mut Vec<u8>, src: &[u8]) {
    dst.push(b'\'');
    for &byte in src {
        if byte == b'\'' || byte == b'!' {
            dst.extend_from_slice(b"'\\");
            dst.push(byte);
            dst.push(b'\'');
        } else {
            dst.push(byte);
        }
    }
    dst.push(b'\'');
}

/// ```c
/// void sq_quote_argv(struct strbuf *dst, const char **argv)
/// {
///         for (i = 0; argv[i]; ++i) {
///                 strbuf_addch(dst, ' ');
///                 sq_quote_buf(dst, argv[i]);
///         }
/// }
/// ```
///
/// (`quote.c:85-95`.) Every element is preceded by a space, including the first —
/// which is why `git rev-parse --sq-quote a b` prints a leading blank, and an empty
/// argument list prints nothing but the newline `cmd_sq_quote()` adds.
pub(crate) fn sq_quote_argv(dst: &mut Vec<u8>, argv: &[String]) {
    for arg in argv {
        dst.push(b' ');
        sq_quote_buf(dst, arg.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// `--parseopt` (builtin/rev-parse.c:395-567) over a port of parse-options.c
// ---------------------------------------------------------------------------

/// `PARSE_OPT_NOARG`.
const PO_NOARG: u32 = 1 << 0;
/// `PARSE_OPT_OPTARG`.
const PO_OPTARG: u32 = 1 << 1;
/// `PARSE_OPT_NONEG`.
const PO_NONEG: u32 = 1 << 2;
/// `PARSE_OPT_HIDDEN`.
const PO_HIDDEN: u32 = 1 << 3;

/// `PARSE_OPT_KEEP_DASHDASH`.
const CTX_KEEP_DASHDASH: u32 = 1 << 0;
/// `PARSE_OPT_STOP_AT_NON_OPTION`.
const CTX_STOP_AT_NON_OPTION: u32 = 1 << 1;
/// `PARSE_OPT_SHELL_EVAL`: wrap `-h` output in a `cat <<\EOF` heredoc so the caller
/// can `eval` it.
const CTX_SHELL_EVAL: u32 = 1 << 2;

/// One entry of the `struct option options[]` `cmd_parseopt()` builds out of the
/// spec on stdin, plus the `OPTION_GROUP` headers it also puts in that array.
#[derive(Clone)]
struct PoOption {
    /// `OPTION_GROUP` rather than `OPTION_CALLBACK`: a heading, never matched.
    group: bool,
    short_name: Option<char>,
    long_name: Option<String>,
    /// The `<arghint>` after the flag characters, if the spec line gave one.
    argh: Option<String>,
    help: String,
    flags: u32,
}

impl PoOption {
    fn allow_unset(&self) -> bool {
        self.flags & PO_NONEG == 0
    }
}

/// `struct parse_opt_ctx_t`, reduced to the fields `cmd_parseopt()`'s two
/// `parse_options()` calls actually read.
struct PoCtx {
    argv: Vec<String>,
    /// Index of the argument being looked at; `argc` in the C is `argv.len() - at`.
    at: usize,
    /// `p->opt`: the not-yet-consumed tail of a short-option cluster, or the text
    /// after a long option's `=`.
    opt: Option<String>,
    /// `ctx->out`: the non-option arguments, in order.
    out: Vec<String>,
    /// `ctx->total`: how many arguments the parse started with.
    total: usize,
    flags: u32,
}

/// `enum parse_opt_result`, minus the values `cmd_parseopt()` cannot see.
enum PoResult {
    Done,
    NonOption,
    Unknown,
    Error,
    Help,
}

/// `git rev-parse --parseopt` — read an option spec on stdin, parse the arguments
/// after `--` against it, and print the `set --` line a shell function evaluates.
///
/// ```c
/// static int cmd_parseopt(int argc, const char **argv, const char *prefix)
/// {
///         int keep_dashdash = 0, stop_at_non_option = 0;
///         …
///         strbuf_addstr(&parsed, "set --");
///         argc = parse_options(argc, argv, prefix, parseopt_opts, parseopt_usage,
///                              PARSE_OPT_KEEP_DASHDASH);
///         if (argc < 1 || strcmp(argv[0], "--"))
///                 usage_with_options(parseopt_usage, parseopt_opts);
/// ```
///
/// (`builtin/rev-parse.c:429-456`.) `args` is everything after the `--parseopt`
/// token itself, matching the C's `argv + 1`.
fn parseopt(args: &[String]) -> Result<ExitCode> {
    let usage = vec!["git rev-parse --parseopt [<options>] -- [<args>...]".to_string()];
    let mut own = vec![
        po_bool("keep-dashdash", "keep the `--` passed as an arg"),
        po_bool("stop-at-non-option", "stop parsing after the first non-option argument"),
        po_bool("stuck-long", "output in stuck long form"),
    ];
    let mut selected = [false; 3];

    let mut ctx = PoCtx {
        argv: args.to_vec(),
        at: 0,
        opt: None,
        out: Vec::new(),
        total: args.len(),
        flags: CTX_KEEP_DASHDASH,
    };
    let mut dump = Vec::new();
    match parse_options_step(&mut ctx, &own, &mut |idx, _arg, unset| {
        selected[idx] = !unset;
        Ok(())
    }) {
        // The first `parse_options()` runs with no `PARSE_OPT_SHELL_EVAL`, so its
        // `-h` block is the bare usage on stdout — no `cat <<\EOF` wrapper.
        PoResult::Help => {
            render_usage(&mut std::io::stdout(), &usage, &own, 0)?;
            return Ok(ExitCode::from(129));
        }
        PoResult::Error => return Ok(ExitCode::from(129)),
        PoResult::Unknown => {
            eprintln!("error: {}", unknown_name(&ctx));
            render_usage(&mut std::io::stderr(), &usage, &own, 0)?;
            return Ok(ExitCode::from(129));
        }
        PoResult::Done | PoResult::NonOption => {}
    }
    let rest = parse_options_end(ctx);
    let (keep_dashdash, stop_at_non_option, stuck_long) = (selected[0], selected[1], selected[2]);

    if rest.first().map(String::as_str) != Some("--") {
        render_usage(&mut std::io::stderr(), &usage, &own, 0)?;
        return Ok(ExitCode::from(129));
    }
    own.clear();

    // ```c
    // /* get the usage up to the first line with a -- on it */
    // for (;;) {
    //         if (strbuf_getline(&sb, stdin) == EOF)
    //                 die(_("premature end of input"));
    //         if (!strcmp("--", sb.buf)) {
    //                 if (!usage.nr)
    //                         die(_("no usage string given before the `--' separator"));
    //                 break;
    //         }
    //         strvec_push(&usage, sb.buf);
    // }
    // ```
    // (`builtin/rev-parse.c:458-470`.)
    let spec = std::io::read_to_string(std::io::stdin())?;
    let mut lines = spec.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let mut spec_usage: Vec<String> = Vec::new();
    loop {
        let Some(line) = lines.next() else {
            eprintln!("fatal: premature end of input");
            return Ok(ExitCode::from(128));
        };
        if line == "--" {
            if spec_usage.is_empty() {
                eprintln!("fatal: no usage string given before the `--' separator");
                return Ok(ExitCode::from(128));
            }
            break;
        }
        spec_usage.push(line.to_string());
    }

    let opts = match parse_spec_lines(lines) {
        Ok(opts) => opts,
        Err(message) => {
            eprintln!("fatal: {message}");
            return Ok(ExitCode::from(128));
        }
    };

    // ```c
    // argc = parse_options(argc, argv, prefix, opts, usage.v,
    //                 (keep_dashdash ? PARSE_OPT_KEEP_DASHDASH : 0) |
    //                 (stop_at_non_option ? PARSE_OPT_STOP_AT_NON_OPTION : 0) |
    //                 PARSE_OPT_SHELL_EVAL);
    // ```
    // (`builtin/rev-parse.c:548-551`.) `argv[0]` is still the `--`, and the flag
    // decides whether it survives into the output.
    let mut flags = CTX_SHELL_EVAL;
    if keep_dashdash {
        flags |= CTX_KEEP_DASHDASH;
    }
    if stop_at_non_option {
        flags |= CTX_STOP_AT_NON_OPTION;
    }
    // `parse_options_start_1()` (`parse-options.c:746-757`) drops `argv[0]` before it
    // parses anything — the slot a normal command's program name sits in. Here that
    // slot holds the `--` separator the check above just verified, so the scan starts
    // at index 1 and `total` counts what is left. `total` is what decides whether a
    // `-h` is *lone*, so it has to be that reduced count.
    let mut ctx = PoCtx { argv: rest, at: 1, opt: None, out: Vec::new(), total: 0, flags };
    ctx.total = ctx.argv.len().saturating_sub(1);
    let step = parse_options_step(&mut ctx, &opts, &mut |idx, arg, unset| {
        parseopt_dump(&mut dump, &opts[idx], arg, unset, stuck_long);
        Ok(())
    });
    match step {
        PoResult::Help => {
            render_usage(&mut std::io::stdout(), &spec_usage, &opts, CTX_SHELL_EVAL)?;
            return Ok(ExitCode::from(129));
        }
        PoResult::Error => return Ok(ExitCode::from(129)),
        PoResult::Unknown => {
            eprintln!("error: {}", unknown_name(&ctx));
            render_usage(&mut std::io::stderr(), &spec_usage, &opts, 0)?;
            return Ok(ExitCode::from(129));
        }
        PoResult::Done | PoResult::NonOption => {}
    }
    let rest = parse_options_end(ctx);

    // `strbuf_addstr(&parsed, " --"); sq_quote_argv(&parsed, argv); puts(parsed.buf);`
    let mut line = b"set --".to_vec();
    line.extend_from_slice(&dump);
    line.extend_from_slice(b" --");
    sq_quote_argv(&mut line, &rest);
    line.push(b'\n');
    std::io::stdout().write_all(&line)?;
    Ok(ExitCode::SUCCESS)
}

/// `OPT_BOOL(0, name, …)`, the shape all three of `--parseopt`'s own options have.
fn po_bool(name: &str, help: &str) -> PoOption {
    PoOption {
        group: false,
        short_name: None,
        long_name: Some(name.to_string()),
        argh: None,
        help: help.to_string(),
        flags: PO_NOARG,
    }
}

/// ```c
/// static int parseopt_dump(const struct option *o, const char *arg, int unset)
/// {
///         struct strbuf *parsed = o->value;
///         if (unset)
///                 strbuf_addf(parsed, " --no-%s", o->long_name);
///         else if (o->short_name && (o->long_name == NULL || !stuck_long))
///                 strbuf_addf(parsed, " -%c", o->short_name);
///         else
///                 strbuf_addf(parsed, " --%s", o->long_name);
///         if (arg) {
///                 if (!stuck_long)
///                         strbuf_addch(parsed, ' ');
///                 else if (o->long_name)
///                         strbuf_addch(parsed, '=');
///                 sq_quote_buf(parsed, arg);
///         }
///         return 0;
/// }
/// ```
///
/// (`builtin/rev-parse.c:395-412`.)
fn parseopt_dump(dst: &mut Vec<u8>, o: &PoOption, arg: Option<&str>, unset: bool, stuck_long: bool) {
    if unset {
        dst.extend_from_slice(format!(" --no-{}", o.long_name.as_deref().unwrap_or("")).as_bytes());
    } else if o.short_name.is_some() && (o.long_name.is_none() || !stuck_long) {
        dst.extend_from_slice(format!(" -{}", o.short_name.expect("checked")).as_bytes());
    } else {
        dst.extend_from_slice(format!(" --{}", o.long_name.as_deref().unwrap_or("")).as_bytes());
    }
    if let Some(arg) = arg {
        if !stuck_long {
            dst.push(b' ');
        } else if o.long_name.is_some() {
            dst.push(b'=');
        }
        sq_quote_buf(dst, arg.as_bytes());
    }
}

/// ```c
/// /* parse: (<short>|<short>,<long>|<long>)[*=?!]*<arghint>? SP+ <help> */
/// ```
///
/// (`builtin/rev-parse.c:472-542`.) An empty line is skipped. A line with no
/// whitespace in it, or whose first character is whitespace, is an `OPTION_GROUP`
/// heading. Otherwise the text up to the first whitespace run is the name plus its
/// flag characters plus an optional argument hint, and the rest — leading whitespace
/// stripped — is the help.
fn parse_spec_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> std::result::Result<Vec<PoOption>, String> {
    let mut opts: Vec<PoOption> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some(space) = line.find(char::is_whitespace) else {
            opts.push(PoOption {
                group: true,
                short_name: None,
                long_name: None,
                argh: None,
                help: line.trim_start().to_string(),
                flags: 0,
            });
            continue;
        };
        if space == 0 {
            opts.push(PoOption {
                group: true,
                short_name: None,
                long_name: None,
                argh: None,
                help: line.trim_start().to_string(),
                flags: 0,
            });
            continue;
        }
        let (spec, help) = line.split_at(space);
        let help = help[1..].trim_start().to_string();
        let mut o = PoOption {
            group: false,
            short_name: None,
            long_name: None,
            argh: None,
            help,
            flags: PO_NOARG,
        };

        // `s = strpbrk(sb.buf, "*=?!")` — the first flag character, or the end of
        // the name when there is none.
        let bytes = spec.as_bytes();
        let flag_at = bytes.iter().position(|b| b"*=?!".contains(b)).unwrap_or(bytes.len());
        if flag_at == 0 {
            return Err("missing opt-spec before option flags".to_string());
        }
        let names = &spec[..flag_at];
        if flag_at == 1 {
            o.short_name = names.chars().next();
        } else if bytes[1] != b',' {
            o.long_name = Some(names.to_string());
        } else {
            o.short_name = names.chars().next();
            o.long_name = Some(names[2..].to_string());
        }

        let mut i = flag_at;
        while i < bytes.len() {
            match bytes[i] {
                b'=' => o.flags &= !PO_NOARG,
                b'?' => {
                    o.flags &= !PO_NOARG;
                    o.flags |= PO_OPTARG;
                }
                b'!' => o.flags |= PO_NONEG,
                b'*' => o.flags |= PO_HIDDEN,
                _ => break,
            }
            i += 1;
        }
        if i < bytes.len() {
            o.argh = Some(spec[i..].to_string());
        }
        opts.push(o);
    }
    Ok(opts)
}

/// `error(_("unknown option `%s'"))` / `error(_("unknown switch `%c'"))`
/// (`parse-options.c:1214-1222`), rendered from whatever the scan stopped on.
fn unknown_name(ctx: &PoCtx) -> String {
    let arg = ctx.argv.get(ctx.at).cloned().unwrap_or_default();
    if arg.starts_with("--") {
        format!("unknown option `{}'", &arg[2..])
    } else {
        match ctx.opt.as_deref().and_then(|s| s.chars().next()) {
            Some(c) if c.is_ascii() => format!("unknown switch `{c}'"),
            _ => format!("unknown non-ascii option in string: `{arg}'"),
        }
    }
}

/// `parse_options_end()` (`parse-options.c:1040-1048`): the collected non-options
/// followed by everything the scan stopped before.
fn parse_options_end(ctx: PoCtx) -> Vec<String> {
    let mut out = ctx.out;
    out.extend_from_slice(&ctx.argv[ctx.at.min(ctx.argv.len())..]);
    out
}

/// `parse_options_step()` (`parse-options.c:995-1080`), for the option shapes
/// `cmd_parseopt()` can build: `OPTION_GROUP` headings and `OPTION_CALLBACK`
/// entries. `hit` is the callback, called with the option's index in `options`.
fn parse_options_step(
    ctx: &mut PoCtx,
    options: &[PoOption],
    hit: &mut dyn FnMut(usize, Option<&str>, bool) -> Result<()>,
) -> PoResult {
    ctx.opt = None;
    while ctx.at < ctx.argv.len() {
        let arg = ctx.argv[ctx.at].clone();

        // `if (*arg != '-' || !arg[1])`: a bare `-` and anything not starting with
        // one are non-options.
        if !arg.starts_with('-') || arg.len() == 1 {
            if ctx.flags & CTX_STOP_AT_NON_OPTION != 0 {
                return PoResult::NonOption;
            }
            ctx.out.push(arg);
            ctx.at += 1;
            continue;
        }

        // `if (internal_help && ctx->total == 1 && !strcmp(arg + 1, "h"))`: a *lone*
        // `-h` is help even when the spec declares an `h` of its own.
        if ctx.total == 1 && arg == "-h" {
            return PoResult::Help;
        }

        if !arg.starts_with("--") {
            ctx.opt = Some(arg[1..].to_string());
            loop {
                match parse_short_opt(ctx, options, hit) {
                    ShortResult::Error => return PoResult::Error,
                    ShortResult::Unknown => {
                        if ctx.opt.as_deref().is_some_and(|s| s.starts_with('h')) {
                            return PoResult::Help;
                        }
                        return PoResult::Unknown;
                    }
                    ShortResult::Done => {}
                }
                if ctx.opt.is_none() {
                    break;
                }
            }
            ctx.at += 1;
            continue;
        }

        if arg == "--" {
            if ctx.flags & CTX_KEEP_DASHDASH == 0 {
                ctx.at += 1;
            }
            break;
        }
        if arg == "--help-all" || arg == "--help" {
            return PoResult::Help;
        }

        match parse_long_opt(ctx, &arg[2..], options, hit) {
            LongResult::Error => return PoResult::Error,
            LongResult::Help => return PoResult::Help,
            LongResult::Unknown => return PoResult::Unknown,
            LongResult::Done => {}
        }
        ctx.at += 1;
    }
    PoResult::Done
}

enum ShortResult {
    Done,
    Unknown,
    Error,
}

enum LongResult {
    Done,
    Unknown,
    Error,
    Help,
}

/// `parse_short_opt()` (`parse-options.c:426-461`): the first character of
/// `ctx->opt` names the option, and whatever follows it stays for the next round —
/// which is how `-abc` and `-Cvalue` are both read.
fn parse_short_opt(
    ctx: &mut PoCtx,
    options: &[PoOption],
    hit: &mut dyn FnMut(usize, Option<&str>, bool) -> Result<()>,
) -> ShortResult {
    let Some(rest) = ctx.opt.clone() else { return ShortResult::Unknown };
    let Some(c) = rest.chars().next() else { return ShortResult::Unknown };
    for (idx, o) in options.iter().enumerate() {
        if o.group || o.short_name != Some(c) {
            continue;
        }
        let tail = &rest[c.len_utf8()..];
        ctx.opt = (!tail.is_empty()).then(|| tail.to_string());
        return get_value(ctx, options, idx, false, hit);
    }
    ShortResult::Unknown
}

/// `parse_long_opt()` (`parse-options.c:519-594`): exact match first, then the
/// unique abbreviation, with `no-` handled on both the typed name and the table's.
fn parse_long_opt(
    ctx: &mut PoCtx,
    arg: &str,
    options: &[PoOption],
    hit: &mut dyn FnMut(usize, Option<&str>, bool) -> Result<()>,
) -> LongResult {
    // `arg_end = strchrnul(arg, '=')` and `arg_start = arg`: the *whole* token, value
    // included, is what the exact match is tried against — which is how `--bar=b`
    // leaves `=b` in `rest` and lands the value in `p->opt`. Only the abbreviation
    // compare is cut at the `=`.
    let eq = arg.find('=').unwrap_or(arg.len());
    let mut off = 0usize;
    let mut unset = false;
    let mut no_no = false;
    if arg[off..].starts_with("no-") {
        off += 3;
        if arg[off..].starts_with("no-") {
            off += 3;
            no_no = true;
        } else {
            unset = true;
        }
    }
    let start = &arg[off..];
    // `arg_end - arg_start`, which can be zero once the `no-` prefixes are off.
    let cmp_len = eq.saturating_sub(off);

    let mut abbrev: Option<(usize, bool)> = None;
    let mut ambiguous: Option<(usize, bool)> = None;
    for (idx, o) in options.iter().enumerate() {
        if o.group {
            continue;
        }
        let Some(full) = o.long_name.as_deref() else { continue };
        let (long_name, opt_unset) = match full.strip_prefix("no-") {
            Some(stem) => (stem, true),
            None if no_no => continue,
            None => (full, false),
        };
        let sense = unset != opt_unset;
        if sense && !o.allow_unset() {
            continue;
        }

        if let Some(rest) = start.strip_prefix(long_name) {
            if let Some(value) = rest.strip_prefix('=') {
                ctx.opt = Some(value.to_string());
            } else if !rest.is_empty() {
                continue;
            }
            return get_value_long(ctx, options, idx, sense, hit);
        }

        let register = |cand: (usize, bool), abbrev: &mut Option<(usize, bool)>, ambiguous: &mut Option<(usize, bool)>| {
            if let Some(prev) = *abbrev {
                if prev != cand {
                    *ambiguous = Some(prev);
                }
            }
            *abbrev = Some(cand);
        };
        // `!strncmp(long_name, arg_start, arg_end - arg_start)`.
        if long_name.len() >= cmp_len && long_name.as_bytes()[..cmp_len] == start.as_bytes()[..cmp_len]
        {
            register((idx, sense), &mut abbrev, &mut ambiguous);
        }
        // `starts_with("no-", arg)`: whether the typed text is a prefix of `no-`,
        // which is what makes `--n` name every negatable option at once.
        if o.allow_unset() && "no-".starts_with(arg) {
            register((idx, !opt_unset), &mut abbrev, &mut ambiguous);
        }
    }

    if let (Some((ai, aunset)), Some((bi, bunset))) = (ambiguous, abbrev) {
        eprintln!(
            "error: ambiguous option: {arg} (could be --{}{} or --{}{})",
            if aunset { "no-" } else { "" },
            options[ai].long_name.as_deref().unwrap_or(""),
            if bunset { "no-" } else { "" },
            options[bi].long_name.as_deref().unwrap_or(""),
        );
        return LongResult::Help;
    }
    if let Some((idx, sense)) = abbrev {
        // `if (*arg_end) p->opt = arg_end + 1;`
        if eq < arg.len() {
            ctx.opt = Some(arg[eq + 1..].to_string());
        }
        return get_value_long(ctx, options, idx, sense, hit);
    }
    LongResult::Unknown
}

fn get_value_long(
    ctx: &mut PoCtx,
    options: &[PoOption],
    idx: usize,
    unset: bool,
    hit: &mut dyn FnMut(usize, Option<&str>, bool) -> Result<()>,
) -> LongResult {
    match get_value(ctx, options, idx, unset, hit) {
        ShortResult::Done => LongResult::Done,
        ShortResult::Error => LongResult::Error,
        ShortResult::Unknown => LongResult::Unknown,
    }
}

/// `get_value()`'s `OPTION_CALLBACK` arm (`parse-options.c:236-259`) — the only one
/// `cmd_parseopt()` builds:
///
/// ```c
/// if (unset)                                        p_unset = 1;
/// else if (opt->flags & PARSE_OPT_NOARG)            p_unset = 0;
/// else if (opt->flags & PARSE_OPT_OPTARG && !p->opt) p_unset = 0;
/// else if (get_arg(p, opt, flags, &arg))            return -1;
/// else { p_unset = 0; p_arg = arg; }
/// ```
fn get_value(
    ctx: &mut PoCtx,
    options: &[PoOption],
    idx: usize,
    unset: bool,
    hit: &mut dyn FnMut(usize, Option<&str>, bool) -> Result<()>,
) -> ShortResult {
    let o = &options[idx];
    let (arg, p_unset) = if unset {
        (None, true)
    } else if o.flags & PO_NOARG != 0 {
        (None, false)
    } else if o.flags & PO_OPTARG != 0 && ctx.opt.is_none() {
        (None, false)
    } else {
        // `get_arg()` (`parse-options.c:47-62`): the attached value, else the next
        // argument, else `%s requires a value`.
        match ctx.opt.take() {
            Some(v) => (Some(v), false),
            None => {
                if ctx.at + 1 < ctx.argv.len() {
                    ctx.at += 1;
                    (Some(ctx.argv[ctx.at].clone()), false)
                } else {
                    eprintln!("error: {} requires a value", optname(o, unset));
                    return ShortResult::Error;
                }
            }
        }
    };
    if hit(idx, arg.as_deref(), p_unset).is_err() {
        return ShortResult::Error;
    }
    ShortResult::Done
}

/// `optname()` (`parse-options.c:30-45`).
fn optname(o: &PoOption, unset: bool) -> String {
    match (&o.long_name, o.short_name) {
        (None, Some(c)) => format!("switch `{c}'"),
        (Some(name), _) if unset => format!("option `no-{name}'"),
        (Some(name), _) => format!("option `{name}'"),
        (None, None) => String::new(),
    }
}

/// `usage_with_options_internal()` (`parse-options.c:1312-1479`): the usage lines,
/// then one padded row per option, then a trailing blank line. `ctx_flags` carries
/// `PARSE_OPT_SHELL_EVAL`, which wraps the whole block in a `cat <<\EOF` heredoc —
/// and only ever on the stdout (`-h`) path, never on the stderr (error) one.
fn render_usage(
    out: &mut impl Write,
    usage: &[String],
    opts: &[PoOption],
    ctx_flags: u32,
) -> Result<()> {
    const USAGE_OPTS_WIDTH: usize = 26;
    let shell_eval = ctx_flags & CTX_SHELL_EVAL != 0;
    if shell_eval {
        write!(out, "cat <<\\EOF\n")?;
    }

    let mut prefix = "usage: ";
    let mut saw_empty_line = false;
    for entry in usage {
        if !saw_empty_line && entry.is_empty() {
            saw_empty_line = true;
        }
        for (j, line) in entry.split('\n').enumerate() {
            if saw_empty_line && !line.is_empty() {
                writeln!(out, "    {line}")?;
            } else if saw_empty_line {
                writeln!(out)?;
            } else if j == 0 {
                writeln!(out, "{prefix}{line}")?;
            } else {
                writeln!(out, "{:width$}{line}", "", width = "usage: ".len())?;
            }
        }
        prefix = "   or: ";
    }

    let mut need_newline = true;
    for o in opts {
        if o.group {
            writeln!(out)?;
            need_newline = false;
            if !o.help.is_empty() {
                writeln!(out, "{}", o.help)?;
            }
            continue;
        }
        if o.flags & PO_HIDDEN != 0 {
            continue;
        }
        if need_newline {
            writeln!(out)?;
            need_newline = false;
        }

        let mut row = String::from("    ");
        if let Some(c) = o.short_name {
            row.push('-');
            row.push(c);
        }
        if o.long_name.is_some() && o.short_name.is_some() {
            row.push_str(", ");
        }
        let mut positive_name: Option<&str> = None;
        if let Some(name) = o.long_name.as_deref() {
            if o.flags & PO_NONEG != 0 {
                row.push_str(&format!("--{name}"));
            } else if let Some(stem) = name.strip_prefix("no-") {
                positive_name = Some(stem);
                row.push_str(&format!("--{name}"));
            } else {
                row.push_str(&format!("--[no-]{name}"));
            }
        }
        if o.flags & PO_NOARG == 0 {
            row.push_str(&usage_argh(o));
        }
        write!(out, "{row}")?;

        let mut pos = display_width(&row);
        let help = if o.help.is_empty() { "" } else { o.help.as_str() };
        let mut rest = help;
        loop {
            let (chunk, tail) = match rest.find('\n') {
                Some(at) => (&rest[..=at], &rest[at + 1..]),
                None => (rest, ""),
            };
            if chunk.is_empty() {
                break;
            }
            usage_padding(out, pos)?;
            out.write_all(chunk.as_bytes())?;
            pos = 0;
            if tail.is_empty() {
                break;
            }
            rest = tail;
        }
        writeln!(out)?;

        // `if (positive_name) { if (find_option_by_long_name(...)) continue; … }`
        if let Some(stem) = positive_name {
            if opts.iter().any(|c| c.long_name.as_deref() == Some(stem)) {
                continue;
            }
            let row = format!("    --{stem}");
            write!(out, "{row}")?;
            usage_padding(out, display_width(&row))?;
            writeln!(out, "opposite of --no-{stem}")?;
        }
    }
    writeln!(out)?;
    if shell_eval {
        write!(out, "EOF\n")?;
    }
    let _ = USAGE_OPTS_WIDTH;
    Ok(())
}

/// `usage_padding()` (`parse-options.c:1296-1302`): pad to column 26, or wrap onto
/// a fresh line indented to it when the option row is already at least that wide.
fn usage_padding(out: &mut impl Write, pos: usize) -> Result<()> {
    const USAGE_OPTS_WIDTH: usize = 26;
    if pos < USAGE_OPTS_WIDTH {
        write!(out, "{:width$}", "", width = USAGE_OPTS_WIDTH - pos)?;
    } else {
        write!(out, "\n{:width$}", "", width = USAGE_OPTS_WIDTH)?;
    }
    Ok(())
}

/// `usage_argh()` (`parse-options.c:1237-1286`). The `<>` decoration is dropped
/// when the hint is already punctuated — or missing, in which case the hint itself
/// is the literal `...`.
fn usage_argh(o: &PoOption) -> String {
    let literal = o.argh.as_deref().is_none_or(|a| a.contains(['(', ')', '<', '>', '[', ']', '|']));
    let argh = o.argh.as_deref().unwrap_or("...");
    if o.flags & PO_OPTARG != 0 {
        if o.long_name.is_some() {
            if literal {
                format!("[={argh}]")
            } else {
                format!("[=<{argh}>]")
            }
        } else if literal {
            format!("[{argh}]")
        } else {
            format!("[<{argh}>]")
        }
    } else if literal {
        format!(" {argh}")
    } else {
        format!(" <{argh}>")
    }
}

/// `utf8_fprintf()`'s return value: the *column* count of what was printed, which
/// is what the padding is measured against.
fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}
