//! `git update-ref` — update the object name stored in a ref, safely.
//!
//! Covered: the two command-line forms (`<ref> <new-oid> [<old-oid>]` and
//! `-d <ref> [<old-oid>]`) with `-m`, `--no-deref`/`--deref` and
//! `--create-reflog`, plus `--stdin` (and `--stdin -z`) with the `update`,
//! `create`, `delete`, `verify`, `symref-update`, `symref-create`,
//! `symref-delete`, `symref-verify`, `option no-deref`, and the transaction
//! controls `start`, `prepare`, `commit` and `abort`. A batch without
//! `--batch-updates` is applied through a single gitoxide ref transaction, so it
//! is all-or-nothing exactly like stock git. Ref edits print nothing on success;
//! the explicit transaction controls each print `<command>: ok` to stdout, as
//! stock git does. Exit codes match (0 on success, 128 on a fatal failure, 129
//! on a usage error, 1 for a failed `-d`).
//!
//! The `--stdin` transaction state machine mirrors git's: an implicit
//! transaction auto-commits at end of input, while an explicit `start` (or a
//! `prepare` left uncommitted) auto-aborts. A `prepare`d transaction accepts
//! only `commit`/`abort` ("prepared transactions can only be closed"), a closed
//! one accepts only `start` ("transaction is closed"), and `start` cannot
//! restart an already-started transaction ("cannot restart ongoing
//! transaction"). `prepare` validates the staged edits by acquiring the same
//! locks git would and rolling them back, so a doomed batch fails at `prepare`
//! just as it does under git.
//!
//! `--batch-updates` is accepted: outside `--stdin` it is the fatal error git
//! reports, and with `--stdin` each staged edit is applied on its own so one
//! rejection no longer aborts the rest.
//!
//! Deleting the branch `HEAD` points at leaves a reflog entry behind. git's
//! `split_head_update()` (refs/files-backend.c) adds a `REF_LOG_ONLY` update for `HEAD`
//! alongside the real deletion, carrying the deletion's ids and `-m <reason>`, so
//! `.git/logs/HEAD` gains a `<old> <null>` line and survives while the branch's own log is
//! unlinked. That applies to `-d <ref>`, `-d HEAD` and the `--stdin` `delete` command alike,
//! and to a detached `HEAD` not at all. `--create-reflog` has no say in it: `cmd_update_ref`
//! ORs `create_reflog_flag` into its `update_ref()` call only, never into `delete_ref()`, so
//! a deletion force-creates nothing.
//!
//! One-level lowercase ref names such as `foo` are served too, even though
//! `gix-validate` refuses them: `ref_transaction_update()` validates an update
//! with `check_refname_format(refname, REFNAME_ALLOW_ONELEVEL)`, so
//! `git update-ref main <oid>` writes `$GIT_DIR/main` and this port writes it
//! directly rather than through a gitoxide transaction. A null new value takes
//! git's other branch (`refname_is_safe()`), which refuses such a name, and no
//! reflog is autocreated for one.
//!
//! Not covered: `--create-reflog` on a one-level name, which would have to write
//! `$GIT_DIR/logs/<name>` by hand; it keeps the bad-name refusal instead.

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

/// git's `-h` / `usage_with_options()` block, byte for byte.
const USAGE: &str = "\
usage: git update-ref [<options>] -d <refname> [<old-oid>]
   or: git update-ref [<options>]    <refname> <new-oid> [<old-oid>]
   or: git update-ref [<options>] --stdin [-z] [--batch-updates]

    -m <reason>           reason of the update
    -d                    delete the reference
    --no-deref            update <refname> not the one it points to
    --deref               opposite of --no-deref
    -z                    stdin has NUL-terminated arguments
    --[no-]stdin          read updates from stdin
    --[no-]create-reflog  create a reflog
    -0, --[no-]batch-updates
                          batch reference updates

";

/// `cmd_update_ref`'s `struct option options[]` (builtin/update-ref.c), in table
/// order, as [`super::resolve_long`] reads it. Only the entries with a
/// `long_name` appear; `-m`, `-d` and `-z` have none.
///
/// `no-deref` is spelled with its negation baked into the name, so it is the
/// *unset* sense of `deref` rather than a name of its own — which is why
/// `--deref=x` is refused as ``option `no-no-deref' takes no value``:
/// `optname()` prefixes `no-` to the table's own spelling.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "no-deref",      neg: true, arg: super::Arg::None },
    super::LongOpt { name: "stdin",         neg: true, arg: super::Arg::None },
    super::LongOpt { name: "create-reflog", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "batch-updates", neg: true, arg: super::Arg::None },
];

/// The parsed command line, mirroring the variables in git's `cmd_update_ref`.
#[derive(Default)]
struct Opts {
    msg: Option<String>,
    delete: bool,
    no_deref: bool,
    end_null: bool,
    read_stdin: bool,
    create_reflog: bool,
    batch_updates: bool,
    positionals: Vec<String>,
}

