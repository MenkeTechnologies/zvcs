//! `git diff-tree` — compare the content and mode of blobs found via two tree objects.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). The tree-vs-tree walk is
//! implemented here rather than through `gix::Repository::diff_tree_to_tree` because
//! that helper always recurses breadth-first and always descends, while `diff-tree`
//! needs git's depth-first emission order plus `-r`/`-t` control over which tree
//! entries are reported. The entry comparison used below is the same
//! `tree-entry-comparison` gitoxide implements in
//! `src/ported/gix-diff/src/tree/function.rs` (names compared with an implicit `/`
//! appended to trees).
//!
//! ### Covered (byte-identical stdout and exit code against stock git)
//!
//! * `diff-tree <tree-ish> <tree-ish> [<path>...]` — raw output, no commit-id line
//! * `diff-tree <commit> [<path>...]` — commit vs. its first parent, prefixed by the
//!   commit id; a root commit prints nothing unless `--root` is given, and a merge
//!   prints nothing unless `-m` is given
//! * `-r`, `-t` (implies `-r`), `--root`, `-m`
//! * `--raw` (the default), `--name-only`, `--name-status`, `-s`/`--no-patch`
//! * `-z`, `--abbrev=<n>` (clamped to 4..=hash length, like git)
//! * `--no-commit-id`, `--always`
//! * literal `<path>` filters (exact entry, directory prefix, or a tree that a filter
//!   points below), before or after `--`
//! * `-h` — git's usage text on stdout, exit 129; no arguments — the same text on
//!   stderr, exit 129
//!
//! ### Honest limitations (bailed on with a precise message, never silently ignored)
//!
//! * `-p`/`-u`/`--patch` and the `--stat`/`--numstat`/`--dirstat`/`--summary` family.
//!   Patch output abbreviates the `index` line to git's *auto* abbreviation length,
//!   which is derived from the repository's approximate object count (`core.abbrev`
//!   when set). The vendored crates expose no equivalent, so a patch here would
//!   differ from git in the `index` line rather than being byte-identical.
//! * bare `--abbrev` (no `=<n>`) and `--abbrev-commit`, for the same reason.
//! * `-c`/`--cc`/`--combined-all-paths` — combined merge diffs have no substrate in
//!   the vendored `gix-diff`.
//! * `--stdin`, `-v`, `--pretty`/`--format` — these need commit-message formatting,
//!   which belongs to the `log`/`show` machinery, not the tree diff.
//! * `--merge-base`, rename/copy detection (`-M`/`-C`/`-B`), pickaxe (`-S`/`-G`),
//!   `-R`, `-O`, `--find-copies-harder`.
//! * magic (`:(...)`) and glob pathspecs.

use anyhow::{bail, Result};
use std::cmp::Ordering;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::tree::EntryMode;

/// Stock git's `diff-tree` usage block, byte-for-byte (1755 bytes), including the
/// trailing blank line. Printed on `-h` (stdout) and when no `<tree-ish>` is given
/// (stderr); both exit 129.
const USAGE: &str = r#"usage: git diff-tree [--stdin] [-m] [-s] [-v] [--no-commit-id] [--pretty]
              [-t] [-r] [-c | --cc] [--combined-all-paths] [--root] [--merge-base]
              [<common-diff-options>] <tree-ish> [<tree-ish>] [<path>...]

  -r            diff recursively
  -c            show combined diff for merge commits
  --cc          show combined diff for merge commits removing uninteresting hunks
  --combined-all-paths
                show name of file in all parents for combined diffs
  --root        include the initial commit as diff against /dev/null

common diff options:
  -z            output diff-raw with lines terminated with NUL.
  -p            output patch format.
  -u            synonym for -p.
  --patch-with-raw
                output both a patch and the diff-raw format.
  --stat        show diffstat instead of patch.
  --numstat     show numeric diffstat instead of patch.
  --patch-with-stat
                output a patch and prepend its diffstat.
  --name-only   show only names of changed files.
  --name-status show names and status of changed files.
  --full-index  show full object name on index lines.
  --abbrev=<n>  abbreviate object names in diff-tree header and diff-raw.
  -R            swap input file pairs.
  -B            detect complete rewrites.
  -M            detect renames.
  -C            detect copies.
  --find-copies-harder
                try unchanged files as candidate for copy detection.
  -l<n>         limit rename attempts up to <n> paths.
  -O<file>      reorder diffs according to the <file>.
  -S<string>    find filepair whose only one side contains the string.
  --pickaxe-all
                show all files diff when -S is used and hit is found.
  -a  --text    treat all files as text.

