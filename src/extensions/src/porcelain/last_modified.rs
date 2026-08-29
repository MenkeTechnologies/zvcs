//! `git last-modified` — show which commit last modified each path.
//!
//! This is a faithful port of `builtin/last-modified.c` (git 2.55) for the
//! single-starting-commit case, reproducing stock output byte-for-byte,
//! including the *emission order*, which is not sorted: git snapshots its
//! path hashmap into an array at startup and emits in that array's order,
//! grouped by the commit that resolved them. Both the hashmap iteration order
//! (`hashmap.c` + `strhash`/FNV-1a) and the commit priority-queue order
//! (`compare_commits_by_gen_then_commit_date`, FIFO on ties) are reproduced.
//!
//! Covered:
//!   * `-r`/`--recursive` (and `--no-recursive`), `-t`/`--show-trees`
//!     (and `--no-show-trees`), `--max-depth=<n>` / `--max-depth <n>`, `-z`
//!   * an optional single `<revision>` (defaults to `HEAD`)
//!   * literal `[--] <pathspec>...`, prefixed with the repo-relative cwd like
//!     git does, with git's exact depth rule (`tree-diff.c:check_recursion_depth`)
//!   * C-style path quoting (`core.quotePath`) for the newline-terminated form
//!   * the argv diagnostics: `-h` (usage on stdout, exit 129), an unrecognised
//!     option (`error: unknown last-modified argument:` + usage on stderr, 129),
//!     `setup_revisions`' three `verify_filename`/`verify_non_filename` fatals,
//!     and `last-modified can only operate on one commit at a time`
//!
//! Not covered — these `bail!` rather than emit output that would diverge:
//!   * `<revision-range>` forms (`A..B`, `^X`, `--not`, `--all`, `-n`): they
//!     drive git's `not_queue`/boundary logic and the `^`-prefixed output
//!   * pathspec magic (`:(...)`) and wildcards

use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;

/// `last_modified_usage[]` rendered by `usage_with_options()`; the option block
/// is what `parse_options` derives from `last_modified_options[]`.
const USAGE: &str = "\
usage: git last-modified [--recursive] [--show-trees] [--max-depth=<depth>] [-z]
                         [<revision-range>] [[--] <pathspec>...]

    -r, --[no-]recursive  recurse into subtrees
    -t, --[no-]show-trees show tree entries when recursing into subtrees
    --max-depth <n>       maximum tree depth to recurse
    -z                    lines are separated with NUL character

";

