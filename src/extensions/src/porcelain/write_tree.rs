//! `git write-tree` — create a tree object from the current index.
//!
//! Covered: the whole documented surface — `--missing-ok` / `--no-missing-ok`,
//! `--prefix=<prefix>/` (and the separate-argument `--prefix <prefix>/`) /
//! `--no-prefix`, and `-h`. Stdout is the 40-hex tree id plus a newline, exactly
//! as stock git prints it. The failure paths (unmerged index, missing object,
//! unknown prefix, bad usage) reproduce git's stderr text and exit codes.
//!
//! The tree itself comes from the index's `TREE` (cache-tree) extension, exactly
//! as `write_index_as_tree()` (cache-tree.c:797-831) builds it: unchanged
//! directories are reused from the extension and only the invalidated ones are
//! re-serialised, and the refreshed extension is written back to the index
//! afterwards — "Not being able to write is fine -- we are only interested in
//! updating the cache-tree part" (cache-tree.c:820-825).
//!
//! This module also owns the object-database glue
//! ([`RepoOdb`], [`refresh_cache_tree`]) that the other index-writing verbs use,
//! because it is the one whose whole job is turning an index into a tree.

use anyhow::Result;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::index::entry::Stage;
use gix::index::extension::tree::update as cache_tree;
use gix::objs::tree::EntryMode;

/// Stock git's `write-tree` usage block, byte-for-byte (208 bytes), including
/// the trailing blank line. Printed on `-h` (stdout) and after the `error:`
/// line for a usage error (stderr).
/// `cmd_write_tree()`'s `struct option write_tree_options[]`
/// (builtin/write-tree.c), in table order, as [`super::resolve_long`] reads it.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "missing-ok",                  neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "prefix",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "ignore-cache-tree",           neg: true,  arg: super::Arg::None },
];

