//! `repo_migrate_ref_storage_format()` (refs.c:3344-3542, v2.56.0) for the one
//! direction this build can write: a `files` repository into `reftable`.
//!
//! The steps are git's, in git's order:
//!
//!  1. `mkdtemp("<gitdir>/ref_migration.XXXXXX")` and create a reftable store in
//!     it — `reftable/` plus the `HEAD` and `refs/heads` stubs that keep older
//!     clients from mistaking the repository for a files one;
//!  2. one initial transaction holding every reference (root refs, symrefs and
//!     broken refs included) and, unless `--no-reflog`, every reflog entry, each
//!     entry under its own update index so a reflog keeps its order;
//!  3. `--dry-run` stops here and names the directory;
//!  4. otherwise the files store is deleted (`refs/`, `logs/`, the root refs,
//!     `packed-refs`), the new store's files are renamed into the git directory,
//!     and the repository format is rewritten to `extensions.refStorage =
//!     reftable`.
//!
//! The table itself is written by `gix-reftable`, the port of git's `reftable/`
//! library, exactly as `write_transaction_table()` (refs/reftable-backend.c:
//! 1463-1636) feeds it.

use anyhow::Result;
use gix::bstr::{BString, ByteSlice};
use gix_reftable::{LogRecord, LogUpdate, LogValue, RefRecord, RefValue, Stack, WriteOptions};
use std::path::{Path, PathBuf};

/// How a migration ended when it did not succeed: the text git collects in
/// `errbuf`, which `cmd_refs_migrate()` prints through `error("%s", …)`.
pub(super) struct Failed(pub String);

/// One reference of the old store, as `migrate_one_ref()` hands it to the
/// transaction: `ref_transaction_create()` for an object id,
/// `ref_transaction_update()` with a `new_target` for a symref.
struct Ref {
    name: BString,
    value: RefValue,
}

/// One reflog entry, as `migrate_one_reflog_entry()` hands it to
/// `ref_transaction_update_reflog()`.
struct Log {
    refname: BString,
    old: gix::ObjectId,
    new: gix::ObjectId,
    name: BString,
    email: BString,
    time: u64,
    tz_offset: i16,
    message: String,
    index: u64,
}

/// Migrate `repo`, which uses the files backend, to reftable. The caller has
/// already made `cmd_refs_migrate()`'s checks and refused a repository with
/// worktrees.
pub(super) fn files_to_reftable(
    repo: &gix::Repository,
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
        let opts = write_options(repo);
        create_on_disk(repo, &new_abs)?;
        let refs = collect_refs(repo, &gitdir)?;
        let logs = match skip_reflog {
            true => Vec::new(),
            false => collect_logs(repo, &gitdir)?,
        };
        let mut stack = Stack::new(&new_abs.join("reftable"), &opts)
            .map_err(|e| anyhow::anyhow!("reftable: {e}"))?;
        if let Err(e) = commit_initial(&mut stack, &opts, refs, logs) {
            return Ok(Err(Failed(format!("reftable: transaction failure: {e}"))));
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
        files_remove_on_disk(&gitdir)?;
        move_files(&new_gitdir, &new_abs, &gitdir_str, &gitdir)?;
        if let Err(e) = std::fs::remove_dir(&new_abs) {
            eprintln!(
                "warning: could not remove temporary migration directory '{}': {}",
                new_gitdir.display(),
                crate::external::strerror(&e)
            );
        }
        initialize_repository_version(repo).map_err(|e| format!("{e:#}"))
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

/// `reftable_be_write_options()` (refs/reftable-backend.c:361-392): the
/// library's write options with the `reftable.*` configuration applied, a
/// 100ms lock timeout, and new files created `0666` less the umask, widened by
/// `core.sharedRepository`.
fn write_options(repo: &gix::Repository) -> WriteOptions {
    let config = repo.config_snapshot();
    let ulong = |key: &str| config.integer(key).and_then(|v| u64::try_from(v).ok());
    // SAFETY: `umask()` only swaps the process mask; it is read and restored.
    let mask = unsafe {
        let mask = libc::umask(0);
        libc::umask(mask);
        u32::from(mask)
    };
    let shared = config
        .string("core.sharedRepository")
        .and_then(|v| super::init::parse_shared_value(&v.to_string()).ok())
        .unwrap_or(0);
    WriteOptions {
        hash_id: match repo.object_hash() {
            gix::hash::Kind::Sha256 => gix_reftable::HashId::Sha256,
            _ => gix_reftable::HashId::Sha1,
        },
        block_size: ulong("reftable.blockSize").map_or(gix_reftable::DEFAULT_BLOCK_SIZE, |v| v as u32),
        restart_interval: ulong("reftable.restartInterval").map_or(0, |v| v as u16),
        skip_index_objects: config.boolean("reftable.indexObjects").is_some_and(|v| !v),
        auto_compaction_factor: ulong("reftable.geometricFactor").map_or(0, |v| v as u8),
        lock_timeout_ms: config.integer("reftable.lockTimeout").unwrap_or(100),
        default_permissions: Some(super::init::calc_shared_perm(shared, 0o666 & !mask)),
        disable_auto_compact: !crate::setup::git_env_bool("GIT_TEST_REFTABLE_AUTOCOMPACTION", true),
        ..WriteOptions::default()
    }
}

/// `ref_store_create_on_disk()` for the reftable backend (refs.c:2226-2244 and
/// refs/reftable-backend.c:497-510): the `reftable/` directory, then
/// `refs_create_refdir_stubs()` (refs.c:2202-2223) — a `HEAD` pointing at the
/// invalid branch `.invalid`, and a `refs/heads` *file* naming the format.
fn create_on_disk(repo: &gix::Repository, dir: &Path) -> Result<()> {
    std::fs::create_dir(dir.join("reftable"))?;
    std::fs::write(dir.join("HEAD"), "ref: refs/heads/.invalid\n")?;
    std::fs::create_dir(dir.join("refs"))?;
    std::fs::write(dir.join("refs/heads"), "this repository uses the reftable format\n")?;
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

/// `refs_for_each_ref_ext(old_refs, migrate_one_ref, …)` with
/// `REFS_FOR_EACH_INCLUDE_ROOT_REFS | REFS_FOR_EACH_INCLUDE_BROKEN`: the root
/// refs in the git directory (`HEAD`, `ORIG_HEAD`, … — never `FETCH_HEAD` or
/// `MERGE_HEAD`), then everything under `refs/`, loose over packed.
fn collect_refs(repo: &gix::Repository, gitdir: &Path) -> Result<Vec<Ref>> {
    let mut out = Vec::new();
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
            out.push(Ref { name: name.into(), value: ref_value(&r.target) });
        }
    }
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow::anyhow!("{e}"))?;
        let name = r.name().as_bstr().to_owned();
        if !name.starts_with(b"refs/") {
            continue;
        }
        let value = ref_value(&r.inner.target);
        out.push(Ref { name, value });
    }
    Ok(out)
}

