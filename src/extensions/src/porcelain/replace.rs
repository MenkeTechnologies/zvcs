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
//!   * `-f`/`--force`/`--no-force`, `--raw`/`--no-raw` and `-h`, plus git's
//!     option/cmdmode validation (`--format cannot be used when not listing`,
//!     `-f only makes sense when writing a replacement`, `--raw only makes sense
//!     with --edit`, `-d needs at least one argument`, `bad number of
//!     arguments`, `only one pattern can be given with -l`, and the
//!     `options '<a>' and '<b>' cannot be used together` conflict).
//!
//! Not covered, and refused rather than approximated:
//!   * `--edit`/`-e` — spawns `$GIT_EDITOR` on a pretty-printed object and
//!     re-parses the result; interactive, no substrate for it here.
//!   * `--convert-graft-file` — rewrites `$GIT_DIR/info/grafts` into replace
//!     refs and unlinks it; the vendored crates expose no graft-file reader.
//!   * `--graft` on a commit carrying a `mergetag` header — git's
//!     `check_mergetags` re-hashes and parses each mergetag to decide whether it
//!     is discarded, which needs tag parsing this port does not do; refused
//!     instead of silently dropping the mergetag.
//!   * `GIT_REPLACE_REF_BASE` — the namespace is always `refs/replace/`.
//!
//! Exit codes follow git: 0 on success, 129 for usage errors, 1 when `-d` had a
//! failure, and 255 for every `return error(...)` path (git's `cmd_replace`
//! returns -1, which `git.c` truncates to 255).

use anyhow::{anyhow, bail, Result};
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

    let mut i = 1;
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
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `format' requires a value"))?;
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
            create_graft(&positionals, force)
        }
        Mode::Edit => bail!(
            "unsupported flag \"--edit\" (ported: -l, -d, -g/--graft, -f, --format; --edit needs an interactive editor round-trip)"
        ),
        Mode::ConvertGraftFile => bail!(
            "unsupported flag \"--convert-graft-file\" (ported: -l, -d, -g/--graft, -f, --format; no graft-file reader in the vendored crates)"
        ),
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

/// git's `return error(...)` from `cmd_replace`, which `git.c` reports as 255.
fn err(msg: &str) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(255))
}

/// git's `die()`: a `fatal:` line and exit 128.
fn fatal(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
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

    replace_object_oid(&repo, object_ref, object, replace_ref, repl, force)
}

/// `replace_object_oid`: the type check, the existence check, and the ref write.
fn replace_object_oid(
    repo: &gix::Repository,
    object_ref: &str,
    object: ObjectId,
    replace_ref: &str,
    repl: ObjectId,
    force: bool,
) -> Result<ExitCode> {
    let obj_type = object_kind(repo, object);
    let repl_type = object_kind(repo, repl);
    if !force && obj_type != repl_type {
        return err(&format!(
            "Objects must be of the same type.\n\
             '{object_ref}' points to a replaced object of type '{}'\n\
             while '{replace_ref}' points to a replacement object of type '{}'.",
            type_name(obj_type),
            type_name(repl_type)
        ));
    }

    let name = format!("{REPLACE_BASE}{object}");
    let prev = read_replace_ref(repo, &name)?;
    if prev.is_some() && !force {
        return err(&format!("replace ref '{name}' already exists"));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
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
    Ok(ExitCode::SUCCESS)
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

/// `lookup_commit_reference` — read `oid` and peel it (through tags) to a commit.
fn peel_commit(repo: &gix::Repository, oid: ObjectId) -> Option<gix::Commit<'_>> {
    repo.find_object(oid).ok()?.peel_to_commit().ok()
}

/// `create_graft` — rewrite `<commit>`'s parents and replace it with the result.
///
/// `argv[0]` is the commit to graft; the rest are its new parents (none means a
/// root commit).
fn create_graft(argv: &[String], force: bool) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let old_ref = argv[0].as_str();

    let Some(old_oid) = resolve(&repo, old_ref) else {
        return fatal(&format!("not a valid object name: '{old_ref}'"));
    };
    let Some(commit) = peel_commit(&repo, old_oid) else {
        return fatal(&format!("could not parse {old_ref}"));
    };
    // `Commit` implements `Drop` (it returns its buffer to the repo's pool), so
    // the raw bytes have to be copied rather than moved out.
    let commit_id = commit.id;
    let mut buf = commit.data.clone();

    // `check_mergetags` needs tag re-hashing and parsing to decide whether a
    // mergetag survives the new parent list; refuse instead of dropping it.
    if header_lines(&buf).any(|l| l.starts_with(b"mergetag ")) {
        bail!("--graft on a commit with a mergetag header is not supported (git's check_mergetags needs tag parsing that is not ported)");
    }

    let hexsz = repo.object_hash().len_in_hex();
    let mut new_parents: Vec<u8> = Vec::new();
    for spec in &argv[1..] {
        let Some(oid) = resolve(&repo, spec) else {
            return fatal(&format!("not a valid object name: '{spec}'"));
        };
        let Some(parent) = peel_commit(&repo, oid) else {
            return fatal(&format!("could not parse {spec} as a commit"));
        };
        new_parents.extend_from_slice(format!("parent {}\n", parent.id).as_bytes());
    }
    replace_parents(&mut buf, hexsz, &new_parents)?;

    if remove_signature(&mut buf) {
        eprintln!("warning: the original commit '{old_ref}' has a gpg signature");
        eprintln!("warning: the signature will be removed in the replacement commit!");
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let new_oid = repo
        .objects
        .write_buf(Kind::Commit, &buf)
        .map_err(|e| anyhow!("could not write replacement commit for: '{old_ref}': {e}"))?;

    if new_oid == commit_id {
        return err(&format!(
            "new commit is the same as the old one: '{commit_id}'"
        ));
    }

    replace_object_oid(&repo, old_ref, commit_id, "replacement", new_oid, force)
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
