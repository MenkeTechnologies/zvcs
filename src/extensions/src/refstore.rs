//! The reference-store operations the porcelain needs beyond gix's reference
//! API, answered by whichever backend the repository uses: the `files` backend
//! (refs/files-backend.c, v2.56.0) or the `reftable` backend
//! (refs/reftable-backend.c). Code outside this module never touches
//! `$GIT_DIR/logs`, a root ref's file or a reftable stack itself.
//!
//! In a `files` repository every helper reads and writes exactly the files the
//! porcelain read and wrote before it existed; in a `reftable` repository the
//! same call goes through the backend, as git's `refs_*()` functions do.
//!
//! | helper                    | git                                                   |
//! |---------------------------|-------------------------------------------------------|
//! | [`ref_storage_format`]    | `the_repository->ref_storage_format`                  |
//! | [`reflog_names`]          | `refs_for_each_reflog()`, `add_reflogs_to_pending()`  |
//! | [`reflog_exists`]         | `refs_reflog_exists()`                                |
//! | [`for_each_reflog_entry`] | `refs_for_each_reflog_ent[_reverse]()`                |
//! | [`state_ref_read`]        | `refs_read_raw_ref()` of a root ref                   |
//! | [`state_ref_write`]       | `refs_update_ref(…, REF_NO_DEREF, …)` of a root ref   |
//! | [`state_ref_delete`]      | `refs_delete_ref(…, REF_NO_DEREF)` of a root ref      |

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use anyhow::Result;
use gix::bstr::{BStr, BString, ByteSlice};
use gix::refs::reftable::{parse_worktree_ref, WorktreeType};
use gix::refs::store::RefStorage;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};
use gix::ObjectId;

/// One reflog entry as `each_reflog_ent_fn` (refs.h) receives it: the old and
/// new value, `Name <email>`, the time, the zone as the signed decimal `HHMM`
/// and the message with its trailing newline.
pub use gix::refs::reftable::ReflogEntry;

/// The format the repository's references are stored in,
/// `the_repository->ref_storage_format`: [`RefStorage::Reftable`] for a
/// repository at format version 1 declaring `extensions.refStorage = reftable`,
/// [`RefStorage::Files`] otherwise.
pub fn ref_storage_format(repo: &gix::Repository) -> RefStorage {
    repo.ref_storage()
}

/// Whether the repository stores its references in reftables.
pub fn is_reftable(repo: &gix::Repository) -> bool {
    ref_storage_format(repo) == RefStorage::Reftable
}

/// `is_pseudo_ref()` (refs.c:887-900): the two root refs that are files in the
/// git directory whatever the ref storage format (refs.c:2099-2101).
pub fn is_pseudo_ref(name: &str) -> bool {
    name == "FETCH_HEAD" || name == "MERGE_HEAD"
}

// ---------------------------------------------------------------------------
// reflogs
// ---------------------------------------------------------------------------

/// The names of every reference that has a reflog, `refs_for_each_reflog()`
/// on the main ref store: in a linked worktree its own per-worktree reflogs
/// merged with the shared ones (`ref_iterator_select()`, refs/iterator.c:97-130).
///
/// With `all_worktrees`, followed by the names of every other worktree's ref
/// store, each per-worktree name prefixed with `main-worktree/` or
/// `worktrees/<id>/` (`strbuf_worktree_ref()`, worktree.c:587-600), which is
/// what `add_reflogs_to_pending()` (revision.c:1716-1747) walks for `--reflog`
/// and every reachability walk; a shared reflog is then listed once per
/// worktree, as git visits it.
///
/// - files: the regular files under `<git dir>/logs` whose basename is a valid
///   refname, each directory in `strcmp()` order with sub-directories descended
///   in place (`files_reflog_iterator_begin()`, refs/files-backend.c:2411-2493).
/// - reftable: every name with a log record that is not a deletion
///   (`reftable_be_reflog_iterator_begin()`, refs/reftable-backend.c:2151-2165).
pub fn reflog_names(repo: &gix::Repository, all_worktrees: bool) -> Result<Vec<BString>> {
    let mut names = store_reflog_names(repo)?;
    if !all_worktrees {
        return Ok(names);
    }
    for wt in other_worktrees(repo)? {
        let Some(store) = wt.open(repo) else { continue };
        for name in store_reflog_names(&store)? {
            names.push(worktree_ref(&wt.prefix, name.as_ref()));
        }
    }
    Ok(names)
}