/// `migrate_one_ref()`: a symref keeps its referent (`REF_NO_DEREF`), anything
/// else its object id. `ref_transaction_create()` is never given a peeled
/// value, so a tag is a `VAL1` record, not a `VAL2`.
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

/// `refs_for_each_reflog(old_refs, migrate_one_reflog, …)`: every reflog under
/// `<gitdir>/logs`, walked as `dir_iterator_begin(…, DIR_ITERATOR_SORTED)`
/// walks it — each directory's entries in `strcmp()` order, a directory's
/// contents right after the directory itself — skipping what is not a regular
/// file or whose basename is not a well-formed one-level refname
/// (`files_reflog_iterator_advance()`, refs/files-backend.c:2411-2430). Each
/// entry takes the next value of `data->index`, starting at 0.
fn collect_logs(repo: &gix::Repository, gitdir: &Path) -> Result<Vec<Log>> {
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
                    value: r.value.clone(),
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
                        message: l.message.as_bytes()[..l.message.len().min(limit)].into(),
                    }),
                })
                .collect();
            if !records.is_empty() {
                wr.add_logs(&mut records)?;
            }
            Ok(())
        },
        gix_reftable::stack::NEW_ADDITION_RELOAD,
    )
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

/// `initialize_repository_version(repo, hash, REF_STORAGE_FORMAT_REFTABLE,
/// reinit = 1)` (setup.c:2444-2512): `extensions.objectformat` set for SHA-256
/// and dropped for SHA-1, `extensions.refstorage = reftable`,
/// `extensions.submodulepathconfig` when `init.defaultSubmodulePathConfig`
/// asks for it, and `core.repositoryformatversion = 1`, in that order.
fn initialize_repository_version(repo: &gix::Repository) -> Result<()> {
    let path = repo.common_dir().join("config");
    let mut file = gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match repo.object_hash() {
        gix::hash::Kind::Sha256 => {
            file.set_raw_value_by("extensions", None, "objectformat", "sha256")?;
        }
        _ => {
            if let Ok(mut section) = file.section_mut("extensions", None) {
                section.remove("objectformat");
            }
        }
    }
    file.set_raw_value_by("extensions", None, "refstorage", "reftable")?;
    if repo.config_snapshot().boolean("init.defaultSubmodulePathConfig") == Some(true) {
        file.set_raw_value_by("extensions", None, "submodulepathconfig", "true")?;
    }
    file.set_raw_value_by("core", None, "repositoryformatversion", "1")?;
    std::fs::write(&path, file.to_bstring())?;
    Ok(())
}