"#;

/// The `S_IFMT` mask git uses to decide whether a pair is a *type* change (`T`) or a
/// plain modification (`M`); `100644` and `100755` share a type, `120000` and
/// `160000` do not.
const IFMT: u16 = 0o170000;

/// git's `MINIMUM_ABBREV`: `--abbrev=<n>` below this is raised to it.
const MINIMUM_ABBREV: usize = 4;

/// How the change list should be rendered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    /// `:<omode> <nmode> <ooid> <noid> <status>\t<path>` — git's default.
    Raw,
    NameOnly,
    NameStatus,
    /// `-s`/`--no-patch`: the commit-id line only.
    NoOutput,
}

/// Parsed command-line options for a single `diff-tree` invocation.
struct Opts {
    recurse: bool,       // -r (also implied by -t)
    show_trees: bool,    // -t: report tree entries themselves while recursing
    nul: bool,           // -z: NUL instead of TAB/LF
    root: bool,          // --root: show a parentless commit as a full creation
    merges: bool,        // -m: diff a merge against every parent
    no_commit_id: bool,  // --no-commit-id
    always: bool,        // --always: print the commit id even with no changes
    abbrev: usize,       // object-id width in the raw output
    format: Format,
    paths: Vec<BString>, // literal path filters (empty = whole tree)
}

/// One file-level change, in the form the raw/name output needs.
///
/// `None` on a side means the entry is absent there (an addition or a deletion).
#[derive(Clone, Copy)]
struct Side {
    mode: EntryMode,
    id: ObjectId,
}

struct Change {
    old: Option<Side>,
    new: Option<Side>,
    path: BString,
}

