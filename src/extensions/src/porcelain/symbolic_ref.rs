//! `git symbolic-ref` — read, write and delete symbolic references.
//!
//! All three documented forms are implemented on top of gitoxide's reference
//! store:
//!
//!   * `symbolic-ref [-q] [--short] [--recurse|--no-recurse] <name>` — print the
//!     ref `<name>` points at.
//!   * `symbolic-ref [-m <reason>] <name> <ref>` — create or update `<name>`.
//!   * `symbolic-ref --delete [-q] <name>` — remove a symbolic ref.
//!
//! Exit codes and stdout bytes match stock git: `0` on success, `1` for the
//! quiet "not a symbolic ref" case, `128` for the `fatal:` paths and `129` for
//! usage errors.
//!
//! Not covered: symbolic targets that are not fully-qualified reference names
//! (git accepts `git symbolic-ref FOO bar`, gitoxide's `FullName` does not), and
//! reflog placement for refs addressed through the `main-worktree/` and
//! `worktrees/<id>/` namespaces. Both `bail!` rather than write a diverging
//! repository state.

use anyhow::{anyhow, bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{Category, FullName, FullNameRef, Target};

/// git's `SYMREF_MAXDEPTH` — the number of indirections `resolve_ref_unsafe`
/// follows before giving up.
const SYMREF_MAXDEPTH: usize = 5;

/// `ref_rev_parse_rules` as `(prefix, suffix)` pairs. A rule matches a refname
/// when the name carries `prefix`; the suffix is only used when *building* a
/// candidate name, mirroring `sscanf`, whose `%s` swallows the remainder and
/// never enforces the trailing literal.
const REV_PARSE_RULES: [(&str, &str); 6] = [
    ("", ""),
    ("refs/", ""),
    ("refs/tags/", ""),
    ("refs/heads/", ""),
    ("refs/remotes/", ""),
    ("refs/remotes/", "/HEAD"),
];

/// The usage block stock git prints for every argument error, verbatim.
const USAGE: &str = "\
usage: git symbolic-ref [-m <reason>] <name> <ref>
   or: git symbolic-ref [-q] [--short] [--no-recurse] <name>
   or: git symbolic-ref --delete [-q] <name>

    -q, --[no-]quiet      suppress error message for non-symbolic (detached) refs
    -d, --[no-]delete     delete symbolic ref
    --[no-]short          shorten ref output
    --[no-]recurse        recursively dereference (default)
    -m <reason>           reason of the update
";

/// Parsed command line for one invocation.
struct Opts {
    quiet: bool,
    short: bool,
    recurse: bool,
    delete: bool,
    message: Option<String>,
}

pub fn symbolic_ref(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand appearing at index 0 — dispatch strips it, but the
    // contract is stated both ways and a leading literal `symbolic-ref` can never
    // be a valid `<name>` (gitoxide rejects lower-case one-level ref names).
    let args = match args.first() {
        Some(first) if first == "symbolic-ref" => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        quiet: false,
        short: false,
        recurse: true,
        delete: false,
        message: None,
    };
    let mut positional: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || a == "-" || !a.starts_with('-') {
            positional.push(a);
            i += 1;
            continue;
        }
        match a {
            "--" => no_more_opts = true,
            "-q" | "--quiet" => opts.quiet = true,
            "--no-quiet" => opts.quiet = false,
            "--short" => opts.short = true,
            "--no-short" => opts.short = false,
            "--recurse" => opts.recurse = true,
            "--no-recurse" => opts.recurse = false,
            "-d" | "--delete" => opts.delete = true,
            "--no-delete" => opts.delete = false,
            "-m" => {
                i += 1;
                let Some(reason) = args.get(i) else {
                    return usage_error(Some("switch `m' requires a value"));
                };
                opts.message = Some(reason.clone());
            }
            _ if a.starts_with("-m") => opts.message = Some(a[2..].to_string()),
            _ => {
                let name = a.trim_start_matches('-');
                return usage_error(Some(format!("unknown option `{name}'").as_str()));
            }
        }
        i += 1;
    }

    // git's parse_options arity checks, which precede any repository access.
    if opts.delete {
        if positional.len() != 1 {
            return usage_error(None);
        }
    } else if positional.is_empty() || positional.len() > 2 {
        return usage_error(None);
    }

    let repo = gix::discover(".")?;

    if opts.delete {
        delete_symref(&repo, positional[0])
    } else if positional.len() == 2 {
        set_symref(&repo, positional[0], positional[1], opts.message.as_deref())
    } else {
        read_symref(&repo, positional[0], &opts)
    }
}