/// `git update-ref` — see the module docs for the covered surface.
pub fn update_ref(args: &[String]) -> Result<ExitCode> {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };

    // git: `if (msg && !*msg) die(...)`, before every other consistency check.
    if opts.msg.as_deref() == Some("") {
        return fatal(anyhow!("Refusing to perform update with empty message."));
    }

    // Every ref this moves carries a reflog line, and git writes those with an
    // identity it synthesizes from the OS when `user.*` is unset — only a
    // `commit` with nothing determinable is refused. Without this a bare runner,
    // a container or a `sudo` shell cannot switch branches at all, and a
    // recursive submodule walk aborts on the first one it reaches.
    let mut repo = gix::discover(".")?;
    crate::ensure_reflog_identity(&mut repo);
    let deref = !opts.no_deref;

    if opts.read_stdin {
        // git rejects `-d` and positionals here, before looking at `-z`.
        if opts.delete || !opts.positionals.is_empty() {
            return usage();
        }
        return run_stdin(
            &repo,
            opts.end_null,
            deref,
            opts.create_reflog,
            opts.batch_updates,
            opts.msg.as_deref(),
        );
    }

    // Both of these outrank the argument-count check in git's source order.
    if opts.batch_updates {
        return fatal(anyhow!("--batch-updates can only be used with --stdin"));
    }
    if opts.end_null {
        return usage();
    }

    let p = &opts.positionals;
    // Command-line form: build exactly one edit.
    let (name, new_spec, old_spec) = if opts.delete {
        match p.len() {
            1 => (p[0].as_str(), None, None),
            2 => (p[0].as_str(), None, Some(p[1].as_str())),
            _ => return usage(),
        }
    } else {
        match p.len() {
            2 => (p[0].as_str(), Some(p[1].as_str()), None),
            3 => (p[0].as_str(), Some(p[1].as_str()), Some(p[2].as_str())),
            _ => return usage(),
        }
    };

    // Value parsing failures are `fatal:` in git and exit 128, not usage errors.
    // `cmd_update_ref` resolves the new value, then the old one, and only then
    // hands the refname to `refs_delete_ref` — so a bad old SHA1 outranks a bad
    // ref name: `update-ref -d main v0.2.0` reports the *value*, not the name.
    let new = match parse_slot(&repo, new_spec, false, Slot::New) {
        Ok(v) => v,
        Err(e) => return fatal(e),
    };
    let old = match parse_slot(&repo, old_spec, false, Slot::Old) {
        Ok(v) => v,
        Err(e) => return fatal(e),
    };

    // A deletion only requires a "safe" name, not a well-formed one, and a bad
    // one is an `error:` with exit 1 rather than a fatal.
    if opts.delete && !refname_is_safe(name) {
        eprintln!("error: refusing to update ref with bad name '{name}'");
        return Ok(ExitCode::from(1));
    }
    // git reports this one through `update_ref`'s die-on-error wrapper, so it
    // carries the ref name and the same 128 the other update failures use.
    if let Err(e) = check_new_object(&repo, name, &new) {
        eprintln!("fatal: update_ref failed for ref '{name}': {e:#}");
        return Ok(ExitCode::from(128));
    }

    let edit = match build_edit(name, &new, &old, deref, opts.create_reflog, opts.msg.as_deref()) {
        Ok(e) => e,
        // `gix-validate` is stricter than git's `refname_is_safe`; a deletion
        // git would accept targets a ref that cannot exist, so it is a no-op.
        Err(_) if opts.delete => return Ok(ExitCode::SUCCESS),
        // `ref_transaction_update()` validates an update with
        // `check_refname_format(refname, REFNAME_ALLOW_ONELEVEL)`, so a
        // single-component name like `main` or `v0.2.0` is well formed and lands
        // in `$GIT_DIR/<name>`. gitoxide's `FullName` refuses those (its
        // `SomeLowercase` rule wants either a `/` or an all-caps pseudo-ref), so
        // the write goes through directly rather than through a transaction.
        Err(_)
            if !opts.create_reflog
                && matches!(new, Val::Oid(_))
                && one_level_update_ok(name) =>
        {
            return write_one_level_ref(&repo, name, &new, &old);
        }
        Err(_) => {
            eprintln!(
                "fatal: update_ref failed for ref '{name}': refusing to update ref with bad name '{name}'"
            );
            return Ok(ExitCode::from(128));
        }
    };

    match repo.edit_reference(edit) {
        Ok(_) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            // `-d` reports `error:` and exits 1; the update form dies with 128.
            let msg = lock_error(&e);
            if opts.delete {
                eprintln!("error: {msg}");
                Ok(ExitCode::from(1))
            } else {
                eprintln!("fatal: update_ref failed for ref '{name}': {msg}");
                Ok(ExitCode::from(128))
            }
        }
    }
}

