//! `git replace` — create, list and delete refs under `refs/replace/`.
//!
//! Covered, following `builtin/replace.c` step for step:
//!   * `git replace [-f] <object> <replacement>` — the same-type check, the
//!     "already exists" check, and the ref write with git's `<old-oid>`
//!     constraint (must-not-exist when new, must-match when `-f` overwrites).
//!   * `git replace -d <object>...` — resolves each name, reports
//!     `replace ref '<hex>' not found` for the ones without a ref, deletes the
//!     rest and prints `Deleted replace ref '<hex>'`.
//!   * `git replace [--format=<format>] [-l [<pattern>]]` — the `short`,
//!     `medium` and `long` formats, byte-for-byte, with the pattern matched by a
//!     port of `wildmatch(pattern, refname, 0)` (`*`, `?`, `[...]`, `\`).
//!   * `git replace [-f] --graft <commit> [<parent>...]` — splices new `parent`
//!     header lines into the raw commit buffer at exactly the offsets git uses,
//!     strips a `gpgsig`/`gpgsig-sha256` header (with git's two warnings),
//!     writes the commit and replaces the original with it.
//!   * `git replace [-f] --convert-graft-file` — reads the graft file
//!     (`$GIT_GRAFT_FILE`, else `<common-dir>/info/grafts`), runs each line
//!     through `create_graft` in git's `gentle` mode, and unlinks the file when
//!     every line converted; otherwise reports git's `could not convert the
//!     following graft(s)` warning and exits 1.
//!   * `-f`/`--force`/`--no-force`, `--raw`/`--no-raw` and `-h`, plus git's
//!     option/cmdmode validation (`--format cannot be used when not listing`,
//!     `-f only makes sense when writing a replacement`, `--raw only makes sense
//!     with --edit`, `-d needs at least one argument`, `bad number of
//!     arguments`, `-e needs exactly one argument`, `--convert-graft-file takes
//!     no argument`, `only one pattern can be given with -l`, and the
//!     `options '<a>' and '<b>' cannot be used together` conflict).
//!
//! Not covered, and refused rather than approximated:
//!   * `--edit`/`-e` — spawns `$GIT_EDITOR` on a pretty-printed object and
//!     re-parses the result; interactive, no substrate for it here. The argument
//!     count is still validated exactly as git does before the refusal.
//!   * `--graft` on a commit carrying a `mergetag` header — git's
//!     `check_mergetags` re-hashes and parses each mergetag to decide whether it
//!     is discarded, which needs tag parsing this port does not do; refused
//!     instead of silently dropping the mergetag.
//!   * `core.graftFile` — git 2.55 does not honour it either (only
//!     `$GIT_GRAFT_FILE` and the default path), so neither does this.
//!   * `GIT_REPLACE_REF_BASE` — the namespace is always `refs/replace/`.
//!
//! Exit codes follow git: 0 on success, 129 for usage errors, 1 when `-d` or
//! `--convert-graft-file` had a failure, 128 for `die()`, and 255 for every
//! `return error(...)` path (git's `cmd_replace` returns -1, which `git.c`
//! truncates to 255).
//!
//! `args` excludes the `replace` verb itself — `dispatch::run` is handed
//! `&argv[2..]` — so option scanning starts at index 0.

use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::{Kind, Write as _};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

/// The namespace every replace ref lives in.
const REPLACE_BASE: &str = "refs/replace/";

/// `git replace`'s usage block, verbatim, including the trailing blank line.
const USAGE: &str = "\
usage: git replace [-f] <object> <replacement>
   or: git replace [-f] --edit <object>
   or: git replace [-f] --graft <commit> [<parent>...]
   or: git replace [-f] --convert-graft-file
   or: git replace -d <object>...
   or: git replace [--format=<format>] [-l [<pattern>]]

    -l, --list            list replace refs
    -d, --delete          delete replace refs
    -e, --edit            edit existing object
    -g, --graft           change a commit's parents
    --convert-graft-file  convert existing graft file
    -f, --[no-]force      replace the ref if it exists
    --[no-]raw            do not pretty-print contents for --edit
    --[no-]format <format>
                          use this format

