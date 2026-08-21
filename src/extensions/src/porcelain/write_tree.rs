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
pub(super) fn repair_cache_tree(repo: &gix::Repository, index: &mut gix::index::File) {
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