/// Parse the command line the way git's `parse_options()` does for this command:
/// long options accept unique abbreviations and `--no-` negations, short options
/// cluster, and any error is reported then exits 129.
fn parse_args(args: &[String]) -> Result<Opts, ExitCode> {
    let mut o = Opts::default();
    let mut end_of_opts = false;

    // `args` arrives without the subcommand (dispatch::run is handed `&args[1..]`),
    // so the first element is already the first option or positional.
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts || !a.starts_with('-') || a == "-" {
            o.positionals.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // placed after that `--` break and ahead of parse_long_opt(): the name
        // never abbreviates and never takes an `=<value>`, so `resolve_long()`
        // is never asked about it. update-ref's table has no `PARSE_OPT_HIDDEN`
        // entry, so `USAGE_FULL` renders the same block `-h` prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Err(ExitCode::from(129));
        }
        if let Some(long) = a.strip_prefix("--") {
            let (opt, unset) = match super::resolve_long(LONG_OPTS, long) {
                super::Resolved::One(opt, unset) => (opt, unset),
                super::Resolved::Ambiguous(first, second) => {
                    return Err(super::ambiguous_option(a, &first, &second, USAGE))
                }
                super::Resolved::Unknown => {
                    eprintln!("error: unknown option `{long}'");
                    eprint!("{USAGE}");
                    return Err(ExitCode::from(129));
                }
            };
            // Every entry here is a flag, so an attached value is
            // `PARSE_OPT_ERROR` out of `get_value()`: one line, no usage block.
            if long.contains('=') {
                eprintln!("error: option `{}' takes no value", optname(opt, unset));
                return Err(ExitCode::from(129));
            }
            apply_long(&mut o, opt.name, !unset);
            i += 1;
            continue;
        }

        // A short-option cluster such as `-dz` or `-mreason`.
        let cluster: Vec<char> = a[1..].chars().collect();
        let mut c = 0;
        while c < cluster.len() {
            match cluster[c] {
                'd' => o.delete = true,
                'z' => o.end_null = true,
                '0' => o.batch_updates = true,
                'h' => {
                    print!("{USAGE}");
                    return Err(ExitCode::from(129));
                }
                'm' => {
                    let rest: String = cluster[c + 1..].iter().collect();
                    if rest.is_empty() {
                        i += 1;
                        match args.get(i) {
                            Some(v) => o.msg = Some(v.clone()),
                            None => {
                                eprintln!("error: switch `m' requires a value");
                                return Err(ExitCode::from(129));
                            }
                        }
                    } else {
                        o.msg = Some(rest);
                    }
                    break; // `-m` swallows the rest of the cluster
                }
                other => {
                    eprintln!("error: unknown switch `{other}'");
                    eprint!("{USAGE}");
                    return Err(ExitCode::from(129));
                }
            }
            c += 1;
        }
        i += 1;
    }
    Ok(o)
}

/// `optname()` (parse-options.c:69-91) for a long option: the table's own
/// spelling, with `no-` prefixed when the unset sense was selected. That is
/// literally `"no-%s"` on `long_name`, so an entry already named `no-deref`
/// reports as `no-no-deref` — verified against stock 2.55.0, where
/// `git update-ref --deref=x` says ``error: option `no-no-deref' takes no
/// value``.
fn optname(opt: &super::LongOpt, unset: bool) -> String {
    match unset {
        true => format!("no-{}", opt.name),
        false => opt.name.to_string(),
    }
}

/// Set the flag a resolved long option controls. `set` is the sense the spelling
/// selected, so `--deref` reaches the `no-deref` entry with `set == false`.
fn apply_long(o: &mut Opts, name: &str, set: bool) {
    match name {
        "no-deref" => o.no_deref = set,
        "stdin" => o.read_stdin = set,
        "create-reflog" => o.create_reflog = set,
        "batch-updates" => o.batch_updates = set,
        _ => unreachable!("resolve_long only returns LONG_OPTS entries"),
    }
}

/// Whether `name` is the single-component form git accepts for an update and
/// gitoxide does not: well formed under `REFNAME_ALLOW_ONELEVEL` and carrying no
/// `/`, which is exactly the set `FullName` rejects out of what git allows.
///
/// `HEAD` and the other all-caps pseudo-refs are excluded because `FullName`
/// already takes them, so they never reach here.
///
/// Only an update to a real object may use this. `ref_transaction_update()`
/// picks its check on the new value — `check_refname_format(…, ALLOW_ONELEVEL)`
/// for a non-null one, `refname_is_safe()` for a null one — and the latter
/// refuses a lowercase one-level name, which is why `git update-ref main 0{40}`
/// is `refusing to update ref with bad name 'main'` rather than a deletion.
fn one_level_update_ok(name: &str) -> bool {
    !name.contains('/')
        && super::check_ref_format::check_refname_format_onelevel(name.as_bytes())
}