";

/// The mutually exclusive command modes, mirroring git's `OPT_CMDMODE` set.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    Delete,
    Edit,
    Graft,
    ConvertGraftFile,
    Replace,
}

/// The `--format` values git accepts when listing.
#[derive(Clone, Copy)]
enum Format {
    Short,
    Medium,
    Long,
}

/// `git replace` — see the module docs for the covered surface.
pub fn replace(args: &[String]) -> Result<ExitCode> {
    let mut force = false;
    let mut raw = false;
    let mut format: Option<String> = None;
    // The chosen cmdmode plus the exact spelling it was given as, which git
    // quotes back in its "cannot be used together" message.
    let mut cmdmode: Option<(Mode, String)> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut end_of_opts = false;

    // Select the cmdmode, or bail out with git's `OPT_CMDMODE` conflict error
    // (an `error:` line and exit 129) when a different one is already set.
    macro_rules! cmdmode {
        ($m:expr, $spelling:expr) => {{
            let clash = match &cmdmode {
                Some((prev, prev_spelling)) if *prev != $m => Some(prev_spelling.clone()),
                _ => None,
            };
            if let Some(prev_spelling) = clash {
                eprintln!(
                    "error: options '{}' and '{}' cannot be used together",
                    $spelling, prev_spelling
                );
                return Ok(ExitCode::from(129));
            }
            cmdmode = Some(($m, $spelling.to_string()));
        }};
    }

    // `args[0]` is the first option/operand, not the verb: see the module docs.
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts || a == "-" || !a.starts_with('-') {
            positionals.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "list" => cmdmode!(Mode::List, "--list"),
                "delete" => cmdmode!(Mode::Delete, "--delete"),
                "edit" => cmdmode!(Mode::Edit, "--edit"),
                "graft" => cmdmode!(Mode::Graft, "--graft"),
                "convert-graft-file" => {
                    cmdmode!(Mode::ConvertGraftFile, "--convert-graft-file")
                }
                "force" => force = true,
                "no-force" => force = false,
                "raw" => raw = true,
                "no-raw" => raw = false,
                "no-format" => format = None,
                "help" => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                "format" => {
                    i += 1;
                    let Some(v) = args.get(i) else {
                        // parse-options' `opterror`: an `error:` line, the usage
                        // block, exit 129.
                        eprintln!("error: option `format' requires a value");
                        eprint!("{USAGE}");
                        return Ok(ExitCode::from(129));
                    };
                    format = Some(v.clone());
                }
                _ if long.starts_with("format=") => {
                    format = Some(long["format=".len()..].to_string());
                }
                _ => return unknown_option(a),
            }
            i += 1;
            continue;
        }
        // Grouped short flags, e.g. `-lf`.
        for c in a[1..].chars() {
            match c {
                'l' => cmdmode!(Mode::List, "-l"),
                'd' => cmdmode!(Mode::Delete, "-d"),
                'e' => cmdmode!(Mode::Edit, "-e"),
                'g' => cmdmode!(Mode::Graft, "-g"),
                'f' => force = true,
                'h' => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                _ => return unknown_option(&format!("-{c}")),
            }
        }
        i += 1;
    }

    // No explicit mode: replacing when there are arguments, listing otherwise.
    let mode = match &cmdmode {
        Some((m, _)) => *m,
        None => {
            if positionals.is_empty() {
                Mode::List
            } else {
                Mode::Replace
            }
        }
    };

    if format.is_some() && !matches!(mode, Mode::List) {
        return usage_msg_opt("--format cannot be used when not listing");
    }
    if force
        && !matches!(
            mode,
            Mode::Replace | Mode::Edit | Mode::Graft | Mode::ConvertGraftFile
        )
    {
        return usage_msg_opt("-f only makes sense when writing a replacement");
    }
    if raw && !matches!(mode, Mode::Edit) {
        return usage_msg_opt("--raw only makes sense with --edit");
    }

    match mode {
        Mode::Delete => {
            if positionals.is_empty() {
                return usage_msg_opt("-d needs at least one argument");
            }
            delete_replace_refs(&positionals)
        }
        Mode::Replace => {
            if positionals.len() != 2 {
                return usage_msg_opt("bad number of arguments");
            }
            replace_object(&positionals[0], &positionals[1], force)
        }
        Mode::List => {
            if positionals.len() > 1 {
                return usage_msg_opt("only one pattern can be given with -l");
            }
            list_replace_refs(positionals.first().map(String::as_str), format.as_deref())
        }
        Mode::Graft => {
            if positionals.is_empty() {
                return usage_msg_opt("-g needs at least one argument");
            }
            let repo = gix::discover(".")?;
            // The lock is held for the whole graft: `create_graft` writes an
            // object and then a ref, and `RepoLock` is not reentrant.
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            Ok(create_graft(&repo, &positionals, force, false)?.exit_code())
        }
        Mode::Edit => {
            if positionals.len() != 1 {
                return usage_msg_opt("-e needs exactly one argument");
            }
            bail!(
                "unsupported flag \"--edit\" (ported: -l, -d, -g/--graft, --convert-graft-file, -f, --format; --edit needs an interactive editor round-trip)"
            )
        }
        Mode::ConvertGraftFile => {
            if !positionals.is_empty() {
                return usage_msg_opt("--convert-graft-file takes no argument");
            }
            convert_graft_file(force)
        }
    }
}

