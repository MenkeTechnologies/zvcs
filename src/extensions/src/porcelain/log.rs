use anyhow::{anyhow, bail, Result};
use std::collections::HashSet;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::objs::tree::EntryKind;

/// The terminal width git assumes for `--stat` when stdout is not a terminal.
const STAT_TERM_WIDTH: usize = 80;

/// `git log` — commit history reachable from a starting revision (default `HEAD`).
///
/// Ported invocation forms:
///   * `git log [<rev>...]`                      → history from `HEAD`, a revision, or the
///     union of several revisions
///   * `-- <pathspec>...`                        → path-limited traversal: show only commits
///     that touched a matching plain pathspec (magic pathspecs surfaced terse)
///   * `-n N` / `--max-count=N` / `-N` / `-nN`   → limit the number of commits shown
///   * `--all`                                   → start from every ref plus `HEAD`
///   * `--merges` / `--no-merges`                → keep only (or drop) multi-parent commits
///   * `--reverse`                               → emit the selected commits oldest-first
///   * `--date-order` / `--topo-order`           → git's two topological sort orders
///   * `--oneline`, `--pretty=`/`--format=` with
///     `oneline`, `short`, `medium`, and `format:`/`tformat:` strings (last flag wins;
///     an invalid value is rejected exactly as git's `get_commit_format` does)
///   * `--name-only`, `--name-status`, `--stat`  → per-commit diff against the first parent
///     (`--name-only`/`--name-status` are mutually exclusive and suppress `--stat`)
///   * `--graph`                                 → git's ASCII commit graph (see below)
///
/// Output separation follows git's `format:` (separator) versus `tformat:`
/// (terminator) distinction, which is why `--format=%s` and `--pretty=format:%s`
/// lay out differently; `--oneline`/`--pretty=oneline` are terminator formats.
///
/// Deviations, surfaced rather than faked:
///   * `--graph` renders commits with at most two parents. An octopus merge is
///     rejected instead of being drawn wrong.
///   * Rename detection is off, so a rename shows as a delete plus an add.
///   * `--stat` assumes an 80-column terminal and measures paths in `char`s.
///   * Pathspec limiting compares each commit to its first parent only, so merge
///     simplification (TREESAME across multiple parents) is not modelled.
///   * `-p`, date/author filters, and every flag not listed above are rejected
///     explicitly.
pub fn log(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let mut max_count: Option<usize> = None;
    let mut pretty = Pretty::Medium;
    let mut terminator = false;
    let mut abbrev_commit = false;
    let mut name_only = false;
    let mut name_status = false;
    let mut stat = false;
    let mut graph = false;
    let mut all = false;
    let mut reverse = false;
    let mut only_merges = false;
    let mut no_merges = false;
    let mut order = Order::Default;
    let mut revs: Vec<String> = Vec::new();
    let mut pathspecs: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            // Everything after `--` is a pathspec, even tokens that look like
            // flags — git stops option parsing at the separator.
            pathspecs.extend(args[i + 1..].iter().cloned());
            break;
        } else if a == "-n" || a == "--max-count" {
            i += 1;
            let v = args
                .get(i)
                .ok_or_else(|| anyhow!("option `{a}` requires a value"))?;
            match parse_max_count(v) {
                Ok(mc) => max_count = mc,
                Err(()) => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--max-count=") {
            match parse_max_count(v) {
                Ok(mc) => max_count = mc,
                Err(()) => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--oneline" {
            pretty = Pretty::Oneline;
            terminator = true;
            abbrev_commit = true;
        } else if let Some(v) = a.strip_prefix("--pretty=") {
            match get_commit_format(v)? {
                Some((p, t)) => {
                    pretty = p;
                    terminator = t;
                }
                None => {
                    eprintln!("fatal: invalid --pretty format: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--format=") {
            // `--format=<s>` is git's alias for `--pretty=<s>` (same parser, not a
            // blind `tformat:` wrapper — `--format=abc` is rejected just like
            // `--pretty=abc`).
            match get_commit_format(v)? {
                Some((p, t)) => {
                    pretty = p;
                    terminator = t;
                }
                None => {
                    eprintln!("fatal: invalid --pretty format: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--name-only" {
            name_only = true;
        } else if a == "--name-status" {
            name_status = true;
        } else if a == "--stat" {
            stat = true;
        } else if a == "--graph" {
            graph = true;
        } else if a == "--all" {
            all = true;
        } else if a == "--reverse" {
            reverse = true;
        } else if a == "--merges" {
            only_merges = true;
        } else if a == "--no-merges" {
            no_merges = true;
        } else if a == "--date-order" {
            order = Order::Date;
        } else if a == "--topo-order" {
            order = Order::Topo;
        } else if a.starts_with('-') {
            let body = &a[1..];
            if let Some(num) = body.strip_prefix('n') {
                // `-nN` shorthand (e.g. `-n5`).
                match parse_max_count(num) {
                    Ok(mc) => max_count = mc,
                    Err(()) => {
                        eprintln!("fatal: '{num}': not an integer");
                        return Ok(ExitCode::from(128));
                    }
                }
            } else if !body.is_empty() && body.bytes().all(|c| c.is_ascii_digit()) {
                // `-N` shorthand (e.g. `-5`): show N commits, so N is positive.
                match parse_max_count(body) {
                    Ok(mc) => max_count = mc,
                    Err(()) => {
                        eprintln!("fatal: '{body}': not an integer");
                        return Ok(ExitCode::from(128));
                    }
                }
            } else {
                bail!("unsupported flag {a:?}");
            }
        } else {
            // A non-flag token before `--` is a revision; git accepts several and
            // walks the union of their histories.
            revs.push(a.clone());
        }
        i += 1;
    }

    // git checks this combination before touching the repository.
    if graph && reverse {
        eprintln!("fatal: options '--graph' and '--reverse' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    // `--name-only` and `--name-status` are mutually exclusive diff formats;
    // git rejects the pair in `diff_setup_done` before any traversal.
    if name_only && name_status {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }

    // Collect the starting tips in git's order: the named revision (or HEAD),
    // then every ref sorted by full name, then HEAD again for `--all`.
    let mut tips: Vec<ObjectId> = Vec::new();
    for spec in &revs {
        match repo.rev_parse_single(spec.as_str()) {
            Ok(id) => tips.push(id.detach()),
            Err(_) => {
                let hex_len = repo.object_hash().len_in_hex();
                eprint!("{}", bad_revision_message(spec, hex_len));
                return Ok(ExitCode::from(128));
            }
        }
    }
    if all {
        // Materialise the names first: the iterator holds the packed-refs buffer,
        // which would block the per-ref object lookups below.
        let mut names: Vec<Vec<u8>> = Vec::new();
        for r in repo.references()?.all()? {
            let r = r.map_err(|e| anyhow!("{e}"))?;
            names.push(r.name().as_bstr().to_vec());
        }
        // git walks `refs/` in sorted full-name order, which decides the tie-break
        // between tips that share a commit date.
        names.sort();
        for name in names {
            let Ok(full) = name.to_str() else { continue };
            let Ok(reference) = repo.find_reference(full) else {
                continue;
            };
            let Ok(id) = reference.into_fully_peeled_id() else {
                continue;
            };
            let oid = id.detach();
            // A tag pointing at a tree or blob is not a history tip.
            if let Ok(obj) = repo.find_object(oid) {
                if obj.kind == gix::objs::Kind::Commit {
                    tips.push(oid);
                }
            }
        }
    }
    if revs.is_empty() || all {
        let head = repo.head()?;
        if head.is_unborn() && !all {
            let branch = head
                .referent_name()
                .map(|n| n.shorten().to_str_lossy().into_owned())
                .unwrap_or_else(|| "master".to_owned());
            eprintln!("fatal: your current branch '{branch}' does not have any commits yet");
            return Ok(ExitCode::from(128));
        }
        if let Some(id) = repo.head()?.try_peel_to_id()? {
            tips.push(id.detach());
        }
    }

    // Walk in git's default commit-date order, then re-sort if a topological
    // order was asked for. `--graph` implies `--topo-order` unless `--date-order`
    // was given explicitly.
    let mut nodes = walk(&repo, &tips)?;
    let effective_order = match (order, graph) {
        (Order::Default, true) => Order::Topo,
        (o, _) => o,
    };
    if effective_order != Order::Default {
        nodes = topo_sort(nodes, effective_order == Order::Date);
    }

    // Path-limited traversal: keep only commits that touched a matching pathspec,
    // measured against the first parent (the empty tree for a root commit).
    if !pathspecs.is_empty() {
        let mut kept = Vec::with_capacity(nodes.len());
        for node in nodes.into_iter() {
            if commit_touches(&repo, &node, &pathspecs)? {
                kept.push(node);
            }
        }
        nodes = kept;
    }

    if only_merges {
        nodes.retain(|n| n.parents.len() >= 2);
    }
    if no_merges {
        nodes.retain(|n| n.parents.len() < 2);
    }
    if let Some(limit) = max_count {
        nodes.truncate(limit);
    }
    if reverse {
        nodes.reverse();
    }

    if graph && nodes.iter().any(|n| n.parents.len() > 2) {
        bail!("--graph is not ported for octopus merges");
    }

    let want_diff = name_only || name_status || stat;
    let mut blocks: Vec<Vec<u8>> = Vec::with_capacity(nodes.len());
    for node in &nodes {
        let commit = repo.find_object(node.id)?.try_into_commit()?;
        let mut block: Vec<u8> = Vec::new();
        render_entry(&mut block, &commit, &pretty, abbrev_commit)?;
        // A `tformat:` record is terminated by a newline, but an empty record
        // (an empty user format) emits nothing at all — no stray terminator.
        if terminator && !block.is_empty() {
            block.push(b'\n');
        }

        if want_diff && node.parents.len() < 2 {
            // `--name-only`/`--name-status` are the reported format when present;
            // git suppresses `--stat` in that case, so the blob reads it needs
            // are skipped too.
            let show_stat = stat && !name_only && !name_status;
            let files = collect_changes(&repo, &commit, node.parents.first().copied(), show_stat)?;
            let mut diff: Vec<u8> = Vec::new();
            if name_status {
                for f in &files {
                    diff.push(f.status);
                    diff.push(b'\t');
                    diff.extend_from_slice(&f.path);
                    diff.push(b'\n');
                }
            } else if name_only {
                for f in &files {
                    diff.extend_from_slice(&f.path);
                    diff.push(b'\n');
                }
            } else if show_stat {
                emit_stat(&mut diff, &files)?;
            }
            if !diff.is_empty() {
                // git puts a blank line between the log message and the diff for
                // every format but `oneline` — and only when the message block
                // rendered something to separate from.
                if !matches!(pretty, Pretty::Oneline) && !block.is_empty() {
                    block.push(b'\n');
                }
                block.extend_from_slice(&diff);
            }
        }
        blocks.push(block);
    }

    // `format:` separates records with a newline; `tformat:` already terminated
    // each one above.
    if !terminator {
        let last = blocks.len().saturating_sub(1);
        for (idx, block) in blocks.iter_mut().enumerate() {
            if idx != last {
                block.push(b'\n');
            }
        }
    }

    let out = if graph {
        render_graph(&nodes, &blocks)?
    } else {
        blocks.concat()
    };

    match std::io::stdout().write_all(&out) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        // A downstream `| head` closing the pipe is not an error.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        Err(e) => Err(e.into()),
    }
}

/// Parse a `-n`/`--max-count` value the way git does: a base-10 signed integer
/// with no trailing garbage. A negative value means "unlimited" (git's `-1`
/// sentinel), reported as `Ok(None)`; a non-negative value caps the walk.
/// `Err(())` marks a value git rejects with `fatal: '<value>': not an integer`.
fn parse_max_count(value: &str) -> Result<Option<usize>, ()> {
    match parse_int(value) {
        Some(n) if n < 0 => Ok(None),
        Some(n) => Ok(Some(n as usize)),
        None => Err(()),
    }
}

/// A base-10 signed integer git would accept: optional `+`/`-`, then digits only,
/// no trailing characters, no overflow. Returns `None` for anything else.
fn parse_int(value: &str) -> Option<i64> {
    let (neg, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// git distinguishes a well-formed but absent object id from an unresolvable name:
/// the former is a "bad object", the latter an "ambiguous argument".
fn bad_revision_message(spec: &str, hex_len: usize) -> String {
    if spec.len() == hex_len && spec.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("fatal: bad object {spec}\n")
    } else {
        format!(
            "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'\n"
        )
    }
}

// ---------------------------------------------------------------------------
// Revision walk
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    /// git's default: pure commit-date order.
    Default,
    /// `--date-order`: topological, breaking ties by commit date.
    Date,
    /// `--topo-order`: topological, following the graph rather than the clock.
    Topo,
}

/// What the walk needs to know about a commit, read once up front.
struct Node {
    id: ObjectId,
    parents: Vec<ObjectId>,
    time: i64,
}

fn read_node(repo: &gix::Repository, id: ObjectId) -> Result<Node> {
    let commit = repo.find_object(id)?.try_into_commit()?;
    Ok(Node {
        id,
        parents: commit.parent_ids().map(|p| p.detach()).collect(),
        time: commit.time()?.seconds,
    })
}

/// git's `commit_list_insert_by_date`: keep the list newest-first, and place a
/// commit *after* every commit with the same date so equal timestamps come out
/// in insertion order — the tie-break git's priority queue also uses.
fn insert_by_date(list: &mut Vec<Node>, node: Node) {
    let pos = list
        .iter()
        .position(|e| e.time < node.time)
        .unwrap_or(list.len());
    list.insert(pos, node);
}

/// Breadth-first walk over the reachable history, newest commit first.
fn walk(repo: &gix::Repository, tips: &[ObjectId]) -> Result<Vec<Node>> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut pending: Vec<Node> = Vec::new();
    for tip in tips {
        if seen.insert(*tip) {
            insert_by_date(&mut pending, read_node(repo, *tip)?);
        }
    }

    let mut out: Vec<Node> = Vec::new();
    while !pending.is_empty() {
        let node = pending.remove(0);
        for parent in &node.parents {
            if seen.insert(*parent) {
                insert_by_date(&mut pending, read_node(repo, *parent)?);
            }
        }
        out.push(node);
    }
    Ok(out)
}

/// git's `sort_in_topological_order`: an indegree count over the already-walked
/// set, drained through a queue that is date-ordered for `--date-order` and a
/// LIFO stack for `--topo-order`.
fn topo_sort(nodes: Vec<Node>, by_date: bool) -> Vec<Node> {
    let mut indegree: std::collections::HashMap<ObjectId, usize> =
        nodes.iter().map(|n| (n.id, 1usize)).collect();
    for node in &nodes {
        for parent in &node.parents {
            if let Some(d) = indegree.get_mut(parent) {
                *d += 1;
            }
        }
    }

    let index: std::collections::HashMap<ObjectId, usize> =
        nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();

    // Tips are queued in list order. A LIFO stack is reversed first so that
    // popping still yields them in that order, exactly as git does.
    let mut queue: Vec<usize> = (0..nodes.len())
        .filter(|&i| indegree.get(&nodes[i].id) == Some(&1))
        .collect();
    if !by_date {
        queue.reverse();
    }

    let mut out: Vec<usize> = Vec::with_capacity(nodes.len());
    while !queue.is_empty() {
        let at = if by_date {
            // Highest commit date wins; the earliest-queued entry breaks ties.
            let mut best = 0usize;
            for (k, &i) in queue.iter().enumerate() {
                if nodes[i].time > nodes[queue[best]].time {
                    best = k;
                }
            }
            best
        } else {
            queue.len() - 1
        };
        let i = queue.remove(at);

        for parent in &nodes[i].parents {
            if let Some(d) = indegree.get_mut(parent) {
                if *d == 0 {
                    continue;
                }
                *d -= 1;
                if *d == 1 {
                    if let Some(&pi) = index.get(parent) {
                        queue.push(pi);
                    }
                }
            }
        }
        out.push(i);
    }

    // Anything the drain could not reach keeps its original relative position.
    let mut placed: Vec<bool> = vec![false; nodes.len()];
    for &i in &out {
        placed[i] = true;
    }
    for i in 0..nodes.len() {
        if !placed[i] {
            out.push(i);
        }
    }

    let mut slots: Vec<Option<Node>> = nodes.into_iter().map(Some).collect();
    out.into_iter()
        .filter_map(|i| slots[i].take())
        .collect()
}

// ---------------------------------------------------------------------------
// Pretty formats
// ---------------------------------------------------------------------------

enum Pretty {
    /// git's default: `commit`/`Merge`/`Author`/`Date` and an indented message.
    Medium,
    /// `medium` without the `Date` line.
    Short,
    /// `<hash> <subject>` on one line.
    Oneline,
    /// A `--format=`/`format:` string with `%` placeholders.
    User(String),
}

/// git's `get_commit_format`, the shared parser behind `--pretty=` and
/// `--format=`. Returns the format and whether it terminates (rather than
/// separates) records:
///   * `Ok(Some(..))` — a valid, supported format.
///   * `Ok(None)`     — a value git itself rejects (`fatal: invalid --pretty
///     format: <arg>`, exit 128): non-empty, no `%`, not a `format:`/`tformat:`
///     prefix, and not a known format name.
///   * `Err(..)`      — a value git accepts but this port does not yet render
///     (an unsupported `%` placeholder), surfaced terse rather than faked.
///
/// An empty value is git's empty user format: it renders nothing per commit and,
/// as a terminator format, drops even the trailing newline.
fn get_commit_format(spec: &str) -> Result<Option<(Pretty, bool)>> {
    if spec.is_empty() {
        return Ok(Some((Pretty::User(String::new()), true)));
    }
    if let Some(fmt) = spec.strip_prefix("format:") {
        check_format(fmt)?;
        return Ok(Some((Pretty::User(fmt.to_string()), false)));
    }
    if let Some(fmt) = spec.strip_prefix("tformat:") {
        check_format(fmt)?;
        return Ok(Some((Pretty::User(fmt.to_string()), true)));
    }
    if spec.contains('%') {
        check_format(spec)?;
        return Ok(Some((Pretty::User(spec.to_string()), true)));
    }
    match spec {
        "oneline" => Ok(Some((Pretty::Oneline, true))),
        "medium" => Ok(Some((Pretty::Medium, false))),
        "short" => Ok(Some((Pretty::Short, false))),
        // Valid git format names this port does not render yet: surfaced terse,
        // not as git's "invalid" — they are legal, just unported.
        "full" | "fuller" | "reference" | "email" | "raw" | "mboxrd" => {
            bail!("pretty format {spec:?} is not ported")
        }
        _ => Ok(None),
    }
}

/// Reject any placeholder [`expand_format`] does not implement, so an unsupported
/// format fails loudly instead of expanding to something plausible but wrong.
fn check_format(fmt: &str) -> Result<()> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            continue;
        }
        match it.next() {
            Some('H' | 'h' | 'T' | 't' | 'P' | 'p' | 's' | 'n' | '%') => {}
            Some('a') => match it.next() {
                Some('n' | 'e') => {}
                Some(x) => bail!("unsupported format placeholder %a{x}"),
                None => bail!("unsupported trailing % in format"),
            },
            Some(x) => bail!("unsupported format placeholder %{x}"),
            None => bail!("unsupported trailing % in format"),
        }
    }
    Ok(())
}

/// Expand the placeholders accepted by [`check_format`] for `commit`.
fn expand_format(out: &mut Vec<u8>, commit: &gix::Commit<'_>, fmt: &str) -> Result<()> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match it.next() {
            Some('H') => out.extend_from_slice(commit.id().to_string().as_bytes()),
            Some('h') => out.extend_from_slice(commit.id().shorten_or_id().to_string().as_bytes()),
            Some('T') => out.extend_from_slice(commit.tree_id()?.to_string().as_bytes()),
            Some('t') => {
                out.extend_from_slice(commit.tree_id()?.shorten_or_id().to_string().as_bytes());
            }
            Some('P') => write_parents(out, commit, false),
            Some('p') => write_parents(out, commit, true),
            Some('s') => out.extend_from_slice(&subject(commit.message_raw()?)),
            Some('n') => out.push(b'\n'),
            Some('%') => out.push(b'%'),
            Some('a') => {
                let author = commit.author()?;
                match it.next() {
                    Some('n') => out.extend_from_slice(author.name),
                    Some('e') => out.extend_from_slice(author.email),
                    _ => unreachable!("check_format rejected this already"),
                }
            }
            _ => unreachable!("check_format rejected this already"),
        }
    }
    Ok(())
}

/// Space-separated parent ids, abbreviated for `%p` and full for `%P`.
fn write_parents(out: &mut Vec<u8>, commit: &gix::Commit<'_>, abbrev: bool) {
    for (i, p) in commit.parent_ids().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        let text = if abbrev {
            p.shorten_or_id().to_string()
        } else {
            p.to_string()
        };
        out.extend_from_slice(text.as_bytes());
    }
}

/// git's subject: the first paragraph of the message, folded onto one line.
fn subject(msg: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for line in msg.split(|&b| b == b'\n') {
        let line = trim_end_ws(line);
        if line.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    out
}

/// Render one commit's header in the selected format. Built-in formats end with
/// a newline; user formats and `oneline` do not, because their record ending is
/// supplied by the separator/terminator rule in [`log`].
fn render_entry(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    pretty: &Pretty,
    abbrev_commit: bool,
) -> Result<()> {
    let id = if abbrev_commit {
        commit.id().shorten_or_id().to_string()
    } else {
        commit.id().to_string()
    };

    match pretty {
        Pretty::Oneline => {
            out.extend_from_slice(id.as_bytes());
            out.push(b' ');
            out.extend_from_slice(&subject(commit.message_raw()?));
        }
        Pretty::User(fmt) => expand_format(out, commit, fmt)?,
        Pretty::Medium | Pretty::Short => {
            let author = commit.author()?;
            writeln!(out, "commit {id}")?;

            // A merge commit lists its abbreviated parents right after `commit`.
            let parents: Vec<_> = commit.parent_ids().collect();
            if parents.len() > 1 {
                out.extend_from_slice(b"Merge:");
                for pid in &parents {
                    out.push(b' ');
                    out.extend_from_slice(pid.shorten_or_id().to_string().as_bytes());
                }
                out.push(b'\n');
            }

            out.extend_from_slice(b"Author: ");
            out.extend_from_slice(author.name);
            out.extend_from_slice(b" <");
            out.extend_from_slice(author.email);
            out.extend_from_slice(b">\n");

            if matches!(pretty, Pretty::Medium) {
                let time = author.time()?;
                writeln!(out, "Date:   {}", format_git_date(time.seconds, time.offset))?;
            }
            out.push(b'\n');

            // The message is indented four spaces; blank lines stay blank.
            for line in commit.message_raw()?.lines() {
                if !line.is_empty() {
                    out.extend_from_slice(b"    ");
                    out.extend_from_slice(line);
                }
                out.push(b'\n');
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-commit diff
// ---------------------------------------------------------------------------

/// One changed path, with the line counts `--stat` needs.
struct FileChange {
    path: Vec<u8>,
    status: u8,
    added: usize,
    deleted: usize,
    is_binary: bool,
    old_size: usize,
    new_size: usize,
}

/// Diff `commit`'s tree against `parent`'s (or the empty tree for a root commit),
/// dropping the directory entries gix reports alongside the files it recurses into.
/// Blob contents are only read when `with_counts` is set, which is the only case
/// that needs them.
fn collect_changes(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
    with_counts: bool,
) -> Result<Vec<FileChange>> {
    let new_tree = commit.tree()?;
    let old_tree = match parent {
        Some(pid) => Some(repo.find_object(pid)?.try_into_commit()?.tree()?),
        None => None,
    };

    let mut changes = repo.diff_tree_to_tree(
        old_tree.as_ref(),
        Some(&new_tree),
        gix::diff::Options::default(),
    )?;
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));

    let mut out = Vec::with_capacity(changes.len());
    for change in &changes {
        if let Some(f) = prepare_change(repo, change, with_counts)? {
            out.push(f);
        }
    }
    Ok(out)
}

/// Whether `node`'s commit touched any path matching `pathspecs`, diffed against
/// its first parent (the empty tree for a root commit). This is the predicate
/// git's path-limited traversal shows a commit on.
fn commit_touches(repo: &gix::Repository, node: &Node, pathspecs: &[String]) -> Result<bool> {
    let commit = repo.find_object(node.id)?.try_into_commit()?;
    let files = collect_changes(repo, &commit, node.parents.first().copied(), false)?;
    for f in &files {
        for spec in pathspecs {
            if pathspec_matches(spec, &f.path)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Does a plain git pathspec match a repo-relative path? Matches git's default
/// (non-magic) rules: an exact path, a leading directory (`src` matches
/// `src/lib.rs`), or a wildcard where `*`/`?` span the whole path. Magic
/// pathspecs (`:(glob)`, `:!exclude`, …) and bracket classes are surfaced terse
/// rather than matched wrong.
fn pathspec_matches(spec: &str, path: &[u8]) -> Result<bool> {
    if spec.starts_with(':') {
        bail!("magic pathspecs are not ported");
    }
    let spec = spec.strip_prefix("./").unwrap_or(spec);
    let spec = spec.trim_end_matches('/');
    if spec.is_empty() || spec == "." {
        return Ok(true);
    }
    let sb = spec.as_bytes();
    if path == sb {
        return Ok(true);
    }
    // Leading-directory match: the path lives under the pathspec directory.
    if path.len() > sb.len() && path.starts_with(sb) && path[sb.len()] == b'/' {
        return Ok(true);
    }
    if spec.bytes().any(|b| b == b'[') {
        bail!("bracket-class pathspecs are not ported");
    }
    if spec.bytes().any(|b| b == b'*' || b == b'?') {
        return Ok(wildmatch(sb, path));
    }
    Ok(false)
}

/// Glob match for a plain pathspec: `*` matches any run (slashes included, as
/// git's default fnmatch does), `?` matches one byte. Linear-time with the usual
/// single backtrack point for `*`.
fn wildmatch(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star_p, mut star_t): (Option<usize>, usize) = (None, 0);
    while t < text.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star_p = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// Turn one gix change into a [`FileChange`], or `None` for the directory entries
/// git does not report (gix emits those *and* recurses into them).
fn prepare_change(
    repo: &gix::Repository,
    change: &ChangeDetached,
    with_counts: bool,
) -> Result<Option<FileChange>> {
    let (path, status, old, new) = match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            (
                location.to_vec(),
                b'A',
                None,
                Some((*id, entry_mode.is_commit())),
            )
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            (
                location.to_vec(),
                b'D',
                Some((*id, entry_mode.is_commit())),
                None,
            )
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            // A directory whose contents changed; the changed files themselves are
            // reported separately by the recursive walk.
            if entry_mode.is_tree() && previous_entry_mode.is_tree() {
                return Ok(None);
            }
            let status = if type_class(previous_entry_mode.kind()) == type_class(entry_mode.kind()) {
                b'M'
            } else {
                b'T'
            };
            (
                location.to_vec(),
                status,
                Some((*previous_id, previous_entry_mode.is_commit())),
                Some((*id, entry_mode.is_commit())),
            )
        }
        // Never produced: rewrite tracking is disabled via Options::default().
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    };

    let mut f = FileChange {
        path,
        status,
        added: 0,
        deleted: 0,
        is_binary: false,
        old_size: 0,
        new_size: 0,
    };

    if with_counts {
        let old_content = match old {
            Some((id, is_sub)) => content_of(repo, id, is_sub)?,
            None => Vec::new(),
        };
        let new_content = match new {
            Some((id, is_sub)) => content_of(repo, id, is_sub)?,
            None => Vec::new(),
        };
        f.old_size = old_content.len();
        f.new_size = new_content.len();
        f.is_binary = is_binary(&old_content) || is_binary(&new_content);
        let mode_only = matches!((old, new), (Some((a, _)), Some((b, _))) if a == b);
        if !f.is_binary && !mode_only {
            let (added, deleted) = count_changed_lines(&old_content, &new_content)?;
            f.added = added;
            f.deleted = deleted;
        }
    }
    Ok(Some(f))
}

/// git's status letters distinguish a change of file *type* (`T`) from a change of
/// contents or permissions (`M`); regular and executable files are the same type.
fn type_class(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Tree => 0,
        EntryKind::Blob | EntryKind::BlobExecutable => 1,
        EntryKind::Link => 2,
        EntryKind::Commit => 3,
    }
}

/// The bytes to diff for an entry: a real blob is read from the object database; a
/// submodule (commit entry) is rendered as its `Subproject commit <oid>` line.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// git's binary heuristic: a NUL byte within the first 8000 bytes.
fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

/// The path of a change, for stable diff ordering.
fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

/// Total added and removed lines, for `--stat`.
fn count_changed_lines(old: &[u8], new: &[u8]) -> Result<(usize, usize)> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let counter = LineCounter {
        added: 0,
        deleted: 0,
    };
    Ok(UnifiedDiff::new(&diff, &input, counter, ContextSize::symmetrical(3)).consume()?)
}

/// Counts changed lines, ignoring context.
struct LineCounter {
    added: usize,
    deleted: usize,
}

impl ConsumeHunk for LineCounter {
    type Out = (usize, usize);

    fn consume_hunk(&mut self, _header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        for &(kind, _) in lines {
            match kind {
                DiffLineKind::Add => self.added += 1,
                DiffLineKind::Remove => self.deleted += 1,
                DiffLineKind::Context => {}
            }
        }
        Ok(())
    }

    fn finish(self) -> (usize, usize) {
        (self.added, self.deleted)
    }
}

// ---------------------------------------------------------------------------
// --stat
// ---------------------------------------------------------------------------

/// git's `--stat`: a right-aligned change count and a `+`/`-` bar per file, scaled
/// to fit an 80-column terminal, then a summary line.
fn emit_stat(out: &mut Vec<u8>, files: &[FileChange]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    let mut max_len = 0usize;
    let mut max_change = 0usize;
    let mut number_width = 0usize;
    for f in files {
        max_len = max_len.max(display_width(&f.path));
        if f.is_binary {
            // Change counts are aligned with the literal "Bin" for binary files.
            number_width = 3;
            continue;
        }
        max_change = max_change.max(f.added + f.deleted);
    }
    number_width = number_width.max(decimal_width(max_change));

    let width = STAT_TERM_WIDTH;
    let mut name_width = max_len;
    let mut graph_width = max_change;
    // Fixed overhead per line is 6 columns: " ", " | ", and " " before the bar.
    if name_width + number_width + 6 + graph_width > width {
        let graph_cap = (width * 3 / 8).saturating_sub(number_width + 6);
        if graph_width > graph_cap {
            graph_width = graph_cap.max(6);
        }
        let name_cap = width.saturating_sub(number_width + 6 + graph_width);
        if name_width > name_cap {
            name_width = name_cap;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    let mut total_added = 0usize;
    let mut total_deleted = 0usize;
    for f in files {
        let (prefix, name) = elide_name(&f.path, name_width);
        let padding = name_width.saturating_sub(prefix.len() + display_width(name));
        out.push(b' ');
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        out.extend_from_slice(b" | ");

        if f.is_binary {
            // For binaries the counts are byte sizes, not lines.
            write!(out, "{:>width$}", "Bin", width = number_width)?;
            if f.old_size == 0 && f.new_size == 0 {
                out.push(b'\n');
            } else {
                writeln!(out, " {} -> {} bytes", f.old_size, f.new_size)?;
            }
            continue;
        }

        total_added += f.added;
        total_deleted += f.deleted;
        let change = f.added + f.deleted;
        write!(out, "{change:>number_width$}")?;

        let (mut add, mut del) = (f.added, f.deleted);
        if graph_width < max_change {
            let mut total = scale_linear(add + del, graph_width, max_change);
            if total < 2 && add > 0 && del > 0 {
                total = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = total.saturating_sub(add);
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = total.saturating_sub(del);
            }
        }
        if add > 0 || del > 0 {
            out.push(b' ');
            out.extend_from_slice(&b"+".repeat(add));
            out.extend_from_slice(&b"-".repeat(del));
        }
        out.push(b'\n');
    }

    let n = files.len();
    write!(out, " {n} file{} changed", if n == 1 { "" } else { "s" })?;
    if total_added > 0 || total_deleted == 0 {
        write!(
            out,
            ", {total_added} insertion{}(+)",
            if total_added == 1 { "" } else { "s" }
        )?;
    }
    if total_deleted > 0 || total_added == 0 {
        write!(
            out,
            ", {total_deleted} deletion{}(-)",
            if total_deleted == 1 { "" } else { "s" }
        )?;
    }
    out.push(b'\n');
    Ok(())
}

/// Scale `it` into `width` columns, guaranteeing at least one column for any
/// non-zero value — git widens by one and adds it back for exactly that reason.
fn scale_linear(it: usize, width: usize, max_change: usize) -> usize {
    if it == 0 || max_change == 0 {
        return 0;
    }
    1 + (it * width.saturating_sub(1) / max_change)
}

/// Shorten an over-long path the way git does: a `...` prefix, cut back to a
/// directory boundary when one falls inside the retained tail.
fn elide_name<'p>(path: &'p [u8], name_width: usize) -> (&'static str, &'p [u8]) {
    if display_width(path) <= name_width || name_width < 3 {
        return ("", path);
    }
    let keep = name_width - 3;
    let mut tail = &path[path.len() - keep..];
    if let Some(slash) = tail.iter().position(|&b| b == b'/') {
        tail = &tail[slash..];
    }
    ("...", tail)
}

fn decimal_width(mut n: usize) -> usize {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// Approximate display width. Paths are treated as UTF-8 and counted in `char`s,
/// which matches git for everything but wide and combining characters.
fn display_width(path: &[u8]) -> usize {
    String::from_utf8_lossy(path).chars().count()
}

// ---------------------------------------------------------------------------
// --graph
// ---------------------------------------------------------------------------

/// Prefix every line of every commit's block with git's ASCII graph, flushing the
/// merge and collapse rows that fall between commits.
fn render_graph(nodes: &[Node], blocks: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut graph = Graph::default();
    let mut out: Vec<u8> = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
        graph.update(node.id, &node.parents);

        let block = &blocks[i];
        let ends_nl = block.ends_with(b"\n");
        let mut lines: Vec<&[u8]> = block.split(|&b| b == b'\n').collect();
        if ends_nl {
            lines.pop();
        }

        for (j, line) in lines.iter().enumerate() {
            out.extend_from_slice(&graph.next_line());
            out.extend_from_slice(line);
            if ends_nl || j + 1 < lines.len() {
                out.push(b'\n');
            }
        }

        // Rows the commit's own text did not consume: the `|\` of a merge and the
        // `|/` of a collapse both appear on lines of their own. A collapse needs at
        // most one row per column, so the bound below can only trip on a bug here —
        // failing beats hanging the caller.
        let mut guard = graph.columns.len() + graph.new_columns.len() + 8;
        while graph.state != GraphState::Padding {
            out.extend_from_slice(&graph.next_line());
            out.push(b'\n');
            guard -= 1;
            if guard == 0 {
                bail!("--graph failed to settle the commit graph");
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphState {
    Padding,
    Commit,
    PostMerge,
    Collapsing,
}

/// git's `graph.c` column state machine, for commits with at most two parents.
struct Graph {
    /// Columns as of the previous commit.
    columns: Vec<ObjectId>,
    /// Columns as of the current commit.
    new_columns: Vec<ObjectId>,
    /// Screen-slot to new-column index, `-1` for an empty slot.
    mapping: Vec<i32>,
    old_mapping: Vec<i32>,
    commit: ObjectId,
    num_parents: usize,
    width: usize,
    state: GraphState,
    prev_state: GraphState,
}

impl Default for Graph {
    fn default() -> Self {
        Graph {
            columns: Vec::new(),
            new_columns: Vec::new(),
            mapping: Vec::new(),
            old_mapping: Vec::new(),
            commit: ObjectId::null(gix::hash::Kind::Sha1),
            num_parents: 0,
            width: 0,
            state: GraphState::Padding,
            prev_state: GraphState::Padding,
        }
    }
}

impl Graph {
    fn update(&mut self, id: ObjectId, parents: &[ObjectId]) {
        self.commit = id;
        self.num_parents = parents.len();
        self.update_columns(parents);
        // Every commit's rows are fully flushed before the next one starts, so
        // the skip and pre-commit states git needs for interrupted output and
        // octopus expansion never arise here.
        self.state = GraphState::Commit;
    }

    fn update_columns(&mut self, parents: &[ObjectId]) {
        std::mem::swap(&mut self.columns, &mut self.new_columns);
        self.new_columns.clear();

        let num_columns = self.columns.len();
        let max_new_columns = num_columns + self.num_parents;
        self.mapping = vec![-1i32; 2 * max_new_columns.max(1)];

        let mut seen_this = false;
        let mut mapping_idx = 0usize;
        let mut is_commit_in_columns = true;
        let mut i = 0usize;
        while i <= num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                is_commit_in_columns = false;
                self.commit
            } else {
                self.columns[i]
            };

            if col_commit == self.commit {
                let old_mapping_idx = mapping_idx;
                seen_this = true;
                for parent in parents {
                    insert_column(&mut self.new_columns, &mut self.mapping, *parent, &mut mapping_idx);
                }
                // A commit occupies at least two screen slots even with no parents.
                if mapping_idx == old_mapping_idx {
                    mapping_idx += 2;
                }
            } else {
                insert_column(&mut self.new_columns, &mut self.mapping, col_commit, &mut mapping_idx);
            }
            i += 1;
        }

        while self.mapping.len() > 1 && *self.mapping.last().unwrap_or(&0) < 0 {
            self.mapping.pop();
        }

        // Every row of this commit is padded to the widest row it can produce.
        let mut max_cols = num_columns + self.num_parents;
        if self.num_parents < 1 {
            max_cols += 1;
        }
        if is_commit_in_columns && max_cols > 0 {
            max_cols -= 1;
        }
        self.width = max_cols * 2;
    }

    fn mapping_correct(&self) -> bool {
        self.mapping
            .iter()
            .enumerate()
            .all(|(i, &t)| t < 0 || t == (i as i32) / 2)
    }

    fn next_line(&mut self) -> Vec<u8> {
        match self.state {
            GraphState::Commit => self.commit_line(),
            GraphState::PostMerge => self.post_merge_line(),
            GraphState::Collapsing => self.collapsing_line(),
            GraphState::Padding => {
                let mut line = Vec::new();
                for _ in 0..self.new_columns.len() {
                    line.extend_from_slice(b"| ");
                }
                pad_to(&mut line, self.width);
                line
            }
        }
    }

    fn commit_line(&mut self) -> Vec<u8> {
        let mut line: Vec<u8> = Vec::new();
        let mut seen_this = false;
        let num_columns = self.columns.len();
        let mut i = 0usize;
        while i <= num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                self.commit
            } else {
                self.columns[i]
            };

            if col_commit == self.commit {
                seen_this = true;
                line.push(b'*');
            } else if seen_this && self.num_parents > 1 {
                line.push(b'\\');
            } else if self.prev_state == GraphState::Collapsing
                && self.old_mapping.get(2 * i + 1).copied().unwrap_or(-1) == i as i32
                && self.mapping.get(2 * i).copied().unwrap_or(-1) < i as i32
            {
                line.push(b'/');
            } else {
                line.push(b'|');
            }
            line.push(b' ');
            i += 1;
        }
        pad_to(&mut line, self.width);

        self.prev_state = GraphState::Commit;
        self.state = if self.num_parents > 1 {
            GraphState::PostMerge
        } else if self.mapping_correct() {
            GraphState::Padding
        } else {
            GraphState::Collapsing
        };
        line
    }

    fn post_merge_line(&mut self) -> Vec<u8> {
        let mut line: Vec<u8> = Vec::new();
        let mut seen_this = false;
        let num_columns = self.columns.len();
        let mut i = 0usize;
        while i <= num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                self.commit
            } else {
                self.columns[i]
            };

            if col_commit == self.commit {
                seen_this = true;
                line.push(b'|');
                for _ in 1..self.num_parents {
                    line.push(b'\\');
                }
            } else if seen_this {
                line.extend_from_slice(b"\\ ");
            } else {
                line.extend_from_slice(b"| ");
            }
            i += 1;
        }
        pad_to(&mut line, self.width);

        self.prev_state = GraphState::PostMerge;
        self.state = if self.mapping_correct() {
            GraphState::Padding
        } else {
            GraphState::Collapsing
        };
        line
    }

    fn collapsing_line(&mut self) -> Vec<u8> {
        std::mem::swap(&mut self.mapping, &mut self.old_mapping);
        let size = self.old_mapping.len();
        self.mapping = vec![-1i32; size];

        let mut horizontal_edge: i32 = -1;
        let mut horizontal_edge_target: i32 = -1;

        for i in 0..size {
            let target = self.old_mapping[i];
            if target < 0 {
                continue;
            }
            if (target as usize) * 2 == i {
                // Already where it belongs.
                self.mapping[i] = target;
            } else if i >= 1 && self.mapping[i - 1] < 0 {
                // Nothing to the left: step one slot over.
                self.mapping[i - 1] = target;
                if horizontal_edge == -1 {
                    horizontal_edge = i as i32;
                    horizontal_edge_target = target;
                    let mut j = (target as usize) * 2 + 3;
                    while (j as i64) < i as i64 - 2 {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            } else if i >= 1 && self.mapping[i - 1] == target {
                // Shares a parent with the line to its left; already drawn.
            } else if i >= 2 {
                // Cross over the unrelated line to the left.
                self.mapping[i - 2] = target;
            }
        }

        if size > 0 && self.mapping[size - 1] < 0 {
            self.mapping.pop();
        }

        let mut line: Vec<u8> = Vec::new();
        let mut used_horizontal = false;
        for i in 0..self.mapping.len() {
            let target = self.mapping[i];
            if target < 0 {
                line.push(b' ');
            } else if (target as usize) * 2 == i {
                line.push(b'|');
            } else if target == horizontal_edge_target && i as i32 != horizontal_edge - 1 {
                if i != (target as usize) * 2 + 3 {
                    self.mapping[i] = -1;
                }
                used_horizontal = true;
                line.push(b'_');
            } else {
                if used_horizontal && (i as i32) < horizontal_edge {
                    self.mapping[i] = -1;
                }
                line.push(b'/');
            }
        }
        pad_to(&mut line, self.width);

        self.prev_state = GraphState::Collapsing;
        if self.mapping_correct() {
            self.state = GraphState::Padding;
        }
        line
    }
}

/// Record `id` in the new column list (reusing its column when it is already
/// there) and point the next screen slot at it.
fn insert_column(
    new_columns: &mut Vec<ObjectId>,
    mapping: &mut [i32],
    id: ObjectId,
    mapping_idx: &mut usize,
) {
    let col = match new_columns.iter().position(|c| *c == id) {
        Some(i) => i,
        None => {
            new_columns.push(id);
            new_columns.len() - 1
        }
    };
    if let Some(slot) = mapping.get_mut(*mapping_idx) {
        *slot = col as i32;
    }
    *mapping_idx += 2;
}

/// Every graph row for one commit is the same width, so text to its right lines up.
fn pad_to(line: &mut Vec<u8>, width: usize) {
    while line.len() < width {
        line.push(b' ');
    }
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// Format a commit time exactly like stock `git log`'s default (`DATE_NORMAL`)
/// mode: `Www Mmm <sp-padded-day> HH:MM:SS YYYY +ZZZZ`, in the commit's own
/// timezone offset. Done by hand because gix's exported `DEFAULT` format uses an
/// unpadded day (`%-d`) whereas git space-pads it (`%e`); nothing else in the
/// crate lets us construct a custom format string.
fn format_git_date(seconds: i64, offset: i32) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    // Shift into the commit's local wall-clock time, then split into whole days
    // (since the Unix epoch) and the seconds within the day. `div_euclid` /
    // `rem_euclid` keep the split correct for pre-1970 (negative) timestamps.
    let local = seconds + offset as i64;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    // 1970-01-01 (day 0) was a Thursday, index 4 with Sunday = 0.
    let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
    let (year, month, day) = civil_from_days(days);

    let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
    let (off_h, off_m) = (off / 3600, (off % 3600) / 60);

    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {} {}{:02}{:02}",
        WEEKDAYS[weekday],
        MONTHS[(month - 1) as usize],
        day,
        hour,
        min,
        sec,
        year,
        sign,
        off_h,
        off_m,
    )
}

/// Convert a day count since the Unix epoch into a civil `(year, month, day)`,
/// month and day 1-based. Howard Hinnant's `civil_from_days` algorithm, which is
/// exact for the whole representable range and needs no calendar tables.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month as u32, day)
}

/// Strip trailing whitespace (git trims a subject line this way).
fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last == b'\n' || last == b'\r' || last == b' ' || last == b'\t' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}
