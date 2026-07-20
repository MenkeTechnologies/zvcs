//! `git update-ref` — update the object name stored in a ref, safely.
//!
//! Covered: the two command-line forms (`<ref> <new-oid> [<old-oid>]` and
//! `-d <ref> [<old-oid>]`) with `-m`, `--no-deref`/`--deref` and
//! `--create-reflog`, plus `--stdin` (and `--stdin -z`) with the `update`,
//! `create`, `delete`, `verify`, `symref-update`, `symref-create`,
//! `symref-delete`, `symref-verify`, `option no-deref`, `start`, `commit` and
//! `abort` commands. Every batch is applied through a single gitoxide ref
//! transaction, so it is all-or-nothing exactly like stock git. Stock git
//! prints nothing on success; so does this, and exit codes match (0 on
//! success, 128 on a fatal failure, 1 for a failed `-d`).
//!
//! Not covered, and rejected with an error rather than approximated:
//! `--batch-updates` (needs per-update rejection inside one transaction, which
//! the vendored `gix-ref` transaction does not expose) and the `--stdin`
//! `prepare` command (needs ref locks held across commands). One-level
//! lowercase ref names such as `foo` are rejected by `gix-validate`, where
//! stock git would write `.git/foo`.

use anyhow::{anyhow, bail, Result};
use std::io::Read;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// One `<old-oid>`/`<new-oid>` slot as it appears on the command line or on stdin.
enum Val {
    /// The value and its preceding separator were omitted entirely.
    Missing,
    /// The all-zero object id, or (outside `-z`) the empty string.
    Zero,
    /// A resolved object name.
    Oid(ObjectId),
}

/// `git update-ref` — see the module docs for the covered surface.
pub fn update_ref(args: &[String]) -> Result<ExitCode> {
    let mut msg: Option<String> = None;
    let mut delete = false;
    let mut deref = true;
    let mut stdin_mode = false;
    let mut nul = false;
    let mut create_reflog = false;
    let mut positionals: Vec<String> = Vec::new();
    let mut end_of_opts = false;

    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts || !a.starts_with('-') || a == "-" {
            positionals.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "--" => end_of_opts = true,
            "-d" | "--delete" => delete = true,
            "--no-deref" => deref = false,
            "--deref" => deref = true,
            "--stdin" => stdin_mode = true,
            "-z" => nul = true,
            "--create-reflog" => create_reflog = true,
            "--batch-updates" | "-0" => {
                bail!("unsupported flag \"--batch-updates\" (ported: -m, -d, --no-deref, --deref, --stdin, -z, --create-reflog)")
            }
            "-m" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("switch `m' requires a value"))?;
                msg = Some(v.clone());
            }
            _ if a.starts_with("-m") => msg = Some(a[2..].to_string()),
            _ => bail!("unsupported flag {a:?} (ported: -m, -d, --no-deref, --deref, --stdin, -z, --create-reflog)"),
        }
        i += 1;
    }

    if nul && !stdin_mode {
        bail!("-z requires --stdin");
    }
    if delete && create_reflog {
        bail!("--create-reflog does not make sense without <new-oid>");
    }

    let repo = gix::discover(".")?;

    if stdin_mode {
        if !positionals.is_empty() {
            bail!("--stdin takes no positional arguments");
        }
        return run_stdin(&repo, nul, deref, create_reflog, msg.as_deref());
    }

    // Command-line form: build exactly one edit.
    let (name, new_spec, old_spec) = if delete {
        match positionals.len() {
            1 => (positionals[0].as_str(), None, None),
            2 => (positionals[0].as_str(), None, Some(positionals[1].as_str())),
            _ => return usage(),
        }
    } else {
        match positionals.len() {
            2 => (
                positionals[0].as_str(),
                Some(positionals[1].as_str()),
                None,
            ),
            3 => (
                positionals[0].as_str(),
                Some(positionals[1].as_str()),
                Some(positionals[2].as_str()),
            ),
            _ => return usage(),
        }
    };

    // Value parsing failures are `fatal:` in git and exit 128, not usage errors.
    let new = match parse_val(&repo, new_spec, false) {
        Ok(v) => v,
        Err(e) => return fatal(e),
    };
    let old = match parse_val(&repo, old_spec, false) {
        Ok(v) => v,
        Err(e) => return fatal(e),
    };

    let edit = match build_edit(name, &new, &old, deref, create_reflog, msg.as_deref()) {
        Ok(e) => e,
        Err(e) => return fatal(e),
    };

    match repo.edit_reference(edit) {
        Ok(_) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            // `-d` reports `error:` and exits 1; the update form dies with 128.
            if delete {
                eprintln!("error: {e}");
                Ok(ExitCode::from(1))
            } else {
                eprintln!("fatal: update_ref failed for ref '{name}': {e}");
                Ok(ExitCode::from(128))
            }
        }
    }
}