/// git's `unknown option` report: an `error:` line, the usage block, exit 129.
fn unknown_option(opt: &str) -> Result<ExitCode> {
    if let Some(long) = opt.strip_prefix("--") {
        eprintln!("error: unknown option `{long}'");
    } else {
        eprintln!("error: unknown switch `{}'", &opt[1..]);
    }
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// git's `usage_msg_opt`: a `fatal:` line, a blank line, the usage block, 129.
fn usage_msg_opt(msg: &str) -> Result<ExitCode> {
    eprint!("fatal: {msg}\n\n{USAGE}");
    Ok(ExitCode::from(129))
}

/// git's `error()`: one `error:` line on stderr. The caller decides what the
/// return value becomes — `cmd_replace` turns it into -1, i.e. exit 255.
fn error_line(msg: &str) {
    eprintln!("error: {msg}");
}

/// git's `return error(...)` from `cmd_replace`, which `git.c` reports as 255.
fn err(msg: &str) -> Result<ExitCode> {
    error_line(msg);
    Ok(ExitCode::from(255))
}

/// What one `create_graft` call did, mirroring the three ways git's version can
/// leave the process.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GraftResult {
    /// Returned 0.
    Ok,
    /// Returned -1 via `error(...)`; the message is already on stderr.
    Failed,
    /// Called `die()`; the message is already on stderr and git exits at once.
    Died,
}

impl GraftResult {
    /// The exit code `cmd_replace` produces for this outcome.
    fn exit_code(self) -> ExitCode {
        match self {
            GraftResult::Ok => ExitCode::SUCCESS,
            GraftResult::Failed => ExitCode::from(255),
            GraftResult::Died => ExitCode::from(128),
        }
    }
}

/// The object type as `type_name()` renders it, with git's `(null)` for an
/// object that is not in the odb (`oid_object_info` returned -1).
fn type_name(kind: Option<Kind>) -> String {
    match kind {
        Some(k) => k.to_string(),
        None => "(null)".to_string(),
    }
}

/// The type of `oid`, or `None` when the object is not present.
fn object_kind(repo: &gix::Repository, oid: ObjectId) -> Option<Kind> {
    repo.find_header(oid).ok().map(|h| h.kind())
}

/// `repo_get_oid` — resolve a revision to the object it names, without peeling.
fn resolve(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    repo.rev_parse_single(spec).ok().map(|id| id.detach())
}

/// The value a replace ref currently holds, or `None` when it does not exist.
fn read_replace_ref(repo: &gix::Repository, name: &str) -> Result<Option<ObjectId>> {
    Ok(repo
        .try_find_reference(name)?
        .and_then(|r| r.target().try_id().map(|id| id.to_owned())))
}

