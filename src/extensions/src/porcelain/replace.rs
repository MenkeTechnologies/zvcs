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
//!   * `--edit`/`-e` — `export_object()` writes the object to
//!     `$GIT_DIR/REPLACE_EDITOBJ` through `git --no-replace-objects cat-file`
//!     (`-p`, or the bare type under `--raw`), the editor is opened on it, and
//!     `import_object()` hashes the result back in as the same type — through
//!     `git mktree` for a pretty-printed tree. An unchanged object is git's
//!     `new object is the same as the old one` error (255). As in git, the
//!     scratch file is left behind, so a second `--edit` in the same repository
//!     fails on the `O_EXCL` create.
//!
//! Not covered, and refused rather than approximated:
//!   * `index_fd()`'s `INDEX_FORMAT_CHECK`, the object-format validation git
//!     runs over the edited bytes before writing them.
//!   * `--graft` on a commit carrying a `mergetag` header — git's
//!     `check_mergetags` re-hashes and parses each mergetag to decide whether it
//!     is discarded, which needs tag parsing this port does not do; refused
//!     instead of silently dropping the mergetag.
//!   * `core.graftFile` — git 2.55 does not honour it either (only
//!     `$GIT_GRAFT_FILE` and the default path), so neither does this.
//!   * `GIT_REPLACE_REF_BASE` — the namespace is always `refs/replace/`.
//!
//! The replacements themselves *are* in effect: every command that reads an object
//! goes through the odb's replacement map, which is built from `refs/replace/*` at
//! open time and switched off by `core.useReplaceRefs=false`,
//! `GIT_NO_REPLACE_OBJECTS` or the `--no-replace-objects` that sets it.
//!   * `error: Could not read <oid>` followed by `fatal: Failed to traverse
//!     parents of commit <oid>`, which `repo_parse_commit_internal()`
//!     (commit.c:644) emits once a graft has named a parent the object database
//!     does not have. The [graft table](gix::graft) substitutes such a parent, and
//!     a walk that reaches it stops there rather than dying.
//!
//! Graft *registration* itself is no longer missing: `--convert-graft-file` reads
//! the graft file as a list of lines, and [`gix::graft`] additionally reads it as
//! the *graft table* git builds in `prepare_commit_graft()` → `read_graft_file()`
//! (commit.c:287-330) and applies in `parse_commit_buffer()` (commit.c:554-590),
//! so `log`, `rev-list`, `merge-base`, `describe` and `blame` all follow the
//! substituted parents. The two diagnostics that ride on that read —
//! `error: bad graft data: <line>` and `error: duplicate graft data: <line>`
//! (commit.c:281, commit.c:309) — and the
//! `Support for <GIT_DIR>/info/grafts is deprecated` advice are emitted there;
//! this command suppresses the advice with git's
//! `no_graft_file_deprecated_advice = 1` (builtin/replace.c:522).
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

use super::{Arg, LongOpt};
// Every object name here reaches `repo_get_oid()`, whose full-length-hex branch
// resolves without consulting the odb — see [`crate::objname`]. `git replace`
// depends on that: `<40 hex>` naming an object the repository does not have is
// how the type check reports `(null)`, how `-d` reaches its "not found" report,
// and how `-f` writes a replace ref for an object that is not present yet.
use crate::objname;

/// The namespace every replace ref lives in.
const REPLACE_BASE: &str = "refs/replace/";

/// `cmd_replace()`'s `struct option options[]` (builtin/replace.c), in table
/// order, as [`super::resolve_long`] reads it. The five mode selectors are
/// `OPT_CMDMODE`, which carries `PARSE_OPT_NONEG`; `--force`, `--raw` and
/// `--format` negate.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "list", neg: false, arg: Arg::None },
    LongOpt { name: "delete", neg: false, arg: Arg::None },
    LongOpt { name: "edit", neg: false, arg: Arg::None },
    LongOpt { name: "graft", neg: false, arg: Arg::None },
    LongOpt { name: "convert-graft-file", neg: false, arg: Arg::None },
    LongOpt { name: "force", neg: true, arg: Arg::None },
    LongOpt { name: "raw", neg: true, arg: Arg::None },
    LongOpt { name: "format", neg: true, arg: Arg::Required },
];

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

