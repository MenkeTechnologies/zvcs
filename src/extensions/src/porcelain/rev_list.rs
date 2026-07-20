use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// The usage block stock git prints on a usage error, verbatim. git exits 129
/// for these, not 1, so the block travels with an explicit exit code rather
/// than through `anyhow`.
const USAGE: &str = "usage: git rev-list [<options>] <commit>... [--] [<path>...]\n\
\n\
  limiting output:\n\
    --max-count=<n>\n\
    --max-age=<epoch>\n\
    --min-age=<epoch>\n\
    --sparse\n\
    --no-merges\n\
    --min-parents=<n>\n\
    --no-min-parents\n\
    --max-parents=<n>\n\
    --no-max-parents\n\
    --remove-empty\n\
    --all\n\
    --branches\n\
    --tags\n\
    --remotes\n\
    --stdin\n\
    --exclude-hidden=[fetch|receive|uploadpack]\n\
    --quiet\n\
  ordering output:\n\
    --topo-order\n\
    --date-order\n\
    --reverse\n\
  formatting output:\n\
    --parents\n\
    --children\n\
    --objects | --objects-edge\n\
    --disk-usage[=human]\n\
    --unpacked\n\
    --header | --pretty\n\
    --[no-]object-names\n\
    --abbrev=<n> | --no-abbrev\n\
    --abbrev-commit\n\
    --left-right\n\
    --count\n\
    -z\n\
  special purpose:\n\
    --bisect\n\
    --bisect-vars\n\
    --bisect-all\n";

/// Print the usage block and return git's usage exit code.
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Print a `fatal:` line and return git's fatal exit code.
fn fatal(message: &str) -> ExitCode {
    eprintln!("fatal: {message}");
    ExitCode::from(128)
}

/// How commits are ordered before filtering and limiting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Commit date, newest first — git's default.
    Date,
    /// `--topo-order`: no parent before all its children, branches kept contiguous.
    Topo,
    /// `--date-order`: no parent before all its children, otherwise by date.
    DateTopo,
}