/// `git diff-tree` — see the module documentation for the covered surface.
pub fn diff_tree(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("diff-tree") => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        recurse: false,
        show_trees: false,
        nul: false,
        root: false,
        merges: false,
        no_commit_id: false,
        always: false,
        abbrev: 0, // filled in from the repository's hash length below
        format: Format::Raw,
        paths: Vec::new(),
    };

    // `-h` must work outside a repository, so it is answered before discovery.
    if args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "-h")
    {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let mut abbrev_request: Option<usize> = None;
    let mut revs: Vec<String> = Vec::new();
    let mut raw_paths: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    let repo = gix::discover(".")?;
    let hash = repo.object_hash();

    for a in args {
        if after_dashdash {
            raw_paths.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => after_dashdash = true,
            // Normally answered before discovery; kept so `-h` never reaches the
            // unsupported-flag arm.
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-r" => opts.recurse = true,
            "-t" => {
                opts.recurse = true; // -t implies -r
                opts.show_trees = true;
            }
            "-z" => opts.nul = true,
            "--root" => opts.root = true,
            "-m" => opts.merges = true,
            "--no-commit-id" => opts.no_commit_id = true,
            "--always" => opts.always = true,
            "--raw" => opts.format = Format::Raw,
            "--name-only" => opts.format = Format::NameOnly,
            "--name-status" => opts.format = Format::NameStatus,
            "-s" | "--no-patch" => opts.format = Format::NoOutput,
            s if s.starts_with("--abbrev=") => {
                let v = &s["--abbrev=".len()..];
                let n: usize = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid --abbrev value {v:?}"))?;
                abbrev_request = Some(n.clamp(MINIMUM_ABBREV, hash.len_in_hex()));
            }
            s if s.starts_with('-') => bail!(
                "unsupported flag {s:?} (ported: -r, -t, -z, -m, -s/--no-patch, --root, \
                 --raw, --name-only, --name-status, --no-commit-id, --always, --abbrev=<n>, -h)"
            ),
            // The first positional is always the <tree-ish>; a second one is a
            // <tree-ish> only when it resolves as a revision, as git decides it.
            s if revs.is_empty() && raw_paths.is_empty() => revs.push(s.to_string()),
            s if revs.len() == 1 && raw_paths.is_empty() && repo.rev_parse_single(s).is_ok() => {
                revs.push(s.to_string());
            }
            s => raw_paths.push(s.to_string()),
        }
    }

    opts.abbrev = abbrev_request.unwrap_or_else(|| hash.len_in_hex());

    for p in &raw_paths {
        if p.starts_with(':') || p.bytes().any(|b| matches!(b, b'*' | b'?' | b'[')) {
            bail!("magic/glob pathspecs are not supported, got {p:?}");
        }
        opts.paths.push(BString::from(p.trim_end_matches('/').as_bytes()));
    }

    if revs.is_empty() {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let mut out: Vec<u8> = Vec::new();
    let code = if revs.len() == 2 {
        // Two tree-ishes: a plain tree-vs-tree diff with no commit-id line.
        let Some(old) = resolve_tree(&repo, &revs[0])? else {
            return Ok(ExitCode::from(128));
        };
        let Some(new) = resolve_tree(&repo, &revs[1])? else {
            return Ok(ExitCode::from(128));
        };
        let changes = collect(&repo, Some(old), Some(new), &opts)?;
        render_all(&mut out, &changes, &opts);
        ExitCode::SUCCESS
    } else {
        single_commit(&repo, &revs[0], &opts, &mut out)?
    };

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(code)
}

/// Resolve a `<tree-ish>` to the id of the tree it names.
///
/// `Ok(None)` means the spec could not be resolved; git reports that as a fatal
/// error and exits 128, so the caller propagates that code.
fn resolve_tree(repo: &gix::Repository, spec: &str) -> Result<Option<ObjectId>> {
    let Ok(id) = repo.rev_parse_single(spec) else {
        eprintln!("fatal: ambiguous argument '{spec}': unknown revision");
        return Ok(None);
    };
    let Ok(object) = id.object() else {
        eprintln!("fatal: bad object '{spec}'");
        return Ok(None);
    };
    match object.peel_to_tree() {
        Ok(tree) => Ok(Some(tree.id)),
        Err(_) => {
            eprintln!("fatal: not a tree object: '{spec}'");
            Ok(None)
        }
    }
}

/// The single-`<commit>` form: diff the commit against its parent(s), each diff
/// prefixed by the commit id unless suppressed.
fn single_commit(
    repo: &gix::Repository,
    spec: &str,
    opts: &Opts,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let Ok(id) = repo.rev_parse_single(spec) else {
        eprintln!("fatal: ambiguous argument '{spec}': unknown revision");
        return Ok(ExitCode::from(128));
    };
    let object = id.object()?;
    let (found_id, found_kind) = (object.id, object.kind);
    let Ok(commit) = object.peel_to_commit() else {
        // git treats this as non-fatal: it complains and exits 0.
        eprintln!("error: object {found_id} is a {found_kind}, not a commit");
        return Ok(ExitCode::SUCCESS);
    };

    let commit_id = commit.id;
    let new_tree = commit.tree_id()?.detach();
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();

    // Which "before" trees to diff against: `None` stands for the empty tree.
    let befores: Vec<Option<ObjectId>> = if parents.is_empty() {
        if opts.root {
            vec![None]
        } else {
            return Ok(ExitCode::SUCCESS);
        }
    } else if parents.len() > 1 && !opts.merges {
        // A merge is silently skipped unless -m asks for per-parent diffs.
        return Ok(ExitCode::SUCCESS);
    } else if opts.merges {
        let mut trees = Vec::with_capacity(parents.len());
        for p in &parents {
            trees.push(Some(tree_of(repo, *p)?));
        }
        trees
    } else {
        vec![Some(tree_of(repo, parents[0])?)]
    };

    let term = if opts.nul { b'\0' } else { b'\n' };
    for before in befores {
        let changes = collect(repo, before, Some(new_tree), opts)?;
        if opts.always || (!opts.no_commit_id && !changes.is_empty()) {
            out.extend_from_slice(commit_id.to_hex().to_string().as_bytes());
            out.push(term);
        }
        render_all(out, &changes, opts);
    }
    Ok(ExitCode::SUCCESS)
}

/// The tree a commit points at.
fn tree_of(repo: &gix::Repository, commit: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_object(commit)?.peel_to_tree()?.id)
}