/// Whether `GIT_NO_REPLACE_OBJECTS` was already in the environment when
/// [`replace`] started, as opposed to being the one this command sets for
/// itself. Only [`launch_editor`] reads it.
static INHERITED_NO_REPLACE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// `git replace` — see the module docs for the covered surface.
pub fn replace(args: &[String]) -> Result<ExitCode> {
    // ```c
    // read_replace_refs = 0;
    // git_config(git_default_config, NULL);
    // ```
    //
    // (`cmd_replace`, builtin/replace.c:562.) The whole command runs with the
    // replacement map *off*: every object this verb reads is the object named,
    // never the object `refs/replace/<oid>` substitutes for it. That is what
    // makes `git replace --graft <commit>` graft the original commit's buffer —
    // `create_graft()`'s `get_commit_buffer(commit, &size)` — rather than the
    // buffer of a replacement that already stands in for it, and what makes
    // `--edit` export the original bytes. Without it, grafting an
    // already-replaced commit produced a replacement built from the *previous*
    // replacement's message.
    //
    // git flips a process-global; the equivalent here is the environment
    // variable `gix`'s open path tests for presence of (`open/repository.rs`,
    // `GIT_NO_REPLACE_OBJECTS`), set before any [`crate::setup::discover`] call
    // below opens the repository. `git --no-replace-objects` sets the same
    // variable (`lib.rs`), so this is the spelling the rest of the port already
    // uses for "read objects unsubstituted".
    //
    // Whether the *caller* already had it set is remembered so the one child this
    // command launches — `--edit`'s editor — sees the environment stock would
    // have given it; see [`launch_editor`].
    INHERITED_NO_REPLACE.store(
        std::env::var_os("GIT_NO_REPLACE_OBJECTS").is_some(),
        std::sync::atomic::Ordering::Relaxed,
    );
    std::env::set_var("GIT_NO_REPLACE_OBJECTS", "1");
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
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, which is why it is not a `LONG_OPTS` entry. This table
        // has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the same
        // block `-h` prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        let resolved = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        let a = resolved.as_ref();
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
            let repo = crate::setup::discover()?;
            // The lock is held for the whole graft: `create_graft` writes an
            // object and then a ref, and `RepoLock` is not reentrant.
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            Ok(create_graft(&repo, &positionals, force, false)?.exit_code())
        }
        Mode::Edit => {
            if positionals.len() != 1 {
                return usage_msg_opt("-e needs exactly one argument");
            }
            edit_and_replace(&positionals[0], force, raw)
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

/// `edit_and_replace()`: round-trip an object through the editor and replace it
/// with whatever came back.
///
/// The object is exported to `$GIT_DIR/REPLACE_EDITOBJ` by `git cat-file`
/// (`-p`, or the bare type under `--raw`) run with replacement resolution off,
/// the editor is opened on it, and the result is hashed back in as the *same*
/// type — through `git mktree` for a pretty-printed tree, since that listing is
/// not a tree object's on-disk form. An unchanged object is git's
/// `new object is the same as the old one` error (255), not a no-op success.
fn edit_and_replace(object_ref: &str, force: bool, raw: bool) -> Result<ExitCode> {
    let repo = crate::setup::discover()?;

    let Some(old) = objname::resolve(&repo, object_ref) else {
        return err(&format!("not a valid object name: '{object_ref}'"));
    };
    let Some(kind) = object_kind(&repo, old) else {
        return err(&format!("unable to get object type for {old}"));
    };

    // `check_ref_valid()`: the ref must be writable *before* the editor runs.
    let name = format!("{REPLACE_BASE}{old}");
    if read_replace_ref(&repo, &name)?.is_some() && !force {
        return err(&format!("replace ref '{name}' already exists"));
    }

    // git never removes `REPLACE_EDITOBJ`, and creates it `O_EXCL`, so a second
    // `--edit` in the same repository dies on the leftover. Verified against
    // stock 2.55.0.
    let tmpfile = repo.git_dir().join("REPLACE_EDITOBJ");
    export_object(&tmpfile, old, kind, raw)?;

    let new = if launch_editor(&repo, &tmpfile) {
        import_object(&repo, &tmpfile, kind, raw)
    } else {
        error_line("editing object file failed");
        None
    };

    let Some(new) = new else {
        return Ok(ExitCode::from(255));
    };
    if new == old {
        return err(&format!("new object is the same as the old one: '{old}'"));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    // git passes the literal `"replacement"` as the name it would quote back in
    // the type-mismatch diagnostic, which `--edit` can never trigger anyway.
    let ok = replace_object_oid(&repo, object_ref, old, "replacement", new, force)?;
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(255)
    })
}

/// Render an `io::Error` the way `die_errno()`'s `strerror` would, i.e. without
/// Rust's trailing ` (os error N)`.
fn os_msg(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(at) => s[..at].to_string(),
        None => s,
    }
}

