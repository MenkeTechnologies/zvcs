//! `repo_migrate_ref_storage_format()` (refs.c:3344-3542, v2.56.0): move a
//! repository's references and reflogs from the `files` backend into
//! `reftable`, or back.
//!
//! The steps are git's, in git's order:
//!
//!  1. `mkdtemp("<gitdir>/ref_migration.XXXXXX")` and create the new store in
//!     it (`ref_store_create_on_disk()`): `reftable/` plus the `HEAD` and
//!     `refs/heads` stubs that keep older clients from mistaking the repository
//!     for a files one, or `refs/`, `refs/heads/` and `refs/tags/`;
//!  2. one initial transaction holding every reference of the old store (root
//!     refs, symrefs and broken refs included) and, unless `--no-reflog`, every
//!     reflog entry, each under its own index so a reflog keeps its order;
//!  3. `--dry-run` stops here and names the directory;
//!  4. otherwise the old store is deleted (`ref_store_remove_on_disk()`), the new
//!     store's files are renamed into the git directory, and the repository
//!     format is rewritten (`initialize_repository_version()`).
//!
//! A reftable table is written by `gix-reftable`, the port of git's `reftable/`
//! library, exactly as `write_transaction_table()` (refs/reftable-backend.c:
//! 1463-1636) feeds it. A files store gets what `files_transaction_finish_initial()`
//! (refs/files-backend.c:3194-3320) writes: every reference that is neither
//! symbolic nor a root ref in `packed-refs`, the others as loose files, and the
//! reflog entries appended to `logs/`.

