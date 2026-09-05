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
//! usage errors. Note that `-q` only silences "not a symbolic ref" — a name git
//! cannot resolve at all still dies with `No such ref` and `128`.
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

use super::{Arg, LongOpt};

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

/// `cmd_symbolic_ref()`'s `struct option options[]` (builtin/symbolic-ref.c), in
/// table order, as [`super::resolve_long`] reads it. No entry carries
/// `PARSE_OPT_NONEG`; `-m <reason>` is short-only and so has no entry.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "quiet", neg: true, arg: Arg::None },
    LongOpt { name: "delete", neg: true, arg: Arg::None },
    LongOpt { name: "short", neg: true, arg: Arg::None },
    LongOpt { name: "recurse", neg: true, arg: Arg::None },
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
        let resolved = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        let a = resolved.as_ref();
        match a {
            "--" => no_more_opts = true,
            // parse_options_step() answers `-h` on stdout at 129, ahead of
            // `usage_error()`'s stderr path for a rejection.
            // `--help-all` reaches the same renderer with USAGE_FULL, which this
            // table renders identically: it has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => return Ok(super::show_usage(USAGE)),
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
                opts.message = Some(super::value_at(args, i, a)?.to_string());
            }
            _ if a.starts_with("-m") => opts.message = Some(a[2..].to_string()),
            // A long name no entry claims is `PARSE_OPT_UNKNOWN`.
            _ if a.starts_with("--") => return Ok(super::unknown_option(a, USAGE)),
            // Every remaining `-<chars>` token, walked the way
            // `parse_options_step()` walks a short cluster
            // (parse-options.c:1061-1107): each character is its own option, `-m`
            // swallows the rest of the cluster or the next argv element, and the
            // first character the table does not claim is named on its own —
            // against the synthetic `-<rest>` the C builds at :1095. `git
            // symbolic-ref -qa` therefore reports `a`, where this used to report
            // the whole `qa` as a long option's name.
            _ => {
                for (off, c) in a.char_indices().skip(1) {
                    match c {
                        'q' => opts.quiet = true,
                        'd' => opts.delete = true,
                        'm' => {
                            let rest = &a[off + c.len_utf8()..];
                            opts.message = Some(match rest.is_empty() {
                                true => {
                                    i += 1;
                                    super::value_at(args, i, "-m")?.to_string()
                                }
                                false => rest.to_string(),
                            });
                            break;
                        }
                        'h' => return Ok(super::show_usage(USAGE)),
                        _ => return Ok(super::unknown_option(&format!("-{}", &a[off..]), USAGE)),
                    }
                }
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

    let repo = crate::setup::discover()?;

    // `core.preferSymlinkRefs` is read when the files ref store is created
    // (refs/files-backend.c:129), which every form of this command does — so an
    // unreadable value refuses the read form as much as the write form, and
    // refuses before any update is applied.
    let prefer_symlink =
        match crate::repo_settings::config_bool_strict(&repo, "core.prefersymlinkrefs") {
            Ok(v) => v.unwrap_or(false),
            Err(msg) => return fatal(&msg),
        };

    if opts.delete {
        delete_symref(&repo, positional[0])
    } else if positional.len() == 2 {
        set_symref(&repo, positional[0], positional[1], opts.message.as_deref(), prefer_symlink)
    } else {
        read_symref(&repo, positional[0], &opts)
    }
}