/// `export_object()`: `git --no-replace-objects cat-file (-p|<type>) <oid>` into
/// a freshly created `filename`, run as our own binary (git's `cmd.git_cmd = 1`).
/// `--no-replace-objects` is exactly `GIT_NO_REPLACE_OBJECTS=1` in the child.
fn export_object(filename: &std::path::Path, oid: ObjectId, kind: Kind, raw: bool) -> Result<()> {
    // `xopen(filename, O_CREAT | O_EXCL | O_WRONLY, 0666)`.
    let file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(filename)
        .map_err(|e| anyhow!("unable to create '{}': {}", filename.display(), os_msg(&e)))?;
    let selector = if raw { kind.to_string() } else { "-p".to_string() };
    let status = std::process::Command::new(crate::hosted::git_exe()?)
        .args(["cat-file", &selector, &oid.to_string()])
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(std::process::Stdio::null())
        .stdout(file)
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => {
            error_line(&format!(
                "cannot run 'git cat-file' to export object '{oid}'"
            ));
            Err(anyhow!("cat-file failed"))
        }
    }
}

/// `import_object()`: hash the edited file back in as `kind`. A pretty-printed
/// tree goes through `git mktree`; everything else is written verbatim.
/// `None` is git's -1 return, with the `error:` line already printed.
fn import_object(
    repo: &gix::Repository,
    filename: &std::path::Path,
    kind: Kind,
    raw: bool,
) -> Option<ObjectId> {
    if !raw && kind == Kind::Tree {
        let listing = std::fs::File::open(filename).ok()?;
        let out = std::process::Command::new(crate::hosted::git_exe().ok()?)
            .arg("mktree")
            .stdin(listing)
            .output()
            .ok()
            .or_else(|| {
                error_line("unable to spawn mktree");
                None
            })?;
        if !out.status.success() {
            error_line("mktree reported failure");
            return None;
        }
        let hex = out.stdout.split(|&b| b == b'\n').next().unwrap_or_default();
        return match ObjectId::from_hex(hex) {
            Ok(id) => Some(id),
            Err(_) => {
                error_line("mktree did not return an object name");
                None
            }
        };
    }

    let data = std::fs::read(filename)
        .map_err(|e| error_line(&format!("unable to fstat {}: {e}", filename.display())))
        .ok()?;
    match repo.objects.write_buf(kind, &data) {
        Ok(id) => Some(id),
        Err(_) => {
            error_line("unable to write object to database");
            None
        }
    }
}

/// `launch_editor(tmpfile, NULL, NULL)`: open the file and wait, without reading
/// it back — `import_object()` re-reads it from disk. `false` is git's -1.
fn launch_editor(repo: &gix::Repository, path: &std::path::Path) -> bool {
    let Some(editor) = super::bugreport::git_editor(Some(repo)) else {
        error_line("Terminal is dumb, but EDITOR unset");
        return false;
    };
    // `:` is git's documented no-op editor; it is never actually run.
    if editor == ":" {
        return true;
    }
    // `cmd_replace`'s `read_replace_refs = 0` is a process-global in git, so the
    // editor it launches inherits nothing from it. Here it is an environment
    // variable, which a child *would* inherit — so the one this command set for
    // itself is taken back out. A `GIT_NO_REPLACE_OBJECTS` the caller set (or
    // `git --no-replace-objects`) is left in place, because stock passes that on.
    let mut command = super::bugreport::editor_command(&editor, path);
    if !INHERITED_NO_REPLACE.load(std::sync::atomic::Ordering::Relaxed) {
        command.env_remove("GIT_NO_REPLACE_OBJECTS");
    }
    match command.status() {
        Ok(s) if s.success() => true,
        Ok(_) | Err(_) => {
            error_line(&format!("there was a problem with the editor '{editor}'"));
            false
        }
    }
}

/// What one `create_graft` call did — git's version returns 0 or -1 and never
/// `die()`s, so there is no third outcome to model.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GraftResult {
    /// Returned 0.
    Ok,
    /// Returned -1 via `error(...)`; the message is already on stderr.
    Failed,
}