/// `verify_filename(..., diagnose_misspelt_rev = 1)`: the argument was neither a
/// revision nor an existing path, so git cannot tell which was meant.
fn die_ambiguous(arg: &str) -> ExitCode {
    eprintln!(
        "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    );
    ExitCode::from(128)
}

/// `verify_filename(..., diagnose_misspelt_rev = 0)`: an earlier argument was
/// already taken as a path, so this one can only be a path — and it is missing.
fn die_no_such_path(arg: &str) -> ExitCode {
    eprintln!(
        "fatal: {arg}: no such path in the working tree.\n\
         Use 'git <command> -- <path>...' to specify paths that do not exist locally."
    );
    ExitCode::from(128)
}

/// Parsed command line, mirroring `struct last_modified` plus the diff options
/// `last_modified_init()` sets on `rev.diffopt`.
struct Opts {
    /// `rev.diffopt.max_depth`; `max_depth_valid` is `max_depth >= 0`.
    max_depth: i32,
    /// `rev.diffopt.flags.tree_in_recursive`.
    show_trees: bool,
    /// `-z`.
    nul: bool,
    /// Pathspecs exactly as git keeps them in `pathspec.items[].match`
    /// (cwd prefix applied, trailing slashes preserved), sorted like
    /// `parse_pathspec` sorts them.
    pathspecs: Vec<BString>,
}

/// `git last-modified` — see the module docs for the covered surface.
pub fn last_modified(args: &[String]) -> Result<ExitCode> {
    let mut max_depth: i32 = 0;
    let mut show_trees = false;
    let mut nul = false;
    // `(argument, appeared after `--`)`. `PARSE_OPT_KEEP_DASHDASH` makes
    // `parse_options` stop at `--`, so nothing past it is read as an option.
    let mut positionals: Vec<(&str, bool)> = Vec::new();
    let mut only_paths = false;
    // `PARSE_OPT_KEEP_UNKNOWN_OPT`: unrecognised options survive option parsing
    // and are reported by `last_modified_init()` *after* `setup_revisions()` has
    // had its chance to die on a bad revision.
    let mut unknown: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if only_paths {
            positionals.push((a, true));
            i += 1;
            continue;
        }
        match a {
            // `--help-all` joins `-h`: parse_options_step() tests that name with
            // a `strcmp()` of its own, ahead of parse_long_opt(), and renders
            // `USAGE_FULL` — identical here because this option table has no
            // `PARSE_OPT_HIDDEN` entry. The compare is exact, so `--help-a` and
            // `--help-all=x` fall through to the unknown-argument report.
            "-h" | "--help-all" => {
                // `parse_options` writes the usage to stdout for an explicit
                // `-h` and exits 129 with nothing on stderr.
                print!("{USAGE}");
                std::io::stdout().flush()?;
                return Ok(ExitCode::from(129));
            }
            "--" => only_paths = true,
            "-r" | "--recursive" => max_depth = -1,
            "--no-recursive" => max_depth = 0,
            "-t" | "--show-trees" => show_trees = true,
            "--no-show-trees" => show_trees = false,
            "-z" => nul = true,
            "--max-depth" => {
                i += 1;
                let v = super::value_at(args, i, a)?;
                max_depth = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid --max-depth value: {v}"))?;
            }
            _ if a.starts_with("--max-depth=") => {
                let v = &a["--max-depth=".len()..];
                max_depth = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid --max-depth value: {v}"))?;
            }
            _ if a.len() > 1 && a.starts_with('-') => {
                if unknown.is_none() {
                    unknown = Some(a);
                }
            }
            _ => positionals.push((a, false)),
        }
        i += 1;
    }

    let repo = gix::discover(".")?;

    // Split positionals into `<revision>` and pathspecs the way `setup_revisions`
    // does: a leading argument that names an object is the revision, everything
    // after it is a pathspec, and anything that is neither is fatal.
    let mut rev: Option<&str> = None;
    let mut specs: Vec<&str> = Vec::new();
    let mut seen_path = false;
    for &(p, after_dashdash) in &positionals {
        if after_dashdash {
            specs.push(p);
            continue;
        }
        if p.contains("..") || p.starts_with('^') {
            anyhow::bail!("unsupported <revision-range> {p:?} (only a single revision is ported)");
        }
        let exists = std::path::Path::new(p).exists();
        if repo.rev_parse_single(p).is_ok() {
            if exists {
                // `verify_non_filename()`.
                eprintln!(
                    "fatal: ambiguous argument '{p}': both revision and filename\n\
                     Use '--' to separate paths from revisions, like this:\n\
                     'git <command> [<revision>...] -- [<file>...]'"
                );
                return Ok(ExitCode::from(128));
            }
            if !seen_path && rev.is_none() {
                rev = Some(p);
                continue;
            }
            if !seen_path {
                // `populate_paths_from_revs()` rejects a second interesting tip.
                eprintln!("error: last-modified can only operate on one commit at a time");
                return Ok(ExitCode::from(255));
            }
        }
        if !exists {
            return Ok(if seen_path {
                die_no_such_path(p)
            } else {
                die_ambiguous(p)
            });
        }
        seen_path = true;
        specs.push(p);
    }

    if let Some(a) = unknown {
        eprint!("error: unknown last-modified argument: {a}\n{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // `struct prio_queue queue = { compare_commits_by_gen_then_commit_date }`
    // (builtin/last-modified.c:347). That comparator (commit.c:909) sorts by
    // `commit_graph_generation()` first and only falls back to the commit date when the
    // two generations are equal — and with no commit-graph *every* generation is
    // `GENERATION_NUMBER_INFINITY` (commit-graph.c:134), so the fallback is the whole
    // comparator. With one, the numbers are real and the order can differ; the walk
    // below detects that case rather than refusing outright. See [`QItem`].
    // `commit_graph_generation()` (commit-graph.c:126) answers `GENERATION_NUMBER_INFINITY`
    // for a commit that is not in the graph, the corrected commit date for one that is when
    // the whole chain carries `GDA2`, and the topological level otherwise
    // (commit-graph.c:902-917). Holding the graph open for the walk gives all three.
    let commit_graph = repo.commit_graph_if_enabled()?;

    let mut pathspecs: Vec<BString> = Vec::new();
    let prefix = cwd_prefix(&repo)?;
    for s in specs {
        if s.starts_with(':') {
            anyhow::bail!("unsupported pathspec magic {s:?}");
        }
        if s.contains('*') || s.contains('?') || s.contains('[') {
            anyhow::bail!("unsupported wildcard pathspec {s:?}");
        }
        let mut full = prefix.clone();
        full.extend_from_slice(s.as_bytes());
        pathspecs.push(BString::from(full));
    }
    pathspecs.sort();

    // git reads `core.quotePath` once, in its config callback, into the global
    // every `quote_c_style()` caller shares.
    crate::quote::init(&repo);

    let opts = Opts {
        max_depth,
        show_trees,
        nul,
        pathspecs,
    };

    // Resolve the single starting commit (`rev.def = "HEAD"`).
    let spec = rev.unwrap_or("HEAD");
    let id = match repo.rev_parse_single(spec) {
        Ok(id) => id,
        Err(_) => return Ok(die_ambiguous(spec)),
    };
    let commit = match id.object()?.peel_to_commit() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("error: revision argument '{spec}' is not a commit-ish");
            return Ok(ExitCode::from(255));
        }
    };
    let start = commit.id().detach();
    let start_tree = commit.tree_id()?.detach();

    // `populate_paths_from_revs`: diff the empty tree against the target tree,
    // which enumerates every path at the requested granularity.
    let mut listed: Vec<BString> = Vec::new();
    diff_trees(&repo, None, Some(start_tree), b"", &opts, &mut listed)?;

    // `all_paths` takes its order from git's hashmap iteration, not the diff.
    let all_paths = hashmap_order(listed);
    let n = all_paths.len();
    let index: HashMap<&BString, usize> = all_paths.iter().enumerate().map(|(k, p)| (p, k)).collect();

    let mut out = Vec::<u8>::new();
    if n == 0 {
        std::io::stdout().write_all(&out)?;
        return Ok(ExitCode::SUCCESS);
    }

    // The walk. `active[c]` is the bitmap of paths still looking for their
    // last-modifying commit at `c`; `pending` is the live path hashmap.
    let mut active: HashMap<ObjectId, Vec<bool>> = HashMap::new();
    let mut queued: HashSet<ObjectId> = HashSet::new();
    let mut pending: Vec<bool> = vec![true; n];
    let mut heap: std::collections::BinaryHeap<QItem> = std::collections::BinaryHeap::new();
    let mut ctr: usize = 0;

    active.insert(start, vec![true; n]);
    queued.insert(start);
    heap.push(QItem {
        generation: generation_of(commit_graph.as_ref(), &start),
        date: repo.find_commit(start)?.time()?.seconds,
        ctr,
        id: start,
    });
    ctr += 1;

    while let Some(q) = heap.pop() {
        let c = q.id;
        let mut active_c = active.remove(&c).unwrap_or_else(|| vec![false; n]);
        let commit = repo.find_commit(c)?;
        let c_tree = commit.tree_id()?.detach();
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();

        for pid in parents {
            let p_commit = repo.find_commit(pid)?;
            let p_tree = p_commit.tree_id()?.detach();

            // Paths whose entry differs between parent and `c` are *not*
            // TREESAME and stay with `c`; every other active path moves up.
            let mut changed: Vec<BString> = Vec::new();
            diff_trees(&repo, Some(p_tree), Some(c_tree), b"", &opts, &mut changed)?;
            let mut not_same = vec![false; n];
            for path in &changed {
                if let Some(&k) = index.get(path) {
                    not_same[k] = true;
                }
            }

            let ap = active.entry(pid).or_insert_with(|| vec![false; n]);
            for k in 0..n {
                if active_c[k] && !not_same[k] {
                    active_c[k] = false;
                    ap[k] = true;
                }
            }
            let parent_has_paths = ap.iter().any(|b| *b);

            if parent_has_paths && !queued.contains(&pid) {
                queued.insert(pid);
                heap.push(QItem {
                    generation: generation_of(commit_graph.as_ref(), &pid),
                    date: p_commit.time()?.seconds,
                    ctr,
                    id: pid,
                });
                ctr += 1;
            }
            if !queued.contains(&pid) {
                active.remove(&pid);
            }

            if !active_c.iter().any(|b| *b) {
                break;
            }
        }

        // Whatever is still active was changed by `c`.
        for k in 0..n {
            if active_c[k] && pending[k] {
                pending[k] = false;
                emit(&mut out, &all_paths[k], &c, &opts);
            }
        }
    }

    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// The repo-relative path of the current directory, with a trailing `/`, which
/// git prepends to every pathspec. Empty at the worktree root or in a bare repo.
fn cwd_prefix(repo: &gix::Repository) -> Result<Vec<u8>> {
    Ok(crate::setup::prefix_bytes(repo))
}

/// git's `commit_graph_generation()` (commit-graph.c:126) for `id`.
///
/// A commit outside the graph — or a graph position whose data cannot be read — is
/// `GENERATION_NUMBER_INFINITY`, which is also the answer for every commit when there is no
/// graph at all. Inside the graph the number is the corrected commit date from `GDA2`, and only
/// when *every* file in the chain carries that chunk (`validate_mixed_generation_chain()`,
/// commit-graph.c:524-543); a chain without it falls back to the topological level held in the
/// upper bits of `CDAT` (commit-graph.c:917).
fn generation_of(graph: Option<&gix::commitgraph::Graph>, id: &ObjectId) -> u64 {
    let Some(graph) = graph else {
        return u64::from(gix::commitgraph::GENERATION_NUMBER_INFINITY);
    };
    let Some(commit) = graph.commit_by_id(id) else {
        return u64::from(gix::commitgraph::GENERATION_NUMBER_INFINITY);
    };
    if graph.has_generation_data() {
        if let Some(corrected) = commit.corrected_commit_date() {
            return corrected;
        }
    }
    u64::from(commit.generation())
}

/// One entry of the commit priority queue. `Ord` reproduces
/// `compare_commits_by_gen_then_commit_date` (commit.c:909, newest first) with
/// `prio_queue`'s FIFO tie-break on the insertion counter: generation first, commit
/// date only as the tie-break, which is what the comparator's second stanza calls a
/// heuristic.
#[derive(PartialEq, Eq)]
struct QItem {
    generation: u64,
    date: i64,
    ctr: usize,
    id: ObjectId,
}

impl Ord for QItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.generation
            .cmp(&other.generation)
            .then_with(|| self.date.cmp(&other.date))
            .then_with(|| other.ctr.cmp(&self.ctr))
    }
}