const USAGE: &str = "usage: git write-tree [--missing-ok] [--prefix=<prefix>/]\n\
                     \n\
                     \x20   --[no-]missing-ok     allow missing objects\n\
                     \x20   --[no-]prefix <prefix>/\n\
                     \x20                         write tree object for a subdirectory <prefix>\n\
                     \n";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]ignore-cache-tree`.
/// Captured byte-for-byte from stock git 2.55.0's `git write-tree --help-all`.
const USAGE_ALL: &str = r#"usage: git write-tree [--missing-ok] [--prefix=<prefix>/]

    --[no-]missing-ok     allow missing objects
    --[no-]prefix <prefix>/
                          write tree object for a subdirectory <prefix>
    --[no-]ignore-cache-tree
                          only useful for debugging

"#;

/// git reports at most this many unmerged index entries before printing `...`
/// and giving up (`cache-tree.c`'s counter is global across directories, which
/// a flat walk of the index in path order reproduces).
const MAX_UNMERGED_REPORTED: usize = 10;

/// `git write-tree` — build a tree object from the index and print its id.
///
/// Supported forms (matching stock git byte-for-byte, including exit codes):
///   * `git write-tree`                       → id of the tree the index names
///   * `--missing-ok` / `--no-missing-ok`     → skip/perform the odb presence check
///   * `--prefix=<prefix>/`, `--prefix <p>/`, `--no-prefix` → id of a sub-tree
///   * `-h`                                   → usage on stdout, exit 129
///
/// Extra positional arguments are ignored, as stock git ignores them.
pub fn write_tree(args: &[String]) -> Result<ExitCode> {
    // Dispatch may or may not include the verb itself at index 0; `write-tree`
    // has no positional of its own, so dropping a leading copy is unambiguous.
    let args = match args.first().map(String::as_str) {
        Some("write-tree") => &args[1..],
        _ => args,
    };

    let mut missing_ok = false;
    let mut ignore_cache_tree = false;
    let mut prefix: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        // Respell a unique abbreviation as the name it resolves to, so an
        // abbreviation lands on the arm its full spelling lands on.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        match a {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // `if (internal_help && !strcmp(arg + 2, "help-all"))`
            // (parse-options.c:1122): an exact match, never an abbreviation and
            // never with an `=<value>`, rendering `USAGE_FULL`.
            "--help-all" => {
                print!("{USAGE_ALL}");
                return Ok(ExitCode::from(129));
            }
            // `--ignore-cache-tree` (builtin/write-tree.c:37, "only useful for
            // debugging") makes git recompute every tree instead of reusing the
            // index's `TREE` extension: `write_index_as_tree_internal()` frees the
            // cache-tree outright and declares it invalid (cache-tree.c:751-754), so
            // every directory is re-serialised from the entries.
            "--ignore-cache-tree" => ignore_cache_tree = true,
            "--no-ignore-cache-tree" => ignore_cache_tree = false,
            "--missing-ok" => missing_ok = true,
            "--no-missing-ok" => missing_ok = false,
            "--no-prefix" => prefix = None,
            // End-of-options: everything after `--` is a pathspec/positional,
            // which write-tree ignores. Options seen before `--` still apply.
            "--" => break,
            "--prefix" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(usage_error("option `prefix' requires a value"));
                };
                prefix = Some(v.clone());
            }
            s if s.starts_with("--prefix=") => {
                prefix = Some(s["--prefix=".len()..].to_string());
            }
            s if s.starts_with("--") => {
                return Ok(usage_error(&format!("unknown option `{}'", &s[2..])));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                // git's parse-options reports the first unrecognised short switch.
                let c = s[1..].chars().next().unwrap_or('-');
                return Ok(usage_error(&format!("unknown switch `{c}'")));
            }
            // Stock git accepts and ignores stray positionals here.
            _ => {}
        }
        i += 1;
    }

    let repo = gix::discover(".")?;
    // Serialize against other zvcs writers: this appends tree objects to the odb
    // while reading the index, exactly like the tree-build phase of `commit`.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // An absent index file is an empty index, and yields git's empty-tree id.
    let mut index = open_index_for_update(&repo)?;
    if ignore_cache_tree {
        index.remove_tree();
    }

    let tree_id = match refresh_cache_tree(&repo, &mut index, missing_ok)? {
        Ok(id) => id,
        Err(err) => {
            report_tree_build_failure(&err);
            eprintln!("fatal: git-write-tree: error building trees");
            return Ok(ExitCode::from(128));
        }
    };

    // `--prefix` selects a sub-tree of what was just written; the trees are on
    // disk either way, exactly as with stock git when the prefix does not exist.
    let out_id = match prefix.as_deref() {
        None => tree_id,
        Some(p) if p.trim_end_matches('/').is_empty() => tree_id,
        Some(p) => {
            let root = repo.find_tree(tree_id)?;
            // `Path::components` drops the documented trailing slash for us.
            let entry = root.lookup_entry_by_path(std::path::Path::new(p))?;
            match entry {
                Some(e) if e.mode().is_tree() => e.object_id(),
                _ => {
                    eprintln!("fatal: git-write-tree: prefix {p} not found");
                    return Ok(ExitCode::from(128));
                }
            }
        }
    };

    println!("{out_id}");
    Ok(ExitCode::SUCCESS)
}

/// The two object-database operations `cache_tree_update()` performs, bound to a
/// repository: `odb_has_object()` (cache-tree.c:337, :445) and
/// `odb_write_object_ext(..., OBJ_TREE, ...)` (cache-tree.c:501).
///
/// `gix-index` deliberately has no repository handle, so it names these as a trait
/// and lets its caller supply them; this is that supply for every verb in this
/// binary.
pub(super) struct RepoOdb<'repo> {
    /// The repository whose odb answers the presence checks and takes the trees.
    pub(super) repo: &'repo gix::Repository,
}