/// `git rev-list` — list commit ids reachable from the given revisions.
///
/// Supported invocation forms:
///   * `rev-list <rev>...`            — commits reachable from each `<rev>`
///   * `rev-list ^<rev>`              — exclude commits reachable from `<rev>`
///   * `rev-list <a>..<b>`            — reachable from `<b>` but not `<a>` (empty
///                                       side defaults to `HEAD`)
///   * `--all`                        — seed from every ref plus `HEAD`, at the
///                                       position the flag appears in the argv
///   * `--count`                      — print only the number of output lines
///   * `-n <n>` / `-n<n>` / `--max-count=<n>` — limit the number listed
///   * `--reverse`                    — reverse the output (limit applied first)
///   * `--first-parent`               — follow only the first parent of merges
///   * `--topo-order` / `--date-order` — topological orderings
///   * `--merges` / `--no-merges` / `--{min,max}-parents=<n>` — parent-count filter
///   * `--parents`                    — append each commit's parents to its line
///   * `--objects`                    — also list the trees and blobs reachable
///                                       from the listed commits
///
/// Genuinely unsupported forms are rejected: symmetric-difference ranges
/// (`<a>...<b>`) and pathspec filtering (`-- <path>`).
pub fn rev_list(args: &[String]) -> Result<ExitCode> {
    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => {
            return Ok(fatal(
                "not a git repository (or any of the parent directories): .git",
            ))
        }
    };

    let mut count_only = false;
    let mut reverse = false;
    let mut first_parent = false;
    let mut objects = false;
    let mut show_parents = false;
    let mut order = Order::Date;
    let mut min_parents: usize = 0;
    let mut max_parents: Option<usize> = None;
    let mut max_count: Option<usize> = None;
    let mut tips: Vec<ObjectId> = Vec::new();
    let mut hidden: Vec<ObjectId> = Vec::new();
    // Annotated tag objects encountered while peeling seeds. `--objects` lists
    // them, named by the tag's own name field, ahead of any tree.
    let mut pending_tags: Vec<(ObjectId, Vec<u8>)> = Vec::new();

    macro_rules! resolve {
        ($spec:expr) => {
            match resolve(&repo, $spec, &mut pending_tags) {
                Some(id) => id,
                None => {
                    return Ok(fatal(&format!(
                        "ambiguous argument '{}': unknown revision or path not in the working tree.",
                        $spec
                    )))
                }
            }
        };
    }

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--count" => count_only = true,
            "--reverse" => reverse = true,
            "--first-parent" => first_parent = true,
            "--objects" => objects = true,
            "--parents" => show_parents = true,
            "--topo-order" => order = Order::Topo,
            "--date-order" => order = Order::DateTopo,
            "--merges" => min_parents = 2,
            "--no-merges" => max_parents = Some(1),
            "--no-min-parents" => min_parents = 0,
            "--no-max-parents" => max_parents = None,
            "--all" => {
                // Seeded in place: git processes `--all` where it appears, and
                // with every commit sharing a timestamp the seed order is what
                // decides the output order.
                match seed_all(&repo, &mut tips, &mut pending_tags) {
                    Ok(()) => {}
                    Err(e) => return Ok(fatal(&e.to_string())),
                }
            }
            "-n" => {
                i += 1;
                let Some(n) = args.get(i) else {
                    return Ok(usage_error());
                };
                let Ok(n) = n.parse::<usize>() else {
                    return Ok(usage_error());
                };
                max_count = Some(n);
            }
            "--" => {
                if i + 1 < args.len() {
                    return Ok(fatal("pathspec filtering is not supported"));
                }
            }
            s if s.starts_with("--max-count=") => {
                let Ok(n) = s["--max-count=".len()..].parse::<usize>() else {
                    return Ok(usage_error());
                };
                max_count = Some(n);
            }
            s if s.starts_with("--min-parents=") => {
                let Ok(n) = s["--min-parents=".len()..].parse::<usize>() else {
                    return Ok(usage_error());
                };
                min_parents = n;
            }
            s if s.starts_with("--max-parents=") => {
                let Ok(n) = s["--max-parents=".len()..].parse::<usize>() else {
                    return Ok(usage_error());
                };
                max_parents = Some(n);
            }
            s if s.len() > 2 && s.starts_with("-n") && s[2..].bytes().all(|b| b.is_ascii_digit()) => {
                let Ok(n) = s[2..].parse::<usize>() else {
                    return Ok(usage_error());
                };
                max_count = Some(n);
            }
            // Every remaining flag is one git knows and this does not; a
            // revision never starts with `-`, so anything left is a usage error.
            s if s.starts_with('-') => return Ok(usage_error()),
            s if s.starts_with('^') => {
                hidden.push(resolve!(&s[1..]));
            }
            s if s.contains("...") => {
                return Ok(fatal(&format!(
                    "symmetric-difference range '{s}' is not supported"
                )));
            }
            s if s.contains("..") => {
                let (left, right) = s.split_once("..").expect("checked above");
                let left = if left.is_empty() { "HEAD" } else { left };
                let right = if right.is_empty() { "HEAD" } else { right };
                hidden.push(resolve!(left));
                tips.push(resolve!(right));
            }
            s => tips.push(resolve!(s)),
        }
        i += 1;
    }

    // git treats "nothing to walk from" as a usage error, not a fatal one —
    // except under `--objects`, which asks for an object listing and is content
    // to produce an empty one.
    if tips.is_empty() && !objects {
        return Ok(usage_error());
    }
    dedup_in_place(&mut tips);

    // 1. Full commit list in date order — the input every later stage refines.
    let mut commits: Vec<ObjectId> = Vec::new();
    let mut parents_of: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    if !tips.is_empty() {
        let mut platform = repo
            .rev_walk(tips)
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
        if first_parent {
            platform = platform.first_parent_only();
        }
        if !hidden.is_empty() {
            platform = platform.with_hidden(hidden.clone());
        }
        for info in platform.all()? {
            let info = info?;
            parents_of.insert(info.id, info.parent_ids.to_vec());
            commits.push(info.id);
        }
    }

    // 2. Reorder, 3. filter by parent count, 4. limit, 5. reverse — in that
    // order, because git sorts the whole list, then drops commits at output
    // time, and only counts what it actually emits against `--max-count`.
    if order != Order::Date {
        commits = topo_sort(&commits, &parents_of, order == Order::Topo);
    }
    commits.retain(|id| {
        let n = parents_of.get(id).map_or(0, Vec::len);
        n >= min_parents && max_parents.is_none_or(|max| n <= max)
    });
    if let Some(max) = max_count {
        commits.truncate(max);
    }
    if reverse {
        commits.reverse();
    }

    // 6. Render. Lines are bytes, not strings: tree entry names are raw bytes
    // and git writes them through unmodified.
    let mut lines: Vec<Vec<u8>> = Vec::with_capacity(commits.len());
    for id in &commits {
        let mut line = id.to_string().into_bytes();
        if show_parents {
            for parent in parents_of.get(id).into_iter().flatten() {
                line.push(b' ');
                line.extend_from_slice(parent.to_string().as_bytes());
            }
        }
        lines.push(line);
    }

    if objects {
        object_lines(
            &repo,
            &commits,
            &hidden,
            &pending_tags,
            first_parent,
            &mut lines,
        )?;
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if count_only {
        writeln!(out, "{}", lines.len())?;
    } else {
        for line in lines {
            out.write_all(&line)?;
            out.write_all(b"\n")?;
        }
    }
    out.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// Resolve `spec` to the commit it names, recording any annotated tag peeled
/// through on the way. `None` means the spec does not name a commit.
fn resolve(
    repo: &gix::Repository,
    spec: &str,
    tags: &mut Vec<(ObjectId, Vec<u8>)>,
) -> Option<ObjectId> {
    let id = repo.rev_parse_single(spec).ok()?.detach();
    peel_recording_tags(repo, id, tags)
}

/// Peel `id` down to a commit, pushing every tag object passed through onto
/// `tags` under its own name — which is what `--objects` reports for them.
fn peel_recording_tags(
    repo: &gix::Repository,
    id: ObjectId,
    tags: &mut Vec<(ObjectId, Vec<u8>)>,
) -> Option<ObjectId> {
    let mut id = id;
    loop {
        let object = repo.find_object(id).ok()?;
        let kind = object.kind;
        match kind {
            gix::object::Kind::Commit => return Some(id),
            gix::object::Kind::Tag => {
                let tag = object.into_tag();
                let tag_id = tag.id;
                let (name, target) = {
                    let decoded = tag.decode().ok()?;
                    (decoded.name.to_vec(), decoded.target())
                };
                tags.push((tag_id, name));
                id = target;
            }
            _ => return None,
        }
    }
}

/// Seed `tips` the way git's `--all` does: every ref under `refs/` in name
/// order, then `HEAD`.
fn seed_all(
    repo: &gix::Repository,
    tips: &mut Vec<ObjectId>,
    tags: &mut Vec<(ObjectId, Vec<u8>)>,
) -> Result<()> {
    for reference in repo.references()?.all()? {
        let reference = reference.map_err(|e| anyhow!("{e}"))?;
        let target = match reference.try_id() {
            Some(id) => id.detach(),
            // Symbolic: follow it, but then there is no tag object to record.
            None => match reference.into_fully_peeled_id() {
                Ok(id) => id.detach(),
                Err(_) => continue,
            },
        };
        if let Some(id) = peel_recording_tags(repo, target, tags) {
            tips.push(id);
        }
    }
    if let Ok(head) = repo.head_id() {
        if let Some(id) = peel_recording_tags(repo, head.detach(), tags) {
            tips.push(id);
        }
    }
    Ok(())
}

/// Drop repeats while keeping the first occurrence — git ignores a seed it has
/// already queued, so the earliest mention is the one that fixes the order.
fn dedup_in_place(ids: &mut Vec<ObjectId>) {
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
}

/// git's `sort_in_topological_order`: emit no commit before all of its children,
/// breaking ties LIFO for `--topo-order` (keeping a branch contiguous) and by
/// list position for `--date-order`.
fn topo_sort(
    commits: &[ObjectId],
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    lifo: bool,
) -> Vec<ObjectId> {
    // Every listed commit starts at 1, then gains one per listed child.
    let mut indegree: HashMap<ObjectId, usize> =
        commits.iter().map(|id| (*id, 1usize)).collect();
    for id in commits {
        for parent in parents_of.get(id).into_iter().flatten() {
            if let Some(n) = indegree.get_mut(parent) {
                if *n != 0 {
                    *n += 1;
                }
            }
        }
    }

    // The tips are the commits no listed commit reaches.
    let mut queue: Vec<ObjectId> = commits
        .iter()
        .filter(|id| indegree.get(*id) == Some(&1))
        .copied()
        .collect();
    // git reverses the seed queue so that popping a LIFO stack still yields the
    // tips in traversal order.
    if lifo {
        queue.reverse();
    }

    let mut out = Vec::with_capacity(commits.len());
    while !queue.is_empty() {
        let id = if lifo { queue.pop() } else { Some(queue.remove(0)) };
        let Some(id) = id else { break };
        for parent in parents_of.get(&id).into_iter().flatten() {
            if let Some(n) = indegree.get_mut(parent) {
                if *n == 0 {
                    continue;
                }
                *n -= 1;
                if *n == 1 {
                    queue.push(*parent);
                }
            }
        }
        indegree.insert(id, 0);
        out.push(id);
    }
    out
}

/// Append the `--objects` section: the tag objects seeded from refs, then each
/// listed commit's tree walked depth-first, globally de-duplicated.
///
/// Objects reachable from an excluded (`^rev`) commit are pre-marked as seen so
/// they never appear, which is how git keeps `a..b --objects` to b's new data.
fn object_lines(
    repo: &gix::Repository,
    commits: &[ObjectId],
    hidden: &[ObjectId],
    pending_tags: &[(ObjectId, Vec<u8>)],
    first_parent: bool,
    lines: &mut Vec<Vec<u8>>,
) -> Result<()> {
    let mut seen: HashSet<ObjectId> = HashSet::new();

    if !hidden.is_empty() {
        let mut platform = repo
            .rev_walk(hidden.to_vec())
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
        if first_parent {
            platform = platform.first_parent_only();
        }
        for info in platform.all()? {
            if let Some(tree) = commit_tree(repo, info?.id) {
                mark_tree_seen(repo, tree, &mut seen);
            }
        }
    }

    for (id, name) in pending_tags {
        if seen.insert(*id) {
            let mut line = id.to_string().into_bytes();
            line.push(b' ');
            line.extend_from_slice(name);
            lines.push(line);
        }
    }

    for id in commits {
        let Some(tree) = commit_tree(repo, *id) else {
            continue;
        };
        if seen.insert(tree) {
            let mut line = tree.to_string().into_bytes();
            line.push(b' ');
            lines.push(line);
            walk_tree(repo, tree, &[], &mut seen, lines);
        }
    }
    Ok(())
}

/// The tree a commit points at, or `None` if the object is missing or is not a
/// commit. Never panics: gix's `into_commit` would, and a panic reads as a crash.
fn commit_tree(repo: &gix::Repository, id: ObjectId) -> Option<ObjectId> {
    let object = repo.find_object(id).ok()?;
    if object.kind != gix::object::Kind::Commit {
        return None;
    }
    Some(object.into_commit().tree_id().ok()?.detach())
}

/// The entries of a tree object, or `None` if it is missing or not a tree.
fn tree_object(repo: &gix::Repository, id: ObjectId) -> Option<gix::Tree<'_>> {
    let object = repo.find_object(id).ok()?;
    if object.kind != gix::object::Kind::Tree {
        return None;
    }
    Some(object.into_tree())
}

