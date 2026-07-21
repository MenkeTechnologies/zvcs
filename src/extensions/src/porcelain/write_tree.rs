//! `git write-tree` — create a tree object from the current index.
//!
//! Covered: the whole documented surface — `--missing-ok` / `--no-missing-ok`,
//! `--prefix=<prefix>/` (and the separate-argument `--prefix <prefix>/`) /
//! `--no-prefix`, and `-h`. Stdout is the 40-hex tree id plus a newline, exactly
//! as stock git prints it. The failure paths (unmerged index, missing object,
//! unknown prefix, bad usage) reproduce git's stderr text and exit codes.
//!
//! Not covered: git additionally refreshes the index's `TREE` (cache-tree)
//! extension on disk after a successful run. The vendored `gix-index` writer
//! emits that extension "as-is and is **not** recomputed"
//! (`src/ported/gix-index/src/write.rs`), so there is no substrate to rebuild
//! it; this port leaves the index file untouched rather than writing a stale
//! extension. Every tree object git would create is still written to the odb.

use anyhow::Result;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::index::entry::Stage;
use gix::objs::tree::EntryMode;

/// Stock git's `write-tree` usage block, byte-for-byte (208 bytes), including
/// the trailing blank line. Printed on `-h` (stdout) and after the `error:`
/// line for a usage error (stderr).
const USAGE: &str = "usage: git write-tree [--missing-ok] [--prefix=<prefix>/]\n\
                     \n\
                     \x20   --[no-]missing-ok     allow missing objects\n\
                     \x20   --[no-]prefix <prefix>/\n\
                     \x20                         write tree object for a subdirectory <prefix>\n\
                     \n";

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
    let mut prefix: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
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
    let index = repo.index_or_empty()?;
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
        if !missing_ok
            && !entry.mode.is_submodule()
            && repo.try_find_header(entry.id)?.is_none()
        {
            eprintln!(
                "error: invalid object {} {} for '{path}'",
                octal(mode),
                entry.id
            );
            eprintln!("fatal: git-write-tree: error building trees");
            return Ok(ExitCode::from(128));
        }

        editor.upsert(
            path.split(|&b| b == b'/').map(|c| c.as_bstr()),
            mode.kind(),
            entry.id,
        )?;
    }

    if unmerged > 0 {
        eprintln!("fatal: git-write-tree: error building trees");
        return Ok(ExitCode::from(128));
    }

    // Writes the root tree and every sub-tree beneath it into the odb.
    let tree_id = editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?;

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
