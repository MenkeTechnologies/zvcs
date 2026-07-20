use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::Kind;

/// `git fsck` — verify connectivity of the object database.
///
/// What this port actually does: it enumerates every object in the odb (loose,
/// packed and via alternates, through `gix_odb`'s `iter()`), decodes every
/// non-blob to learn its children, computes the reachable set from git's default
/// head set (all refs, `HEAD`, all reflog entries, and the index), and reports
/// the objects that fall outside it. Missing objects — referenced but absent —
/// are reported and force exit code 2, matching stock git.
///
/// Ported flags:
///   * `--unreachable`                — list every unreachable object instead of
///                                       just the dangling tips.
///   * `--dangling` / `--no-dangling` — dangling reporting is on by default.
///   * `--no-reflogs`                 — drop reflog entries from the head set.
///   * `--connectivity-only`          — accepted; this port never inspects blob
///                                       contents, so it is already the behavior.
///   * `--full`                       — accepted; the odb iterator always covers
///                                       packs and alternates.
///   * `--no-references`              — accepted; see the divergence note below.
///   * `--progress` / `--no-progress` — accepted; progress is a stderr-only,
///                                       tty-gated affordance and none is emitted.
///
/// Every other flag `bail!`s rather than being ignored: `--strict`, `--verbose`,
/// `--lost-found`, `--name-objects`, `--root`, `--tags`, `--cache`, `--no-full`,
/// an explicit `--references`, and `<object>` head arguments.
///
/// ### Known divergences from stock git — read before trusting a clean result
///
/// 1. **No fsck message layer.** git additionally lints object *contents*
///    (`badDate`, `missingEmail`, `zeroPaddedFilemode`, `hasDotgit`,
///    `duplicateEntries`, and the rest of the `fsck.<msg-id>` set) and exits 2
///    when an error-severity message fires. None of that lives in the vendored
///    crates, so a repository whose only defect is a semantic lint violation is
///    reported clean here while stock git exits 2. This port is equivalent in
///    depth to `git fsck --connectivity-only`, not to bare `git fsck`.
/// 2. **No `git refs verify`.** git checks the reference database by default
///    (`--references`); this port does not, which is why only `--no-references`
///    is accepted.
/// 3. **No re-hashing.** git recomputes each object's hash to catch a silent
///    `hash mismatch`; this port trusts the odb's own integrity checking.
/// 4. **Corruption exit code is coarse.** An object the odb cannot read is
///    reported `fatal:` with exit 128, which matches git's loose-object
///    corruption path; git distinguishes an unreadable object (128) from a
///    decodable-but-malformed one (2) and this port reports 128 for both.
/// 5. **Gitlink entries are not walked**, matching `gix-fsck`: a submodule
///    commit that happens to live in this odb is not marked reachable by the
///    tree that names it.
///
/// ### Output ordering
///
/// git emits these lines in the slot order of its internal `obj_hash` table
/// (`object.c`), i.e. `u32::from_le_bytes(oid[0..4]) % obj_hash_size` with linear
/// probing, iterated from slot 0. That is reproduced exactly here, including the
/// table's growth schedule. Collision resolution, however, depends on the order
/// in which `builtin/fsck.c` happens to create objects, which this port does not
/// model. So when two or more lines would be printed and any two objects in the
/// table share a home slot, the ordering is not provably git's and the command
/// `bail!`s instead of guessing. A clean repository (no output) and a
/// single-line report are always exact.
pub fn fsck(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 regardless of how the
    // dispatcher slices argv.
    let args: &[String] = match args.first() {
        Some(a) if a == "fsck" => &args[1..],
        _ => args,
    };

    let mut show_unreachable = false;
    let mut show_dangling = true;
    let mut use_reflogs = true;

    for a in args {
        match a.as_str() {
            "--unreachable" => show_unreachable = true,
            "--dangling" => show_dangling = true,
            "--no-dangling" => show_dangling = false,
            "--no-reflogs" => use_reflogs = false,
            // Already the behavior of this port; accepted as a no-op.
            "--connectivity-only" | "--full" | "--no-references" => {}
            "--progress" | "--no-progress" => {}
            "--reflogs" => use_reflogs = true,
            "--strict" | "--no-strict" => bail!(
                "--strict is not ported: it selects stricter fsck.<msg-id> object \
                 checks, and the fsck message layer does not exist in the vendored crates"
            ),
            "--verbose" | "-v" => bail!(
                "--verbose is not ported: its per-object \"Checking ...\" trace is not reproduced"
            ),
            "--lost-found" => bail!(
                "--lost-found is not ported: it writes dangling objects into .git/lost-found/"
            ),
            "--name-objects" | "--no-name-objects" => bail!(
                "--name-objects is not ported: it needs git's reachability-path naming"
            ),
            "--root" => bail!("--root is not ported: `root <oid>` lines follow git's traversal order"),
            "--tags" => bail!("--tags is not ported: `tagged ...` lines follow git's traversal order"),
            "--cache" => bail!(
                "--cache is not ported: it additionally verifies the index checksum and \
                 cache-entry order, which gix-index does not expose"
            ),
            "--no-full" => bail!(
                "--no-full is not ported: gix_odb always iterates packs and alternates"
            ),
            "--references" => bail!(
                "--references is not ported: reference-database verification (`git refs verify`) \
                 has no equivalent in the vendored crates; pass --no-references"
            ),
            s if s.starts_with('-') => bail!(
                "unsupported flag {s:?} (ported: --unreachable, --dangling, --no-dangling, \
                 --no-reflogs, --connectivity-only, --full, --no-references, --progress, \
                 --no-progress)"
            ),
            s => bail!(
                "explicit <object> head arguments are not ported ({s:?}); \
                 the default head set (refs, HEAD, reflogs, index) is always used"
            ),
        }
    }

    let repo = gix::discover(".")?;

    // A linked worktree contributes its own HEAD and index to git's head set;
    // this port only reads the main ones, so refuse rather than mis-report.
    if repo.git_dir() != repo.common_dir() {
        bail!("running from a linked worktree is not supported");
    }
    if has_linked_worktrees(&repo) {
        bail!("repositories with linked worktrees are not supported: their HEAD and index are heads too");
    }

    let mut state = State::default();

    // ---- every object in the odb ------------------------------------------
    let mut all: Vec<ObjectId> = Vec::new();
    for id in repo.objects.iter()? {
        let id = id?;
        if state.note(id) {
            all.push(id);
        }
    }

    // ---- pass A: children of every object, for `used` and `missing` --------
    //
    // git checks every object in the odb, not just the reachable ones, and marks
    // each child it sees as used. `dangling` is precisely "unreachable and never
    // used", so this pass has to cover unreachable objects too.
    for &id in &all {
        let kind = match repo.find_header(id) {
            Ok(h) => h.kind(),
            Err(e) => return Ok(fatal_corrupt(id, &e)),
        };
        if kind == Kind::Blob {
            continue;
        }
        let children = match children_of(&repo, id) {
            Ok(c) => c,
            Err(e) => return Ok(fatal_corrupt(id, &e)),
        };
        for (child, child_kind) in children {
            state.note(child);
            state.used.insert(child);
            if !repo.has_object(child) {
                state.missing.insert(child, child_kind);
            }
        }
    }

    // ---- pass B: reachability from git's default head set ------------------
    let heads = collect_heads(&repo, use_reflogs, &mut state)?;
    let mut queue: Vec<ObjectId> = Vec::new();
    for id in heads {
        if state.reachable.insert(id) {
            queue.push(id);
        }
    }
    while let Some(id) = queue.pop() {
        let kind = match repo.find_header(id) {
            Ok(h) => h.kind(),
            // Missing heads are already recorded; nothing to descend into.
            Err(_) => continue,
        };
        if kind == Kind::Blob {
            continue;
        }
        let children = match children_of(&repo, id) {
            Ok(c) => c,
            Err(e) => return Ok(fatal_corrupt(id, &e)),
        };
        for (child, child_kind) in children {
            if !repo.has_object(child) {
                state.missing.insert(child, child_kind);
                continue;
            }
            if state.reachable.insert(child) {
                queue.push(child);
            }
        }
    }

    // ---- build the report --------------------------------------------------
    let mut lines: Vec<(ObjectId, String)> = Vec::new();
    for (&id, &kind) in &state.missing {
        lines.push((id, format!("missing {kind} {id}")));
    }
    if show_unreachable || show_dangling {
        for &id in &all {
            if state.reachable.contains(&id) {
                continue;
            }
            if show_unreachable {
                let kind = repo.find_header(id)?.kind();
                lines.push((id, format!("unreachable {kind} {id}")));
            } else if !state.used.contains(&id) {
                let kind = repo.find_header(id)?.kind();
                lines.push((id, format!("dangling {kind} {id}")));
            }
        }
    }

    // ---- order the report the way git's obj_hash table does ----------------
    let size = obj_hash_size(state.known.len());
    if lines.len() > 1 && has_slot_collision(&state.known, size) {
        bail!(
            "refusing to guess the output order: git emits these {} lines in obj_hash slot \
             order, and two objects in this repository share a home slot, so their relative \
             order depends on git's internal object-creation sequence, which this port does \
             not model",
            lines.len()
        );
    }
    lines.sort_by_key(|(id, _)| slot(id, size));

    let mut out = String::new();
    for (_, line) in &lines {
        out.push_str(line);
        out.push('\n');
    }
    print!("{out}");

    Ok(if state.missing.is_empty() {
        ExitCode::SUCCESS
    } else {
        // git returns 2 when any fsck error was reported.
        ExitCode::from(2)
    })
}