impl GraftResult {
    /// The exit code `cmd_replace` produces for this outcome.
    fn exit_code(self) -> ExitCode {
        match self {
            GraftResult::Ok => ExitCode::SUCCESS,
            GraftResult::Failed => ExitCode::from(255),
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

/// The value a replace ref currently holds, or `None` when it does not exist.
fn read_replace_ref(repo: &gix::Repository, name: &str) -> Result<Option<ObjectId>> {
    Ok(repo
        .try_find_reference(name)?
        .and_then(|r| r.target().try_id().map(|id| id.to_owned())))
}

/// `git replace <object> <replacement>` — write one `refs/replace/<hex>` ref.
fn replace_object(object_ref: &str, replace_ref: &str, force: bool) -> Result<ExitCode> {
    let repo = crate::setup::discover()?;

    let Some(object) = objname::resolve(&repo, object_ref) else {
        return err(&format!("failed to resolve '{object_ref}' as a valid ref"));
    };
    let Some(repl) = objname::resolve(&repo, replace_ref) else {
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

    // `ref_transaction_update()` (`refs.c`) verifies the *new* value before the
    // transaction is allowed to prepare:
    //
    // ```c
    // if ((flags & REF_HAVE_NEW) && !new_target && !is_null_oid(new_oid) &&
    //     !(flags & REF_SKIP_OID_VERIFICATION) && !(flags & REF_LOG_ONLY)) {
    //         struct object *o = parse_object(transaction->ref_store->repo, new_oid);
    //         if (!o) {
    //                 strbuf_addf(err, _("trying to write ref '%s' with nonexistent object %s"),
    //                             refname, oid_to_hex(new_oid));
    // ```
    //
    // `replace` sets none of those skip flags, and `replace_object_oid` turns the
    // failure into `error("%s", err.buf)`. It is reachable precisely because
    // `repo_get_oid()` hands back a full-length hex id without checking the odb:
    // `git replace -f <ref> <40-hex-that-is-absent>` gets all the way here. Only
    // the replacement is checked — the object being *replaced* may be absent,
    // which is what makes `replace -f <absent> HEAD` succeed.
    if repo.find_object(repl).is_err() {
        error_line(&format!(
            "trying to write ref '{name}' with nonexistent object {repl}"
        ));
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
    let repo = crate::setup::discover()?;
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let mut had_error = false;

    for spec in names {
        let Some(oid) = objname::resolve(&repo, spec) else {
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
                message: Default::default(),
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

    let repo = crate::setup::discover()?;

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

    let Some(old_oid) = objname::resolve(repo, old_ref) else {
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

    // `replace_parents` runs before the signature and mergetag handling. Both of
    // its rejections are `return error(...)`, which `create_graft` propagates as
    // its own -1 — not `die()`, so the process leaves with 255 and, under
    // `--convert-graft-file`, the line is merely recorded as unconverted:
    //
    // ```c
    // if (repo_get_oid(the_repository, argv[i], &oid) < 0) {
    //         strbuf_release(&new_parents);
    //         return error(_("not a valid object name: '%s'"), argv[i]);
    // }
    // commit = lookup_commit_reference(the_repository, &oid);
    // if (!commit) {
    //         strbuf_release(&new_parents);
    //         return error(_("could not parse %s as a commit"), argv[i]);
    // }
    // ```
    //
    // The second branch is the one a full-length hex id reaches: `repo_get_oid()`
    // resolved it without the odb, so an absent parent is "could not parse", not
    // "not a valid object name".
    let hexsz = repo.object_hash().len_in_hex();
    let mut new_parents: Vec<u8> = Vec::new();
    for spec in &argv[1..] {
        let Some(oid) = objname::resolve(repo, spec) else {
            error_line(&format!("not a valid object name: '{spec}'"));
            return Ok(GraftResult::Failed);
        };
        let Some(parent) = lookup_commit_reference(repo, oid) else {
            error_line(&format!("could not parse {spec} as a commit"));
            return Ok(GraftResult::Failed);
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
    // `no_graft_file_deprecated_advice = 1` (builtin/replace.c:522), set before the
    // read so neither this read nor the commit parsing below re-advertises the
    // deprecation this command exists to resolve. It must come before the first
    // commit is looked at, because that is what triggers `prepare_commit_graft()`.
    gix::graft::suppress_deprecation_advice();
    let repo = crate::setup::discover()?;
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
        }
    }

    if !failed.is_empty() {
        // `warning(_("could not convert the following graft(s):\n%s"), err.buf)`
        // — the format string ends in a newline and every accumulated line
        // already starts with one, so the first failure is preceded by a blank
        // line.
        eprintln!("warning: could not convert the following graft(s):\n{failed}");
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
        crate::git_fatal!("malformed commit object: no tree header");
    }
    let mut end = start;
    // "parent " + <hex> + "\n"
    while buf[end..].starts_with(b"parent ") {
        end += hexsz + 8;
        if end > buf.len() {
            crate::git_fatal!("malformed commit object: truncated parent header");
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