impl cache_tree::Odb for RepoOdb<'_> {
    fn has_object(&self, id: &gix::hash::oid) -> bool {
        use gix::objs::Exists;
        // The empty tree is present in every repository whether or not anyone ever
        // stored it: git's object database always carries a synthetic in-memory
        // source holding exactly that object (`odb/source-inmemory.c:18-31`), so
        // `odb_has_object()` answers yes for it in a repository with no objects at
        // all. gitoxide synthesises it when *reading* but not when asked whether it
        // exists, which would otherwise make the cache-tree of an empty index
        // permanently invalid.
        id == gix::ObjectId::empty_tree(self.repo.object_hash()) || self.repo.objects.exists(id)
    }

    fn write_tree(
        &self,
        tree: &[u8],
    ) -> std::result::Result<gix::ObjectId, Box<dyn std::error::Error + Send + Sync + 'static>> {
        use gix::objs::Write;
        self.repo
            .objects
            .write_buf(gix::object::Kind::Tree, tree)
            .map_err(Into::into)
    }
}

/// `index.threads` as `repo_config_get_index_threads()` resolves it (config.c:2533-2552):
/// the `GIT_TEST_INDEX_THREADS` override first, then the key read as bool-or-int, where
/// `true` is `0` ("one thread per core"), `false` is `1`, and a number is itself. `None`
/// is git's "not configured", which every caller turns into one thread.
fn index_threads(repo: &gix::Repository) -> Option<u32> {
    // `val = git_env_ulong("GIT_TEST_INDEX_THREADS", 0); if (val) ...` — a zero or unparsable
    // value falls through to the config, which is what `git_env_ulong`'s default does.
    if let Some(val) = std::env::var("GIT_TEST_INDEX_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v != 0)
    {
        return Some(val);
    }
    let snap = repo.config_snapshot();
    let raw = snap.string("index.threads")?;
    let raw = raw.to_str().ok()?.trim().to_owned();
    Some(match raw.to_ascii_lowercase().as_str() {
        // `git_parse_maybe_bool_text()` (parse.c:166-181), which compares with
        // `strcasecmp` and returns *false* for an empty value (`if (!*value) return 0`).
        // A bare `[index] threads` with no value at all is git's `true`, but gix reports
        // no string for such a key, so it arrives here as "unset" — one thread instead of
        // one per core. The two only differ above 20 000 entries, below which the
        // per-core path produces no offset table either (`cache_nr / THREAD_COST`).
        "" | "false" | "no" | "off" => 1,
        "true" | "yes" | "on" => 0,
        other => other.parse::<u32>().ok()?,
    })
}

/// How many threads the next index write should size its `IEOT` extension for, or `None` when
/// git would write none at all.
///
/// This is `do_write_index()`'s gate, which needs the repository and so cannot live in
/// `gix-index`:
///
/// ```text
/// if (!HAVE_THREADS || repo_config_get_index_threads(the_repository, &nr_threads))
///         nr_threads = 1;
///
/// if (nr_threads != 1 && record_ieot()) {
/// ```
/// (read-cache.c:2874-2877), with `record_ieot()` (read-cache.c:2788-2801) being
/// `index.recordOffsetTable` when it is set, and otherwise "written by default if the user
/// explicitly requested threaded index reads" — i.e. `index.threads` set to anything but one
/// thread.
///
/// Both halves have to hold, which is why `index.recordOffsetTable=true` on its own writes no
/// offset table: `nr_threads` is still 1 there. Verified against stock git 2.55.0, which leaves
/// such an index at `TREE` alone.
///
/// The block count, and whether there are enough blocks to be worth writing at all, is decided
/// by `gix-index` at write time — see
/// [`gix::index::extension::index_entry_offset_table::entries_per_block()`].
pub(crate) fn offset_table_threads(repo: &gix::Repository) -> Option<u32> {
    let nr_threads = index_threads(repo).unwrap_or(1);
    if nr_threads == 1 {
        return None;
    }
    let record = repo
        .config_snapshot()
        .boolean("index.recordOffsetTable")
        // Unset: the default is "threading was asked for", which `nr_threads != 1` already is.
        .unwrap_or(true);
    record.then_some(nr_threads)
}

/// Attach the `IEOT` decision to `index` so its next write carries the extension exactly when
/// stock git's would — the one line every `do_write_index()` caller gets for free in C, because
/// there the decision is made inside the writer.
pub(crate) fn prepare_offset_table(repo: &gix::Repository, index: &mut gix::index::File) {
    index.set_offset_table_threads(offset_table_threads(repo));
}

/// Bring `index`'s cache-tree up to date with its entries and hand back the root
/// tree id, writing the index back when — and only when — that changed something.
///
/// This is `write_index_as_tree()` (cache-tree.c:797-831) with the lock handling
/// left to the caller:
///
/// * a cache-tree that is *fully valid* — every node has a count and its tree
///   object is still in the odb (`cache_tree_fully_valid()`, cache-tree.c:278-292)
///   — already names the answer, so nothing is written at all. This is what makes
///   a second `write-tree` over an untouched index free, and why it must not
///   rewrite the index file: `was_valid` gates the `write_locked_index()` call
///   (cache-tree.c:818).
/// * otherwise `cache_tree_update()` re-serialises exactly the invalidated
///   directories, and the index is rewritten so the refreshed extension is there
///   for the next reader — stock git included.
///
/// The returned `Err` is the raw failure so each verb can render git's own wording
/// for it; see [`report_tree_build_failure`] for the `write-tree` phrasing.
pub(super) fn refresh_cache_tree(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    missing_ok: bool,
) -> Result<std::result::Result<gix::ObjectId, cache_tree::Error>> {
    let odb = RepoOdb { repo };
    if index.cache_tree_fully_valid(&odb) {
        // `was_valid` — the id is already recorded, and the index stays untouched.
        return Ok(Ok(index
            .tree()
            .expect("a fully valid cache-tree is a present cache-tree")
            .id));
    }
    match index.cache_tree_update(
        &odb,
        cache_tree::Options {
            missing_ok,
            repair: false,
        },
    ) {
        Ok(id) => {
            prepare_offset_table(repo, index);
            index.write(crate::config::index_write_options(repo))?;
            Ok(Ok(id))
        }
        Err(err) => Ok(Err(err)),
    }
}

/// The as-is `prepare_index()` step (builtin/commit.c:486-491): update the
/// cache-tree if it is not fully valid, and write the index back when that
/// happened. The root tree id is discarded — callers that need it use
/// [`refresh_cache_tree`], which is the same operation with the id kept.
pub(super) fn update_cache_tree_if_stale(repo: &gix::Repository, index: &mut gix::index::File) -> Result<()> {
    refresh_cache_tree(repo, index, false)?.ok();
    Ok(())
}

/// `unpack_trees()`'s parting cache-tree refresh: recompute the extension without
/// writing a single object, keeping only the nodes whose tree the repository
/// already has.
///
/// `unpack_trees()` ends with
/// `cache_tree_update(&o->internal.result, WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`,
/// which is why a `read-tree -m` or a merge leaves a partly-valid cache-tree behind
/// even though it never mints a tree object. `WRITE_TREE_REPAIR` accepts a node only
/// after hashing what it would serialise and finding that object present
/// (cache-tree.c:490-497), so nothing here can invent a tree id.
///
/// Failures — an unmerged result, most often — leave the index with no cache-tree,
/// which is what the callers want: git ignores the return value here too.
/// What every `unpack_trees()`-shaped verb must do to an index between "the entries are final"
/// and `write_locked_index()`: replace the cache-tree with one recomputed from the entries in
/// `WRITE_TREE_REPAIR` mode, and settle the `IEOT` decision.
///
/// This is the shared route for the whole class of verbs that arrive at an index by *reading a
/// tree* — checkout, switch, reset, restore, merge and its strategies, cherry-pick, revert,
/// rebase, stash, sparse-checkout, `read-tree`, `apply --index`. In git they all funnel through
/// `unpack_trees()`, which never discards the extension: it carries the source index's over with
/// `move_index_extensions()` and ends with
/// `cache_tree_update(&o->internal.result, WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`
/// (unpack-trees.c:2079-2093). An index this port writes without that step has no `TREE` at all,
/// which costs stock git a full rebuild — and a rewrite of the index file — on its next
/// `write-tree`, `commit` or `status`.
///
/// **The [`remove_tree()`](gix::index::File::remove_tree()) is not optional.** git can afford to
/// keep the old nodes because every entry it moves is invalidated as it moves
/// (`invalidate_ce_path()`, unpack-trees.c:2298-2304); the verbs routed here mutate entries
/// without doing that, so a surviving node could still be marked valid while the entries below
/// it have changed — and `update_one()` reuses exactly such a node without looking
/// (cache-tree.c:336-339). Dropping first costs a recomputation of directories git would have
/// skipped and buys the guarantee that no node can outlive the entries it describes.
///
/// Verbs that mutate *entries* rather than read a tree — `add`, `rm`, `mv`, `update-index` — must
/// **not** come here: git invalidates their paths and leaves the extension partly invalid, so
/// repairing it instead would write a fully valid cache-tree where stock wrote a stale-marked
/// one. Use [`gix::index::State::invalidate_path_in_tree()`] per touched path there.
/// Carry `old`'s cache-tree onto `new` and invalidate exactly the paths whose entry changed —
/// the shape a **`MIXED` reset** leaves behind, and the one thing that is *not* an
/// `unpack_trees()` repair.
///
/// `cmd_reset()` sends every `--mixed` through `read_from_tree()` (builtin/reset.c:494), not
/// through `reset_index()`: it diffs the index against the target tree with `oneway_diff()` and
/// stages the differences one entry at a time, so each one goes through
/// `add_index_entry_with_check()` (read-cache.c:1273-1274) or `remove_file_from_index()`
/// (read-cache.c:632) and invalidates only its own path. Nothing repairs the result afterwards,
/// which is why stock's index after `git reset` still shows the root invalid while the
/// directories the reset did not reach keep their ids.
///
/// `git stash push` inherits this exactly, because it performs its reset by running `git reset`
/// (builtin/stash.c's `do_push_stash`), and so does the autostash snapshot.
///
/// Repairing instead would produce a *fully valid* cache-tree where stock leaves a partly
/// invalid one — not unsafe, but a structure git would not have written, and one that costs the
/// next `write-tree` nothing while making the index file 19 bytes longer than git's.
///
/// A path counts as changed when its blob id, mode or stage differs, or when it exists on only
/// one side; comparing the two sorted entry lists in one pass is the same set `oneway_diff()`
/// would have produced.
pub(crate) fn carry_cache_tree_invalidating_changes(
    repo: &gix::Repository,
    old: &gix::index::File,
    new: &mut gix::index::File,
) {
    use gix::bstr::BString;
    use std::collections::HashMap;

    let Some(tree) = old.tree().cloned() else {
        // No cache-tree to carry: git would have had one to invalidate into, this index
        // simply has nothing, and the next reader rebuilds. Never repair here — that would
        // mint the fully valid extension this function exists to avoid.
        prepare_offset_table(repo, new);
        return;
    };
    let key = |index: &gix::index::File| -> HashMap<BString, (gix::ObjectId, u32, u32)> {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode.bits(), e.stage_raw())))
            .collect()
    };
    let before = key(old);
    let after = key(new);

    new.set_tree(Some(tree));
    for (path, state) in &after {
        if before.get(path) != Some(state) {
            new.invalidate_path_in_tree(path.as_ref());
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            new.invalidate_path_in_tree(path.as_ref());
        }
    }
    prepare_offset_table(repo, new);
}