/// `refs_for_each_reflog()` on the ref store of `repo`.
fn store_reflog_names(repo: &gix::Repository) -> Result<Vec<BString>> {
    if is_reftable(repo) {
        return Ok(repo
            .reftable_reflog_names()?
            .into_iter()
            .map(|name| name.as_bstr().to_owned())
            .collect());
    }
    let git_dir = repo.git_dir();
    let common_dir = repo.common_dir();
    if git_dir == common_dir {
        return Ok(sorted_reflog_files(&common_dir.join("logs")));
    }
    let worktree = sorted_reflog_files(&git_dir.join("logs"));
    let common = sorted_reflog_files(&common_dir.join("logs"));
    Ok(select_worktree_and_common(worktree, common))
}

/// `files_reflog_iterator_advance()` over `dir_iterator_begin(logs,
/// DIR_ITERATOR_SORTED)`: the path below `logs` of every regular file whose
/// basename passes `check_refname_format(…, REFNAME_ALLOW_ONELEVEL)`. Entries
/// are not followed through symlinks, so a symlinked log is not listed.
fn sorted_reflog_files(logs: &Path) -> Vec<BString> {
    fn walk(dir: &Path, prefix: &[u8], out: &mut Vec<BString>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<(Vec<u8>, std::fs::FileType)> = read
            .filter_map(|e| e.ok())
            .filter_map(|e| Some((gix::path::os_str_into_bstr(&e.file_name()).ok()?.to_vec(), e.file_type().ok()?)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, kind) in entries {
            let mut path = prefix.to_vec();
            path.extend_from_slice(&name);
            if kind.is_dir() {
                path.push(b'/');
                walk(&dir.join(gix::path::from_byte_slice(&name)), &path, out);
            } else if kind.is_file() && crate::porcelain::check_ref_format::check_refname_format_onelevel(&name) {
                out.push(path.into());
            }
        }
    }
    let mut out = Vec::new();
    walk(logs, b"", &mut out);
    out
}

/// `merge_ref_iterator_begin(worktree, common, ref_iterator_select)`
/// (refs/iterator.c:97-206): both streams in name order, a worktree name
/// shadowing the same common one, and a common name that is per-worktree
/// dropped as the main worktree's.
fn select_worktree_and_common(worktree: Vec<BString>, common: Vec<BString>) -> Vec<BString> {
    let mut out = Vec::with_capacity(worktree.len() + common.len());
    let mut wt = worktree.into_iter().peekable();
    let mut co = common.into_iter().peekable();
    loop {
        match (wt.peek(), co.peek()) {
            (None, None) => break,
            (Some(_), None) => out.extend(wt.next()),
            (w, Some(c)) => {
                if let Some(w) = w {
                    match w.cmp(c) {
                        std::cmp::Ordering::Less => {
                            out.extend(wt.next());
                            continue;
                        }
                        std::cmp::Ordering::Equal => {
                            out.extend(wt.next());
                            co.next();
                            continue;
                        }
                        std::cmp::Ordering::Greater => {}
                    }
                }
                let c = co.next().expect("peeked");
                if parse_worktree_ref(c.as_ref()).0 == WorktreeType::Shared {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// `refs_reflog_exists()`: whether `name` has a reflog.
///
/// - files: `<logs>/<name>` is a regular file, the path chosen by
///   `files_reflog_path()` (refs/files-backend.c:239-264).
/// - reftable: it has a log record that is not a deletion
///   (`reftable_be_reflog_exists()`, refs/reftable-backend.c:2306-2366); a
///   stack that cannot be read has none.
pub fn reflog_exists(repo: &gix::Repository, name: &str) -> bool {
    if is_reftable(repo) {
        return full_name(name).is_some_and(|name| repo.reftable_reflog_exists(name.as_ref()).unwrap_or(false));
    }
    files_reflog_path(repo, name.as_bytes().as_bstr()).is_file()
}

/// `refs_for_each_reflog_ent()`, or `refs_for_each_reflog_ent_reverse()` with
/// `reverse`: call `each` for the entries of the reflog of `name`, oldest first
/// or newest first, until it breaks.
///
/// Returns git's "success": `Ok(false)` where git returns -1 for a reflog that
/// is not there, `Ok(true)` once the entries were walked.
///
/// - files: the lines of `<logs>/<name>` that `show_one_reflog_ent()`
///   (refs/files-backend.c:2224-2254) accepts, others skipped silently; a
///   missing file is `Ok(false)`. A line without a message yields `"\n"`.
/// - reftable: the stored records without the existence marker
///   (`yield_log_record()`, refs/reftable-backend.c:2167-2191), the stack not
///   reloaded first, as in git. A reference without a reflog walks nothing and
///   is `Ok(true)`, as git does not tell the two apart for this backend.
pub fn for_each_reflog_entry(
    repo: &gix::Repository,
    name: &str,
    reverse: bool,
    mut each: impl FnMut(&ReflogEntry) -> ControlFlow<()>,
) -> Result<bool> {
    if is_reftable(repo) {
        let Some(full) = full_name(name) else {
            return Ok(true);
        };
        for entry in repo.reftable_reflog_entries(full.as_ref(), reverse)? {
            if each(&entry).is_break() {
                break;
            }
        }
        return Ok(true);
    }
    let path = files_reflog_path(repo, name.as_bytes().as_bstr());
    let Ok(file) = std::fs::File::open(&path) else {
        return Ok(false);
    };
    // A directory opens and then fails to read, which git's `fopen()` +
    // `strbuf_getwholeline()` also take as an empty log.
    let mut buf = Vec::new();
    if std::io::Read::read_to_end(&mut &file, &mut buf).is_err() {
        return Ok(true);
    }
    let kind = repo.object_hash();
    let lines = buf.split_inclusive(|&b| b == b'\n');
    let mut walk = |line: &[u8]| match parse_reflog_line(line, kind) {
        Some(entry) => each(&entry),
        None => ControlFlow::Continue(()),
    };
    if reverse {
        let lines: Vec<&[u8]> = lines.collect();
        for line in lines.into_iter().rev() {
            if walk(line).is_break() {
                break;
            }
        }
    } else {
        for line in lines {
            if walk(line).is_break() {
                break;
            }
        }
    }
    Ok(true)
}

/// `show_one_reflog_ent()` (refs/files-backend.c:2224-2254): the entry of one
/// line of a reflog file, LF included, or `None` for a line git skips as
/// corrupt (`old SP new SP name <email> SP time SP tz [TAB msg] LF`).
pub fn parse_reflog_line(line: &[u8], kind: gix::hash::Kind) -> Option<ReflogEntry> {
    if line.last() != Some(&b'\n') {
        return None;
    }
    let hexsz = kind.len_in_hex();
    let oid_at = |p: &[u8]| -> Option<ObjectId> {
        let hex = p.get(..hexsz)?;
        if !hex.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        ObjectId::from_hex(hex).ok()
    };
    let old_oid = oid_at(line)?;
    let p = line.get(hexsz..)?.strip_prefix(b" ")?;
    let new_oid = oid_at(p)?;
    let p = p.get(hexsz..)?.strip_prefix(b" ")?;
    // `strchr()` sees the line up to its first NUL.
    let visible = p.split(|&b| b == 0).next().unwrap_or_default();
    let email_end = visible.find_byte(b'>')?;
    if p.get(email_end + 1) != Some(&b' ') {
        return None;
    }
    let (timestamp, rest) = strtoumax(&p[email_end + 2..]);
    if timestamp == 0 {
        return None;
    }
    // ` +HHMM`, the four digits checked one by one.
    if rest.first() != Some(&b' ')
        || !matches!(rest.get(1), Some(b'+' | b'-'))
        || !rest.get(2..6).is_some_and(|d| d.iter().all(u8::is_ascii_digit))
    {
        return None;
    }
    let tz = strtol(&rest[1..]) as i32;
    let message = if rest.get(6) == Some(&b'\t') { &rest[7..] } else { &rest[6..] };
    Some(ReflogEntry {
        old_oid,
        new_oid,
        committer: p[..=email_end].into(),
        timestamp,
        tz,
        message: message.split(|&b| b == 0).next().unwrap_or_default().into(),
    })
}

/// `strtoumax(s, &end, 10)`: leading white space, an optional sign, then the
/// digits; the value saturates on overflow and a `-` negates it as unsigned.
/// Without digits the value is 0 and nothing is consumed.
fn strtoumax(s: &[u8]) -> (u64, &[u8]) {
    // The C library's `isspace()`: space and \t \n \v \f \r.
    let start = s.iter().position(|c| !matches!(c, b' ' | b'\t'..=b'\r')).unwrap_or(s.len());
    let (negative, digits_at) = match s.get(start) {
        Some(b'-') => (true, start + 1),
        Some(b'+') => (false, start + 1),
        _ => (false, start),
    };
    let ndigits = s[digits_at..].iter().take_while(|c| c.is_ascii_digit()).count();
    if ndigits == 0 {
        return (0, s);
    }
    let value = s[digits_at..digits_at + ndigits]
        .iter()
        .try_fold(0u64, |acc, &c| acc.checked_mul(10)?.checked_add(u64::from(c - b'0')));
    let value = match value {
        Some(v) if negative => v.wrapping_neg(),
        Some(v) => v,
        None => u64::MAX,
    };
    (value, &s[digits_at + ndigits..])
}

/// `strtol(s, NULL, 10)` of a string starting with its sign.
fn strtol(s: &[u8]) -> i64 {
    let (negative, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let value = digits
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .try_fold(0i64, |acc, &c| acc.checked_mul(10)?.checked_add(i64::from(c - b'0')))
        .unwrap_or(i64::MAX);
    if negative {
        -value
    } else {
        value
    }
}

/// `files_reflog_path()` (refs/files-backend.c:239-264): where the files
/// backend keeps the reflog of `name`.
fn files_reflog_path(repo: &gix::Repository, name: &BStr) -> PathBuf {
    let (kind, worktree, bare) = parse_worktree_ref(name);
    let path = gix::path::from_bstr(bare);
    match kind {
        WorktreeType::Current => repo.git_dir().join("logs").join(gix::path::from_bstr(name)),
        WorktreeType::Shared | WorktreeType::Main => repo.common_dir().join("logs").join(path),
        WorktreeType::Other => repo
            .common_dir()
            .join("worktrees")
            .join(gix::path::from_bstr(worktree.expect("set for Other")))
            .join("logs")
            .join(path),
    }
}

// ---------------------------------------------------------------------------
// root refs
// ---------------------------------------------------------------------------

/// The value of a root ref: an object, or another reference it points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateRef {
    /// `<oid>`
    Object(ObjectId),
    /// `ref: <target>`
    Symbolic(BString),
}

/// `refs_read_raw_ref()` (refs.c:2094-2105) of the root ref `name` (`HEAD`,
/// `ORIG_HEAD`, `AUTO_MERGE`, `CHERRY_PICK_HEAD`, `NOTES_MERGE_REF`, …,
/// optionally as `main-worktree/<name>` or `worktrees/<id>/<name>`): its value
/// exactly as stored, a symbolic one unresolved. `None` if it does not exist
/// or, as git's `-1` does not distinguish the two either, cannot be parsed.
///
/// - files, and `FETCH_HEAD`/`MERGE_HEAD` in any format: the file
///   `files_ref_path()` names (refs/files-backend.c:266-290), parsed by
///   `parse_loose_ref_contents()` (:668-697): `ref:` and white space then the
///   target, or a full object id followed by the end or white space.
/// - reftable: the record in the stack of its worktree
///   (`reftable_be_read_raw_ref()`, refs/reftable-backend.c:880-908).
pub fn state_ref_read(repo: &gix::Repository, name: &str) -> Result<Option<StateRef>> {
    if is_pseudo_ref(name) {
        // `refs_read_special_head()` (refs.c:2070-2092): the file as it is,
        // without trimming, and any failure to read it is a missing ref.
        return Ok(std::fs::read(repo.git_dir().join(name))
            .ok()
            .and_then(|contents| parse_loose_ref_contents(&contents, repo.object_hash())));
    }
    if is_reftable(repo) {
        let Some(full) = full_name(name) else {
            return Ok(None);
        };
        return Ok(repo.reftable_read_raw_ref(full.as_ref())?.map(|target| match target {
            Target::Object(id) => StateRef::Object(id),
            Target::Symbolic(target) => StateRef::Symbolic(target.as_bstr().to_owned()),
        }));
    }
    // `read_ref_internal()` (refs/files-backend.c:520-646) for a name that
    // packed-refs never holds.
    let path = files_ref_path(repo, name.as_bytes().as_bstr());
    let missing = |err: &std::io::Error| {
        err.kind() == std::io::ErrorKind::NotFound || err.raw_os_error() == Some(libc::ENOTDIR)
    };
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(err) if missing(&err) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if meta.file_type().is_symlink() {
        // A symlink to a well-formed name below `refs/` is a symbolic ref;
        // any other is read through.
        if let Ok(target) = std::fs::read_link(&path) {
            let target = gix::path::into_bstr(target).into_owned();
            if target.starts_with(b"refs/") && crate::porcelain::check_ref_format::check_refname_format(&target, 0) {
                return Ok(Some(StateRef::Symbolic(target)));
            }
        }
    }
    let contents = match std::fs::read(&path) {
        Ok(contents) => contents,
        Err(err) if missing(&err) || err.kind() == std::io::ErrorKind::IsADirectory => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    // `strbuf_rtrim()` before parsing.
    let end = contents.iter().rposition(|&c| !is_git_space(c)).map_or(0, |i| i + 1);
    Ok(parse_loose_ref_contents(&contents[..end], repo.object_hash()))
}

/// Whether the root ref `name` exists: [`state_ref_read`] finds a value, or
/// in the files backend, as the porcelain has always checked, its file exists.
pub fn state_ref_exists(repo: &gix::Repository, name: &str) -> bool {
    if is_reftable(repo) && !is_pseudo_ref(name) {
        return state_ref_read(repo, name).ok().flatten().is_some();
    }
    files_ref_path(repo, name.as_bytes().as_bstr()).exists()
}

/// Whether git's files backend logs a write of the root ref `name`.
///
/// Every root ref but `HEAD`, `FETCH_HEAD` and `MERGE_HEAD` (see [`is_pseudo_ref`]) is set through a
/// reference transaction, so `-c core.logAllRefUpdates=always` leaves a `logs/<name>` behind it
/// (`logs/AUTO_MERGE`, `logs/REBASE_HEAD`, …) and any reflog that already exists is appended to.
/// Without either, nothing is logged and the plain file write is all there is to do, so that is
/// still what happens.
fn is_logged_root_ref(repo: &gix::Repository, name: &str) -> bool {
    if name == "HEAD" || is_pseudo_ref(name) {
        return false;
    }
    let always = repo
        .config_snapshot()
        .string("core.logAllRefUpdates")
        .is_some_and(|v| v.eq_ignore_ascii_case(b"always"));
    always || repo.git_dir().join("logs").join(name).exists()
}

/// `refs_delete_ref()` takes the reflog with the ref: a root ref that was logged (see
/// [`is_logged_root_ref`]) leaves no `logs/<name>` behind once it is gone.
pub fn remove_root_ref_log(repo: &gix::Repository, name: &str) {
    if name != "HEAD" && !is_pseudo_ref(name) {
        let _ = std::fs::remove_file(repo.git_dir().join("logs").join(name));
    }
}

/// Set the root ref `name` to `value` without dereferencing it, as
/// `refs_update_ref(…, msg, name, oid, NULL, REF_NO_DEREF, …)` does for the
/// sequencer's, merge's, bisect's and notes' state refs. `msg` is the reflog
/// message, `""` for git's `NULL`.
///
/// - files, and `FETCH_HEAD`/`MERGE_HEAD` in any format: the file is written
///   with `<hex>\n` or `ref: <target>\n` and nothing else, no reflog touched,
///   as the porcelain always wrote these files.
/// - reftable: a transaction on the stack of its worktree
///   (`reftable_be_transaction_prepare()`/`_finish()`), logged when
///   `core.logAllRefUpdates` or an existing reflog says so.
pub fn state_ref_write(repo: &gix::Repository, name: &str, value: &StateRef, msg: &str) -> Result<()> {
    if (is_reftable(repo) && !is_pseudo_ref(name)) || (!is_reftable(repo) && is_logged_root_ref(repo, name)) {
        let full = FullName::try_from(name)?;
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: msg.into(),
                },
                expected: PreviousValue::Any,
                new: match value {
                    StateRef::Object(id) => Target::Object(*id),
                    StateRef::Symbolic(target) => Target::Symbolic(FullName::try_from(target.as_bstr())?),
                },
            },
            name: full,
            deref: false,
        })?;
        return Ok(());
    }
    let contents = match value {
        StateRef::Object(id) => format!("{id}\n"),
        StateRef::Symbolic(target) => format!("ref: {target}\n"),
    };
    std::fs::write(files_ref_path(repo, name.as_bytes().as_bstr()), contents)?;
    Ok(())
}

/// Delete the root ref `name` without dereferencing it,
/// `refs_delete_ref(…, msg, name, NULL, REF_NO_DEREF)`. Deleting one that does
/// not exist succeeds.
///
/// - files, and `FETCH_HEAD`/`MERGE_HEAD` in any format: the file is removed.
/// - reftable: a deletion record in the stack of its worktree, written only if
///   the reference exists.
pub fn state_ref_delete(repo: &gix::Repository, name: &str, msg: &str) -> Result<()> {
    if is_reftable(repo) && !is_pseudo_ref(name) {
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
                message: msg.into(),
            },
            name: FullName::try_from(name)?,
            deref: false,
        })?;
        return Ok(());
    }
    remove_root_ref_log(repo, name);
    match std::fs::remove_file(files_ref_path(repo, name.as_bytes().as_bstr())) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}