/// `git replace <object> <replacement>` — write one `refs/replace/<hex>` ref.
fn replace_object(object_ref: &str, replace_ref: &str, force: bool) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let Some(object) = resolve(&repo, object_ref) else {
        return err(&format!("failed to resolve '{object_ref}' as a valid ref"));
    };
    let Some(repl) = resolve(&repo, replace_ref) else {
        return err(&format!("failed to resolve '{replace_ref}' as a valid ref"));
    };

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let ok = replace_object_oid(&repo, object_ref, object, replace_ref, repl, force)?;
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(255)
    })
}

/// `replace_object_oid`: the type check, the existence check, and the ref write.
///
/// Returns whether it succeeded — git's version returns 0 or -1. The caller must
/// already hold the repo lock; `RepoLock` is not reentrant and `create_graft`
/// takes it before calling here.
fn replace_object_oid(
    repo: &gix::Repository,
    object_ref: &str,
    object: ObjectId,
    replace_ref: &str,
    repl: ObjectId,
    force: bool,
) -> Result<bool> {
    let obj_type = object_kind(repo, object);
    let repl_type = object_kind(repo, repl);
    if !force && obj_type != repl_type {
        error_line(&format!(
            "Objects must be of the same type.\n\
             '{object_ref}' points to a replaced object of type '{}'\n\
             while '{replace_ref}' points to a replacement object of type '{}'.",
            type_name(obj_type),
            type_name(repl_type)
        ));
        return Ok(false);
    }

    let name = format!("{REPLACE_BASE}{object}");
    let prev = read_replace_ref(repo, &name)?;
    if prev.is_some() && !force {
        error_line(&format!("replace ref '{name}' already exists"));
        return Ok(false);
    }

    let expected = match prev {
        Some(id) => PreviousValue::MustExistAndMatch(Target::Object(id)),
        None => PreviousValue::MustNotExist,
    };
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: Default::default(),
            },
            expected,
            new: Target::Object(repl),
        },
        name: name
            .as_str()
            .try_into()
            .map_err(|e| anyhow!("'{name}' is not a valid ref name: {e}"))?,
        deref: false,
    })?;
    Ok(true)
}