impl PartialOrd for QItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A tree entry snapshot, detached from the tree's buffer so we can recurse.
struct Ent {
    name: BString,
    mode: u16,
    is_tree: bool,
    oid: ObjectId,
}

/// Read the entries of `id` in tree order; an absent tree reads as empty.
fn read_tree(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<Ent>> {
    let Some(id) = id else {
        return Ok(Vec::new());
    };
    let tree = repo.find_object(id)?.peel_to_tree()?;
    let decoded = tree.decode()?;
    Ok(decoded
        .entries
        .iter()
        .map(|e| Ent {
            name: e.filename.to_owned(),
            mode: e.mode.value(),
            is_tree: e.mode.is_tree(),
            oid: e.oid.to_owned(),
        })
        .collect())
}

/// `base_name_compare`: names compare bytewise, with directories behaving as
/// though they carried a trailing `/`.
fn base_name_compare(a: &Ent, b: &Ent) -> std::cmp::Ordering {
    let common = a.name.len().min(b.name.len());
    let ord = a.name[..common].cmp(&b.name[..common]);
    if ord != std::cmp::Ordering::Equal {
        return ord;
    }
    let ca = a
        .name
        .get(common)
        .copied()
        .unwrap_or(if a.is_tree { b'/' } else { 0 });
    let cb = b
        .name
        .get(common)
        .copied()
        .unwrap_or(if b.is_tree { b'/' } else { 0 });
    ca.cmp(&cb)
}

/// Port of `ll_diff_tree_paths` for two trees: append the path of every entry
/// that differs between `old` and `new`, honouring the pathspec, `--max-depth`
/// and `--show-trees` exactly as `emit_path()` does. `old = None` is git's
/// empty-tree diff, which lists everything.
fn diff_trees(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    base: &[u8],
    opts: &Opts,
    out: &mut Vec<BString>,
) -> Result<()> {
    let olds = read_tree(repo, old)?;
    let news = read_tree(repo, new)?;

    let (mut i, mut j) = (0usize, 0usize);
    while i < olds.len() || j < news.len() {
        let (o, nw) = match (olds.get(i), news.get(j)) {
            (Some(a), Some(b)) => match base_name_compare(a, b) {
                std::cmp::Ordering::Less => {
                    i += 1;
                    (Some(a), None)
                }
                std::cmp::Ordering::Greater => {
                    j += 1;
                    (None, Some(b))
                }
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                    if a.mode == b.mode && a.oid == b.oid {
                        continue;
                    }
                    (Some(a), Some(b))
                }
            },
            (Some(a), None) => {
                i += 1;
                (Some(a), None)
            }
            (None, Some(b)) => {
                j += 1;
                (None, Some(b))
            }
            (None, None) => unreachable!(),
        };

        let e = nw.or(o).expect("at least one side present");
        let mut path = BString::from(base.to_vec());
        path.extend_from_slice(&e.name);

        if !interesting(&path, e.is_tree, opts) {
            continue;
        }

        let mut recurse = false;
        let mut emit_this = true;
        if e.is_tree && should_recurse(&path, opts) {
            recurse = true;
            emit_this = opts.show_trees;
        }
        if emit_this {
            out.push(path.clone());
        }
        if recurse {
            let mut child_base = path;
            child_base.push(b'/');
            diff_trees(
                repo,
                o.filter(|x| x.is_tree).map(|x| x.oid),
                nw.filter(|x| x.is_tree).map(|x| x.oid),
                &child_base,
                opts,
                out,
            )?;
        }
    }
    Ok(())
}