/// `files_ref_path()` (refs/files-backend.c:266-290): where the files backend
/// keeps the loose reference `name`.
fn files_ref_path(repo: &gix::Repository, name: &BStr) -> PathBuf {
    let (kind, worktree, bare) = parse_worktree_ref(name);
    match kind {
        WorktreeType::Current => repo.git_dir().join(gix::path::from_bstr(name)),
        WorktreeType::Shared | WorktreeType::Main => repo.common_dir().join(gix::path::from_bstr(bare)),
        WorktreeType::Other => repo
            .common_dir()
            .join("worktrees")
            .join(gix::path::from_bstr(worktree.expect("set for Other")))
            .join(gix::path::from_bstr(bare)),
    }
}

/// `parse_loose_ref_contents()` (refs/files-backend.c:668-697), with git's
/// broken reference (`REF_ISBROKEN`) as `None`.
fn parse_loose_ref_contents(buf: &[u8], kind: gix::hash::Kind) -> Option<StateRef> {
    // The C string ends at the first NUL.
    let buf = buf.split(|&b| b == 0).next().unwrap_or_default();
    if let Some(rest) = buf.strip_prefix(b"ref:") {
        let start = rest.iter().position(|&c| !is_git_space(c)).unwrap_or(rest.len());
        return Some(StateRef::Symbolic(rest[start..].into()));
    }
    let hexsz = kind.len_in_hex();
    let hex = buf.get(..hexsz)?;
    if !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    // FETCH_HEAD has more after the object id.
    match buf.get(hexsz) {
        Some(&c) if !is_git_space(c) => return None,
        _ => {}
    }
    ObjectId::from_hex(hex).ok().map(StateRef::Object)
}