pub(crate) fn rebuild_cache_tree(repo: &gix::Repository, index: &mut gix::index::File) {
    index.remove_tree();
    repair_cache_tree(repo, index);
    prepare_offset_table(repo, index);
}

/// `unpack_trees()`'s cache-tree handling **in full**: carry the source index's extension over,
/// invalidate every path whose entry moved, and only then repair what is left.
///
/// This is the three steps git actually performs, in git's order:
///
/// 1. `invalidate_ce_path(ce, o)` → `cache_tree_invalidate_path(o->src_index, ce->name)`
///    (unpack-trees.c:190-197), called for every entry the merge adds, removes or replaces;
/// 2. `move_index_extensions(&o->internal.result, o->src_index)` — the *invalidated* cache-tree
///    is what moves onto the result;
/// 3. `if (!o->skip_cache_tree_update && !cache_tree_fully_valid(o->internal.result.cache_tree))
///    cache_tree_update(&o->internal.result, WRITE_TREE_SILENT | WRITE_TREE_REPAIR);`
///    (unpack-trees.c:2085-2093).
///
/// [`rebuild_cache_tree`] collapses 1+2 into "drop everything", which is safe but wrong in the
/// one case that matters: when the result is **unmerged**, step 3 fails at `verify_cache()`
/// (cache-tree.c:218-234) *before* touching `istate->cache_tree`, so git keeps the carried,
/// partly-invalidated structure while dropping first leaves nothing at all. Stock's index after
/// a conflicting `merge`/`cherry-pick`/`revert`/`rebase` therefore still carries `TREE` with the
/// touched nodes marked `-1` and the untouched ones still naming their tree; this reproduces it.
///
/// It is also *stricter* than dropping in the clean case rather than looser: a node survives
/// only when no entry below it changed, which is exactly what step 1 guarantees.
pub(crate) fn carry_and_repair_cache_tree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new: &mut gix::index::File,
) {
    carry_cache_tree_invalidating_changes(repo, old, new);
    repair_cache_tree(repo, new);
    prepare_offset_table(repo, new);
}