use anyhow::Result;
use gix::bstr::{BString, ByteSlice};
use gix::refs::store::RefStorage;
use gix_reftable::{LogRecord, LogUpdate, LogValue, RefRecord, RefValue, Stack, StackOptions, WriteOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// How a migration ended when it did not succeed: the text git collects in
/// `errbuf`, which `cmd_refs_migrate()` prints through `error("%s", …)`.
pub(super) struct Failed(pub String);

/// One reference of the old store, as `migrate_one_ref()` hands it to the
/// transaction: `ref_transaction_create()` for an object id,
/// `ref_transaction_update()` with a `new_target` for a symref. A tag is never
/// peeled: `REF_SKIP_OID_VERIFICATION` keeps `ref_transaction_update()` from
/// reading the object (refs.c:1433-1455).
struct Ref {
    name: BString,
    value: gix::refs::Target,
}

/// One reflog entry, as `migrate_one_reflog_entry()` and
/// `rename_one_reflog_entry()` (builtin/remote.c:630-670) hand it to
/// `ref_transaction_update_reflog()`.
pub(super) struct Log {
    pub refname: BString,
    pub old: gix::ObjectId,
    pub new: gix::ObjectId,
    name: BString,
    email: BString,
    time: u64,
    tz_offset: i16,
    pub message: String,
    pub index: u64,
}

impl Log {
    /// The `committer_info` the entry is written with,
    /// `fmt_ident(name, mail, WANT_BLANK_IDENT, show_date(…, DATE_MODE(NORMAL)), 0)`
    /// as the backend reads it back: `Name <email> <seconds> <+|-HHMM>`.
    pub(super) fn committer_info(&self) -> String {
        format!("{} <{}> {} {:+05}", self.name, self.email, self.time, self.tz_offset)
    }
}

/// Migrate `repo` to the ref storage format `to`, which it does not use yet.
/// The caller has already made `cmd_refs_migrate()`'s checks and refused a
/// repository with worktrees.
pub(super) fn migrate(
    repo: &gix::Repository,
    to: RefStorage,
    dry_run: bool,
    skip_reflog: bool,
) -> Result<std::result::Result<(), Failed>> {
    // `old_refs->gitdir` is `repo->gitdir` as setup left it — `.git` from a
    // subdirectory of the work tree — and git, standing at the top, resolves it
    // from there. That spelling is what the dry-run message prints.
    let gitdir_str = super::rev_parse::repo_get_git_dir(repo);
    let cwd = crate::setup::setup_cwd(repo).unwrap_or_default();
    let gitdir = cwd.join(&gitdir_str);

    let template = gitdir_str.join("ref_migration.XXXXXX");
    let new_gitdir = match crate::tmp_objdir::git_mkdtemp(cwd.join(&template).as_os_str()) {
        Ok(created) => {
            // Report the directory by the name git would: the template with its
            // six characters filled in.
            let suffix = created.file_name().map(|n| n.to_owned()).unwrap_or_default();
            gitdir_str.join(suffix)
        }
        Err(e) => {
            return Ok(Err(Failed(format!(
                "cannot create migration directory: {}",
                crate::external::strerror(&e)
            ))))
        }
    };
    let new_abs = cwd.join(&new_gitdir);

    let migrated = (|| -> Result<std::result::Result<(), Failed>> {
        create_on_disk(repo, to, &new_abs)?;
        let refs = collect_refs(repo, &gitdir)?;
        let logs = match skip_reflog {
            true => Vec::new(),
            false => collect_logs(repo, &gitdir)?,
        };
        match to {
            RefStorage::Reftable => {
                let opts = write_options(repo)?;
                let mut stack = Stack::new(&new_abs.join("reftable"), &stack_options(repo))
                    .map_err(|e| anyhow::anyhow!("reftable: {e}"))?;
                if let Err(e) = commit_initial(&mut stack, &opts, refs, logs) {
                    return Ok(Err(Failed(format!("reftable: transaction failure: {e}"))));
                }
            }
            RefStorage::Files => files_commit_initial(&new_abs, refs, logs)?,
        }
        Ok(Ok(()))
    })()?;
    if let Err(Failed(msg)) = migrated {
        return Ok(Err(Failed(msg)));
    }

    if dry_run {
        println!(
            "Finished dry-run migration of refs, the result can be found at '{}'",
            new_gitdir.display()
        );
        return Ok(Ok(()));
    }

    let finish = (|| -> std::result::Result<(), String> {
        match to {
            // The old store is the other one.
            RefStorage::Reftable => files_remove_on_disk(&gitdir)?,
            RefStorage::Files => gix::refs::reftable::Backend::remove_on_disk(&gitdir)?,
        }
        move_files(&new_gitdir, &new_abs, &gitdir_str, &gitdir)?;
        if let Err(e) = std::fs::remove_dir(&new_abs) {
            eprintln!(
                "warning: could not remove temporary migration directory '{}': {}",
                new_gitdir.display(),
                crate::external::strerror(&e)
            );
        }
        initialize_repository_version(repo, to).map_err(|e| format!("{e:#}"))
    })();
    match finish {
        Ok(()) => Ok(Ok(())),
        // `if (ret && did_migrate_refs) { strbuf_complete(errbuf, '\n');
        // strbuf_addf(errbuf, _("migrated refs can be found at '%s'"), …); }`
        Err(mut msg) => {
            if !msg.is_empty() && !msg.ends_with('\n') {
                msg.push('\n');
            }
            msg.push_str(&format!("migrated refs can be found at '{}'", new_gitdir.display()));
            Ok(Err(Failed(msg)))
        }
    }
}

/// `reftable_be_init()` (refs/reftable-backend.c:422-433): the stack is opened
/// with the repository's hash; everything about writing comes from
/// [`write_options`].
fn stack_options(repo: &gix::Repository) -> StackOptions {
    StackOptions {
        hash_id: match repo.object_hash() {
            gix::hash::Kind::Sha256 => gix_reftable::HashId::Sha256,
            _ => gix_reftable::HashId::Sha1,
        },
        ..StackOptions::default()
    }
}

/// `reftable_be_write_options()` (refs/reftable-backend.c:323-392, v2.56.0),
/// the reftable backend's own reading of the configuration: the `reftable.*`
/// values with git's range checks, `core.sharedRepository` and
/// `GIT_TEST_REFTABLE_AUTOCOMPACTION`. A value git dies on is a `fatal:`.
fn write_options(repo: &gix::Repository) -> Result<WriteOptions> {
    let entries = gix::config::reftable::entries_in_order(&repo.config_snapshot());
    let autocompaction = std::env::var_os("GIT_TEST_REFTABLE_AUTOCOMPACTION");
    // SAFETY: `umask()` only swaps the process mask; it is read and restored.
    let mask = unsafe {
        let mask = libc::umask(0);
        libc::umask(mask);
        u32::from(mask)
    };
    gix::config::reftable::write_config(&entries, mask, autocompaction.as_ref())
        .map(|config| config.opts)
        .map_err(crate::fatal::die)
}

/// `ref_store_create_on_disk(new_refs, 0, …)` (refs.c:2223-2241) in the
/// migration directory `dir`, with `core.sharedRepository` applied to what was
/// created (`adjust_shared_perm()`):
///
/// - reftable: `reftable_be_create_on_disk()` and the stubs, see
///   [`gix::refs::reftable::Backend::create_on_disk`];
/// - files: `files_ref_store_create_on_disk()` (refs/files-backend.c:3658-3700),
///   `refs/`, `refs/heads/` and `refs/tags/`.
fn create_on_disk(repo: &gix::Repository, to: RefStorage, dir: &Path) -> Result<()> {
    match to {
        RefStorage::Reftable => {
            gix::refs::reftable::Backend::create_on_disk(dir).map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        RefStorage::Files => {
            for sub in ["refs", "refs/heads", "refs/tags"] {
                match std::fs::create_dir(dir.join(sub)) {
                    Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
                        crate::git_fatal!("{}: {}", dir.join(sub).display(), crate::external::strerror(&e))
                    }
                    _ => {}
                }
            }
        }
    }
    let shared = repo
        .config_snapshot()
        .string("core.sharedRepository")
        .and_then(|v| super::init::parse_shared_value(&v.to_string()).ok())
        .unwrap_or(0);
    if shared != 0 {
        super::init::adjust_shared_perm_recursive(dir, shared)?;
    }
    Ok(())
}

/// The files [`create_on_disk`] lays down, without the shared-permission pass —
/// what `git init --ref-format=reftable` needs before the repository can even be
/// opened, since a git directory is only recognised by its `HEAD`. `init` widens
/// the permissions of the whole git directory afterwards.
pub(super) fn create_on_disk_stubs(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir(dir.join("reftable"))?;
    std::fs::write(dir.join("HEAD"), "ref: refs/heads/.invalid\n")?;
    std::fs::create_dir(dir.join("refs"))?;
    std::fs::write(dir.join("refs/heads"), "this repository uses the reftable format\n")?;
    Ok(())
}

/// `refs_update_symref(get_main_ref_store(repo), "HEAD", target, NULL)` on a
/// fresh reftable store, as `create_reference_database()` (setup.c:2527-2563)
/// points `HEAD` at the initial branch: one table holding the single symref
/// record at the stack's first update index. The log message is `NULL`, so no
/// reflog record is written.
pub(super) fn init_symref_head(repo: &gix::Repository, target: &gix::bstr::BStr) -> Result<()> {
    let opts = write_options(repo)?;
    let mut stack = Stack::new(&repo.common_dir().join("reftable"), &stack_options(repo))
        .map_err(|e| anyhow::anyhow!("reftable: {e}"))?;
    let head = Ref {
        name: "HEAD".into(),
        value: gix::refs::Target::Symbolic(
            gix::refs::FullName::try_from(target).map_err(|e| anyhow::anyhow!("{e}"))?,
        ),
    };
    commit_initial(&mut stack, &opts, vec![head], Vec::new())
        .map_err(|e| anyhow::anyhow!("reftable: transaction failure: {e}"))
}

/// `refs_for_each_ref_ext(old_refs, migrate_one_ref, …)` with
/// `REFS_FOR_EACH_INCLUDE_ROOT_REFS | REFS_FOR_EACH_INCLUDE_BROKEN`: the root
/// refs (`HEAD`, `ORIG_HEAD`, … — never `FETCH_HEAD` or `MERGE_HEAD`), then
/// everything under `refs/`. A files store has its root refs in the git
/// directory and takes loose over packed; a reftable store has them as records
/// (`reftable_ref_iterator_advance()`, refs/reftable-backend.c:632-646).
fn collect_refs(repo: &gix::Repository, gitdir: &Path) -> Result<Vec<Ref>> {
    let mut out = Vec::new();
    if crate::refstore::is_reftable(repo) {
        let references = repo.references()?;
        for r in references.pseudo()?.chain(references.all()?) {
            let r = r.map_err(|e| anyhow::anyhow!("{e}"))?;
            let name = r.name().as_bstr().to_owned();
            if name.starts_with(b"refs/") || super::for_each_ref::is_root_ref(&name) {
                out.push(Ref { name, value: r.inner.target });
            }
        }
        return Ok(out);
    }
    let mut roots: Vec<String> = std::fs::read_dir(gitdir)?
        .filter_map(std::result::Result::ok)
        .filter(|e| std::fs::metadata(e.path()).is_ok_and(|m| m.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.') && !n.ends_with(".lock"))
        .filter(|n| super::for_each_ref::is_root_ref(n.as_bytes()))
        .collect();
    roots.sort();
    for name in roots {
        if let Some(r) = repo.refs.try_find(name.as_str())?.filter(|r| r.name.as_bstr() == name.as_str()) {
            out.push(Ref { name: name.into(), value: r.target });
        }
    }
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow::anyhow!("{e}"))?;
        let name = r.name().as_bstr().to_owned();
        if !name.starts_with(b"refs/") {
            continue;
        }
        out.push(Ref { name, value: r.inner.target });
    }
    Ok(out)
}

/// `migrate_one_ref()` into a reftable record: a symref keeps its referent
/// (`REF_NO_DEREF`), anything else its object id. `ref_transaction_create()` is
/// never given a peeled value, so a tag is a `VAL1` record, not a `VAL2`.
fn ref_value(target: &gix::refs::Target) -> RefValue {
    match target {
        gix::refs::Target::Symbolic(to) => RefValue::Symref(to.as_bstr().to_owned()),
        gix::refs::Target::Object(id) => RefValue::Val1(hash(id)),
    }
}

/// An object id in the library's fixed-width `Hash`.
fn hash(id: &gix::oid) -> gix_reftable::record::Hash {
    let mut out = [0u8; gix_reftable::basics::HASH_SIZE_MAX];
    out[..id.as_bytes().len()].copy_from_slice(id.as_bytes());
    out
}

/// `refs_for_each_reflog(old_refs, migrate_one_reflog, …)`: every reflog of
/// the old store, each entry taking the next value of `data->index`, from 0.
///
/// A reftable store lists its reflogs and their entries itself; each entry is
/// read back through the same parser a files line takes, which is what
/// `migrate_one_reflog_entry()` makes of the entry either way.
///
/// A files store has them under `<gitdir>/logs`, walked as `dir_iterator_begin(…, DIR_ITERATOR_SORTED)`
/// walks it — each directory's entries in `strcmp()` order, a directory's
/// contents right after the directory itself — skipping what is not a regular
/// file or whose basename is not a well-formed one-level refname
/// (`files_reflog_iterator_advance()`, refs/files-backend.c:2411-2430). Each
/// entry takes the next value of `data->index`, starting at 0.
fn collect_logs(repo: &gix::Repository, gitdir: &Path) -> Result<Vec<Log>> {
    if crate::refstore::is_reftable(repo) {
        let mut out = Vec::new();
        let mut index = 0u64;
        for refname in crate::refstore::reflog_names(repo, false)? {
            out.extend(reftable_reflog(repo, &refname.to_str_lossy(), &mut index)?);
        }
        return Ok(out);
    }
    let logs_dir = gitdir.join("logs");
    let mut names = Vec::new();
    walk_sorted(&logs_dir, "", &mut names);
    let mut out = Vec::new();
    let mut index = 0u64;
    for refname in names {
        let Ok(body) = std::fs::read(logs_dir.join(&refname)) else { continue };
        for line in body.split_inclusive(|b| *b == b'\n') {
            if let Some(log) = parse_reflog_line(repo, &refname, line, index) {
                out.push(log);
                index += 1;
            }
        }
    }
    Ok(out)
}

/// `refs_for_each_reflog_ent()` over the reflog of `refname` in a reftable
/// store, each entry as `migrate_one_reflog_entry()` makes it, taking `index`
/// and the next ones. Each entry is read back through the parser a files line
/// takes, which turns the identity into what `fmt_ident()` would.
pub(super) fn reftable_reflog(repo: &gix::Repository, refname: &str, index: &mut u64) -> Result<Vec<Log>> {
    let mut out = Vec::new();
    crate::refstore::for_each_reflog_entry(repo, refname, false, |e| {
        let message = e.message.strip_suffix(b"\n").unwrap_or(&e.message);
        let mut line = format!(
            "{} {} {} {} {}{:04}",
            e.old_oid,
            e.new_oid,
            e.committer,
            e.timestamp,
            if e.tz < 0 { '-' } else { '+' },
            e.tz.abs()
        )
        .into_bytes();
        if !message.is_empty() {
            line.push(b'\t');
            line.extend_from_slice(message);
        }
        line.push(b'\n');
        if let Some(log) = parse_reflog_line(repo, refname, &line, *index) {
            out.push(log);
            *index += 1;
        }
        std::ops::ControlFlow::Continue(())
    })?;
    Ok(out)
}

/// The sorted, pre-order walk [`collect_logs`] reads reflogs in.
fn walk_sorted(dir: &Path, rel: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(Vec<u8>, PathBuf)> = entries
        .filter_map(std::result::Result::ok)
        .map(|e| (e.file_name().as_encoded_bytes().to_vec(), e.path()))
        .collect();
    entries.sort();
    for (name, path) in entries {
        let Ok(name) = String::from_utf8(name) else { continue };
        let child = match rel.is_empty() {
            true => name.clone(),
            false => format!("{rel}/{name}"),
        };
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            walk_sorted(&path, &child, out);
        } else if meta.is_file()
            && super::check_ref_format::check_refname_format(
                name.as_bytes(),
                super::check_ref_format::ALLOW_ONELEVEL,
            )
        {
            out.push(child);
        }
    }
}