/// git's `isspace()` (git-compat-util.h `sane_ctype`): space, tab, LF and CR.
fn is_git_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// `name` as gix's full reference name, `None` for one gix cannot represent
/// (a lower-case name outside `refs/`), which therefore has no record.
fn full_name(name: &str) -> Option<FullName> {
    FullName::try_from(name).ok()
}

// ---------------------------------------------------------------------------
// worktrees
// ---------------------------------------------------------------------------

/// A worktree other than the current one, as `get_worktrees()` lists them.
struct OtherWorktree {
    /// `get_worktree_git_dir()` (worktree.c:437-445).
    git_dir: PathBuf,
    /// What `strbuf_worktree_ref()` puts before its per-worktree refnames.
    prefix: String,
}

impl OtherWorktree {
    /// `get_worktree_ref_store()`: the worktree opened as a repository, `None`
    /// if that fails.
    fn open(&self, repo: &gix::Repository) -> Option<gix::Repository> {
        gix::open_opts(&self.git_dir, repo.open_options().clone()).ok()
    }
}

/// `get_worktrees()` (worktree.c:186-221) without the current worktree: the
/// main one, then each `worktrees/<id>` with a non-empty `gitdir` file in
/// `readdir()` order (`get_linked_worktree()`, :141-176).
fn other_worktrees(repo: &gix::Repository) -> Result<Vec<OtherWorktree>> {
    let canonical = |p: &Path| gix::path::realpath(p).unwrap_or_else(|_| p.to_owned());
    let current = canonical(repo.git_dir());
    let common = repo.common_dir();
    let mut out = Vec::new();
    if canonical(common) != current {
        out.push(OtherWorktree {
            git_dir: common.to_owned(),
            prefix: "main-worktree/".into(),
        });
    }
    let read = match std::fs::read_dir(common.join("worktrees")) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(err) => return Err(err.into()),
    };
    for entry in read {
        let entry = entry?;
        let git_dir = entry.path();
        if std::fs::read(git_dir.join("gitdir")).map_or(true, |c| c.is_empty()) {
            continue;
        }
        if canonical(&git_dir) == current {
            continue;
        }
        out.push(OtherWorktree {
            prefix: format!("worktrees/{}/", entry.file_name().to_string_lossy()),
            git_dir,
        });
    }
    Ok(out)
}