/// Record `tree` and everything under it as already-emitted, without listing it.
fn mark_tree_seen(repo: &gix::Repository, tree: ObjectId, seen: &mut HashSet<ObjectId>) {
    if !seen.insert(tree) {
        return;
    }
    let Some(object) = tree_object(repo, tree) else {
        return;
    };
    for entry in object.iter() {
        let Ok(entry) = entry else { return };
        if entry.mode().is_commit() {
            continue;
        }
        let id = entry.object_id();
        if entry.mode().is_tree() {
            mark_tree_seen(repo, id, seen);
        } else {
            seen.insert(id);
        }
    }
}

/// Depth-first walk emitting `<oid> <path>` per entry, descending into a subtree
/// immediately after listing it — the order git's `process_tree` produces.
///
/// Gitlink entries are skipped: their commit lives in another repository.
fn walk_tree(
    repo: &gix::Repository,
    tree: ObjectId,
    base: &[u8],
    seen: &mut HashSet<ObjectId>,
    lines: &mut Vec<Vec<u8>>,
) {
    let Some(object) = tree_object(repo, tree) else {
        return;
    };
    for entry in object.iter() {
        let Ok(entry) = entry else { return };
        let mode = entry.mode();
        if mode.is_commit() {
            continue;
        }
        let id = entry.object_id();
        let mut path = Vec::with_capacity(base.len() + 1 + entry.filename().len());
        if !base.is_empty() {
            path.extend_from_slice(base);
            path.push(b'/');
        }
        path.extend_from_slice(entry.filename());
        if !seen.insert(id) {
            continue;
        }
        let mut line = id.to_string().into_bytes();
        line.push(b' ');
        line.extend_from_slice(&path);
        lines.push(line);
        if mode.is_tree() {
            walk_tree(repo, id, &path, seen, lines);
        }
    }
}