/// Everything accumulated across the two passes.
#[derive(Default)]
struct State {
    /// Every object id git's `obj_hash` would hold: present objects plus every
    /// id merely referenced by one. Drives the output ordering.
    known: HashSet<ObjectId>,
    /// Objects referenced by some other object — the complement of `dangling`.
    used: HashSet<ObjectId>,
    /// Objects reachable from the head set.
    reachable: HashSet<ObjectId>,
    /// Referenced but absent, with the type expected at the reference site.
    missing: HashMap<ObjectId, Kind>,
}

impl State {
    /// Record `id` as an object git would have created. Returns whether it is new.
    fn note(&mut self, id: ObjectId) -> bool {
        self.known.insert(id)
    }
}

/// The objects `id` refers to, paired with the type expected at each site (which
/// is what git names in a `missing <type> <oid>` line).
///
/// Gitlink tree entries are skipped: they name commits of a different repository.
fn children_of(repo: &gix::Repository, id: ObjectId) -> Result<Vec<(ObjectId, Kind)>> {
    use gix::objs::tree::EntryKind;

    let object = repo.find_object(id)?;
    let mut out = Vec::new();
    match object.kind {
        Kind::Commit => {
            let commit = gix::objs::CommitRef::from_bytes(&object.data, repo.object_hash())?;
            out.push((commit.tree(), Kind::Tree));
            out.extend(commit.parents().map(|p| (p, Kind::Commit)));
        }
        Kind::Tree => {
            let tree = gix::objs::TreeRef::from_bytes(&object.data, repo.object_hash())?;
            for entry in &tree.entries {
                let kind = match entry.mode.kind() {
                    EntryKind::Tree => Kind::Tree,
                    EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => Kind::Blob,
                    EntryKind::Commit => continue,
                };
                out.push((entry.oid.to_owned(), kind));
            }
        }
        Kind::Tag => {
            let tag = gix::objs::TagRef::from_bytes(&object.data, repo.object_hash())?;
            out.push((tag.target(), tag.target_kind));
        }
        Kind::Blob => {}
    }
    Ok(out)
}

