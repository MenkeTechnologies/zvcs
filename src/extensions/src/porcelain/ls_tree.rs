use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::object::tree::{EntryKind, EntryMode};
use gix::prelude::ObjectIdExt;

/// Parsed command-line options for a single `ls-tree` invocation.
struct Opts {
    recurse: bool,     // -r: descend into sub-trees
    show_trees: bool,  // -t: emit tree lines even while recursing into them
    dirs_only: bool,   // -d: list tree entries only, never their contents
    long: bool,        // -l/--long: append the blob size column
    nul: bool,         // -z: terminate records with NUL instead of newline
    name_only: bool,   // --name-only/--name-status: print the path alone
    object_only: bool, // --object-only: print the object id alone
    abbrev: Option<Option<usize>>, // --abbrev[=N]: None=full, Some(None)=auto, Some(Some(n))=N
    paths: Vec<String>, // path filters (empty = whole tree)
}

/// `git ls-tree` — list the contents of a tree object.
///
/// Supported forms (matching stock `git ls-tree` output byte-for-byte for these):
///   * `git ls-tree <tree-ish>`               → immediate children
///   * `git ls-tree -r <tree-ish>`            → recurse, leaves only
///   * `git ls-tree -r -t <tree-ish>`         → recurse, include tree lines
///   * `git ls-tree -d <tree-ish>`            → tree entries only
///   * `-l`/`--long`, `-z`, `--name-only`/`--name-status`, `--object-only`
///   * `--abbrev[=N]`, `--full-name`/`--full-tree` (accepted; output is already root-relative)
///   * trailing `[--] <path>...` filters (exact file, or a directory prefix)
///
/// `<tree-ish>` is resolved through `rev_parse_single`, so commits, tags, refs,
/// raw tree ids and `<rev>:<path>` all peel to the tree they name.
///
/// Unsupported: `--format=<fmt>` (custom output templates) and pathspec magic
/// (globs, `:(...)` prefixes) — these `bail!` with a precise message rather than
/// producing output that would silently diverge from git.
pub fn ls_tree(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        recurse: false,
        show_trees: false,
        dirs_only: false,
        long: false,
        nul: false,
        name_only: false,
        object_only: false,
        abbrev: None,
        paths: Vec::new(),
    };

    let mut treeish: Option<&str> = None;
    let mut positionals: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    for a in args {
        if !no_more_opts && a == "--" {
            no_more_opts = true;
            continue;
        }
        if !no_more_opts && a.len() > 1 && a.starts_with('-') {
            if let Some(long_opt) = a.strip_prefix("--") {
                match long_opt {
                    "long" => opts.long = true,
                    "name-only" | "name-status" => opts.name_only = true,
                    "object-only" => opts.object_only = true,
                    "full-name" | "full-tree" => {} // already root-relative here
                    "abbrev" => opts.abbrev = Some(None),
                    _ if long_opt.starts_with("abbrev=") => {
                        let n: usize = long_opt["abbrev=".len()..]
                            .parse()
                            .map_err(|_| anyhow::anyhow!("invalid --abbrev value in {a:?}"))?;
                        opts.abbrev = Some(Some(n));
                    }
                    _ if long_opt == "format" || long_opt.starts_with("format=") => {
                        bail!("custom --format is not supported")
                    }
                    _ => bail!("unknown option {a:?}"),
                }
            } else {
                // Grouped short flags, e.g. `-rt`.
                for c in a[1..].chars() {
                    match c {
                        'r' => opts.recurse = true,
                        't' => opts.show_trees = true,
                        'd' => opts.dirs_only = true,
                        'l' => opts.long = true,
                        'z' => opts.nul = true,
                        _ => bail!("unknown option -{c}"),
                    }
                }
            }
            continue;
        }
        if treeish.is_none() {
            treeish = Some(a.as_str());
        } else {
            positionals.push(a.as_str());
        }
    }

    let Some(spec) = treeish else {
        bail!("missing <tree-ish> argument");
    };

    // Path filters: reject pathspec magic we don't honour, strip one trailing '/'.
    for p in &positionals {
        if p.starts_with(':') {
            bail!("pathspec magic is not supported: {p:?}");
        }
        opts.paths.push(p.trim_end_matches('/').to_string());
    }

    let repo = gix::discover(".")?;
    let tree = repo.rev_parse_single(spec)?.object()?.peel_to_tree()?;

    let mut out = String::new();
    walk(&repo, tree, "", &opts, &mut out)?;
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// Recursively render `tree` (rooted at `prefix`, e.g. `"dir/"`) into `out`.
fn walk(
    repo: &gix::Repository,
    tree: gix::Tree<'_>,
    prefix: &str,
    opts: &Opts,
    out: &mut String,
) -> Result<()> {
    // Materialise the entries so the borrow on the tree's data ends before we
    // recurse (child lookups need mutable access to a fresh buffer).
    let entries: Vec<(EntryMode, String, ObjectId)> = tree
        .decode()?
        .entries
        .iter()
        .map(|e| {
            (
                e.mode,
                e.filename.to_string(),
                e.oid.to_owned(),
            )
        })
        .collect();

    for (mode, filename, oid) in entries {
        let name = format!("{prefix}{filename}");

        if mode.is_tree() {
            // A tree line is emitted when: -d (trees only), or we're not
            // recursing (so trees are the leaves shown), or -t while recursing.
            let emit = opts.dirs_only || !opts.recurse || opts.show_trees;
            if emit && path_selects(&name, opts) {
                write_entry(repo, out, mode, &oid, &name, opts)?;
            }
            if should_descend(&name, opts) {
                let child = repo.find_object(oid)?.peel_to_tree()?;
                walk(repo, child, &format!("{name}/"), opts, out)?;
            }
        } else if !opts.dirs_only && path_selects(&name, opts) {
            write_entry(repo, out, mode, &oid, &name, opts)?;
        }
    }
    Ok(())
}