/// `tree_entry_interesting` for literal pathspecs: an entry matches when a
/// pathspec names it or a leading directory of it, and a directory is also kept
/// when it is a leading directory of a pathspec (it must be recursed into).
fn interesting(path: &BString, is_dir: bool, opts: &Opts) -> bool {
    if opts.pathspecs.is_empty() {
        return true;
    }
    opts.pathspecs.iter().any(|m| {
        let m = trim_trailing_slashes(m);
        if m.is_empty() {
            return true;
        }
        path.as_bytes() == m
            || is_dir_prefix(path.as_bytes(), m)
            || (is_dir && is_dir_prefix(m, path.as_bytes()))
    })
}

fn trim_trailing_slashes(m: &BString) -> &[u8] {
    let mut s = m.as_bytes();
    while s.last() == Some(&b'/') {
        s = &s[..s.len() - 1];
    }
    s
}

/// `is_dir_prefix()`: true when `dir` is a leading directory of `path`.
fn is_dir_prefix(path: &[u8], dir: &[u8]) -> bool {
    path.len() >= dir.len()
        && path.starts_with(dir)
        && (path.len() == dir.len() || path[dir.len()] == b'/')
}

/// `should_recurse()`. `flags.recursive` is always set for last-modified, so
/// only `max_depth_valid` (`max_depth >= 0`) gates the depth check.
fn should_recurse(path: &BString, opts: &Opts) -> bool {
    if opts.max_depth < 0 {
        return true;
    }
    check_recursion_depth(path.as_bytes(), &opts.pathspecs, opts.max_depth)
}