/// git's default head set: every reference, `HEAD`, every reflog entry (unless
/// `--no-reflogs`), and every index entry.
///
/// Ids named by a head but absent from the odb are still `note`d, because git
/// creates those objects too and they occupy an `obj_hash` slot.
fn collect_heads(repo: &gix::Repository, use_reflogs: bool, state: &mut State) -> Result<Vec<ObjectId>> {
    let mut heads: Vec<ObjectId> = Vec::new();
    let mut push = |state: &mut State, id: ObjectId| {
        state.note(id);
        heads.push(id);
    };

    // References, taking each ref's direct target rather than its fully peeled
    // one, so an annotated tag object counts as reachable in its own right.
    for reference in repo.references()?.all()? {
        // The iterator yields a boxed error, which anyhow cannot convert via `?`.
        let reference = reference.map_err(|e| anyhow::anyhow!(e))?;
        if let Some(id) = reference.target().try_id() {
            push(state, id.to_owned());
        } else if let Ok(id) = reference.into_fully_peeled_id() {
            push(state, id.detach());
        }
    }

    // HEAD is a pseudo-ref and is not part of the `refs/` iteration above.
    if let Ok(head) = repo.head() {
        if let Some(id) = head.id() {
            push(state, id.detach());
        }
    }

    if use_reflogs {
        let logs_root = repo.common_dir().join("logs");
        let mut names: Vec<String> = Vec::new();
        collect_log_names(&logs_root, "", &mut names)?;
        let mut buf = Vec::new();
        for name in names {
            // A log file whose path is not a well-formed ref name is skipped
            // rather than fatal, matching git's tolerance of stray files there.
            let Ok(Some(iter)) = repo.refs.reflog_iter(name.as_str(), &mut buf) else {
                continue;
            };
            for line in iter {
                let line = line?;
                for id in [line.previous_oid(), line.new_oid()] {
                    if !id.is_null() {
                        push(state, id);
                    }
                }
            }
        }
    }

    // Index entries are heads by default (only `--cache`'s extra index
    // verification is opt-in, not the marking itself).
    if let Ok(index) = repo.index_or_empty() {
        for entry in index.entries() {
            push(state, entry.id);
        }
    }

    Ok(heads)
}