/// One-argument form: print what `name` points at.
fn read_symref(repo: &gix::Repository, name: &str, opts: &Opts) -> Result<ExitCode> {
    let Some(first) = symbolic_target(repo, BStr::new(name))? else {
        return not_a_symbolic_ref(name, opts.quiet);
    };

    let resolved = if opts.recurse {
        match resolve_chain(repo, first)? {
            Some(full) => full,
            // Exceeded SYMREF_MAXDEPTH: git's resolver yields nothing, and the
            // caller reports the same "not a symbolic ref" failure.
            None => return not_a_symbolic_ref(name, opts.quiet),
        }
    } else {
        first
    };

    let out = if opts.short {
        shorten_unambiguous(repo, resolved.as_bstr())
    } else {
        resolved.as_bstr().to_owned()
    };

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.write_all(b"\n")?;
    Ok(ExitCode::SUCCESS)
}

/// Two-argument form: point `name` at `target`, recording a reflog entry the way
/// git does (only when the new target resolves to an object).
fn set_symref(
    repo: &gix::Repository,
    name: &str,
    target: &str,
    message: Option<&str>,
) -> Result<ExitCode> {
    if name == "HEAD" && !target.starts_with("refs/") {
        return fatal("Refusing to point HEAD outside of refs/");
    }
    if gix::validate::reference::name_partial(BStr::new(target)).is_err() {
        return fatal(&format!("Refusing to set '{name}' to invalid ref '{target}'"));
    }

    let name_full = full_name(name)?;
    let target_full = full_name(target)?;

    // Capture the pre-edit resolution so the reflog line carries the same
    // `<old> <new>` pair git writes.
    let previous = leaf_object_id(repo, BStr::new(name))?;
    let new = leaf_object_id(repo, BStr::new(target))?;

    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: BString::default(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(target_full),
        },
        name: name_full.clone(),
        deref: false,
    })?;

    // gitoxide deliberately writes no reflog for symbolic-target updates, so the
    // entry git would have produced is appended here.
    if let Some(new) = new {
        append_reflog(
            repo,
            name_full.as_ref(),
            previous,
            &new,
            message.unwrap_or_default(),
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `--delete` form. Refuses `HEAD`, and anything that is not a symbolic ref —
/// both with git's exact wording and exit code, `-q` notwithstanding.
fn delete_symref(repo: &gix::Repository, name: &str) -> Result<ExitCode> {
    if name == "HEAD" {
        return fatal("deleting 'HEAD' is not allowed");
    }
    let Some(target) = symbolic_target(repo, BStr::new(name))? else {
        return fatal(&format!("Cannot delete {name}, not a symbolic ref"));
    };

    repo.edit_reference(RefEdit {
        change: Change::Delete {
            expected: PreviousValue::MustExistAndMatch(Target::Symbolic(target)),
            log: RefLog::AndReference,
        },
        name: full_name(name)?,
        deref: false,
    })?;
    Ok(ExitCode::SUCCESS)
}

/// The direct symbolic target of `name`, or `None` when the ref is missing or
/// holds an object id.
fn symbolic_target(repo: &gix::Repository, name: &BStr) -> Result<Option<FullName>> {
    let Some(reference) = find_exact(repo, name)? else {
        return Ok(None);
    };
    Ok(match reference.target {
        Target::Symbolic(full) => Some(full),
        Target::Object(_) => None,
    })
}

/// Follow a chain of symbolic refs to the last name in it — the first one that
/// stores an object id, or the first one that does not exist (git reports the
/// dangling name rather than failing). `None` once `SYMREF_MAXDEPTH` is hit.
fn resolve_chain(repo: &gix::Repository, first: FullName) -> Result<Option<FullName>> {
    let mut current = first;
    for _ in 0..SYMREF_MAXDEPTH {
        let found = find_exact(repo, current.as_bstr())?;
        match found {
            Some(reference) => match reference.target {
                Target::Symbolic(next) => current = next,
                Target::Object(_) => return Ok(Some(current)),
            },
            None => return Ok(Some(current)),
        }
    }
    Ok(None)
}

/// The object id stored in the leaf of `name`'s symref chain, if any. This is
/// the raw id of the terminal reference — annotated tags are not peeled, which
/// is what git records in the reflog.
fn leaf_object_id(repo: &gix::Repository, name: &BStr) -> Result<Option<ObjectId>> {
    let mut current = name.to_owned();
    for _ in 0..=SYMREF_MAXDEPTH {
        let found = find_exact(repo, current.as_bstr())?;
        match found {
            Some(reference) => match reference.target {
                Target::Object(id) => return Ok(Some(id)),
                Target::Symbolic(next) => current = next.as_bstr().to_owned(),
            },
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// Look a reference up by its exact full name.
///
/// The store's `try_find` applies git's rev-parse search rules (so `main` would
/// resolve to `refs/heads/main`); `symbolic-ref` addresses refs literally, so
/// the name that came back is compared against the one asked for.
fn find_exact(repo: &gix::Repository, name: &BStr) -> Result<Option<gix::refs::Reference>> {
    let Ok(name) = name.to_str() else {
        return Ok(None);
    };
    let found = match repo.refs.try_find(name) {
        Ok(found) => found,
        // An unusable name simply names nothing.
        Err(gix::refs::file::find::Error::RefnameValidation(_)) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    Ok(found.filter(|reference| reference.name.as_bstr() == BStr::new(name)))
}

/// Whether a reference with exactly this name exists.
fn ref_exists(repo: &gix::Repository, name: &str) -> bool {
    matches!(find_exact(repo, BStr::new(name)), Ok(Some(_)))
}

/// git's `shorten_unambiguous_ref` (non-strict): find the longest well-known
/// prefix whose removal leaves a name that no higher-priority rev-parse rule
/// would resolve to a different, existing ref. Falls back to the full name.
fn shorten_unambiguous(repo: &gix::Repository, refname: &BStr) -> BString {
    let Ok(refname) = refname.to_str() else {
        return refname.to_owned();
    };

    // Rule 0 is the identity rule and always matches, so it is never a candidate.
    for i in (1..REV_PARSE_RULES.len()).rev() {
        let (prefix, _) = REV_PARSE_RULES[i];
        let Some(short) = refname.strip_prefix(prefix) else {
            continue;
        };
        if short.is_empty() {
            continue;
        }
        let ambiguous = REV_PARSE_RULES[..i]
            .iter()
            .any(|(p, s)| ref_exists(repo, &format!("{p}{short}{s}")));
        if !ambiguous {
            return short.into();
        }
    }
    refname.into()
}

/// Append one reflog line for `name`, following git's rules for which refs get a
/// log auto-created.
fn append_reflog(
    repo: &gix::Repository,
    name: &FullNameRef,
    previous: Option<ObjectId>,
    new: &ObjectId,
    message: &str,
) -> Result<()> {
    use gix::refs::store::WriteReflog;

    let force_create = match repo.refs.write_reflog {
        WriteReflog::Disable => return Ok(()),
        WriteReflog::Always => true,
        WriteReflog::Normal => auto_creates_reflog(name),
    };

    let base = match name.category() {
        Some(Category::PseudoRef | Category::Bisect | Category::Rewritten | Category::WorktreePrivate) => {
            repo.git_dir()
        }
        Some(Category::MainPseudoRef | Category::MainRef)
        | Some(Category::LinkedPseudoRef { .. } | Category::LinkedRef { .. }) => {
            bail!("reflogs for worktree-qualified ref {:?} are not supported", name.as_bstr())
        }
        _ => repo.common_dir(),
    };
    let path = base.join("logs").join(gix::path::from_bstr(name.as_bstr()));

    let mut options = std::fs::OpenOptions::new();
    options.append(true).read(false);
    if force_create {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        options.create(true);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        // No log exists and this ref does not get one created: git writes nothing.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    let committer = repo
        .committer()
        .transpose()?
        .ok_or_else(|| anyhow!("committer identity is not configured (user.name / user.email)"))?;

    let previous = previous.unwrap_or_else(|| new.kind().null());
    write!(file, "{previous} {new} ")?;
    committer.trim().write_to(&mut file)?;
    if message.is_empty() {
        writeln!(file)?;
    } else {
        writeln!(file, "\t{message}")?;
    }
    Ok(())
}

/// git's default `core.logAllRefUpdates` set: `HEAD` plus the branch, remote,
/// note and worktree ref hierarchies.
fn auto_creates_reflog(name: &FullNameRef) -> bool {
    let name = name.as_bstr();
    name == BStr::new("HEAD")
        || name.starts_with(b"refs/heads/")
        || name.starts_with(b"refs/remotes/")
        || name.starts_with(b"refs/notes/")
        || name.starts_with(b"refs/worktree/")
}

/// Convert a literal ref name into a `FullName`, which is what the reference
/// transaction requires.
fn full_name(name: &str) -> Result<FullName> {
    FullName::try_from(name)
        .map_err(|e| anyhow!("cannot address reference {name:?} through gitoxide: {e}"))
}

/// Report a `fatal:` message on stderr and yield git's exit code for it.
fn fatal(message: &str) -> Result<ExitCode> {
    eprintln!("fatal: {message}");
    Ok(ExitCode::from(128))
}

/// The shared failure for the read path: loud with `fatal:` and 128, or silent
/// with 1 under `-q`.
fn not_a_symbolic_ref(name: &str, quiet: bool) -> Result<ExitCode> {
    if quiet {
        return Ok(ExitCode::from(1));
    }
    fatal(&format!("ref {name} is not a symbolic ref"))
}

/// git's argument-error path: an optional `error:` line, the usage block, 129.
fn usage_error(error: Option<&str>) -> Result<ExitCode> {
    if let Some(error) = error {
        eprintln!("error: {error}");
    }
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}