/// Print git's usage block to stderr and return its exit code (129).
fn usage() -> Result<ExitCode> {
    eprintln!("usage: git update-ref [<options>] -d <refname> [<old-oid>]");
    eprintln!("   or: git update-ref [<options>]    <refname> <new-oid> [<old-oid>]");
    eprintln!("   or: git update-ref [<options>] --stdin [-z] [--batch-updates]");
    Ok(ExitCode::from(129))
}

/// Report `e` the way git's `die()` does and return its exit code (128).
fn fatal(e: anyhow::Error) -> Result<ExitCode> {
    eprintln!("fatal: {e:#}");
    Ok(ExitCode::from(128))
}

/// Resolve one optional `<oid>` slot.
///
/// `empty_is_missing` distinguishes the two stdin encodings: under `-z` an empty
/// field means "value omitted", everywhere else it means the zero value.
fn parse_val(repo: &gix::Repository, spec: Option<&str>, empty_is_missing: bool) -> Result<Val> {
    let Some(spec) = spec else { return Ok(Val::Missing) };
    if spec.is_empty() {
        return Ok(if empty_is_missing { Val::Missing } else { Val::Zero });
    }
    if spec.len() == repo.object_hash().len_in_hex() && spec.bytes().all(|b| b == b'0') {
        return Ok(Val::Zero);
    }
    let id = repo
        .rev_parse_single(spec)
        .map_err(|_| anyhow!("{spec}: not a valid SHA1"))?
        .detach();
    if !repo.has_object(id) {
        bail!("trying to write ref with nonexistent object {id}");
    }
    Ok(Val::Oid(id))
}

/// Validate `name` as a fully-qualified ref name.
fn refname(name: &str) -> Result<FullName> {
    name.try_into()
        .map_err(|e| anyhow!("invalid ref name '{name}': {e}"))
}

/// The `expected` constraint for an update, per git's `<old-oid>` rules:
/// omitted means "no constraint", zero means "must not exist", anything else
/// means "must exist with exactly this value".
fn expected_for_update(old: &Val) -> PreviousValue {
    match old {
        Val::Missing => PreviousValue::Any,
        Val::Zero => PreviousValue::MustNotExist,
        Val::Oid(id) => PreviousValue::MustExistAndMatch(Target::Object(*id)),
    }
}

/// The `expected` constraint for a deletion. Unlike an update, a zero (or
/// empty) `<old-oid>` imposes no constraint — stock git deletes regardless.
fn expected_for_delete(old: &Val) -> PreviousValue {
    match old {
        Val::Missing | Val::Zero => PreviousValue::Any,
        Val::Oid(id) => PreviousValue::MustExistAndMatch(Target::Object(*id)),
    }
}

/// Turn one `<ref> <new> <old>` triple into a `RefEdit`. A zero `<new-oid>`
/// deletes the ref, matching `git update-ref <ref> 0{40}`.
fn build_edit(
    name: &str,
    new: &Val,
    old: &Val,
    deref: bool,
    create_reflog: bool,
    msg: Option<&str>,
) -> Result<RefEdit> {
    let change = match new {
        Val::Oid(id) => Change::Update {
            log: log_change(create_reflog, msg),
            expected: expected_for_update(old),
            new: Target::Object(*id),
        },
        Val::Zero | Val::Missing => Change::Delete {
            expected: expected_for_delete(old),
            log: RefLog::AndReference,
        },
    };
    Ok(RefEdit {
        change,
        name: refname(name)?,
        deref,
    })
}

/// The reflog policy shared by every edit we emit: write the log alongside the
/// ref, with git's message (empty when `-m` was not given).
fn log_change(create_reflog: bool, msg: Option<&str>) -> LogChange {
    LogChange {
        mode: RefLog::AndReference,
        force_create_reflog: create_reflog,
        message: msg.unwrap_or_default().into(),
    }
}

