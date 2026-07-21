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
//! `--symbolic`, `--symbolic-full-name`, `--git-dir`, `--show-toplevel`,
//! `--is-inside-work-tree`, `--is-bare-repository`, `--all`, `--branches`,
//! `--tags`, plus revision and path arguments. Every other option stock git
//! recognizes is rejected rather than ignored; options git does *not* recognize
//! are echoed, which is what git itself does with them.

use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

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
    "--show-object-format",
    "--show-ref-format",
    "--output-object-format",
    "--resolve-git-dir",
    "--git-path",
    "--shared-index-path",
    "--absolute-git-dir",
    "--git-common-dir",
    "--is-inside-git-dir",
    "--is-shallow-repository",
    "--show-cdup",
    "--show-prefix",
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
    let repo = match gix::discover(".") {
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
    let mut held: Option<(ObjectId, BString)> = None;
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
                        show_rev(&mut out, &repo, &o, &id, Some(echo.as_bstr()), Some(full.as_bstr()))?;
                    }
                }
                Opt::Unknown => {
                    if o.echo_flags {
                        emit(&mut out, arg.as_bytes())?;
                    }
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
            repo.rev_parse_single(arg.as_str()).ok().map(|id| id.detach())
        };

        match resolved {
            Some(id) => {
                if o.verify {
                    revs += 1;
                    held = Some((id, BString::from(arg.as_bytes())));
                } else {
                    show_rev(&mut out, &repo, &o, &id, Some(arg.as_bytes().as_bstr()), None)?;
                }
            }
            None => {
                // A range revspec (`a..b`, `a...b`) is not a single object, so it
                // fails `rev_parse_single` and lands here. Expand it at this
                // position before falling through to path handling.
                let range = if arg.is_empty() {
                    None
                } else {
                    repo.rev_parse(arg.as_str()).ok().and_then(|s| match s.detach() {
                        gix::revision::plumbing::Spec::Range { from, to } => {
                            Some(RangeSpec::Range { from, to })
                        }
                        gix::revision::plumbing::Spec::Merge { theirs, ours } => {
                            Some(RangeSpec::Merge { theirs, ours })
                        }
                        _ => None,
                    })
                };
                if let Some(range) = range {
                    emit_range(&mut out, &repo, &o, range)?;
                    // A range is never a single revision. Under `--verify`/`--short`
                    // the endpoints still print, but the scan then fails afterward
                    // with "Needed a single revision" (git prints them, then dies).
                    if o.verify {
                        revs += 2;
                    }
                    continue;
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
            Some((id, name)) if revs == 1 => {
                show_rev(&mut out, &repo, &o, &id, Some(name.as_bstr()), None)?;
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
}

#[derive(Clone, Copy)]
enum Query {
    GitDir,
    ShowToplevel,
    IsInsideWorkTree,
    IsBareRepository,
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
        "--show-toplevel" => return Ok(Opt::Query(Query::ShowToplevel)),
        "--is-inside-work-tree" => return Ok(Opt::Query(Query::IsInsideWorkTree)),
        "--is-bare-repository" => return Ok(Opt::Query(Query::IsBareRepository)),
        "--all" => return Ok(Opt::Refs(RefSet::All)),
        "--branches" => return Ok(Opt::Refs(RefSet::Branches)),
        "--tags" => return Ok(Opt::Refs(RefSet::Tags)),
        _ => {
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
            // git prints `$GIT_DIR` verbatim when set; otherwise `.git` when the
            // cwd is the top of the worktree, and an absolute path when it is not.
            if let Some(dir) = std::env::var_os("GIT_DIR") {
                emit(out, dir.as_os_str().as_encoded_bytes())?;
            } else {
                match toplevel(repo) {
                    Some(top) if is_cwd(&top) => emit(out, b".git")?,
                    Some(top) => emit(out, top.join(".git").as_os_str().as_encoded_bytes())?,
                    // Bare: git reports `.` when the cwd is the git dir itself.
                    None => match std::fs::canonicalize(repo.git_dir()) {
                        Ok(dir) if is_cwd(&dir) => emit(out, b".")?,
                        _ => emit(out, repo.git_dir().as_os_str().as_encoded_bytes())?,
                    },
                }
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
        Query::IsInsideWorkTree => {
            let inside_git_dir = std::env::current_dir()
                .ok()
                .and_then(|cwd| std::fs::canonicalize(cwd).ok())
                .zip(std::fs::canonicalize(repo.git_dir()).ok())
                .is_some_and(|(cwd, git)| cwd.starts_with(git));
            emit(out, yes_no(repo.workdir().is_some() && !inside_git_dir))?;
        }
        Query::IsBareRepository => emit(out, yes_no(repo.is_bare()))?,
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
) -> Result<()> {
    if o.abbrev_ref || o.sym == Sym::Full {
        let full = match known_full {
            Some(f) => Some(f.to_owned()),
            None => name.and_then(|n| dwim_full_name(repo, n)),
        };
        // A revision that names no ref prints nothing at all in these modes.
        if let Some(full) = full {
            if o.abbrev_ref {
                let short = <&gix::refs::FullNameRef>::try_from(full.as_bstr())
                    .map(|f| f.shorten().to_owned())
                    .unwrap_or_else(|_| full.clone());
                emit(out, &short)?;
            } else {
                emit(out, &full)?;
            }
        }
        return Ok(());
    }

    if o.sym == Sym::AsIs {
        if let Some(n) = name {
            emit(out, n)?;
            return Ok(());
        }
    }

    emit(out, render_id(repo, o, id)?)?;
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

/// git's `dwim_ref`: resolve a bare name to the full ref it designates, then
/// follow symbolic refs so `HEAD` reports the branch it is on. `None` when the
/// name is not a ref at all — which is how `HEAD^` prints nothing under
/// `--symbolic-full-name`.
fn dwim_full_name(repo: &gix::Repository, name: &BStr) -> Option<BString> {
    let name = name.to_str().ok()?;
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