/// `update_ref()`'s die-on-error wrapper around a failed lock: one line,
/// carrying the ref it was updating and the `cannot lock ref` reason inside it,
/// then exit 128.
fn lock_failure(name: &str, reason: &str) -> ExitCode {
    eprintln!("fatal: update_ref failed for ref '{name}': cannot lock ref '{name}': {reason}");
    ExitCode::from(128)
}

/// Write a single-component ref the way `files_transaction_finish()` does:
/// through `<name>.lock` in `$GIT_DIR`, renamed into place, holding the 41-byte
/// `<hex>\n` body.
///
/// No reflog is written. `should_autocreate_reflog()` covers only `HEAD` and the
/// `refs/heads/`, `refs/remotes/`, `refs/notes/` and `refs/worktree/` prefixes,
/// so a one-level ref gets none unless `--create-reflog` forces one — and that
/// spelling is left to the caller's existing rejection rather than served here.
fn write_one_level_ref(
    repo: &gix::Repository,
    name: &str,
    new: &Val,
    old: &Val,
) -> Result<ExitCode> {
    let path = repo.git_dir().join(name);
    let current = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| gix::ObjectId::from_hex(s.trim().as_bytes()).ok());

    // The `<oldvalue>` constraint, with git's two diagnostics: a mismatch names
    // both values, and a zero old value asserts the ref does not exist yet.
    match old {
        Val::Missing => {}
        Val::Zero => {
            if let Some(have) = current {
                let _ = have;
                return Ok(lock_failure(name, "reference already exists"));
            }
        }
        Val::Oid(want) => match current {
            Some(have) if have == *want => {}
            Some(have) => {
                return Ok(lock_failure(name, &format!("is at {have} but expected {want}")));
            }
            None => {
                return Ok(lock_failure(
                    name,
                    &format!("unable to resolve reference '{name}'"),
                ));
            }
        },
    }

    match new {
        Val::Oid(id) => {
            let lock = repo.git_dir().join(format!("{name}.lock"));
            if std::fs::write(&lock, format!("{id}\n")).is_err() {
                return Ok(lock_failure(name, "unable to create lock file"));
            }
            if std::fs::rename(&lock, &path).is_err() {
                let _ = std::fs::remove_file(&lock);
                return Ok(lock_failure(name, "unable to write lock file"));
            }
        }
        Val::Zero | Val::Missing => {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// git's `refname_is_safe()`: the weaker check a deletion has to pass.
fn refname_is_safe(name: &str) -> bool {
    match name.strip_prefix("refs/") {
        Some(rest) => {
            !rest.is_empty()
                && !rest.starts_with('/')
                && !rest.ends_with('/')
                // `normalize_path_copy()` must leave the remainder unchanged.
                && rest
                    .split('/')
                    .all(|c| !c.is_empty() && c != "." && c != "..")
        }
        None => !name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'),
    }
}

/// Print git's usage block to stderr and return its exit code (129).
fn usage() -> Result<ExitCode> {
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// Report `e` the way git's `die()` does and return its exit code (128).
fn fatal(e: anyhow::Error) -> Result<ExitCode> {
    eprintln!("fatal: {e:#}");
    Ok(ExitCode::from(128))
}

/// The two spellings git uses when a value slot fails to parse.
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    New,
    Old,
}

/// Resolve one optional `<oid>` slot, naming the slot in the failure message.
///
/// `empty_is_missing` distinguishes the two stdin encodings: under `-z` an empty
/// field means "value omitted", everywhere else it means the zero value.
///
/// git resolves both slots with `repo_get_oid_with_flags(…,
/// GET_OID_SKIP_AMBIGUITY_CHECK)`, whose `get_oid_basic` accepts a
/// full-length hex string *without* looking the object up. That is what makes
/// `<old-oid>` an opaque expected value: a stale guard that names no object is
/// a lost race (the lock check reports "is at … but expected …", exit 1), not
/// bad input (exit 128). Only a value that is neither full hex nor a resolvable
/// revision is a parse failure.
fn parse_slot(
    repo: &gix::Repository,
    spec: Option<&str>,
    empty_is_missing: bool,
    slot: Slot,
) -> Result<Val> {
    let Some(spec) = spec else { return Ok(Val::Missing) };
    if spec.is_empty() {
        return Ok(if empty_is_missing { Val::Missing } else { Val::Zero });
    }
    if spec.len() == repo.object_hash().len_in_hex() && spec.bytes().all(|b| b == b'0') {
        return Ok(Val::Zero);
    }
    // git's `get_oid_hex` fast path: a full-length hex string is taken verbatim.
    if let Ok(id) = ObjectId::from_hex(spec.as_bytes()) {
        return Ok(Val::Oid(id));
    }
    // `GET_OID_SKIP_AMBIGUITY_CHECK` silences `get_oid_basic()`'s *first* warning
    // and nothing else, so a plain name matching more than one ref still earns the
    // second one: stock `git update-ref refs/heads/z dup` prints
    // `warning: refname 'dup' is ambiguous.` once per slot it resolves.
    crate::objname::warn_ambiguous_operand(
        repo,
        spec,
        crate::objname::OidFlags { skip_ambiguity_check: true, ..Default::default() },
    );
    let id = repo
        .rev_parse_single(spec)
        .map_err(|_| match slot {
            Slot::New => anyhow!("{spec}: not a valid SHA1"),
            Slot::Old => anyhow!("{spec}: not a valid old SHA1"),
        })?
        .detach();
    Ok(Val::Oid(id))
}

/// Restate a gitoxide ref-transaction failure in git's `lock_ref_oid_basic`
/// wording, so a caller branching on the message sees what stock git prints.
///
/// gitoxide reports the precondition that failed; git reports it as a failure to
/// take the lock. The three preconditions `update-ref` can violate map one to
/// one, and anything else is passed through unchanged.
fn lock_error(e: &gix::reference::edit::Error) -> String {
    use gix::refs::file::transaction::prepare::Error as P;
    let gix::reference::edit::Error::FileTransactionPrepare(p) = e else {
        return e.to_string();
    };
    match p {
        P::ReferenceOutOfDate {
            full_name,
            expected,
            actual,
        } => format!("cannot lock ref '{full_name}': is at {actual} but expected {expected}"),
        P::MustNotExist { full_name, .. } => {
            format!("cannot lock ref '{full_name}': reference already exists")
        }
        P::MustExist { full_name, .. } => format!(
            "cannot lock ref '{full_name}': unable to resolve reference '{full_name}'"
        ),
        _ => e.to_string(),
    }
}

/// git's `ref_transaction_prepare` check: the object a ref is about to point at
/// has to exist. The `<old-oid>` guard is deliberately exempt.
fn check_new_object(repo: &gix::Repository, name: &str, new: &Val) -> Result<()> {
    if let Val::Oid(id) = new {
        if !repo.has_object(*id) {
            crate::git_fatal!("trying to write ref '{name}' with nonexistent object {id}");
        }
    }
    Ok(())
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
            message: delete_message(msg),
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

/// The `-m <reason>` a deletion records, for the one log that outlives it.
///
/// A deleted ref's own log is unlinked, so this only ever reaches the `REF_LOG_ONLY`
/// mirror git's `split_head_update()`/`split_symref_update()` add for `HEAD` — the
/// `<old> <null> … <reason>` line `update-ref -m <reason> -d refs/heads/main` leaves in
/// `.git/logs/HEAD` when `HEAD` points at the branch being deleted.
///
/// `--create-reflog` is deliberately absent: `cmd_update_ref` (builtin/update-ref.c) ORs
/// `create_reflog_flag` into the `update_ref()` call only, never into `delete_ref()`, so
/// `update-ref -d --create-reflog` force-creates nothing. Verified against stock 2.55.0 in
/// a bare repo with no `logs/` at all: the update form creates `logs/HEAD`, the delete form
/// leaves the directory empty.
fn delete_message(msg: Option<&str>) -> gix::bstr::BString {
    msg.unwrap_or_default().into()
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

/// The `--stdin` transaction state, mirroring git's `enum update_refs_state`.
#[derive(Clone, Copy, PartialEq)]
enum TxnState {
    /// The implicit transaction that exists before any explicit `start`.
    /// Auto-commits at end of input.
    Open,
    /// An explicit `start` opened a transaction. Auto-aborts at end of input.
    Started,
    /// `prepare` succeeded; only `commit`/`abort` may follow. Auto-aborts at EOF.
    Prepared,
    /// `commit`/`abort` closed the transaction; only `start` may follow.
    Closed,
}

/// git recognises the command name first (so an unknown command outranks the
/// state guard), then classifies it to drive the state machine.
#[derive(Clone, Copy, PartialEq)]
enum Cat {
    /// A ref edit or `option`: allowed only while the transaction is open.
    Edit,
    Start,
    Prepare,
    Commit,
    Abort,
}

/// Classify one `--stdin` command, or `None` if git would `die("unknown command")`.
fn categorize(cmd: &str) -> Option<Cat> {
    Some(match cmd {
        "update" | "create" | "delete" | "verify" | "symref-update" | "symref-create"
        | "symref-delete" | "symref-verify" | "option" => Cat::Edit,
        "start" => Cat::Start,
        "prepare" => Cat::Prepare,
        "commit" => Cat::Commit,
        "abort" => Cat::Abort,
        _ => return None,
    })
}

/// `builtin/update-ref.c:679-693`'s `command[]`, reduced to what the dispatch
/// loop reads off it: the prefix, and whether the entry declares arguments.
const COMMANDS: &[(&str, bool)] = &[
    ("update", true),
    ("create", true),
    ("delete", true),
    ("verify", true),
    ("symref-update", true),
    ("symref-create", true),
    ("symref-delete", true),
    ("symref-verify", true),
    ("option", true),
    ("start", false),
    ("prepare", false),
    ("abort", false),
    ("commit", false),
];

/// `builtin/update-ref.c:720-738` — pick the command out of one whole input line.
///
/// ```c
/// for (i = 0; i < ARRAY_SIZE(command); i++) {
///         const char *prefix = command[i].prefix;
///         char c;
///
///         if (!starts_with(input.buf, prefix))
///                 continue;
///
///         /*
///          * If the command has arguments, verify that it's
///          * followed by a space. Otherwise, it shall be followed
///          * by a line terminator.
///          */
///         c = command[i].args ? ' ' : line_termination;
///         if (input.buf[strlen(prefix)] != c)
///                 continue;
///
///         cmd = &command[i];
///         break;
/// }
/// ```
///
/// The byte after the prefix has to be exactly right, which is why `commit foo`
/// and `option` are *unknown commands* rather than a mis-argued `commit` and a
/// mis-argued `option`, and why a final line with no terminator at all is one
/// too: `input.buf[strlen("start")]` is then the string's NUL, not `'\n'`.
///
/// `line` carries its terminator, so the `-z` case where that NUL *is* the
/// terminator falls out of the same test.
fn match_command(line: &str, terminator: char) -> Option<&'static str> {
    COMMANDS.iter().find_map(|&(prefix, takes_args)| {
        let rest = line.strip_prefix(prefix)?;
        let want = if takes_args { ' ' } else { terminator };
        rest.starts_with(want).then_some(prefix)
    })
}

/// C's `isspace()` over the ASCII range: space, `\t`, `\n`, `\v`, `\f`, `\r`.
/// Rust's `is_ascii_whitespace()` omits the vertical tab.
fn is_c_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

/// What C's `%s` prints for `input.buf`: everything up to the first NUL.
///
/// The whole line is interpolated, terminator included — `strbuf_getwholeline()`
/// keeps it — so `die("unknown command: %s", input.buf)` emits `bogus line\n`
/// and `die()` then adds its own newline, giving a stderr that ends `\n\n`. In
/// `-z` mode the terminator *is* the NUL, so `%s` stops before it and only
/// `die()`'s newline shows.
fn c_string(s: &str) -> &str {
    s.split('\0').next().unwrap_or(s)
}

/// git's `prepare`: acquire the locks the staged batch needs and validate its
/// preconditions, then roll everything back. gitoxide's prepared transaction is
/// perfectly rolled back when dropped, so this catches a doomed batch at
/// `prepare` time exactly like stock git, without writing anything.
fn validate_prepare(repo: &gix::Repository, batch: &Batch) -> Result<()> {
    for name in &batch.absent {
        refname(name)?;
        if repo.try_find_reference(name.as_str())?.is_some() {
            crate::git_fatal!("cannot lock ref '{name}': reference already exists");
        }
    }
    if batch.edits.is_empty() {
        return Ok(());
    }
    let prepared = repo
        .refs
        .transaction()
        .prepare(
            batch.edits.clone(),
            gix::lock::acquire::Fail::Immediately,
            gix::lock::acquire::Fail::Immediately,
        )
        .map_err(|e| anyhow!("{e}"))?;
    drop(prepared); // rolls the acquired locks back, writing nothing.
    Ok(())
}

/// Read `--stdin` instructions, then apply them as one atomic transaction.
fn run_stdin(
    repo: &gix::Repository,
    nul: bool,
    deref: bool,
    create_reflog: bool,
    batch_updates: bool,
    msg: Option<&str>,
) -> Result<ExitCode> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| anyhow!("failed to read stdin: {e}"))?;

    let mut batch = Batch::default();
    // `option no-deref` applies to the next command naming a ref, and only that one.
    let mut next_no_deref = false;
    // The transaction state machine; starts in the implicit open transaction.
    let mut state = TxnState::Open;

    // git's loop is `while (!strbuf_getwholeline(&input, stdin, line_termination))`,
    // so what it dispatches on — and interpolates into every diagnostic — is one
    // whole terminator-carrying chunk of the input. Both splitters hand that
    // chunk back verbatim; `-z` additionally pre-collects the NUL-separated
    // value slots that belong to it, which the line form takes from the chunk
    // itself.
    let terminator = if nul { '\0' } else { '\n' };
    let records: Vec<(String, Option<Vec<String>>)> = if nul {
        split_nul_records(&input).into_iter().map(|(raw, f)| (raw, Some(f))).collect()
    } else {
        split_line_records(&input).into_iter().map(|raw| (raw, None)).collect()
    };

    for (raw, staged) in records {
        // `builtin/update-ref.c:715-718`, both checked on the raw line and both
        // ahead of the command table:
        //
        //     if (*input.buf == line_termination)
        //             die("empty command in input");
        //     else if (isspace(*input.buf))
        //             die("whitespace before command: %s", input.buf);
        if raw.starts_with(terminator) {
            return fatal(anyhow!("empty command in input"));
        }
        if raw.starts_with(is_c_space) {
            return fatal(anyhow!("whitespace before command: {}", c_string(&raw)));
        }

        // git recognises the command before consulting the state machine, so an
        // unknown command is reported even from a closed/prepared transaction.
        // The whole line is the `%s`, terminator and all.
        let Some(cmd) = match_command(&raw, terminator) else {
            return fatal(anyhow!("unknown command: {}", c_string(&raw)));
        };
        let cat = categorize(cmd).expect("the command table and the classifier list the same names");

        // `parse_cmd_*` reads its arguments off the same line. Splitting it here
        // rather than up front keeps a malformed *later* line from pre-empting
        // the output of the commands ahead of it, which is what git's
        // line-at-a-time loop gives.
        let fields = match staged {
            Some(fields) => fields,
            None => tokenize(raw.strip_suffix(terminator).unwrap_or(&raw))?,
        };
        let args = &fields[1..];

        // State guard, matching git's per-state restrictions. Each violation is a
        // fatal error exiting 128.
        match state {
            TxnState::Started if cat == Cat::Start => {
                eprintln!("fatal: cannot restart ongoing transaction");
                return Ok(ExitCode::from(128));
            }
            TxnState::Prepared if !matches!(cat, Cat::Commit | Cat::Abort) => {
                eprintln!("fatal: prepared transactions can only be closed");
                return Ok(ExitCode::from(128));
            }
            TxnState::Closed if cat != Cat::Start => {
                eprintln!("fatal: transaction is closed");
                return Ok(ExitCode::from(128));
            }
            _ => {}
        }

        let edit_deref = deref && !next_no_deref;
        let mut consumed_option = false;

        match cmd {
            "start" => {
                println!("start: ok");
                state = TxnState::Started;
            }
            "prepare" => {
                if let Err(e) = validate_prepare(repo, &batch) {
                    eprintln!("fatal: prepare: {e:#}");
                    return Ok(ExitCode::from(128));
                }
                println!("prepare: ok");
                state = TxnState::Prepared;
            }
            "commit" => {
                if let Err(e) = apply(repo, std::mem::take(&mut batch), batch_updates) {
                    eprintln!("fatal: commit: {e:#}");
                    return Ok(ExitCode::from(128));
                }
                println!("commit: ok");
                state = TxnState::Closed;
            }
            "abort" => {
                batch = Batch::default();
                println!("abort: ok");
                state = TxnState::Closed;
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
            _ => unreachable!("categorize accepts exactly this command set"),
        }

        if !consumed_option {
            next_no_deref = false;
        }
    }

    // End of input: the implicit transaction auto-commits; an explicit `start`
    // (or a `prepare` left uncommitted) auto-aborts; a closed one is already done.
    if state == TxnState::Open {
        if let Err(e) = apply(repo, batch, batch_updates) {
            return fatal(e);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Commit one accumulated batch: absence checks first, then the ref edits.
///
/// Without `--batch-updates` the edits go through one all-or-nothing gitoxide
/// transaction. With it, each edit is applied on its own so that a rejection
/// leaves the rest of the batch in place, which is the whole point of the flag.
fn apply(repo: &gix::Repository, batch: Batch, batch_updates: bool) -> Result<()> {
    for name in &batch.absent {
        refname(name)?; // reject malformed names the same way an edit would
        if repo.try_find_reference(name.as_str())?.is_some() {
            crate::git_fatal!("cannot lock ref '{name}': reference already exists");
        }
    }
    if batch.is_empty() {
        return Ok(());
    }
    if !batch_updates {
        if let Err(e) = repo.edit_references(batch.edits) {
            crate::git_fatal!("{}", lock_error(&e));
        }
        return Ok(());
    }
    let zero = ObjectId::null(repo.object_hash());
    for edit in batch.edits {
        let name = edit.name.to_string();
        let (new, old) = edit_oids(&edit, &zero);
        if let Err(e) = repo.edit_reference(edit) {
            let msg = lock_error(&e);
            eprintln!("error: {msg}");
            println!("rejected {name} {new} {old} {msg}");
        }
    }
    Ok(())
}

/// The `<new-oid>`/`<old-oid>` pair a `rejected` line reports for one edit.
fn edit_oids(edit: &RefEdit, zero: &ObjectId) -> (String, String) {
    let (new, expected) = match &edit.change {
        Change::Update { new, expected, .. } => (target_oid(new, zero), expected),
        Change::Delete { expected, .. } => (zero.to_string(), expected),
    };
    let old = match expected {
        PreviousValue::MustExistAndMatch(t) | PreviousValue::ExistingMustMatch(t) => {
            target_oid(t, zero)
        }
        _ => zero.to_string(),
    };
    (new, old)
}

/// Render a ref target as the oid a `rejected` line wants, zero for a symref.
fn target_oid(target: &Target, zero: &ObjectId) -> String {
    match target {
        Target::Object(id) => id.to_string(),
        Target::Symbolic(_) => zero.to_string(),
    }
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
                crate::git_fatal!("update: wrong number of arguments");
            }
            let new = parse_slot(repo, slot(1), nul, Slot::New)?;
            let old = parse_slot(repo, slot(2), nul, Slot::Old)?;
            if matches!(new, Val::Missing) {
                crate::git_fatal!("update {name}: missing <new-oid>");
            }
            check_new_object(repo, name, &new)?;
            batch
                .edits
                .push(build_edit(name, &new, &old, deref, create_reflog, msg)?);
        }
        "create" => {
            if args.len() != 2 {
                crate::git_fatal!("create: wrong number of arguments");
            }
            let new = parse_slot(repo, slot(1), nul, Slot::New)?;
            let Val::Oid(id) = new else {
                crate::git_fatal!("create {name}: zero <new-oid>");
            };
            check_new_object(repo, name, &new)?;
            // git's `create` refuses outright when the ref is already there;
            // gitoxide's `MustNotExist` tolerates an existing ref that already
            // holds the value being written, so the check is made explicit.
            batch.absent.push(name.to_string());
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
                crate::git_fatal!("delete: wrong number of arguments");
            }
            let old = parse_slot(repo, slot(1), nul, Slot::Old)?;
            // Unlike the command-line `-d`, the stdin `delete` command rejects an
            // explicit all-zero `<old-oid>` outright rather than deleting.
            if matches!(old, Val::Zero) {
                crate::git_fatal!("delete {name}: zero <old-oid>");
            }
            batch.edits.push(RefEdit {
                change: Change::Delete {
                    expected: expected_for_delete(&old),
                    log: RefLog::AndReference,
                    message: delete_message(msg),
                },
                name: refname(name)?,
                deref,
            });
        }
        "verify" => {
            if args.len() > 2 {
                crate::git_fatal!("verify: wrong number of arguments");
            }
            // `verify` is an update to the value it already has: gitoxide skips
            // the reflog when old == new, so nothing is logged, as in git.
            match parse_slot(repo, slot(1), nul, Slot::Old)? {
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
                Some("oid") => match parse_slot(repo, slot(3), nul, Slot::Old)? {
                    Val::Oid(id) => PreviousValue::MustExistAndMatch(Target::Object(id)),
                    Val::Zero | Val::Missing => PreviousValue::MustNotExist,
                },
                Some(kind) => crate::git_fatal!("symref-update {name}: invalid old value kind '{kind}'"),
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
                    message: delete_message(msg),
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
///
/// Each record is returned with the head as `strbuf_getwholeline(&input, stdin,
/// '\0')` produced it — its NUL terminator restored — beside the fields, so the
/// caller's diagnostics can interpolate the same `input.buf` git's do. An
/// unrecognised head is not rejected here: git meets it in the dispatch loop,
/// after the commands ahead of it have already run, so it is passed through with
/// no value slots for the loop to refuse.
fn split_nul_records(input: &str) -> Vec<(String, Vec<String>)> {
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
            _ => 0,
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
        records.push((format!("{head}\0"), record));
    }
    records
}

/// Split newline-terminated `--stdin` input into the lines
/// `strbuf_getwholeline(&input, stdin, '\n')` would hand back one at a time.
///
/// Terminators are kept and empty lines are kept: both are things git's dispatch
/// loop looks at. `str::lines()` did neither, which turned `\n` on its own into
/// a skipped record where git says `empty command in input`, dropped the `\r` of
/// a CRLF line git keeps, and cost every diagnostic the trailing newline git
/// interpolates. A final line with no terminator is handed back as it arrived,
/// which is what makes it an unknown command in [`match_command`].
fn split_line_records(input: &str) -> Vec<String> {
    input.split_inclusive('\n').map(str::to_string).collect()
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
                crate::git_fatal!("unexpected character after quoted field in: {line}");
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
            crate::git_fatal!("unterminated quoted string");
        };
        i += 1;
        match c {
            b'"' => break,
            b'\\' => {
                let Some(&e) = b.get(i) else {
                    crate::git_fatal!("unterminated escape in quoted string");
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
                            crate::git_fatal!("octal escape out of range in quoted string");
                        }
                        out.push(v as u8);
                    }
                    _ => crate::git_fatal!("invalid escape '\\{}' in quoted string", e as char),
                }
            }
            _ => out.push(c),
        }
    }
    let s = String::from_utf8(out).map_err(|_| anyhow!("quoted string is not valid UTF-8"))?;
    Ok((s, i))
}