/// State accumulated while reading `--stdin`: the pending transaction.
#[derive(Default)]
struct Batch {
    edits: Vec<RefEdit>,
    /// Refs a `verify` with a zero/absent old value requires to not exist.
    absent: Vec<String>,
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.absent.is_empty()
    }
}

/// Read `--stdin` instructions, then apply them as one atomic transaction.
fn run_stdin(
    repo: &gix::Repository,
    nul: bool,
    deref: bool,
    create_reflog: bool,
    msg: Option<&str>,
) -> Result<ExitCode> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| anyhow!("failed to read stdin: {e}"))?;

    let mut batch = Batch::default();
    // `option no-deref` applies to the next command naming a ref, and only that one.
    let mut next_no_deref = false;
    // Whether an explicit `start` opened a transaction that has not been committed.
    let mut open_txn = false;

    let records = if nul {
        split_nul_records(&input)?
    } else {
        split_line_records(&input)?
    };

    for fields in records {
        let Some(cmd) = fields.first().map(String::as_str) else {
            continue;
        };
        let args = &fields[1..];
        let edit_deref = deref && !next_no_deref;
        let mut consumed_option = false;

        match cmd {
            "start" => {
                if open_txn {
                    return fatal(anyhow!("transaction already started"));
                }
                open_txn = true;
            }
            "commit" => {
                if let Err(e) = apply(repo, std::mem::take(&mut batch)) {
                    return fatal(e);
                }
                open_txn = false;
            }
            "abort" => {
                batch = Batch::default();
                open_txn = false;
            }
            "prepare" => {
                bail!("stdin command \"prepare\" is not supported (gix-ref cannot hold locks across commands)")
            }
            "option" => {
                let [opt] = args else {
                    return fatal(anyhow!("option takes exactly one argument"));
                };
                if opt != "no-deref" {
                    return fatal(anyhow!("unknown option: {opt}"));
                }
                next_no_deref = true;
                consumed_option = true;
            }
            "update" | "create" | "delete" | "verify" => {
                match stage_oid_command(repo, &mut batch, cmd, args, nul, edit_deref, create_reflog, msg) {
                    Ok(()) => {}
                    Err(e) => return fatal(e),
                }
            }
            "symref-update" | "symref-create" | "symref-delete" | "symref-verify" => {
                match stage_symref_command(repo, &mut batch, cmd, args, nul, create_reflog, msg) {
                    Ok(()) => {}
                    Err(e) => return fatal(e),
                }
            }
            _ => return fatal(anyhow!("unknown command: {}", fields.join(" "))),
        }

        if !consumed_option {
            next_no_deref = false;
        }
    }

    // A transaction opened with `start` and never committed is discarded, as git does.
    if open_txn {
        return Ok(ExitCode::SUCCESS);
    }
    if let Err(e) = apply(repo, batch) {
        return fatal(e);
    }
    Ok(ExitCode::SUCCESS)
}

/// Commit one accumulated batch: absence checks first, then the ref transaction.
fn apply(repo: &gix::Repository, batch: Batch) -> Result<()> {
    for name in &batch.absent {
        refname(name)?; // reject malformed names the same way an edit would
        if repo.try_find_reference(name.as_str())?.is_some() {
            bail!("cannot lock ref '{name}': reference already exists");
        }
    }
    if batch.is_empty() {
        return Ok(());
    }
    repo.edit_references(batch.edits)?;
    Ok(())
}