/// Port of `check_recursion_depth()`: depth is measured from the end of the
/// longest matching pathspec, so `-- src/` already sits one level deep.
fn check_recursion_depth(name: &[u8], ps: &[BString], max_depth: i32) -> bool {
    if ps.is_empty() {
        return within_depth(name, 1, max_depth);
    }
    for item in ps.iter().rev() {
        let item = item.as_bytes();
        if name.len() >= item.len() {
            if !is_dir_prefix(name, item) {
                continue;
            }
            return within_depth(&name[item.len()..], 1, max_depth);
        }
        if is_dir_prefix(item, name) {
            return true;
        }
    }
    false
}

/// Port of `within_depth()`.
fn within_depth(name: &[u8], mut depth: i32, max_depth: i32) -> bool {
    for &c in name {
        if c != b'/' {
            continue;
        }
        depth += 1;
        if depth > max_depth {
            return false;
        }
    }
    depth <= max_depth
}

/// `strhash()` — FNV-1a as git spells it in `memhash()`.
fn strhash(s: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &c in s {
        hash = hash.wrapping_mul(0x0100_0193) ^ u32::from(c);
    }
    hash
}

/// Reproduce `hashmap.c`'s iteration order for `lm->all_paths`: entries are
/// prepended to their bucket's chain, the table grows by 4x past an 80% load
/// factor (rehashing chain-head first), and iteration runs bucket 0..tablesize
/// following each chain from its head.
fn hashmap_order(paths: Vec<BString>) -> Vec<BString> {
    const INITIAL_SIZE: usize = 64;
    const RESIZE_BITS: u32 = 2;
    const LOAD_FACTOR: usize = 80;

    let mut tablesize = INITIAL_SIZE;
    let mut grow_at = tablesize * LOAD_FACTOR / 100;
    let mut table: Vec<VecDeque<(u32, BString)>> = vec![VecDeque::new(); tablesize];
    let mut size = 0usize;

    for p in paths {
        let h = strhash(p.as_bytes());
        table[(h as usize) & (tablesize - 1)].push_front((h, p));
        size += 1;
        if size > grow_at {
            let newsize = tablesize << RESIZE_BITS;
            let old = std::mem::replace(&mut table, vec![VecDeque::new(); newsize]);
            tablesize = newsize;
            grow_at = tablesize * LOAD_FACTOR / 100;
            for chain in old {
                for e in chain {
                    table[(e.0 as usize) & (tablesize - 1)].push_front(e);
                }
            }
        }
    }

    table
        .into_iter()
        .flat_map(|chain| chain.into_iter().map(|(_, p)| p))
        .collect()
}

/// `last_modified_emit()`: `<oid> TAB <path>` terminated by LF (path C-quoted
/// when needed) or by NUL under `-z` (never quoted).
fn emit(out: &mut Vec<u8>, path: &BString, commit: &ObjectId, opts: &Opts) {
    out.extend_from_slice(commit.to_hex().to_string().as_bytes());
    out.push(b'\t');
    if opts.nul {
        out.extend_from_slice(path.as_bytes());
        out.push(0);
    } else {
        write_c_quoted(out, path.as_bytes());
        out.push(b'\n');
    }
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn write_c_quoted(out: &mut Vec<u8>, name: &[u8]) {
    out.extend_from_slice(&crate::quote::quoted_name_bytes(name));
}