/// Whether `name` is selected by the path filters (empty filters select all).
///
/// A filter `p` selects `name` when it names the entry exactly (`name == p`) or
/// when the entry lives inside the directory `p` (`name` starts with `p/`).
fn path_selects(name: &str, opts: &Opts) -> bool {
    opts.paths.is_empty()
        || opts
            .paths
            .iter()
            .any(|p| name == p.as_str() || name.starts_with(&format!("{p}/")))
}

/// Whether the sub-tree `name` must be descended into.
///
/// Always when `-r` is set; otherwise only when a path filter points strictly
/// below this tree (so an exact `<dir>` filter shows the tree line without
/// recursing, while `<dir>/<file>` descends to reach the file).
fn should_descend(name: &str, opts: &Opts) -> bool {
    opts.recurse
        || opts
            .paths
            .iter()
            .any(|p| p.starts_with(&format!("{name}/")))
}

/// Render one entry line into `out`, honouring `--name-only`, `--object-only`,
/// `--long` and `-z`.
fn write_entry(
    repo: &gix::Repository,
    out: &mut String,
    mode: EntryMode,
    oid: &ObjectId,
    name: &str,
    opts: &Opts,
) -> Result<()> {
    let term = if opts.nul { '\0' } else { '\n' };

    if opts.name_only {
        out.push_str(name);
        out.push(term);
        return Ok(());
    }
    if opts.object_only {
        out.push_str(&object_id_str(repo, oid, opts)?);
        out.push(term);
        return Ok(());
    }

    let mode_str = git_mode(mode);
    let type_str = git_type(mode);
    let oid_str = object_id_str(repo, oid, opts)?;

    if opts.long {
        // Blobs (incl. symlinks) carry a size; trees and submodule commits show '-'.
        let size = if mode.is_blob_or_symlink() {
            repo.find_header(*oid)?.size().to_string()
        } else {
            "-".to_string()
        };
        out.push_str(&format!(
            "{mode_str} {type_str} {oid_str} {size:>7}\t{name}{term}"
        ));
    } else {
        out.push_str(&format!("{mode_str} {type_str} {oid_str}\t{name}{term}"));
    }
    Ok(())
}

/// The 6-digit octal mode exactly as stock `git ls-tree` prints it.
fn git_mode(mode: EntryMode) -> &'static str {
    match mode.kind() {
        EntryKind::Tree => "040000",
        EntryKind::Blob => "100644",
        EntryKind::BlobExecutable => "100755",
        EntryKind::Link => "120000",
        EntryKind::Commit => "160000",
    }
}

/// The object type column: `blob`, `tree`, or `commit` (as git names them).
fn git_type(mode: EntryMode) -> &'static str {
    match mode.kind() {
        EntryKind::Tree => "tree",
        EntryKind::Commit => "commit",
        EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => "blob",
    }
}

/// The object id column, full or abbreviated per `--abbrev`.
fn object_id_str(repo: &gix::Repository, oid: &ObjectId, opts: &Opts) -> Result<String> {
    Ok(match opts.abbrev {
        None => oid.to_hex().to_string(),
        Some(Some(n)) => oid.to_hex_with_len(n).to_string(),
        Some(None) => oid.attach(repo).shorten_or_id().to_string(),
    })
}