/// Stage `update`/`create`/`delete`/`verify`.
#[allow(clippy::too_many_arguments)]
fn stage_oid_command(
    repo: &gix::Repository,
    batch: &mut Batch,
    cmd: &str,
    args: &[String],
    nul: bool,
    deref: bool,
    create_reflog: bool,
    msg: Option<&str>,
) -> Result<()> {
    // Under -z the slots are always present (possibly empty); otherwise a
    // trailing slot may be omitted entirely.
    let slot = |n: usize| -> Option<&str> { args.get(n).map(String::as_str) };

    let name = slot(0).ok_or_else(|| anyhow!("{cmd}: missing <ref>"))?;

    match cmd {
        "update" => {
            if args.len() < 2 || args.len() > 3 {
                bail!("update: wrong number of arguments");
            }
            let new = parse_val(repo, slot(1), nul)?;
            let old = parse_val(repo, slot(2), nul)?;
            if matches!(new, Val::Missing) {
                bail!("update {name}: missing <new-oid>");
            }
            batch
                .edits
                .push(build_edit(name, &new, &old, deref, create_reflog, msg)?);
        }
        "create" => {
            if args.len() != 2 {
                bail!("create: wrong number of arguments");
            }
            let new = parse_val(repo, slot(1), nul)?;
            let Val::Oid(id) = new else {
                bail!("create {name}: zero <new-oid>");
            };
            batch.edits.push(RefEdit {
                change: Change::Update {
                    log: log_change(create_reflog, msg),
                    expected: PreviousValue::MustNotExist,
                    new: Target::Object(id),
                },
                name: refname(name)?,
                deref,
            });
        }
        "delete" => {
            if args.len() > 2 {
                bail!("delete: wrong number of arguments");
            }
            let old = parse_val(repo, slot(1), nul)?;
            batch.edits.push(RefEdit {
                change: Change::Delete {
                    expected: expected_for_delete(&old),
                    log: RefLog::AndReference,
                },
                name: refname(name)?,
                deref,
            });
        }
        "verify" => {
            if args.len() > 2 {
                bail!("verify: wrong number of arguments");
            }
            // `verify` is an update to the value it already has: gitoxide skips
            // the reflog when old == new, so nothing is logged, as in git.
            match parse_val(repo, slot(1), nul)? {
                Val::Oid(id) => batch.edits.push(RefEdit {
                    change: Change::Update {
                        log: log_change(create_reflog, msg),
                        expected: PreviousValue::MustExistAndMatch(Target::Object(id)),
                        new: Target::Object(id),
                    },
                    name: refname(name)?,
                    deref,
                }),
                // Zero or missing old value: the ref must not exist.
                Val::Zero | Val::Missing => batch.absent.push(name.to_string()),
            }
        }
        _ => unreachable!("caller filters the command set"),
    }
    Ok(())
}

/// Stage the `symref-*` commands. These always operate on the named ref itself
/// (never through it), which is the only mode git allows for `symref-verify`.
fn stage_symref_command(
    repo: &gix::Repository,
    batch: &mut Batch,
    cmd: &str,
    args: &[String],
    nul: bool,
    create_reflog: bool,
    msg: Option<&str>,
) -> Result<()> {
    let slot = |n: usize| -> Option<&str> { args.get(n).map(String::as_str) };
    let name = slot(0).ok_or_else(|| anyhow!("{cmd}: missing <ref>"))?;

    match cmd {
        "symref-create" => {
            let target = slot(1).ok_or_else(|| anyhow!("symref-create: missing <new-target>"))?;
            batch.edits.push(RefEdit {
                change: Change::Update {
                    log: log_change(create_reflog, msg),
                    expected: PreviousValue::MustNotExist,
                    new: Target::Symbolic(refname(target)?),
                },
                name: refname(name)?,
                deref: false,
            });
        }
        "symref-update" => {
            let target = slot(1).ok_or_else(|| anyhow!("symref-update: missing <new-target>"))?;
            // Optional old value: `ref <old-target>` or `oid <old-oid>`.
            let expected = match slot(2) {
                None | Some("") => PreviousValue::Any,
                Some("ref") => {
                    let old = slot(3)
                        .ok_or_else(|| anyhow!("symref-update {name}: missing <old-target>"))?;
                    PreviousValue::MustExistAndMatch(Target::Symbolic(refname(old)?))
                }
                Some("oid") => match parse_val(repo, slot(3), nul)? {
                    Val::Oid(id) => PreviousValue::MustExistAndMatch(Target::Object(id)),
                    Val::Zero | Val::Missing => PreviousValue::MustNotExist,
                },
                Some(kind) => bail!("symref-update {name}: invalid old value kind '{kind}'"),
            };
            batch.edits.push(RefEdit {
                change: Change::Update {
                    log: log_change(create_reflog, msg),
                    expected,
                    new: Target::Symbolic(refname(target)?),
                },
                name: refname(name)?,
                deref: false,
            });
        }
        "symref-delete" => {
            let expected = match slot(1) {
                None | Some("") => PreviousValue::Any,
                Some(old) => PreviousValue::MustExistAndMatch(Target::Symbolic(refname(old)?)),
            };
            batch.edits.push(RefEdit {
                change: Change::Delete {
                    expected,
                    log: RefLog::AndReference,
                },
                name: refname(name)?,
                deref: false,
            });
        }
        "symref-verify" => match slot(1) {
            None | Some("") => batch.absent.push(name.to_string()),
            Some(old) => {
                let target = Target::Symbolic(refname(old)?);
                batch.edits.push(RefEdit {
                    change: Change::Update {
                        log: log_change(create_reflog, msg),
                        expected: PreviousValue::MustExistAndMatch(target.clone()),
                        new: target,
                    },
                    name: refname(name)?,
                    deref: false,
                });
            }
        },
        _ => unreachable!("caller filters the command set"),
    }
    Ok(())
}