pub(crate) fn repair_cache_tree(repo: &gix::Repository, index: &mut gix::index::File) {
    let odb = RepoOdb { repo };
    let _ = index.cache_tree_update(
        &odb,
        cache_tree::Options {
            missing_ok: false,
            repair: true,
        },
    );
}

/// Recompute `index`'s cache-tree in place, discarding any failure — git's
/// `cache_tree_update(the_repository->index, WRITE_TREE_SILENT);` with the return
/// value ignored, as `prepare_index()` does before writing the real index on the
/// partial-commit path (builtin/commit.c:537) and the as-is path
/// (builtin/commit.c:488).
///
/// Ignoring the failure is safe because a failed update leaves *no* cache-tree
/// rather than a half-built one, so the index that gets written afterwards simply
/// has none — the same state this crate produced before cache-trees existed here.
pub(super) fn update_cache_tree_quietly(repo: &gix::Repository, index: &mut gix::index::File) {
    let odb = RepoOdb { repo };
    let _ = index.cache_tree_update(&odb, cache_tree::Options::default());
}

/// Print the diagnostics stock git prints for a failed `cache_tree_update()`,
/// leaving the verb-specific `fatal:`/`error:` summary line to the caller.
///
/// The unmerged report is `verify_cache()`'s (cache-tree.c:218-234): one line per
/// conflicted entry, at most ten of them and then a bare `...`. The D/F report is
/// the same function's second pass (cache-tree.c:240-255), with the same cap. The
/// `invalid object` line is `update_one()`'s (cache-tree.c:450-451) and is emitted
/// once, because git returns from the deepest level as soon as it prints it.
pub(super) fn report_tree_build_failure(err: &cache_tree::Error) {
    match err {
        cache_tree::Error::Unmerged(entries) => {
            for (n, (path, id)) in entries.iter().enumerate() {
                if n >= MAX_UNMERGED_REPORTED {
                    eprintln!("...");
                    break;
                }
                eprintln!("{path}: unmerged ({id})");
            }
        }
        cache_tree::Error::DirectoryFileConflict(pairs) => {
            for (n, (path, conflict)) in pairs.iter().enumerate() {
                if n >= MAX_UNMERGED_REPORTED {
                    eprintln!("...");
                    break;
                }
                eprintln!("You have both {path} and {conflict}");
            }
        }
        // `error("invalid object %06o %s for '%.*s'", ...)` — the `Display` of this
        // variant is that format string, so it is printed verbatim behind `error: `.
        cache_tree::Error::InvalidObject { .. } => eprintln!("error: {err}"),
        // The remaining failures are git's silent ones: `return -1` under
        // `expected_missing` (cache-tree.c:448-449) and the `die()` for an empty
        // sub-tree (cache-tree.c:387), plus odb write errors, which git reports
        // through the odb layer itself.
        cache_tree::Error::IntentToAddSubtree | cache_tree::Error::EmptySubtree => {}
        cache_tree::Error::WriteTree(source) => eprintln!("error: {source}"),
    }
}