/// `show_one_reflog_ent()` (refs/files-backend.c:2224-2254) followed by
/// `migrate_one_reflog_entry()` (refs.c:3225-3253). A line that does not parse
/// is skipped, as git skips it ("corrupt?").
///
/// `migrate_one_reflog_entry()` rebuilds the committer through
/// `fmt_ident(name, mail, WANT_BLANK_IDENT, show_date(…, DATE_MODE(NORMAL)), 0)`
/// and the backend splits it again (`fill_reftable_log_record()`): the name and
/// address lose their crud, an empty name becomes the account name, and the
/// date survives the round trip as the same seconds and offset.
fn parse_reflog_line(repo: &gix::Repository, refname: &str, line: &[u8], index: u64) -> Option<Log> {
    let line = line.strip_suffix(b"\n")?;
    let hexsz = repo.object_hash().len_in_hex();
    let old = gix::ObjectId::from_hex(line.get(..hexsz)?).ok()?;
    let rest = line.get(hexsz..)?.strip_prefix(b" ")?;
    let new = gix::ObjectId::from_hex(rest.get(..hexsz)?).ok()?;
    let rest = rest.get(hexsz..)?.strip_prefix(b" ")?;
    let email_end = rest.find_byte(b'>')?;
    let committer = &rest[..=email_end];
    let after = rest[email_end + 1..].strip_prefix(b" ")?;
    let digits = after.iter().take_while(|b| b.is_ascii_digit()).count();
    let time: u64 = std::str::from_utf8(&after[..digits]).ok()?.parse().ok()?;
    if time == 0 {
        return None;
    }
    let tail = &after[digits..];
    if tail.len() < 6 || tail[0] != b' ' || !(tail[1] == b'+' || tail[1] == b'-') || !tail[2..6].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let tz: i16 = std::str::from_utf8(&tail[2..6]).ok()?.parse().ok()?;
    let tz_offset = if tail[1] == b'-' { -tz } else { tz };
    let message = match tail.get(6) {
        Some(b'\t') => &tail[7..],
        _ => &tail[6..],
    };

    // `split_ident_line()` on `name <email>`.
    let lt = committer.find_byte(b'<')?;
    let name = committer[..lt].trim_end();
    let email = &committer[lt + 1..committer.len() - 1];
    let mut name = without_crud(name);
    if name.is_empty() {
        name = crate::passwd_self().0.into_bytes();
    }
    Some(Log {
        refname: refname.into(),
        old,
        new,
        name: name.into(),
        email: without_crud(email).into(),
        time,
        tz_offset,
        message: super::reflog::normalize_reflog_message(&String::from_utf8_lossy(message)),
        index,
    })
}