/// Split NUL-terminated `--stdin -z` input into records.
///
/// The first field of each record is `<command> SP <ref>` (or a bare command),
/// and every following NUL-separated field up to the record's argument count is
/// a value slot. The per-command field counts come straight from the man page.
fn split_nul_records(input: &str) -> Result<Vec<Vec<String>>> {
    let mut fields: Vec<&str> = input.split('\0').collect();
    // A well-formed stream ends with a trailing NUL, producing one empty tail.
    if fields.last().is_some_and(|f| f.is_empty()) {
        fields.pop();
    }

    let mut records = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let head = fields[i];
        i += 1;
        let (cmd, first) = match head.split_once(' ') {
            Some((c, r)) => (c.to_string(), Some(r.to_string())),
            None => (head.to_string(), None),
        };
        // Number of NUL-separated value slots that follow the head.
        let extra = match cmd.as_str() {
            "update" => 2,
            "create" | "delete" | "verify" => 1,
            "symref-update" => 3,
            "symref-create" => 1,
            "symref-delete" | "symref-verify" => 1,
            "option" | "start" | "prepare" | "commit" | "abort" => 0,
            _ => bail!("unknown command: {head}"),
        };
        let mut record = vec![cmd];
        if let Some(f) = first {
            record.push(f);
        }
        for _ in 0..extra {
            match fields.get(i) {
                Some(v) => {
                    record.push((*v).to_string());
                    i += 1;
                }
                // Trailing optional slots may be absent at end of input.
                None => break,
            }
        }
        records.push(record);
    }
    Ok(records)
}

/// Split newline-terminated `--stdin` input into space-separated, optionally
/// C-quoted fields.
fn split_line_records(input: &str) -> Result<Vec<Vec<String>>> {
    let mut records = Vec::new();
    for line in input.lines() {
        if line.is_empty() {
            continue;
        }
        records.push(tokenize(line)?);
    }
    Ok(records)
}

/// Split one instruction line into fields, honouring C-style quoting.
fn tokenize(line: &str) -> Result<Vec<String>> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i <= b.len() {
        if i == b.len() {
            // A trailing separator yields one final empty field.
            out.push(String::new());
            break;
        }
        if b[i] == b'"' {
            let (s, used) = unquote_c(&b[i..])?;
            out.push(s);
            i += used;
        } else {
            let start = i;
            while i < b.len() && b[i] != b' ' {
                i += 1;
            }
            out.push(line[start..i].to_string());
        }
        if i < b.len() {
            if b[i] != b' ' {
                bail!("unexpected character after quoted field in: {line}");
            }
            i += 1;
        } else {
            break;
        }
    }
    Ok(out)
}

/// Undo one C-style quoted string starting at `b[0] == '"'`.
///
/// Returns the decoded value and the number of bytes consumed, closing quote
/// included.
fn unquote_c(b: &[u8]) -> Result<(String, usize)> {
    let mut out: Vec<u8> = Vec::new();
    let mut i = 1;
    loop {
        let Some(&c) = b.get(i) else {
            bail!("unterminated quoted string");
        };
        i += 1;
        match c {
            b'"' => break,
            b'\\' => {
                let Some(&e) = b.get(i) else {
                    bail!("unterminated escape in quoted string");
                };
                i += 1;
                match e {
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(0x0b),
                    b'\\' | b'"' => out.push(e),
                    b'0'..=b'7' => {
                        let mut v = u32::from(e - b'0');
                        for _ in 0..2 {
                            match b.get(i) {
                                Some(&d) if d.is_ascii_digit() && d < b'8' => {
                                    v = v * 8 + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        if v > 0xff {
                            bail!("octal escape out of range in quoted string");
                        }
                        out.push(v as u8);
                    }
                    _ => bail!("invalid escape '\\{}' in quoted string", e as char),
                }
            }
            _ => out.push(c),
        }
    }
    let s = String::from_utf8(out).map_err(|_| anyhow!("quoted string is not valid UTF-8"))?;
    Ok((s, i))
}