/// Append every reflog file below `dir` to `out` as a `/`-joined ref name.
fn collect_log_names(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in read {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let full = format!("{prefix}{name}");
        if entry.file_type()?.is_dir() {
            collect_log_names(&entry.path(), &format!("{full}/"), out)?;
        } else {
            out.push(full);
        }
    }
    Ok(())
}

/// Whether `$GIT_COMMON_DIR/worktrees` holds at least one linked worktree.
fn has_linked_worktrees(repo: &gix::Repository) -> bool {
    std::fs::read_dir(repo.common_dir().join("worktrees"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// The size `obj_hash` ends at after `n` objects have been created, replaying
/// git's growth rule from `object.c::create_object`: before each insertion, grow
/// when `obj_hash_size - 1 <= nr_objs * 2`, to 32 initially and by doubling after.
fn obj_hash_size(n: usize) -> usize {
    let mut size: i64 = 0;
    for nr in 0..n as i64 {
        if size - 1 <= nr * 2 {
            size = if size < 32 { 32 } else { size * 2 };
        }
    }
    size.max(32) as usize
}

/// An object's home slot: the first four bytes of the id read as a native
/// little-endian `u32`, modulo the table size (`object.c::hashtable_index`).
fn slot(id: &ObjectId, size: usize) -> usize {
    let b = id.as_bytes();
    let head = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    head as usize % size
}

/// Whether any two known objects hash to the same home slot. When they do, git's
/// linear probing makes the final ordering depend on insertion order.
fn has_slot_collision(known: &HashSet<ObjectId>, size: usize) -> bool {
    let mut seen = HashSet::with_capacity(known.len());
    known.iter().any(|id| !seen.insert(slot(id, size)))
}

/// git aborts with `fatal:` and exit 128 when it cannot read an object.
fn fatal_corrupt(id: ObjectId, err: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("fatal: object {id} is corrupt: {err}");
    ExitCode::from(128)
}