/// A tree entry, materialised so the borrow on the tree's buffer ends before we
/// recurse into child trees.
struct Entry {
    mode: EntryMode,
    name: BString,
    id: ObjectId,
}

/// Read the entries of `id` in stored (git-sorted) order; `None` yields no entries,
/// which is how the empty tree is represented throughout this module.
fn read_entries(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<Entry>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let tree = repo.find_tree(id)?;
    Ok(tree
        .decode()?
        .entries
        .iter()
        .map(|e| Entry {
            mode: e.mode,
            name: BString::from(e.filename.to_vec()),
            id: e.oid.to_owned(),
        })
        .collect())
}

/// Collect every change turning `old` into `new`, in git's emission order.
fn collect(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    opts: &Opts,
) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    walk(repo, old, new, BStr::new(""), opts, &mut out)?;
    Ok(out)
}

/// git's `tree-entry-comparison`: names compare byte-wise, with an implicit `/`
/// appended to tree entries. Two entries with the same name but different
/// "treeness" therefore never compare `Equal`.
fn entry_cmp(a: &Entry, b: &Entry) -> Ordering {
    let common = a.name.len().min(b.name.len());
    match a.name[..common].cmp(&b.name[..common]) {
        Ordering::Equal => {
            let ac = a.name.get(common).copied().or(a.mode.is_tree().then_some(b'/'));
            let bc = b.name.get(common).copied().or(b.mode.is_tree().then_some(b'/'));
            ac.cmp(&bc)
        }
        other => other,
    }
}