/// One-argument form: print what `name` points at.
fn read_symref(repo: &gix::Repository, name: &str, opts: &Opts) -> Result<ExitCode> {
    let resolved = match resolve_ref(repo, name, opts.recurse)? {
        Resolution::Symbolic(full) => full,
        Resolution::NotSymbolic => return not_a_symbolic_ref(name, opts.quiet),
        // git dies here whether or not `-q` was given: the ref could not be
        // resolved at all, which is a different failure from "not symbolic".
        Resolution::NoSuchRef => return fatal(&format!("No such ref: {name}")),
    };

    let out = if opts.short {
        shorten_unambiguous(repo, resolved.as_bstr())
    } else {
        resolved
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
    prefer_symlink: bool,
) -> Result<ExitCode> {
    if name == "HEAD" && !target.starts_with("refs/") {
        return fatal("Refusing to point HEAD outside of refs/");
    }
    // git refuses to make a *pseudoref* (MERGE_HEAD, FETCH_HEAD, ORIG_HEAD, …)
    // symbolic; HEAD is the one all-caps name it permits. `is_pseudoref_syntax`
    // in refs.c: a slash-free name whose every byte is upper-case / `_` / `-`.
    if is_pseudoref(name) {
        eprintln!("error: refusing to update pseudoref '{name}'");
        return Ok(ExitCode::from(1));
    }
    if gix::validate::reference::name_partial(BStr::new(target)).is_err() {
        return fatal(&format!("Refusing to set '{name}' to invalid ref '{target}'"));
    }

    let name_full = full_name(name)?;
    // `check_refname_format(argv[1], REFNAME_ALLOW_ONELEVEL)` (builtin/symbolic-ref.c:120)
    // is git's only constraint on the target, and the `name_partial` check above
    // is its port — so a slash-free lower-case target such as a stray 64-hex
    // string is a legal symref target that stock writes and reads back happily.
    // `FullName` is stricter than that (a one-level name has to be all upper
    // case, like `HEAD`), so a target gitoxide cannot spell is written straight
    // into the loose ref file instead of through the transaction.
    let Ok(target_full) = FullName::try_from(target) else {
        return set_symref_raw(repo, name_full.as_ref(), target, message, prefer_symlink);
    };

    // Capture the pre-edit resolution so the reflog line carries the same
    // `<old> <new>` pair git writes. A target that resolves to no object is
    // fine — git happily creates a dangling symref (`ref: does-not-exist`),
    // exit 0, and simply writes no reflog entry for it.
    let previous = leaf_object_id(repo, BStr::new(name))?;
    let new = leaf_object_id(repo, BStr::new(target)).unwrap_or(None);

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

    if prefer_symlink {
        write_ref_symlink(repo, name_full.as_ref(), target);
    }

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

/// `create_symref_locked()` for a target the reference transaction cannot carry.
///
/// ```c
/// if (!fdopen_lock_file(&lock->lk, "w"))
///         return error("unable to fdopen %s: %s", ...);
/// update_symref_reflog(refs, lock, refname, target, logmsg);
/// fprintf(get_lock_file_fp(&lock->lk), "ref: %s\n", target);
/// if (commit_ref(lock) < 0)
///         return error("unable to write symref for %s: %s", refname, strerror(errno));
/// ```
///
/// (refs/files-backend.c:1900-1921) — the file is the whole write, and the reflog
/// line only happens when the target resolves to an object, which a target this
/// path is reached for does not.
fn set_symref_raw(
    repo: &gix::Repository,
    name: &FullNameRef,
    target: &str,
    message: Option<&str>,
    prefer_symlink: bool,
) -> Result<ExitCode> {
    let previous = leaf_object_id(repo, name.as_bstr())?;

    // `files_ref_path()`: `HEAD` and the other per-worktree names live in the
    // worktree's git dir, `refs/…` in the common one.
    let base = match name.category() {
        Some(
            Category::PseudoRef
            | Category::Bisect
            | Category::Rewritten
            | Category::WorktreePrivate,
        ) => repo.git_dir(),
        _ => repo.common_dir(),
    };
    let path = base.join(gix::path::from_bstr(name.as_bstr()));
    let mut lock = gix::lock::File::acquire_to_update_resource(
        &path,
        gix::lock::acquire::Fail::Immediately,
        Some(base.to_path_buf()),
    )?;
    lock.with_mut(|file| writeln!(file, "ref: {target}"))?;
    lock.commit()?;

    if prefer_symlink {
        write_ref_symlink(repo, name, target);
    }

    if let Some(new) = leaf_object_id(repo, BStr::new(target)).unwrap_or(None) {
        append_reflog(repo, name, previous, &new, message.unwrap_or_default())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The deprecation notice `create_ref_symlink()` prints the first time it runs
/// (refs/files-backend.c:2108-2118), byte for byte. Only the first line carries
/// git's `warning: ` prefix; the rest spell their own `hint: `, and the line that
/// shows the fix is separated from it by a **tab**.
const SYMLINK_REFS_DEPRECATION: &str = "\
warning: 'core.preferSymlinkRefs=true' is nominated for removal.
hint: The use of symbolic links for symbolic refs is deprecated
hint: and will be removed in Git 3.0. The configuration that
hint: tells Git to use them is thus going away. You can unset
hint: it with:
hint:
hint:\tgit config unset core.preferSymlinkRefs
hint:
hint: Git will then use the textual symref format instead.";

/// `core.preferSymlinkRefs=true`: store the symbolic reference as a **symbolic
/// link** to its target instead of as a `ref: <name>` file.
///
/// Port of `create_ref_symlink()` (refs/files-backend.c:2094-2119):
///
/// ```c
/// ref_path = get_locked_file_path(&lock->lk);
/// unlink(ref_path);
/// ret = symlink(target, ref_path);
/// …
/// if (ret)
///         fprintf(stderr, "no symlink - falling back to symbolic ref\n");
/// ```
///
/// Note `get_locked_file_path` strips the `.lock` suffix, so git unlinks and
/// re-creates the *live* reference rather than swapping a lock file into place —
/// this write is not atomic upstream either. Here the textual form has already
/// been written by the transaction above, so it is read back first and restored if
/// the `symlink(2)` fails, which is the state git's fall-through leaves behind.
///
/// The deprecation warning is printed whether or not the link could be created,
/// exactly as upstream prints it after the `if (ret)` fallback message.
///
/// Reading such a reference back is handled in the vendored `gix-ref`
/// (`store/file/find.rs`'s `symlink_ref_contents`), so a repository written this
/// way — by this port or by stock git — resolves identically either side.
fn write_ref_symlink(repo: &gix::Repository, name: &FullNameRef, target: &str) {
    // `HEAD` lives in the per-worktree git dir, `refs/…` in the common dir; take
    // whichever one the transaction just wrote to.
    let relative = gix::path::from_byte_slice(name.as_bstr());
    let candidates = [repo.git_dir().join(relative), repo.common_dir().join(relative)];
    let Some(ref_path) = candidates.into_iter().find(|p| p.symlink_metadata().is_ok()) else {
        eprintln!("{SYMLINK_REFS_DEPRECATION}");
        eprintln!("no symlink - falling back to symbolic ref");
        return;
    };

    let textual = std::fs::read(&ref_path).ok();
    let _ = std::fs::remove_file(&ref_path);
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(target, &ref_path);
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(target, &ref_path);
    if let Err(_err) = linked {
        // Restore what the transaction wrote; git reaches the same state by
        // letting the textual write proceed after the failed `symlink()`.
        if let Some(textual) = textual {
            let _ = std::fs::write(&ref_path, textual);
        }
        eprintln!("no symlink - falling back to symbolic ref");
    }
    eprintln!("{SYMLINK_REFS_DEPRECATION}");
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
            message: Default::default(),
        },
        name: full_name(name)?,
        deref: false,
    })?;
    Ok(ExitCode::SUCCESS)
}

/// The pseudorefs git's `symbolic-ref` refuses to point elsewhere. Determined
/// empirically against git 2.55: only `MERGE_HEAD` and `FETCH_HEAD` are refused
/// ("refusing to update pseudoref"); other all-caps names — `ORIG_HEAD`,
/// `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `BISECT_HEAD`, … — are written normally.
fn is_pseudoref(name: &str) -> bool {
    matches!(name, "MERGE_HEAD" | "FETCH_HEAD")
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

/// The outcome of git's `resolve_ref_unsafe` as far as `check_symref` cares.
enum Resolution {
    /// The chain ended at this name, and `REF_ISSYMREF` was seen along the way.
    /// The flag word is cumulative in git, so one symbolic hop anywhere makes the
    /// whole resolution symbolic.
    Symbolic(BString),
    /// The name resolved without ever traversing a symbolic ref — including the
    /// case where it does not exist at all, which git reports the same way.
    NotSymbolic,
    /// `resolve_ref_unsafe` returned `NULL`: an unusable name, an unusable
    /// symbolic target, or `SYMREF_MAXDEPTH` indirections without termination.
    NoSuchRef,
}

/// Port of git's `refs_werrres_ref_unsafe` loop for the flags `symbolic-ref`
/// passes (`0`, or `RESOLVE_REF_NO_RECURSE`). Each iteration reads exactly one
/// reference, so the whole resolution must terminate within `SYMREF_MAXDEPTH`
/// reads — counting the starting name.
fn resolve_ref(repo: &gix::Repository, name: &str, recurse: bool) -> Result<Resolution> {
    if !valid_refname(name) {
        return Ok(Resolution::NoSuchRef);
    }

    let mut current = BString::from(name);
    let mut saw_symref = false;
    for _ in 0..SYMREF_MAXDEPTH {
        // A ref file whose body will not parse is `REF_ISBROKEN`, and
        // `refs_werrres_ref_unsafe()` returns NULL for it — the same NULL an
        // unusable name produces, which is `NoSuchRef` and not "absent". This
        // is what makes `symbolic-ref HEAD` die with `No such ref: HEAD` over a
        // branch file the repository's hash width cannot read.
        let found = match find_exact(repo, current.as_bstr()) {
            Ok(found) => found,
            Err(e) if is_unparsable_ref(&e) => return Ok(Resolution::NoSuchRef),
            Err(e) => return Err(e),
        };
        let Some(reference) = found else {
            // A missing ref is not a failure here: git hands the name back with
            // a null id, which is how a dangling symref still prints its target.
            return Ok(terminal(saw_symref, current));
        };
        match reference.target {
            Target::Object(_) => return Ok(terminal(saw_symref, current)),
            Target::Symbolic(next) => {
                saw_symref = true;
                current = next.as_bstr().to_owned();
                if !recurse {
                    return Ok(Resolution::Symbolic(current));
                }
                // git re-validates every target it steps onto and gives up when
                // one is unusable, since `RESOLVE_REF_ALLOW_BAD_NAME` is unset.
                let Ok(next) = current.to_str() else {
                    return Ok(Resolution::NoSuchRef);
                };
                if !valid_refname(next) {
                    return Ok(Resolution::NoSuchRef);
                }
            }
        }
    }
    // ELOOP.
    Ok(Resolution::NoSuchRef)
}

fn terminal(saw_symref: bool, name: BString) -> Resolution {
    if saw_symref {
        Resolution::Symbolic(name)
    } else {
        Resolution::NotSymbolic
    }
}

/// Port of `check_refname_component`. `*` is always rejected because
/// `REFNAME_REFSPEC_PATTERN` is never set on this path.
fn valid_refname_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    let mut last = 0u8;
    for &ch in bytes {
        match ch {
            b'\0'..=b'\x1F' | b'\x7F' | b' ' | b'~' | b'^' | b':' | b'?' | b'[' | b'\\' | b'*' => {
                return false
            }
            b'.' if last == b'.' => return false,
            b'{' if last == b'@' => return false,
            _ => {}
        }
        last = ch;
    }
    // A zero-length component covers the empty name, a leading or trailing
    // slash, and a doubled slash.
    !bytes.is_empty() && bytes[0] != b'.' && !component.ends_with(".lock")
}

/// Port of `check_refname_format(refname, REFNAME_ALLOW_ONELEVEL)`. Only the
/// last component is checked for a trailing dot, matching git — `refs/heads./x`
/// is a legal name.
fn valid_refname(name: &str) -> bool {
    if name == "@" {
        return false;
    }
    let mut last = "";
    for component in name.split('/') {
        if !valid_refname_component(component) {
            return false;
        }
        last = component;
    }
    !last.ends_with('.')
}

/// The object id stored in the leaf of `name`'s symref chain, if any. This is
/// the raw id of the terminal reference — annotated tags are not peeled, which
/// is what git records in the reflog.
pub(super) fn leaf_object_id(repo: &gix::Repository, name: &BStr) -> Result<Option<ObjectId>> {
    let mut current = name.to_owned();
    for _ in 0..=SYMREF_MAXDEPTH {
        // A leaf whose file will not parse resolves to nothing, exactly as one
        // that is not there does. `lock_ref_oid_basic()` reads the old value
        // through `refs_resolve_ref_unsafe()`, which returns NULL for a broken
        // ref without failing the transaction — so writing `HEAD` over a branch
        // whose file cannot be read still succeeds, and simply records no
        // reflog line (git leaves `.git/logs/HEAD` untouched there).
        let found = match find_exact(repo, current.as_bstr()) {
            Ok(found) => found,
            Err(e) if is_unparsable_ref(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
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
///
/// Shared with `remote rename`, which has to write the line itself: git renames the ref in
/// place and logs `<id> <id> … remote: renamed …`, while gitoxide can only create the ref
/// anew and would open the line with the null id.
pub(super) fn append_reflog(
    repo: &gix::Repository,
    name: &FullNameRef,
    previous: Option<ObjectId>,
    new: &ObjectId,
    message: &str,
) -> Result<()> {
    use gix::refs::store::WriteReflog;

    // `log_ref_setup()` (refs/files-backend.c:1859) only lets the policy decide whether a
    // *missing* log may be created; when it may not, the file is still opened `O_APPEND` and
    // an existing log gains the line. `core.logAllRefUpdates = false` therefore keeps
    // appending to logs that already exist.
    let force_create = match repo.refs.write_reflog {
        WriteReflog::Disable => false,
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

/// git's default `core.logAllRefUpdates` set: `HEAD` plus the branch, remote and
/// note ref hierarchies — `should_autocreate_reflog()`'s `LOG_REFS_NORMAL` arm
/// (refs.c:1062). `refs/worktree/` is not in it, so a worktree-private ref never
/// gets a log created for it.
fn auto_creates_reflog(name: &FullNameRef) -> bool {
    let name = name.as_bstr();
    name == BStr::new("HEAD")
        || name.starts_with(b"refs/heads/")
        || name.starts_with(b"refs/remotes/")
        || name.starts_with(b"refs/notes/")
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

/// Whether a ref lookup failed because the ref file's body would not parse —
/// git's *broken ref*, which `refs_resolve_ref_unsafe()` reports as an absence
/// rather than as a failure.
fn is_unparsable_ref(err: &anyhow::Error) -> bool {
    use gix::refs::file::loose::reference::decode::Error as DecodeError;
    err.chain().any(|cause| {
        cause
            .downcast_ref::<DecodeError>()
            .is_some_and(|d| matches!(d, DecodeError::Parse { .. }))
    })
}