/// `strbuf_worktree_ref()` (worktree.c:587-600) for another worktree: a
/// per-worktree `name` gets that worktree's `prefix`, a shared one stays.
fn worktree_ref(prefix: &str, name: &BStr) -> BString {
    if parse_worktree_ref(name).0 != WorktreeType::Current {
        return name.to_owned();
    }
    let mut out = BString::from(prefix);
    out.extend_from_slice(name);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines of a reflog file stock 2.56.0's `log -g` shows and skips:
    /// of a zero timestamp, a zone with a non-digit, a line without its LF, a
    /// line without a message and one with, only the last two are shown.
    #[test]
    fn reflog_lines_git_skips_as_corrupt() {
        let z = "0".repeat(40);
        let h = "1".repeat(40);
        let parse = |line: String| parse_reflog_line(line.as_bytes(), gix::hash::Kind::Sha1);
        assert_eq!(parse(format!("{z} {h} A <a@x> 0 +0000\tzero-ts\n")), None);
        assert_eq!(parse(format!("{z} {h} A <a@x> 5 +00x0\tbad-tz\n")), None);
        assert_eq!(parse(format!("{z} {h} A <a@x> 11 +0000\tno-lf")), None);

        let entry = parse(format!("{z} {h} A <a@x> 7 +0100\n")).expect("shown");
        assert_eq!(entry.message, "\n", "no message is the LF after the zone");
        assert_eq!((entry.timestamp, entry.tz), (7, 100));
        assert_eq!(entry.committer, "A <a@x>");

        let entry = parse(format!("{z} {h} A <a@x> 9 -0130\twith msg\n")).expect("shown");
        assert_eq!(entry.message, "with msg\n");
        assert_eq!(entry.tz, -130);
        assert!(entry.old_oid.is_null());
    }

    #[test]
    fn loose_root_ref_contents() {
        let h = "1".repeat(40);
        let id = ObjectId::from_hex(h.as_bytes()).unwrap();
        let parse = |s: &str| parse_loose_ref_contents(s.as_bytes(), gix::hash::Kind::Sha1);
        assert_eq!(parse(&h), Some(StateRef::Object(id)));
        assert_eq!(parse(&format!("{h}\t\tbranch 'x'")), Some(StateRef::Object(id)));
        assert_eq!(parse(&format!("{h}x")), None);
        assert_eq!(parse(&h[..39]), None);
        assert_eq!(parse("ref: \t refs/heads/x"), Some(StateRef::Symbolic("refs/heads/x".into())));
    }
}
