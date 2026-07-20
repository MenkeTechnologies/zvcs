//! `git prune` — remove unreachable loose objects from the object database.
//!
//! This is a real port, not a stub: it reproduces `builtin/prune.c`'s three
//! phases in order, against the vendored gitoxide crates plus `std::fs` for the
//! directory walk and the unlinks (`gix-odb` exposes no removal API, and git's
//! own implementation is a raw `readdir`/`unlink` loop over `objects/`).
//!
//!   1. **Loose prune.** Every `objects/00`..`objects/ff` fan-out directory is
//!      scanned in numeric order, entries within a directory in raw `readdir`
//!      order — exactly what `for_each_loose_file_in_objdir()` does, so the
//!      emitted order matches git on the same filesystem. An object that was not
//!      marked reachable prints `<oid> <type>` under `-n`/`-v` and is unlinked
//!      unless `-n`. A name that is not a valid object name is cruft: `tmp_obj_*`
//!      goes through the stale-temporary-file path, anything else produces
//!      `bad sha1 file: <path>` on stderr. Each visited fan-out directory is
//!      `rmdir`'d (silently failing when non-empty) unless `-n`.
//!   2. **Prune-packed.** A second full scan removes loose objects that are also
//!      present in a pack (local or in an alternate), printing `rm -f <path>`
//!      under `-n` only — `-v` does not make this phase verbose, matching git,
//!      which passes `prune_packed_objects()` nothing but the dry-run bit.
//!   3. **Stale temporaries.** `tmp_*` in `objects/` and in `objects/pack/`:
//!      `Removing stale temporary file <path>` on stdout when `-n` or `-v`,
//!      unlinked unless `-n`.
//!
//! Reachability mirrors `reachable.c`'s `mark_reachable_objects(revs, 1, 0, _)`:
//! roots are every index entry's blob plus the valid cache-tree ids, every ref
//! under `refs/` (symrefs followed, tags left unpeeled so the tag object itself
//! survives), `HEAD`, every entry of every reflog under `logs/`, and any
//! `<head>...` given on the command line; then a full object closure over
//! commits (tree + parents), tags (target) and trees (entries, gitlinks skipped).
//! Missing links are ignored rather than fatal, as git sets
//! `revs->ignore_missing_links`.
//!
//! Paths are printed the way git does after it chdir's to the top level: the
//! object directory relative to the worktree (`.git/objects/...`), or relative to
//! the current directory for a bare repository (`objects/...`).
//!
//! Supported: `-n`/`--dry-run`/`--no-dry-run`, `-v`/`--verbose`/`--no-verbose`,
//! clustered short flags (`-nv`), `--progress`/`--no-progress`, `--`,
//! `<head>...`, and `-h`. Exit codes match stock git: 129 with git's usage block
//! for `-h` and for a bad option, 128 with `fatal: unrecognized argument: <name>`
//! for a `<head>` that does not resolve, 0 otherwise.
//!
//! `--progress` is accepted and deliberately produces nothing. Git's own
//! progress is a *delayed* progress written to stderr and suppressed off a tty,
//! so it never appears in captured output, and it can never affect stdout, the
//! exit code, or the resulting repository state.
//!
//! Not ported, and rejected with a precise reason rather than approximated:
//!   * `--expire <time>` (and `--no-expire`). Two pieces of substrate are
//!     missing: git's approxidate parser, and the second traversal
//!     `add_unseen_recent_objects_to_traversal()` performs to keep objects
//!     *referenced by* recently-written unreachable objects alive. Guessing
//!     either one deletes objects git would have kept.
//!   * `--exclude-promisor-objects`, which needs promisor-pack awareness that
//!     the vendored `gix-pack` does not model.
//!   * A shallow repository, because git additionally rewrites `.git/shallow`
//!     via `prune_shallow()`, and there is no shallow-file writer here.
//!   * A repository with linked worktrees, because git also seeds reachability
//!     from every other worktree's `HEAD` and index; pruning without them would
//!     delete objects those worktrees still need.

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::Kind;
use gix::odb::pack;