/// `strbuf_addstr_without_crud()` (ident.c:229-266): crud trimmed from both
/// ends, and `\n`, `<` and `>` dropped from what is left.
fn without_crud(s: &[u8]) -> Vec<u8> {
    let crud = |c: u8| c <= 32 || b",:;<>\"\\'".contains(&c);
    let start = s.iter().position(|&c| !crud(c)).unwrap_or(s.len());
    let end = s.iter().rposition(|&c| !crud(c)).map_or(start, |p| p + 1);
    s[start..end.max(start)].iter().copied().filter(|c| !b"\n<>".contains(c)).collect()
}

/// `ref_transaction_commit()` of the initial transaction: one table through
/// `write_transaction_table()` (refs/reftable-backend.c:1463-1636).
///
/// The limits run from the stack's next update index `ts` to `ts + max_index`.
/// Every reference is written at `ts` in name order; every reflog entry at
/// `ts + index`, and the writer sorts them by name and newest first.
fn commit_initial(
    stack: &mut Stack,
    opts: &WriteOptions,
    mut refs: Vec<Ref>,
    logs: Vec<Log>,
) -> gix_reftable::Result<()> {
    refs.sort_by(|a, b| a.name.cmp(&b.name));
    let max_index = logs.last().map_or(0, |l| l.index);
    let limit = (opts.block_size / 2) as usize;
    stack.add(
        |wr, st| {
            let ts = st.next_update_index();
            wr.set_limits(ts, ts + max_index)?;
            for r in &refs {
                wr.add_ref(&RefRecord {
                    refname: r.name.clone(),
                    update_index: ts,
                    value: ref_value(&r.value),
                })?;
            }
            let mut records: Vec<LogRecord> = logs
                .iter()
                .map(|l| LogRecord {
                    refname: l.refname.clone(),
                    update_index: ts + l.index,
                    value: LogValue::Update(LogUpdate {
                        new_hash: hash(&l.new),
                        old_hash: hash(&l.old),
                        name: l.name.clone(),
                        email: l.email.clone(),
                        time: l.time,
                        tz_offset: l.tz_offset,
                        // `xstrndup(u->msg, block_size / 2)`.
                        message: Some(l.message.as_bytes()[..l.message.len().min(limit)].into()),
                    }),
                })
                .collect();
            if !records.is_empty() {
                wr.add_logs(&mut records)?;
            }
            Ok(())
        },
        Some(opts),
    )
}