/// The repository's index, or an empty in-memory one bound to `.git/index` when no
/// index file exists yet.
///
/// `write_index_as_tree()` reads the index under a lock and treats a missing file
/// as zero entries (`read_index_from()` returns 0), which is how `git write-tree`
/// in a freshly-`init`ed repository prints the empty-tree id *and* leaves an index
/// file behind.
pub(super) fn open_index_for_update(repo: &gix::Repository) -> Result<gix::index::File> {
    Ok(if repo.index_path().exists() {
        repo.open_index()?
    } else {
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path())
    })
}

/// The tree `index` names, with every tree object beneath it written into
/// `repo`'s odb — `cmd_write_tree()`'s call into `write_index_as_tree()`, minus
/// the `--prefix` lookup and the printing, which are the caller's.
///
/// `Err(code)` is the exit status stock git leaves after printing the same
/// diagnostics this prints: the per-entry `unmerged` lines, or the `invalid
/// object` line for an entry whose blob is not in the odb.
///
/// Shared rather than duplicated because `filter-branch` needs exactly this and
/// nothing else — the script's `tree=$(git write-tree)` is a bare `write-tree`
/// over the scratch index — and it runs it once per rewritten commit, where a
/// re-execution of this binary is pure per-commit latency.
pub(super) fn tree_from_index(
    repo: &gix::Repository,
    index: &gix::index::State,
    missing_ok: bool,
) -> Result<std::result::Result<gix::ObjectId, u8>> {
    let backing = index.path_backing();

    // One pass in index (path) order, mirroring `cache_tree_update`: report
    // unmerged entries as they are met, bail immediately on a missing object,
    // and otherwise feed the entry to the tree editor.
    let mut editor = gix::objs::tree::Editor::new(
        gix::objs::Tree::empty(),
        &repo.objects,
        repo.object_hash(),
    );
    let mut unmerged = 0usize;

    for entry in index.entries() {
        let path = entry.path_in(backing);

        if entry.stage() != Stage::Unconflicted {
            unmerged += 1;
            if unmerged > MAX_UNMERGED_REPORTED {
                eprintln!("...");
                break;
            }
            eprintln!("{path}: unmerged ({})", entry.id);
            continue;
        }

        let mode = entry
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("index entry `{path}` has an unrepresentable mode"))?;

        // git checks odb presence for everything but gitlinks, whose commits
        // legitimately live in the submodule's own object database.
        if !missing_ok && !entry.mode.is_submodule() && repo.try_find_header(entry.id)?.is_none() {
            eprintln!(
                "error: invalid object {} {} for '{path}'",
                octal(mode),
                entry.id
            );
            eprintln!("fatal: git-write-tree: error building trees");
            return Ok(Err(128));
        }

        editor.upsert(
            path.split(|&b| b == b'/').map(|c| c.as_bstr()),
            mode.kind(),
            entry.id,
        )?;
    }

    if unmerged > 0 {
        eprintln!("fatal: git-write-tree: error building trees");
        return Ok(Err(128));
    }

    // Writes the root tree and every sub-tree beneath it into the odb.
    Ok(Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?))
}

/// git's parse-options failure shape: `error: <msg>` then the usage block on
/// stderr, exit 129.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(129)
}

/// The git-internal octal representation of a tree entry mode, e.g. `100644`.
fn octal(mode: EntryMode) -> String {
    let mut buf = [0u8; 6];
    mode.as_bytes(&mut buf).to_string()
}