/// `for_each_replace_name` + `delete_replace_ref`: resolve, look up, delete.
///
/// Every name is attempted; a failure on one only sets the exit status, exactly
/// as git's `had_error` accumulator does.
fn delete_replace_refs(names: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let mut had_error = false;

    for spec in names {
        let Some(oid) = resolve(&repo, spec) else {
            eprintln!("error: failed to resolve '{spec}' as a valid ref");
            had_error = true;
            continue;
        };
        let full_hex = oid.to_string();
        let name = format!("{REPLACE_BASE}{full_hex}");
        let Some(current) = read_replace_ref(&repo, &name)? else {
            eprintln!("error: replace ref '{full_hex}' not found");
            had_error = true;
            continue;
        };
        match repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::MustExistAndMatch(Target::Object(current)),
                log: RefLog::AndReference,
            },
            name: name
                .as_str()
                .try_into()
                .map_err(|e| anyhow!("'{name}' is not a valid ref name: {e}"))?,
            deref: false,
        }) {
            Ok(_) => println!("Deleted replace ref '{full_hex}'"),
            Err(e) => {
                eprintln!("error: could not delete reference {name}: {e}");
                had_error = true;
            }
        }
    }

    Ok(if had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// `list_replace_refs` — print every replace ref matching `pattern`.
fn list_replace_refs(pattern: Option<&str>, format: Option<&str>) -> Result<ExitCode> {
    let format = match format {
        None | Some("") | Some("short") => Format::Short,
        Some("medium") => Format::Medium,
        Some("long") => Format::Long,
        Some(other) => {
            return err(&format!(
                "invalid replace format '{other}'\n\
                 valid formats are 'short', 'medium' and 'long'"
            ))
        }
    };
    // git defaults the pattern to `*`, which matches every short name.
    let pattern = pattern.unwrap_or("*");

    let repo = gix::discover(".")?;

    // Collect first so the output is ordered by ref name, as git's ref iteration is.
    let mut refs: Vec<(String, ObjectId)> = Vec::new();
    for reference in repo.references()?.all()? {
        let reference = reference.map_err(|e| anyhow!("{e}"))?;
        let full = reference.name().as_bstr().to_string();
        let Some(short) = full.strip_prefix(REPLACE_BASE) else {
            continue;
        };
        let Some(id) = reference.target().try_id().map(|id| id.to_owned()) else {
            continue;
        };
        refs.push((short.to_string(), id));
    }
    refs.sort_by(|a, b| a.0.cmp(&b.0));

    for (refname, oid) in refs {
        if !wildmatch(pattern.as_bytes(), refname.as_bytes()) {
            continue;
        }
        match format {
            Format::Short => println!("{refname}"),
            Format::Medium => println!("{refname} -> {oid}"),
            // A failure here makes git's `show_reference` callback return
            // non-zero, which only stops the iteration — `list_replace_refs`
            // still returns 0, so the exit code stays 0.
            Format::Long => {
                let Ok(object) = ObjectId::from_hex(refname.as_bytes()) else {
                    eprintln!("error: invalid object identifier: {refname}");
                    break;
                };
                let (Some(obj_type), Some(repl_type)) =
                    (object_kind(&repo, object), object_kind(&repo, oid))
                else {
                    break;
                };
                println!("{refname} ({obj_type}) -> {oid} ({repl_type})");
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// `lookup_commit_reference` — read `oid`, follow tags (git's `deref_tag`), and
/// require a commit.
///
/// On a type mismatch git's `object_as_type` reports it before the caller adds
/// its own line, so both appear; that pair is what `git replace --graft <tree>`
/// prints.
fn lookup_commit_reference(repo: &gix::Repository, oid: ObjectId) -> Option<gix::Commit<'_>> {
    let object = repo.find_object(oid).ok()?.peel_tags_to_end().ok()?;
    if object.kind != Kind::Commit {
        error_line(&format!(
            "object {} is a {}, not a commit",
            object.id, object.kind
        ));
        return None;
    }
    Some(object.into_commit())
}

/// `create_graft` — rewrite `<commit>`'s parents and replace it with the result.
///
/// `argv[0]` is the commit to graft; the rest are its new parents (none means a
/// root commit). `gentle` is git's flag for the graft-file conversion loop: it
/// downgrades the "new commit is the same as the old one" error to a warning.
///
/// The caller must hold the repo lock — this writes an object and a ref.
fn create_graft(
    repo: &gix::Repository,
    argv: &[String],
    force: bool,
    gentle: bool,
) -> Result<GraftResult> {
    let old_ref = argv[0].as_str();

    let Some(old_oid) = resolve(repo, old_ref) else {
        error_line(&format!("not a valid object name: '{old_ref}'"));
        return Ok(GraftResult::Failed);
    };
    let Some(commit) = lookup_commit_reference(repo, old_oid) else {
        error_line(&format!("could not parse {old_ref}"));
        return Ok(GraftResult::Failed);
    };
    // `Commit` implements `Drop` (it returns its buffer to the repo's pool), so
    // the raw bytes have to be copied rather than moved out.
    let commit_id = commit.id;
    let mut buf = commit.data.clone();
    drop(commit);

    // `replace_parents` runs before the signature and mergetag handling, and
    // resolves its parents with `die()` rather than `error()`.
    let hexsz = repo.object_hash().len_in_hex();
    let mut new_parents: Vec<u8> = Vec::new();
    for spec in &argv[1..] {
        let Some(oid) = resolve(repo, spec) else {
            eprintln!("fatal: not a valid object name: '{spec}'");
            return Ok(GraftResult::Died);
        };
        let Some(parent) = lookup_commit_reference(repo, oid) else {
            eprintln!("fatal: could not parse {spec} as a commit");
            return Ok(GraftResult::Died);
        };
        new_parents.extend_from_slice(format!("parent {}\n", parent.id).as_bytes());
    }
    replace_parents(&mut buf, hexsz, &new_parents)?;

    if remove_signature(&mut buf) {
        eprintln!("warning: the original commit '{old_ref}' has a gpg signature");
        eprintln!("warning: the signature will be removed in the replacement commit!");
    }

    // `check_mergetags` needs tag re-hashing and parsing to decide whether a
    // mergetag survives the new parent list; refuse instead of dropping it.
    if header_lines(&buf).any(|l| l.starts_with(b"mergetag ")) {
        bail!("--graft on a commit with a mergetag header is not supported (git's check_mergetags needs tag parsing that is not ported)");
    }

    let new_oid = match repo.objects.write_buf(Kind::Commit, &buf) {
        Ok(id) => id,
        Err(_) => {
            error_line(&format!(
                "could not write replacement commit for: '{old_ref}'"
            ));
            return Ok(GraftResult::Failed);
        }
    };

    if new_oid == commit_id {
        if gentle {
            eprintln!("warning: graft for '{commit_id}' unnecessary");
            return Ok(GraftResult::Ok);
        }
        error_line(&format!(
            "new commit is the same as the old one: '{commit_id}'"
        ));
        return Ok(GraftResult::Failed);
    }

    Ok(
        if replace_object_oid(repo, old_ref, commit_id, "replacement", new_oid, force)? {
            GraftResult::Ok
        } else {
            GraftResult::Failed
        },
    )
}

/// `repo_get_graft_file` — `$GIT_GRAFT_FILE`, else `info/grafts` under the
/// common dir (git routes `info/` there, so a linked worktree shares one file).
fn graft_file_path(repo: &gix::Repository) -> PathBuf {
    match std::env::var_os("GIT_GRAFT_FILE") {
        Some(p) => PathBuf::from(p),
        None => repo.common_dir().join("info").join("grafts"),
    }
}

/// `convert_graft_file` — turn every graft line into a replace ref, then unlink
/// the file.
///
/// A missing/unreadable graft file is git's `if (!fp) return -1`, which
/// `cmd_replace`'s `!!` collapses to exit 1 with nothing on stderr.
fn convert_graft_file(force: bool) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let graft_file = graft_file_path(&repo);
    let Ok(contents) = std::fs::read(&graft_file) else {
        return Ok(ExitCode::from(1));
    };

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    // git accumulates the failing lines verbatim, each as "\n\t<line>".
    let mut failed = String::new();
    for raw in contents.split(|&b| b == b'\n') {
        let text = String::from_utf8_lossy(raw);
        // `strbuf_getline` has already stripped the LF; it strips a CR too.
        let line = text.strip_suffix('\r').unwrap_or(&*text);
        if line.starts_with('#') {
            continue;
        }
        // `strvec_split` on whitespace; an empty line yields no arguments and is
        // skipped by git's `args.nr &&` guard.
        let args: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        if args.is_empty() {
            continue;
        }
        match create_graft(&repo, &args, force, true)? {
            GraftResult::Ok => {}
            GraftResult::Failed => {
                failed.push_str("\n\t");
                failed.push_str(line);
            }
            // `die()` ends the process immediately, mid-loop.
            GraftResult::Died => return Ok(ExitCode::from(128)),
        }
    }

    if !failed.is_empty() {
        eprintln!("warning: could not convert the following graft(s):{failed}");
        return Ok(ExitCode::from(1));
    }

    // `unlink_or_warn`: a warning, and a non-zero return, only on a real failure.
    match std::fs::remove_file(&graft_file) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("warning: unable to unlink '{}': {e}", graft_file.display());
            Ok(ExitCode::from(1))
        }
    }
}

/// `replace_parents`: swap the run of `parent` header lines that follows the
/// `tree` line for `new_parents`, working on the raw commit bytes as git does.
fn replace_parents(buf: &mut Vec<u8>, hexsz: usize, new_parents: &[u8]) -> Result<()> {
    // "tree " + <hex> + "\n"
    let start = hexsz + 6;
    if buf.len() < start || !buf.starts_with(b"tree ") {
        bail!("malformed commit object: no tree header");
    }
    let mut end = start;
    // "parent " + <hex> + "\n"
    while buf[end..].starts_with(b"parent ") {
        end += hexsz + 8;
        if end > buf.len() {
            bail!("malformed commit object: truncated parent header");
        }
    }
    buf.splice(start..end, new_parents.iter().copied());
    Ok(())
}

/// `remove_signature`: drop a `gpgsig`/`gpgsig-sha256` header and its
/// continuation lines. Returns whether anything was removed.
fn remove_signature(buf: &mut Vec<u8>) -> bool {
    let mut out: Vec<u8> = Vec::with_capacity(buf.len());
    let mut removed = false;
    let mut pos = 0;
    let mut in_signature = false;

    while pos < buf.len() {
        let line_end = match buf[pos..].iter().position(|&b| b == b'\n') {
            Some(n) => pos + n + 1,
            None => buf.len(),
        };
        let line = &buf[pos..line_end];

        // The blank line ends the header block; the message is copied verbatim.
        if line == b"\n" {
            out.extend_from_slice(&buf[pos..]);
            break;
        }
        if line.starts_with(b"gpgsig ") || line.starts_with(b"gpgsig-sha256 ") {
            in_signature = true;
            removed = true;
        } else if in_signature && line.starts_with(b" ") {
            // continuation of the signature
        } else {
            in_signature = false;
            out.extend_from_slice(line);
        }
        pos = line_end;
    }

    if removed {
        *buf = out;
    }
    removed
}

/// Iterate the header lines of a raw commit, stopping at the blank separator.
fn header_lines(buf: &[u8]) -> impl Iterator<Item = &[u8]> + '_ {
    let header_len = buf
        .windows(2)
        .position(|w| w == b"\n\n")
        .map_or(buf.len(), |n| n + 1);
    buf[..header_len].split(|&b| b == b'\n')
}