/// `files_transaction_finish_initial()` (refs/files-backend.c:3194-3320) into
/// the files store at `dir`: the packed transaction writes `packed-refs` — the
/// header and every reference that is neither symbolic nor a root ref, sorted,
/// none peeled — and the loose one the rest as files, then each reflog entry
/// as a line appended to `logs/<ref>` (`REF_FORCE_CREATE_REFLOG`), its
/// committer `fmt_ident()`'s `Name <email> <time> <zone>`
/// (`log_ref_write_fd()`, :1986-2006).
fn files_commit_initial(dir: &Path, mut refs: Vec<Ref>, logs: Vec<Log>) -> Result<()> {
    refs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut packed = b"# pack-refs with: peeled fully-peeled sorted \n".to_vec();
    for r in &refs {
        match &r.value {
            gix::refs::Target::Object(id) if !super::for_each_ref::is_root_ref(&r.name) => {
                packed.extend_from_slice(format!("{id} {}\n", r.name).as_bytes());
            }
            _ => {}
        }
    }
    write_through_lock(&dir.join("packed-refs"), &packed)?;
    for r in &refs {
        let contents = match &r.value {
            gix::refs::Target::Symbolic(target) => format!("ref: {}\n", target.as_bstr()),
            gix::refs::Target::Object(id) if super::for_each_ref::is_root_ref(&r.name) => format!("{id}\n"),
            gix::refs::Target::Object(_) => continue,
        };
        let path = dir.join(gix::path::from_bstr(r.name.as_bstr()));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_through_lock(&path, contents.as_bytes())?;
    }
    for log in &logs {
        let path = dir.join("logs").join(gix::path::from_bstr(log.refname.as_bstr()));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut line = format!(
            "{} {} {} <{}> {} {:+05}",
            log.old, log.new, log.name, log.email, log.time, log.tz_offset
        );
        if !log.message.is_empty() {
            line.push('\t');
            line.push_str(&log.message);
        }
        line.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes())?;
    }
    Ok(())
}