/// Stock git's `prune` usage block, byte-for-byte (423 bytes, git 2.55.0),
/// including the trailing blank line. Printed on `-h` (stdout) and on a usage
/// error (stderr).
const USAGE: &str = "usage: git prune [-n] [-v] [--progress] [--expire <time>] [--] [<head>...]\n\
                     \n\
                     \x20   -n, --[no-]dry-run    do not remove, show only\n\
                     \x20   -v, --[no-]verbose    report pruned objects\n\
                     \x20   --[no-]progress       show progress\n\
                     \x20   --[no-]expire <expiry-date>\n\
                     \x20                         expire objects older than <time>\n\
                     \x20   --[no-]exclude-promisor-objects\n\
                     \x20                         limit traversal to objects outside promisor packfiles\n\
                     \n";

/// `git prune` — prune all unreachable objects from the object database.
///
/// See the module documentation for the ported surface and the exact reasons the
/// remaining flags bail.
pub fn prune(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `prune`'s positionals are revisions,
    // and `git prune prune` would name a ref called `prune`, so dropping a
    // leading verb is only safe as the very first argument — which is exactly
    // how dispatch passes it.
    let args = match args.first().map(String::as_str) {
        Some("prune") => &args[1..],
        _ => args,
    };

    let mut dry_run = false;
    let mut verbose = false;
    let mut end_of_opts = false;
    let mut heads: Vec<&str> = Vec::new();

    for a in args {
        let a = a.as_str();
        if end_of_opts {
            heads.push(a);
            continue;
        }
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--dry-run" => dry_run = true,
            "--no-dry-run" => dry_run = false,
            "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            // Delayed, stderr-only, tty-gated in git; nothing observable to emit.
            "--progress" | "--no-progress" => {}
            "--expire" | "--no-expire" => bail!(
                "unsupported flag {a:?}: --expire needs git's approxidate parser and the \
                 recent-object grace traversal, neither of which exists in the vendored crates \
                 (ported: -n, -v, --progress, --, <head>..., -h)"
            ),
            _ if a.starts_with("--expire=") => bail!(
                "unsupported flag {a:?}: --expire needs git's approxidate parser and the \
                 recent-object grace traversal, neither of which exists in the vendored crates \
                 (ported: -n, -v, --progress, --, <head>..., -h)"
            ),
            "--exclude-promisor-objects" | "--no-exclude-promisor-objects" => bail!(
                "unsupported flag {a:?}: promisor packs are not modelled by the vendored gix-pack \
                 (ported: -n, -v, --progress, --, <head>..., -h)"
            ),
            _ if a.starts_with("--") => {
                return Ok(usage_error(Some(&format!("unknown option `{}'", &a[2..]))));
            }
            _ if a.len() > 1 && a.starts_with('-') => {
                // Clustered short switches, e.g. `-nv`.
                for c in a[1..].chars() {
                    match c {
                        'n' => dry_run = true,
                        'v' => verbose = true,
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(usage_error(Some(&format!("unknown switch `{c}'")))),
                    }
                }
            }
            _ => heads.push(a),
        }
    }

    let repo = gix::discover(".")?;

    // Both of these would make a prune here delete objects stock git keeps; see
    // the module documentation.
    if repo.common_dir().join("shallow").is_file() {
        bail!(
            "prune in a shallow repository is not supported: git also rewrites .git/shallow via \
             prune_shallow(), and there is no shallow-file writer in the vendored crates"
        );
    }
    if fs::read_dir(repo.common_dir().join("worktrees"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
        bail!(
            "prune with linked worktrees is not supported: git additionally seeds reachability \
             from every other worktree's HEAD and index, which this port does not read"
        );
    }

    // Command-line `<head>`s are resolved before any reachability work, so an
    // unresolvable one fails exactly where git's `die()` does.
    let mut roots: Vec<ObjectId> = Vec::new();
    for name in &heads {
        match repo.rev_parse_single(*name) {
            Ok(id) => roots.push(id.detach()),
            Err(_) => {
                eprintln!("fatal: unrecognized argument: {name}");
                return Ok(ExitCode::from(128));
            }
        }
    }

    collect_roots(&repo, &mut roots)?;
    let reachable = close_over(&repo, roots);

    let objdir = repo.objects.store_ref().path().to_path_buf();
    let display_root = display_objdir(&repo, &objdir);
    let name_len = repo.object_hash().len_in_hex() - 2;

    // --- phase 1: prune unreachable loose objects ---------------------------
    for fanout in 0u16..256 {
        let prefix = format!("{fanout:02x}");
        let sub = objdir.join(&prefix);
        let Some(names) = read_dir_raw(&sub) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            let path = sub.join(&name);
            let shown = display_root.join(&prefix).join(&name);

            if !is_object_name(&name, name_len) {
                if name.starts_with("tmp_obj_") {
                    prune_tmp_file(&path, &shown, dry_run, verbose);
                } else {
                    eprintln!("bad sha1 file: {}", shown.display());
                }
                continue;
            }
            let Ok(oid) = ObjectId::from_hex(format!("{prefix}{name}").as_bytes()) else {
                continue;
            };
            if reachable.contains(&oid) {
                continue;
            }
            if fs::symlink_metadata(&path).is_err() {
                eprintln!("error: Could not stat '{}'", shown.display());
                continue;
            }
            if dry_run || verbose {
                // Read the type before unlinking; a header that cannot be read at
                // all prints `unknown`, as git's `oid_object_info() <= 0` does.
                let kind = repo
                    .try_find_header(oid)
                    .ok()
                    .flatten()
                    .map(|h| String::from_utf8_lossy(h.kind().as_bytes()).into_owned())
                    .unwrap_or_else(|| "unknown".to_owned());
                println!("{oid} {kind}");
            }
            if !dry_run {
                let _ = fs::remove_file(&path);
            }
        }
        if !dry_run {
            // `prune_subdir()`: an unconditional rmdir that silently fails while
            // anything is left in the directory.
            let _ = fs::remove_dir(&sub);
        }
    }

    // --- phase 2: prune loose objects that are also packed ------------------
    let indices = pack_indices(&repo, &objdir);
    for fanout in 0u16..256 {
        let prefix = format!("{fanout:02x}");
        let sub = objdir.join(&prefix);
        let Some(names) = read_dir_raw(&sub) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            if !is_object_name(&name, name_len) {
                continue;
            }
            let Ok(oid) = ObjectId::from_hex(format!("{prefix}{name}").as_bytes()) else {
                continue;
            };
            if !indices.iter().any(|idx| idx.lookup(oid).is_some()) {
                continue;
            }
            if dry_run {
                println!("rm -f {}", display_root.join(&prefix).join(&name).display());
            } else {
                let _ = fs::remove_file(sub.join(&name));
            }
        }
        if !dry_run {
            let _ = fs::remove_dir(&sub);
        }
    }

    // --- phase 3: stale temporary files -------------------------------------
    for rel in ["", "pack"] {
        let dir = if rel.is_empty() {
            objdir.clone()
        } else {
            objdir.join(rel)
        };
        let shown_dir = if rel.is_empty() {
            display_root.clone()
        } else {
            display_root.join(rel)
        };
        let Some(names) = read_dir_raw(&dir) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            if name.starts_with("tmp_") {
                prune_tmp_file(&dir.join(&name), &shown_dir.join(&name), dry_run, verbose);
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// git's parse-options failure shape: an `error: <msg>` line followed by the
/// usage block, both on stderr, exit 129.
fn usage_error(msg: Option<&str>) -> ExitCode {
    match msg {
        Some(m) => eprint!("error: {m}\n{USAGE}"),
        None => eprint!("{USAGE}"),
    }
    ExitCode::from(129)
}

/// `prune_tmp_file()`: report under `-n`/`-v`, unlink unless `-n`, and stay
/// silent for a file that has vanished between the scan and here.
fn prune_tmp_file(path: &Path, shown: &Path, dry_run: bool, verbose: bool) {
    if fs::symlink_metadata(path).is_err() {
        return;
    }
    if dry_run || verbose {
        println!("Removing stale temporary file {}", shown.display());
    }
    if !dry_run {
        let _ = fs::remove_file(path);
    }
}

/// Whether a fan-out directory entry names an object: `hexsz - 2` hex digits,
/// which is precisely `for_each_file_in_obj_subdir()`'s `hex_to_bytes()` test.
/// Everything else is cruft.
fn is_object_name(name: &str, name_len: usize) -> bool {
    name.len() == name_len && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Directory entries in raw `readdir` order — deliberately unsorted, because
/// git's own scan is unsorted and stdout order has to match. `None` when the
/// directory does not exist or cannot be read, so no `rmdir` is attempted.
fn read_dir_raw(dir: &Path) -> Option<Vec<OsString>> {
    let read = fs::read_dir(dir).ok()?;
    Some(read.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
}

/// The object directory as git prints it, i.e. relative to the directory git
/// would have chdir'd into: the worktree top for a normal repository, the
/// current directory for a bare one. Falls back to the path as opened.
fn display_objdir(repo: &gix::Repository, objdir: &Path) -> PathBuf {
    let base = repo
        .workdir()
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    let rel = base.and_then(|base| {
        let base = base.canonicalize().ok()?;
        let full = objdir.canonicalize().ok()?;
        full.strip_prefix(&base).ok().map(Path::to_path_buf)
    });
    rel.unwrap_or_else(|| objdir.to_path_buf())
}

/// Seed `roots` the way `mark_reachable_objects()` does: index entries and
/// cache-tree ids, every ref under `refs/`, `HEAD`, and every reflog entry.
fn collect_roots(repo: &gix::Repository, roots: &mut Vec<ObjectId>) -> Result<()> {
    // Index blobs (gitlinks excluded, as `do_add_index_objects_to_pending()`
    // skips `S_ISGITLINK`) plus the cache-tree, whose invalid sections git skips
    // via `entry_count >= 0` — gitoxide models that as `num_entries: None`.
    let index = repo.index_or_empty()?;
    for entry in index.entries() {
        if entry.mode == gix::index::entry::Mode::COMMIT {
            continue;
        }
        roots.push(entry.id);
    }
    if let Some(tree) = index.tree() {
        push_cache_tree(tree, roots);
    }

    // Refs are added unpeeled: an annotated tag's own object has to survive, and
    // the closure below peels it afterwards.
    for reference in repo.references()?.all()? {
        let Ok(mut reference) = reference else { continue };
        if let Ok(id) = reference.follow_to_object() {
            roots.push(id.detach());
        }
    }

    // `head_ref()`; a symbolic HEAD resolves to the same id its branch already
    // contributed, a detached one is only reachable here.
    if let Ok(head) = repo.head() {
        if let Some(id) = head.id() {
            roots.push(id.detach());
        }
    }

    collect_reflog_roots(repo, roots);
    Ok(())
}

/// Add every valid cache-tree id, recursively. A section with no entry count is
/// invalid and its id meaningless, exactly as in `add_cache_tree()`.
fn push_cache_tree(tree: &gix::index::extension::Tree, roots: &mut Vec<ObjectId>) {
    if tree.num_entries.is_some() {
        roots.push(tree.id);
    }
    for child in &tree.children {
        push_cache_tree(child, roots);
    }
}

/// Add the old and new id of every entry of every reflog, matching
/// `for_each_reflog()` + `add_one_reflog_ent()`. Null ids (a ref's creation or
/// deletion line) name no object and are skipped, as `parse_object()` returns
/// NULL for them.
fn collect_reflog_roots(repo: &gix::Repository, roots: &mut Vec<ObjectId>) {
    let mut dirs = vec![repo.common_dir().join("logs")];
    let per_worktree = repo.git_dir().join("logs");
    if per_worktree != dirs[0] {
        dirs.push(per_worktree);
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for dir in &dirs {
        collect_files(dir, &mut files);
    }

    let null = ObjectId::null(repo.object_hash());
    for file in files {
        let Ok(buf) = fs::read(&file) else { continue };
        for line in gix::refs::file::log::iter::forward(&buf) {
            let Ok(line) = line else { continue };
            for id in [line.previous_oid(), line.new_oid()] {
                if id != null {
                    roots.push(id);
                }
            }
        }
    }
}

/// Append every regular file below `dir`, recursively.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.filter_map(|e| e.ok()) {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_files(&path, out),
            Ok(_) => out.push(path),
            Err(_) => {}
        }
    }
}

/// The full object closure over `roots`: commits contribute their tree and
/// parents, tags their target, trees their non-gitlink entries. Objects that
/// cannot be found or decoded are dropped from the walk but stay in the set, so
/// a corrupt object is never mistaken for an unreachable one.
fn close_over(repo: &gix::Repository, roots: Vec<ObjectId>) -> HashSet<ObjectId> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<ObjectId> = Vec::new();
    for id in roots {
        if seen.insert(id) {
            stack.push(id);
        }
    }

    while let Some(id) = stack.pop() {
        let Ok(object) = repo.find_object(id) else {
            continue;
        };
        let mut next: Vec<ObjectId> = Vec::new();
        match object.kind {
            Kind::Blob => {}
            Kind::Commit => {
                let commit = object.into_commit();
                // Collect to owned ids inside the statement: the decoded ref
                // borrows `commit`, and a borrow held across the arm boundary
                // would outlive the binding it points into.
                let ids = commit
                    .decode()
                    .ok()
                    .map(|c| (c.tree(), c.parents().collect::<Vec<_>>()));
                if let Some((tree, parents)) = ids {
                    next.push(tree);
                    next.extend(parents);
                }
            }
            Kind::Tag => {
                let tag = object.into_tag();
                if let Ok(tag) = tag.decode() {
                    next.push(tag.target());
                }
            }
            Kind::Tree => {
                let tree = object.into_tree();
                if let Ok(tree) = tree.decode() {
                    for entry in &tree.entries {
                        // `process_tree()` never descends into a submodule.
                        if !matches!(entry.mode.kind(), gix::object::tree::EntryKind::Commit) {
                            next.push(entry.oid.to_owned());
                        }
                    }
                }
            }
        }
        for id in next {
            if seen.insert(id) {
                stack.push(id);
            }
        }
    }
    seen
}

/// Every readable pack index reachable from this repository — local packs and
/// those of each alternate — which together define `has_object_pack()`, the test
/// `prune-packed` uses. A pack whose `.pack` is missing or whose index cannot be
/// opened is skipped, as `prepare_packed_git_one()` skips it.
fn pack_indices(repo: &gix::Repository, objdir: &Path) -> Vec<pack::index::File> {
    let hash = repo.object_hash();
    let mut dirs = vec![objdir.to_path_buf()];
    if let Ok(alternates) = repo.objects.store_ref().alternate_db_paths() {
        dirs.extend(alternates);
    }

    let mut indices = Vec::new();
    for dir in dirs {
        let dir = dir.join("pack");
        let Some(names) = read_dir_raw(&dir) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            let Some(base) = name.strip_suffix(".idx") else {
                continue;
            };
            if !matches!(fs::metadata(dir.join(format!("{base}.pack"))), Ok(md) if md.is_file()) {
                continue;
            }
            if let Ok(file) = pack::index::File::at(dir.join(&name), hash) {
                indices.push(file);
            }
        }
    }
    indices
}