/// Depth-first merge-walk of two trees rooted at `prefix`, appending changes to `out`.
fn walk(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    prefix: &BStr,
    opts: &Opts,
    out: &mut Vec<Change>,
) -> Result<()> {
    let lhs = read_entries(repo, old)?;
    let rhs = read_entries(repo, new)?;
    let (mut i, mut j) = (0usize, 0usize);

    while i < lhs.len() || j < rhs.len() {
        let order = match (lhs.get(i), rhs.get(j)) {
            (Some(a), Some(b)) => entry_cmp(a, b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => unreachable!("loop condition guarantees one side has an entry"),
        };
        match order {
            Ordering::Equal => {
                let (a, b) = (&lhs[i], &rhs[j]);
                i += 1;
                j += 1;
                if a.mode == b.mode && a.id == b.id {
                    continue;
                }
                let path = join(prefix, a.name.as_bstr());
                // `Equal` implies both sides are trees or neither is.
                if a.mode.is_tree() {
                    emit_tree(out, opts, &path, Some(side(a)), Some(side(b)));
                    if opts.recurse && descend(&path, opts) {
                        walk(repo, Some(a.id), Some(b.id), path.as_bstr(), opts, out)?;
                    }
                } else if selects(&path, false, opts) {
                    out.push(Change {
                        old: Some(side(a)),
                        new: Some(side(b)),
                        path,
                    });
                }
            }
            Ordering::Less => {
                let a = &lhs[i];
                i += 1;
                let path = join(prefix, a.name.as_bstr());
                if a.mode.is_tree() {
                    emit_tree(out, opts, &path, Some(side(a)), None);
                    if opts.recurse && descend(&path, opts) {
                        walk(repo, Some(a.id), None, path.as_bstr(), opts, out)?;
                    }
                } else if selects(&path, false, opts) {
                    out.push(Change {
                        old: Some(side(a)),
                        new: None,
                        path,
                    });
                }
            }
            Ordering::Greater => {
                let b = &rhs[j];
                j += 1;
                let path = join(prefix, b.name.as_bstr());
                if b.mode.is_tree() {
                    emit_tree(out, opts, &path, None, Some(side(b)));
                    if opts.recurse && descend(&path, opts) {
                        walk(repo, None, Some(b.id), path.as_bstr(), opts, out)?;
                    }
                } else if selects(&path, false, opts) {
                    out.push(Change {
                        old: None,
                        new: Some(side(b)),
                        path,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Record the line for a changed tree entry itself.
///
/// git reports the tree when it is the leaf of the walk (no `-r`) or when `-t` asks
/// for tree entries alongside their recursed contents; with plain `-r` only the
/// contents are reported.
fn emit_tree(out: &mut Vec<Change>, opts: &Opts, path: &BString, old: Option<Side>, new: Option<Side>) {
    if (!opts.recurse || opts.show_trees) && selects(path, true, opts) {
        out.push(Change {
            old,
            new,
            path: path.clone(),
        });
    }
}

fn side(e: &Entry) -> Side {
    Side {
        mode: e.mode,
        id: e.id,
    }
}

fn join(prefix: &BStr, name: &BStr) -> BString {
    let mut p = BString::from(prefix.to_vec());
    if !p.is_empty() {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// `true` if `path` starts with `pat` followed by a `/`.
fn under(path: &[u8], pat: &[u8]) -> bool {
    path.len() > pat.len() && path.starts_with(pat) && path[pat.len()] == b'/'
}

/// Whether an entry is reported under the active path filters.
///
/// A filter selects the entry when it names it exactly, when the entry lives inside
/// the filtered directory, or — for a tree — when the filter points somewhere below
/// the tree (`-- d1/sub` still reports the top-level `d1` without `-r`).
fn selects(path: &BString, is_tree: bool, opts: &Opts) -> bool {
    opts.paths.is_empty()
        || opts.paths.iter().any(|p| {
            path == p || under(path, p) || (is_tree && under(p, path))
        })
}

/// Whether the sub-tree at `path` can contain a filtered entry and so must be entered.
fn descend(path: &BString, opts: &Opts) -> bool {
    opts.paths.is_empty()
        || opts
            .paths
            .iter()
            .any(|p| path == p || under(path, p) || under(p, path))
}

fn render_all(out: &mut Vec<u8>, changes: &[Change], opts: &Opts) {
    for c in changes {
        render(out, c, opts);
    }
}

/// The status letter git prints for a change.
fn status(c: &Change) -> u8 {
    match (c.old, c.new) {
        (None, _) => b'A',
        (_, None) => b'D',
        (Some(o), Some(n)) => {
            if o.mode.value() & IFMT != n.mode.value() & IFMT {
                b'T'
            } else {
                b'M'
            }
        }
    }
}

fn render(out: &mut Vec<u8>, c: &Change, opts: &Opts) {
    let sep = if opts.nul { b'\0' } else { b'\t' };
    let term = if opts.nul { b'\0' } else { b'\n' };

    match opts.format {
        Format::NoOutput => {}
        Format::NameOnly => {
            out.extend_from_slice(&c.path);
            out.push(term);
        }
        Format::NameStatus => {
            out.push(status(c));
            out.push(sep);
            out.extend_from_slice(&c.path);
            out.push(term);
        }
        Format::Raw => {
            // ":<omode> <nmode> <ooid> <noid> <status>" then the separator and path.
            // Absent sides render as an all-zero mode and an all-zero object id.
            let zeros = "0".repeat(opts.abbrev);
            let (omode, ooid) = match c.old {
                Some(s) => (s.mode.value(), s.id.to_hex_with_len(opts.abbrev).to_string()),
                None => (0, zeros.clone()),
            };
            let (nmode, noid) = match c.new {
                Some(s) => (s.mode.value(), s.id.to_hex_with_len(opts.abbrev).to_string()),
                None => (0, zeros),
            };
            out.extend_from_slice(format!(":{omode:06o} {nmode:06o} {ooid} {noid} ").as_bytes());
            out.push(status(c));
            out.push(sep);
            out.extend_from_slice(&c.path);
            out.push(term);
        }
    }
}