/// Write `contents` to `path` through `<path>.lock`, as a lockfile commits.
fn write_through_lock(path: &Path, contents: &[u8]) -> Result<()> {
    let mut lock = gix::lock::File::acquire_to_update_resource(path, gix::lock::acquire::Fail::Immediately, None)?;
    lock.write_all(contents)?;
    lock.commit()?;
    Ok(())
}

/// `ref_store_remove_on_disk()` for the files backend
/// (refs/files-backend.c:3726-3765): `refs/` and `logs/` recursively, every
/// root ref, and `packed-refs`. Each failure is added to the message and the
/// rest still attempted.
fn files_remove_on_disk(gitdir: &Path) -> std::result::Result<(), String> {
    let mut err = String::new();
    if let Err(e) = remove_dir_all_if_present(&gitdir.join("refs")) {
        err.push_str(&format!("could not delete refs: {}", crate::external::strerror(&e)));
    }
    if let Err(e) = remove_dir_all_if_present(&gitdir.join("logs")) {
        err.push_str(&format!("could not delete logs: {}", crate::external::strerror(&e)));
    }
    let roots = std::fs::read_dir(gitdir).map_err(|e| crate::external::strerror(&e))?;
    for entry in roots.filter_map(std::result::Result::ok) {
        let Ok(name) = entry.file_name().into_string() else { continue };
        if name.starts_with('.') || name.ends_with(".lock") {
            continue;
        }
        if !std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file())
            || !super::for_each_ref::is_root_ref(name.as_bytes())
        {
            continue;
        }
        if let Err(e) = std::fs::remove_file(entry.path()) {
            err.push_str(&format!("could not delete {name}: {}\n", crate::external::strerror(&e)));
        }
    }
    // `remove_path()`: a missing file is not an error.
    match std::fs::remove_file(gitdir.join("packed-refs")) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => err.push_str("could not delete packed-refs"),
    }
    match err.is_empty() {
        true => Ok(()),
        false => Err(err),
    }
}