/// `wildmatch(pattern, text, 0)` — glob matching without `WM_PATHNAME`, so `*`
/// spans any byte. Supports `*`, `?`, `[...]` (with `!`/`^` negation and `a-z`
/// ranges) and `\` escaping.
fn wildmatch(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    // Backtracking state for the most recent `*`.
    let (mut star_p, mut star_t) = (usize::MAX, 0);

    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star_p = p;
                p += 1;
                star_t = t;
                continue;
            }
            Some(b'?') => {
                p += 1;
                t += 1;
                continue;
            }
            Some(b'[') => {
                if let Some(next_p) = match_bracket(pattern, p, text[t]) {
                    p = next_p;
                    t += 1;
                    continue;
                }
            }
            Some(b'\\') if p + 1 < pattern.len() => {
                if pattern[p + 1] == text[t] {
                    p += 2;
                    t += 1;
                    continue;
                }
            }
            Some(&c) if c == text[t] => {
                p += 1;
                t += 1;
                continue;
            }
            _ => {}
        }
        // Mismatch: retry the last `*` consuming one more byte, else fail.
        if star_p == usize::MAX {
            return false;
        }
        star_t += 1;
        t = star_t;
        p = star_p + 1;
    }

    pattern[p..].iter().all(|&c| c == b'*')
}

/// Match one `[...]` class at `pattern[start]` against `byte`.
///
/// Returns the pattern index just past the class on a match, `None` otherwise
/// (including a class with no closing `]`, which git treats as a literal `[`).
fn match_bracket(pattern: &[u8], start: usize, byte: u8) -> Option<usize> {
    let mut i = start + 1;
    let negated = matches!(pattern.get(i), Some(b'!') | Some(b'^'));
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pattern.len() {
        // A `]` in the first position is a literal member, not the terminator.
        if pattern[i] == b']' && !first {
            let hit = matched != negated;
            return hit.then_some(i + 1);
        }
        first = false;
        let lo = if pattern[i] == b'\\' && i + 1 < pattern.len() {
            i += 1;
            pattern[i]
        } else {
            pattern[i]
        };
        // `a-z` range, unless the `-` is the last character before `]`.
        if pattern.get(i + 1) == Some(&b'-') && pattern.get(i + 2).is_some_and(|&c| c != b']') {
            let hi = pattern[i + 2];
            if (lo..=hi).contains(&byte) {
                matched = true;
            }
            i += 3;
        } else {
            if lo == byte {
                matched = true;
            }
            i += 1;
        }
    }
    None
}