/// `remove_dir_recursively(&sb, 0)`, which succeeds on a path that is not there.
fn remove_dir_all_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// `move_files()` (refs.c:3262-3326): rename every entry of the migration
/// directory into the git directory. The paths in a failure are git's spelling
/// of them, relative to where git stands.
fn move_files(
    from_str: &Path,
    from_abs: &Path,
    to_str: &Path,
    to_abs: &Path,
) -> std::result::Result<(), String> {
    let entries = std::fs::read_dir(from_abs).map_err(|e| {
        format!(
            "could not open source directory '{}': {}",
            from_str.display(),
            crate::external::strerror(&e)
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            format!(
                "could not read entry from directory '{}': {}",
                from_str.display(),
                crate::external::strerror(&e)
            )
        })?;
        let name = entry.file_name();
        if let Err(e) = std::fs::rename(from_abs.join(&name), to_abs.join(&name)) {
            return Err(format!(
                "could not link file '{}' to '{}': {}",
                from_str.join(&name).display(),
                to_str.join(&name).display(),
                crate::external::strerror(&e)
            ));
        }
    }
    Ok(())
}

/// `initialize_repository_version(repo, hash, to, reinit = 1)`
/// (setup.c:2444-2512): `extensions.objectformat` set for SHA-256 and unset
/// for SHA-1, `extensions.refstorage` set to `reftable` or unset for `files`,
/// `extensions.submodulepathconfig` when `init.defaultSubmodulePathConfig` asks
/// for it, and `core.repositoryformatversion` 1 when any of these, or another
/// extension only version 1 knows, remains — 0 otherwise. Each is one
/// `repo_config_set[_gently]()`, which drops a section its last key leaves.
fn initialize_repository_version(repo: &gix::Repository, to: RefStorage) -> Result<()> {
    use crate::config_store::ValuePattern;
    let path = repo.common_dir().join("config");
    let set = |key: &str, value: Option<&str>| -> Result<()> {
        match crate::config_store::set_multivar_in_file(
            &path,
            key,
            key,
            key.rfind('.').expect("the key has a section"),
            value.map(str::as_bytes),
            ValuePattern::Any,
            None,
            false,
        ) {
            Ok(()) => Ok(()),
            // `repo_config_set_gently()` of a key that is not there.
            Err(_) if value.is_none() => Ok(()),
            Err(_) => Err(crate::fatal::die(format!(
                "could not set '{key}' to '{}'",
                value.unwrap_or_default()
            ))),
        }
    };
    let sha256 = repo.object_hash() == gix::hash::Kind::Sha256;
    let reftable = to == RefStorage::Reftable;
    let mut target_version = if sha256 || reftable { 1 } else { 0 };
    set("extensions.objectformat", sha256.then_some("sha256"))?;
    set("extensions.refstorage", reftable.then_some("reftable"))?;

    // `read_repository_format()` of the config as it now is: an extension only
    // version 1 knows keeps it there (`handle_extension()`, setup.c:653-715).
    const V1_ONLY: &[&str] = &[
        "noop-v1",
        "objectformat",
        "compatobjectformat",
        "refstorage",
        "relativeworktrees",
        "submodulepathconfig",
    ];
    let file = gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let v1_only = file.sections_by_name("extensions").into_iter().flatten().any(|section| {
        section.header().subsection_name().is_none()
            && section
                .value_names()
                .any(|name| V1_ONLY.contains(&name.to_string().to_ascii_lowercase().as_str()))
    });
    if v1_only {
        target_version = 1;
    }
    if repo.config_snapshot().boolean("init.defaultSubmodulePathConfig") == Some(true) {
        if target_version == 0 {
            target_version = 1;
        }
        set("extensions.submodulepathconfig", Some("true"))?;
    }
    set("core.repositoryformatversion", Some(&target_version.to_string()))
}
