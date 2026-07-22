use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::ByteSlice;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::prelude::ObjectIdExt;
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
///   * `--skip=N`                                → drop the first N selected commits
///   * `--all`                                   → start from every ref plus `HEAD`
///   * `--merges` / `--no-merges`                → keep only (or drop) multi-parent commits
///   * `--min-parents=N` / `--max-parents=N` and
///     their `--no-` forms                       → parent-count limiting
///   * `--first-parent`                          → follow only the first parent
///   * `--reverse`                               → emit the selected commits oldest-first
///   * `--date-order` / `--topo-order`           → git's two topological sort orders
///   * `--oneline`, `--pretty=`/`--format=` with
///     `oneline`, `short`, `medium`, `full`, `fuller`, `raw`, `reference`, and
///     `format:`/`tformat:` strings (last flag wins; an invalid value is rejected
///     exactly as git's `get_commit_format` does). User-format placeholders include
///     `%C`/`%C(...)` colors (with `%C(auto)`), `%d`/`%D` ref decorations, and
///     `%cr`/`%ar` relative dates, alongside the hash/tree/parent/author/committer/
///     subject/body set
///   * `--abbrev-commit` / `--no-abbrev-commit`, `--parents`
///   * `--date=<mode>`                           → `default`/`short`/`iso`/`iso-strict`/
///     `rfc`/`unix`/`raw`/`relative` (the remaining zone-dependent modes `human`/`local`
///     are surfaced terse)
///   * `--color[=<when>]` / `--no-color`         → enable/disable the `%C` and
///     `%C(auto)`-gated decoration colors (`always`/`never`/`auto`; auto colors when
///     stdout is a terminal or a pager is in use)
///   * `--name-only`, `--name-status`, `--stat`,
///     `--numstat`, `--shortstat`                → per-commit diff against the first parent
///     (`--name-only`/`--name-status` are mutually exclusive and suppress the count
///     formats); `-s`/`--no-patch` accepted as no-ops
///   * `-p`/`--patch`/`-u`                        → per-commit `diff --git` patch against the
///     first parent (the empty tree for a root commit), three lines of context; suppressed by
///     `--name-only`/`--name-status`, emitted after the count formats otherwise, and skipped
///     for merge commits (git shows no diff there without `-m`/`-c`/`--cc`). Rendered by the
///     same pipeline as `git diff`, so the two produce byte-identical patches.
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
///   * `--grep`/`--author` filters, `--since`/`--until` date filters, revision
///     ranges (`A..B`), and every flag not listed above are rejected explicitly.
pub fn log(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let mut max_count: Option<usize> = None;
    let mut skip: usize = 0;
    let mut pretty = Pretty::Medium;
    let mut terminator = false;
    let mut abbrev_commit = false;
    let mut name_only = false;
    let mut name_status = false;
    let mut stat = false;
    let mut numstat = false;
    let mut shortstat = false;
    let mut patch = false;
    let mut graph = false;
    let mut all = false;
    let mut reverse = false;
    let mut only_merges = false;
    let mut no_merges = false;
    let mut first_parent = false;
    let mut show_parents = false;
    let mut min_parents: Option<usize> = None;
    let mut max_parents: Option<usize> = None;
    let mut date_mode = DateMode::Default;
    let mut color = ColorWhen::Auto;
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
        } else if a == "--pretty" {
            // Bare `--pretty` is git's `--pretty=medium`.
            pretty = Pretty::Medium;
            terminator = false;
        } else if a == "--format" {
            // Bare `--format` (no `=value`) is a git usage error, exit 128.
            eprintln!("fatal: unrecognized argument: --format");
            return Ok(ExitCode::from(128));
        } else if a == "--skip" {
            i += 1;
            let v = args
                .get(i)
                .ok_or_else(|| anyhow!("option `{a}` requires a value"))?;
            match parse_nonneg(v) {
                Some(n) => skip = n,
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--skip=") {
            match parse_nonneg(v) {
                Some(n) => skip = n,
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--date=") {
            match parse_date_mode(v) {
                Some(m) => date_mode = m,
                None => {
                    eprintln!("fatal: unknown date format {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--min-parents=") {
            match parse_nonneg(v) {
                Some(n) => min_parents = Some(n),
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--max-parents=") {
            match parse_nonneg(v) {
                Some(n) => max_parents = Some(n),
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--no-min-parents" {
            min_parents = Some(0);
        } else if a == "--no-max-parents" {
            max_parents = None;
        } else if a == "--first-parent" {
            first_parent = true;
        } else if a == "--parents" {
            show_parents = true;
        } else if a == "--abbrev-commit" {
            abbrev_commit = true;
        } else if a == "--no-abbrev-commit" {
            abbrev_commit = false;
        } else if a == "-p" || a == "--patch" || a == "-u" {
            // `-u` is git's documented synonym for `-p`.
            patch = true;
        } else if a == "-s" || a == "--no-patch" {
            // Suppress diff output — git treats `-s` as order-sensitive, so a
            // later `--stat`/`-p` re-enables whichever format follows it.
            stat = false;
            numstat = false;
            shortstat = false;
            name_only = false;
            name_status = false;
            patch = false;
        } else if a == "--name-only" {
            name_only = true;
        } else if a == "--name-status" {
            name_status = true;
        } else if a == "--stat" {
            stat = true;
        } else if a == "--numstat" {
            numstat = true;
        } else if a == "--shortstat" {
            shortstat = true;
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
        } else if a == "--color" {
            // Bare `--color` is git's `--color=always`.
            color = ColorWhen::Always;
        } else if a == "--no-color" {
            color = ColorWhen::Never;
        } else if let Some(v) = a.strip_prefix("--color=") {
            match v {
                "always" => color = ColorWhen::Always,
                "never" => color = ColorWhen::Never,
                "auto" => color = ColorWhen::Auto,
                _ => {
                    eprintln!("fatal: invalid color value: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
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
    let mut nodes = walk(&repo, &tips, first_parent)?;
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

    // `--merges`/`--no-merges` are git's aliases for `--min-parents=2` /
    // `--max-parents=1`; parent-count limiting happens before commit limiting.
    if only_merges {
        nodes.retain(|n| n.parents.len() >= 2);
    }
    if no_merges {
        nodes.retain(|n| n.parents.len() < 2);
    }
    if let Some(min) = min_parents {
        nodes.retain(|n| n.parents.len() >= min);
    }
    if let Some(max) = max_parents {
        nodes.retain(|n| n.parents.len() <= max);
    }

    // `--skip` drops the first N of the selected commits, then `--max-count` caps
    // what remains — git's order in `get_revision`.
    if skip > 0 {
        let drop = skip.min(nodes.len());
        nodes.drain(0..drop);
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

    // `--name-only`/`--name-status` are git's reported format; they suppress both
    // the count formats and the `-p` patch. The patch is emitted after the count
    // formats otherwise.
    let emit_patch = patch && !name_only && !name_status;
    let want_names = name_only || name_status || stat || numstat || shortstat;
    // Whether `%C`/`%d` emit ANSI: git's auto rule is "stdout is a terminal, or we
    // are paging to one" — `pager::maybe_setup` records the latter via the env flag.
    let want_color = match color {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => {
            std::io::stdout().is_terminal()
                || matches!(
                    std::env::var("GIT_PAGER_IN_USE").ok().as_deref(),
                    Some("true" | "1" | "yes" | "on")
                )
        }
    };
    // `%d`/`%D` need a commit→refs map; build it only when the format asks for one
    // so plain formats pay nothing for the ref scan.
    let decorations = if pretty_uses_decoration(&pretty) {
        Some(build_decorations(&repo)?)
    } else {
        None
    };
    // Relative dates (`%cr`/`%ar`, `--date=relative`) are measured against now.
    let now = now_secs();

    // git emits one terminated record per commit for any non-empty format, even
    // when a given commit expands to nothing (e.g. `%d` on an undecorated commit).
    // Only the genuinely empty user format (`--pretty=`, `tformat:`) emits nothing.
    let empty_user_format = matches!(&pretty, Pretty::User(f) if f.is_empty());

    // `--graph` needs every commit's block up front to lay out the columns, so it
    // buffers; every other format streams commit-by-commit (see the write below).
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut stdout = std::io::stdout().lock();
    let mut first = true;
    for node in &nodes {
        let commit = repo.find_object(node.id)?.try_into_commit()?;
        // `--parents` decorates the header with the commit's own parent ids.
        let extra = if show_parents {
            let mut e = Vec::new();
            for p in &node.parents {
                e.push(b' ');
                let pid = p.attach(&repo);
                if abbrev_commit {
                    e.extend_from_slice(pid.shorten_or_id().to_string().as_bytes());
                } else {
                    e.extend_from_slice(pid.to_string().as_bytes());
                }
            }
            e
        } else {
            Vec::new()
        };
        let ctx = RenderCtx {
            abbrev_commit,
            date_mode,
            extra,
            want_color,
            now,
            decorations: decorations.as_ref(),
        };
        let mut block: Vec<u8> = Vec::new();
        render_entry(&mut block, &commit, &pretty, &ctx)?;
        // A `tformat:` record is terminated by a newline. git still terminates a
        // record whose expansion happened to be empty (so `%d` prints one line per
        // commit); only the genuinely empty user format emits no terminator.
        if terminator && !empty_user_format {
            block.push(b'\n');
        }

        if (want_names || emit_patch) && node.parents.len() < 2 {
            let mut diff: Vec<u8> = Vec::new();
            if want_names {
                // `--name-only`/`--name-status` are the reported format when
                // present; git suppresses the count formats in that case, so the
                // blob reads they need are skipped too.
                let count_formats = (stat || numstat || shortstat) && !name_only && !name_status;
                let files =
                    collect_changes(&repo, &commit, node.parents.first().copied(), count_formats)?;
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
                } else {
                    // git stacks the count formats in a fixed order: numstat, then
                    // the full stat block, then a bare shortstat summary if stat did
                    // not already print one.
                    if numstat {
                        emit_numstat(&mut diff, &files);
                    }
                    if stat {
                        emit_stat(&mut diff, &files)?;
                    } else if shortstat {
                        emit_shortstat(&mut diff, &files)?;
                    }
                }
            }
            if emit_patch {
                // The full patch, rendered by the same pipeline as `git diff` so
                // the two agree byte-for-byte. git separates a preceding count
                // format from the patch with a blank line.
                let p = super::diff::commit_patch(
                    &repo,
                    &commit,
                    node.parents.first().copied(),
                    3,
                )?;
                if !p.is_empty() {
                    if !diff.is_empty() {
                        diff.push(b'\n');
                    }
                    diff.extend_from_slice(&p);
                }
            }
            if !diff.is_empty() {
                // git puts a separator between the log message and the diff for
                // every format but `oneline` — and only when the message block
                // rendered something to separate from. A `--stat` block shown
                // together with `-p` is fenced off with a `---` line; every other
                // diff format uses a plain blank line.
                if !matches!(pretty, Pretty::Oneline) && !block.is_empty() {
                    if stat && emit_patch {
                        block.extend_from_slice(b"---\n");
                    } else {
                        block.push(b'\n');
                    }
                }
                block.extend_from_slice(&diff);
            }
        }
        if graph {
            // Buffer for the column layout, which spans all commits at once.
            blocks.push(block);
            continue;
        }

        // Stream this commit's block immediately, so `git log -p | head` stops
        // after a commit or two instead of computing every patch first. A
        // `format:`/built-in (separator) format precedes every record but the
        // first with a blank line; a `tformat:` record was already terminated
        // above, so no separator is inserted.
        let mut piece: Vec<u8> = Vec::new();
        if !terminator && !first {
            piece.push(b'\n');
        }
        piece.extend_from_slice(&block);
        first = false;
        // Each block ends in a newline, so the line-buffered stdout flushes it here;
        // a closed downstream pipe (`| head`) surfaces as a BrokenPipe on this write,
        // which is a normal stop rather than an error. No per-commit flush is needed.
        if let Err(e) = stdout.write_all(&piece) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(ExitCode::SUCCESS);
            }
            return Err(e.into());
        }
    }

    if graph {
        // `format:` separates records with a newline; `tformat:` already
        // terminated each block above.
        if !terminator {
            let last = blocks.len().saturating_sub(1);
            for (idx, block) in blocks.iter_mut().enumerate() {
                if idx != last {
                    block.push(b'\n');
                }
            }
        }
        let out = render_graph(&nodes, &blocks)?;
        match stdout.write_all(&out) {
            Ok(()) => Ok(ExitCode::SUCCESS),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
            Err(e) => Err(e.into()),
        }
    } else {
        // Flush the tail: a block that did not end in a newline (an empty user
        // format) may still be buffered.
        match stdout.flush() {
            Ok(()) => Ok(ExitCode::SUCCESS),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
            Err(e) => Err(e.into()),
        }
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

/// A non-negative base-10 integer (`--skip`, `--min-parents`, `--max-parents`).
/// `None` for anything git would reject with `fatal: '<value>': not an integer`.
fn parse_nonneg(value: &str) -> Option<usize> {
    match parse_int(value) {
        Some(n) if n >= 0 => Some(n as usize),
        _ => None,
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

/// Breadth-first walk over the reachable history, newest commit first. With
/// `first_parent`, only the first parent of each commit is followed — git's
/// `--first-parent`.
fn walk(repo: &gix::Repository, tips: &[ObjectId], first_parent: bool) -> Result<Vec<Node>> {
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
        let parents: &[ObjectId] = if first_parent {
            &node.parents[..node.parents.len().min(1)]
        } else {
            &node.parents
        };
        for parent in parents {
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
    /// `medium` without the `Date` line, and only the subject.
    Short,
    /// `commit`/`Merge`/`Author`/`Commit` and the full indented message.
    Full,
    /// `full` plus `AuthorDate`/`CommitDate` lines.
    Fuller,
    /// The raw object header: `tree`/`parent`/`author`/`committer`.
    Raw,
    /// `<abbrev> (<subject>, <short-date>)` on one line.
    Reference,
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
        "full" => Ok(Some((Pretty::Full, false))),
        "fuller" => Ok(Some((Pretty::Fuller, false))),
        "raw" => Ok(Some((Pretty::Raw, false))),
        "reference" => Ok(Some((Pretty::Reference, true))),
        // `email`/`mboxrd` need the full mailbox/`From ` framing git's format-patch
        // machinery produces; surfaced terse rather than faked.
        "email" | "mboxrd" => {
            bail!("pretty format {spec:?} is not ported")
        }
        _ => Ok(None),
    }
}

/// Reject any placeholder [`expand_format`] does not implement, so an unsupported
/// format fails loudly instead of expanding to something plausible but wrong.
///
/// `%C` is always accepted: like git, an unrecognized color word after it renders
/// literally rather than erroring, and its `(...)` argument is ordinary text the
/// outer scan skips. `%d`/`%D` are the ref decorations.
fn check_format(fmt: &str) -> Result<()> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            continue;
        }
        match it.next() {
            Some(
                'H' | 'h' | 'T' | 't' | 'P' | 'p' | 's' | 'b' | 'B' | 'f' | 'n' | '%' | 'C' | 'd'
                | 'D',
            ) => {}
            Some('a') => match it.next() {
                Some('n' | 'e' | 'd' | 'i' | 'I' | 't' | 'r') => {}
                Some(x) => bail!("unsupported format placeholder %a{x}"),
                None => bail!("unsupported trailing % in format"),
            },
            Some('c') => match it.next() {
                Some('n' | 'e' | 'd' | 'i' | 'I' | 't' | 'r') => {}
                Some(x) => bail!("unsupported format placeholder %c{x}"),
                None => bail!("unsupported trailing % in format"),
            },
            Some(x) => bail!("unsupported format placeholder %{x}"),
            None => bail!("unsupported trailing % in format"),
        }
    }
    Ok(())
}

/// Expand the placeholders accepted by [`check_format`] for `commit`, using the
/// render knobs in `ctx` (`--date=`, color enablement, decorations, and the clock
/// for relative dates).
fn expand_format(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    fmt: &str,
    ctx: &RenderCtx<'_>,
) -> Result<()> {
    let date_mode = ctx.date_mode;
    // `%C(auto)` latches auto-coloring on for the placeholders that follow it —
    // notably `%d`/`%D`, which stay uncolored until it appears (matching git).
    let mut auto = false;
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '%' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        let Some(&p) = chars.get(i) else { break };
        i += 1;
        match p {
            // Under `%C(auto)`, git paints the commit hash magenta (`\e[35m`).
            'H' => push_maybe_auto(out, &commit.id().to_string(), auto && ctx.want_color),
            'h' => push_maybe_auto(
                out,
                &commit.id().shorten_or_id().to_string(),
                auto && ctx.want_color,
            ),
            'T' => out.extend_from_slice(commit.tree_id()?.to_string().as_bytes()),
            't' => {
                out.extend_from_slice(commit.tree_id()?.shorten_or_id().to_string().as_bytes());
            }
            'P' => write_parents(out, commit, false),
            'p' => write_parents(out, commit, true),
            's' => out.extend_from_slice(&subject(commit.message_raw()?)),
            'b' => out.extend_from_slice(&body(commit.message_raw()?)),
            'B' => out.extend_from_slice(commit.message_raw()?),
            'f' => out.extend_from_slice(&sanitized_subject(&subject(commit.message_raw()?))),
            'n' => out.push(b'\n'),
            '%' => out.push(b'%'),
            'C' => expand_color(out, &chars, &mut i, ctx.want_color, &mut auto),
            'd' => expand_decoration(out, commit, ctx, auto, true),
            'D' => expand_decoration(out, commit, ctx, auto, false),
            'a' => {
                let author = commit.author()?;
                match chars.get(i).copied() {
                    Some('n') => out.extend_from_slice(author.name),
                    Some('e') => out.extend_from_slice(author.email),
                    Some('d') => expand_date(out, &author, date_mode, ctx.now)?,
                    Some('i') => expand_date(out, &author, DateMode::Iso, ctx.now)?,
                    Some('I') => expand_date(out, &author, DateMode::IsoStrict, ctx.now)?,
                    Some('r') => expand_date(out, &author, DateMode::Relative, ctx.now)?,
                    Some('t') => write!(out, "{}", author.time()?.seconds)?,
                    _ => unreachable!("check_format rejected this already"),
                }
                i += 1;
            }
            'c' => {
                let committer = commit.committer()?;
                match chars.get(i).copied() {
                    Some('n') => out.extend_from_slice(committer.name),
                    Some('e') => out.extend_from_slice(committer.email),
                    Some('d') => expand_date(out, &committer, date_mode, ctx.now)?,
                    Some('i') => expand_date(out, &committer, DateMode::Iso, ctx.now)?,
                    Some('I') => expand_date(out, &committer, DateMode::IsoStrict, ctx.now)?,
                    Some('r') => expand_date(out, &committer, DateMode::Relative, ctx.now)?,
                    Some('t') => write!(out, "{}", committer.time()?.seconds)?,
                    _ => unreachable!("check_format rejected this already"),
                }
                i += 1;
            }
            _ => unreachable!("check_format rejected this already"),
        }
    }
    Ok(())
}

/// Expand a `%C…` color placeholder starting just past the `C` (index `i` points
/// at the first following char). Advances `i` over whatever the placeholder
/// consumes. Recognizes git's `%Cred`/`%Cgreen`/`%Cblue`/`%Creset` shortcuts and
/// the general `%C(<spec>)` form; anything else leaves `%C` rendered literally.
fn expand_color(out: &mut Vec<u8>, chars: &[char], i: &mut usize, want_color: bool, auto: &mut bool) {
    // git suppresses the `%C(auto)` reset when nothing has been emitted yet for
    // this commit's format, so record that before appending anything.
    let out_empty = out.is_empty();
    let rest: String = chars[*i..].iter().collect();
    // `%C(<spec>)`
    if rest.starts_with('(') {
        if let Some(close) = rest.find(')') {
            let spec = &rest[1..close];
            out.extend_from_slice(parse_color_spec(spec, want_color, auto, out_empty).as_bytes());
            *i += close + 1; // consume through ')'
            return;
        }
        // No closing paren: git prints the rest verbatim. Fall through to literal.
    }
    // Shortcuts.
    for (name, ansi) in [
        ("red", "\x1b[31m"),
        ("green", "\x1b[32m"),
        ("blue", "\x1b[34m"),
        ("reset", "\x1b[m"),
    ] {
        if rest.starts_with(name) {
            if want_color {
                out.extend_from_slice(ansi.as_bytes());
            }
            *i += name.len();
            return;
        }
    }
    // Unrecognized: git renders the `%C` literally and continues.
    out.extend_from_slice(b"%C");
}

/// Parse a `%C(<spec>)` color specification into an ANSI escape (empty when color
/// is disabled). Handles `reset`, `auto`/`auto,<colors>` (which also latches the
/// auto-color flag on), attribute words (`bold`, `dim`, `ul`, …), and up to two
/// color names (foreground then background).
fn parse_color_spec(spec: &str, want_color: bool, auto: &mut bool, out_empty: bool) -> String {
    let spec = spec.trim();
    let colors = if let Some(rest) = spec.strip_prefix("auto") {
        // `%C(auto)` alone enables auto-coloring and emits a reset — but git omits
        // that reset at the very start of a commit's output. `%C(auto,<colors>)`
        // additionally applies those colors.
        *auto = true;
        let rest = rest.strip_prefix(',').unwrap_or(rest).trim();
        if rest.is_empty() {
            return if want_color && !out_empty {
                "\x1b[m".to_string()
            } else {
                String::new()
            };
        }
        rest
    } else {
        spec
    };
    if !want_color {
        return String::new();
    }
    if colors == "reset" {
        return "\x1b[m".to_string();
    }
    let mut codes: Vec<String> = Vec::new();
    let mut foreground = true;
    for tok in colors.split_whitespace() {
        let attr = match tok {
            "bold" => Some("1"),
            "dim" => Some("2"),
            "italic" => Some("3"),
            "ul" | "underline" => Some("4"),
            "blink" => Some("5"),
            "reverse" => Some("7"),
            "strike" => Some("9"),
            "nobold" | "no-bold" => Some("22"),
            _ => None,
        };
        if let Some(a) = attr {
            codes.push(a.to_string());
        } else if let Some(base) = color_base(tok) {
            codes.push((if foreground { base } else { base + 10 }).to_string());
            foreground = false;
        }
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// Emit `text`, painted magenta when `colored` (git's `%C(auto)` color for the
/// commit hash `%h`/`%H`).
fn push_maybe_auto(out: &mut Vec<u8>, text: &str, colored: bool) {
    if colored {
        out.extend_from_slice(b"\x1b[35m");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[m");
    } else {
        out.extend_from_slice(text.as_bytes());
    }
}

/// Map a color name to its SGR foreground base code (background is `+10`).
fn color_base(name: &str) -> Option<u8> {
    Some(match name {
        "black" => 30,
        "red" => 31,
        "green" => 32,
        "yellow" => 33,
        "blue" => 34,
        "magenta" => 35,
        "cyan" => 36,
        "white" => 37,
        "default" | "normal" => 39,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Decorations (%d / %D)
// ---------------------------------------------------------------------------

/// The kinds of ref a commit can be decorated with, in git's color scheme.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DecoKind {
    Tag,
    LocalBranch,
    RemoteBranch,
}

/// One ref pointing at a commit: `origin/main`, `v1.0`, `main`, …
struct Deco {
    kind: DecoKind,
    name: String,
}

/// The ref→commit map plus HEAD state needed to render `%d`/`%D`.
struct Decorations {
    /// Commit oid → the branch/tag/remote refs pointing at it (annotated tags
    /// peeled through to their commit).
    map: HashMap<ObjectId, Vec<Deco>>,
    /// The commit HEAD resolves to (peeled), or `None` when HEAD is unborn.
    head_oid: Option<ObjectId>,
    /// The branch HEAD symbolically points to, for the `HEAD -> <branch>` form.
    head_branch: Option<String>,
}

/// Does this format use a decoration placeholder, so the ref map is worth
/// building? `%%d` (an escaped percent then a literal `d`) does not count.
fn pretty_uses_decoration(pretty: &Pretty) -> bool {
    let Pretty::User(fmt) = pretty else {
        return false;
    };
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c == '%' && matches!(it.next(), Some('d' | 'D')) {
            return true;
        }
    }
    false
}

/// Build the commit→refs decoration map: every branch, remote-tracking branch,
/// and tag (peeled to its commit), plus where HEAD points.
fn build_decorations(repo: &gix::Repository) -> Result<Decorations> {
    let mut map: HashMap<ObjectId, Vec<Deco>> = HashMap::new();
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow!("{e}"))?;
        let Ok(full) = r.name().as_bstr().to_str().map(str::to_owned) else {
            continue;
        };
        let kind_name = if let Some(n) = full.strip_prefix("refs/heads/") {
            (DecoKind::LocalBranch, n.to_string())
        } else if let Some(n) = full.strip_prefix("refs/remotes/") {
            (DecoKind::RemoteBranch, n.to_string())
        } else if let Some(n) = full.strip_prefix("refs/tags/") {
            (DecoKind::Tag, n.to_string())
        } else {
            // HEAD, refs/stash, refs/notes/* and friends aren't shown by `%d`.
            continue;
        };
        // Peel through annotated tags so a tag ref decorates its target commit.
        let Ok(id) = r.into_fully_peeled_id() else {
            continue;
        };
        map.entry(id.detach()).or_default().push(Deco {
            kind: kind_name.0,
            name: kind_name.1,
        });
    }

    let mut head_oid = None;
    let mut head_branch = None;
    if let Ok(head) = repo.head() {
        if let Some(name) = head.referent_name() {
            if let Ok(full) = name.as_bstr().to_str() {
                if let Some(b) = full.strip_prefix("refs/heads/") {
                    head_branch = Some(b.to_string());
                }
            }
        }
        head_oid = head.id().map(|id| id.detach());
    }

    Ok(Decorations {
        map,
        head_oid,
        head_branch,
    })
}

/// Expand `%d` (`wrap` true: ` (…)`) or `%D` (`wrap` false: bare) for `commit`.
/// Colored only when `auto` (set by a preceding `%C(auto)`) and color is enabled,
/// matching git, whose decorations stay plain until `%C(auto)` appears.
fn expand_decoration(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    ctx: &RenderCtx<'_>,
    auto: bool,
    wrap: bool,
) {
    let Some(decos) = ctx.decorations else {
        return;
    };
    let id = commit.id().detach();
    let head_here = decos.head_oid == Some(id);
    let refs_here = decos.map.get(&id);
    if !head_here && refs_here.map_or(true, |v| v.is_empty()) {
        return;
    }

    let color_on = auto && ctx.want_color;
    const RESET: &str = "\x1b[m";
    let paint = |text: &str, code: &str| -> String {
        if color_on {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    };
    // git colors: HEAD bold cyan, local branch bold green, remote bold red, tag
    // bold yellow, all punctuation plain magenta.
    let punct = |text: &str| paint(text, "\x1b[35m");

    // The branch HEAD points to is folded into `HEAD -> <branch>`, so it is not
    // also listed on its own.
    let head_branch = if head_here { decos.head_branch.as_deref() } else { None };

    let mut entries: Vec<String> = Vec::new();
    if head_here {
        match head_branch {
            Some(b) => {
                entries.push(format!(
                    "{}{}{}",
                    paint("HEAD", "\x1b[1;36m"),
                    punct(" -> "),
                    paint(b, "\x1b[1;32m")
                ));
            }
            None => entries.push(paint("HEAD", "\x1b[1;36m")),
        }
    }

    if let Some(refs) = refs_here {
        // git's display order: local branches, then tags, then remote branches;
        // remote `*/HEAD` symrefs sort after real remote branches.
        let mut locals: Vec<&Deco> = refs
            .iter()
            .filter(|d| d.kind == DecoKind::LocalBranch && Some(d.name.as_str()) != head_branch)
            .collect();
        let mut tags: Vec<&Deco> = refs.iter().filter(|d| d.kind == DecoKind::Tag).collect();
        let mut remotes: Vec<&Deco> =
            refs.iter().filter(|d| d.kind == DecoKind::RemoteBranch).collect();
        locals.sort_by(|a, b| a.name.cmp(&b.name));
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        remotes.sort_by(|a, b| {
            (a.name.ends_with("/HEAD"), &a.name).cmp(&(b.name.ends_with("/HEAD"), &b.name))
        });
        for d in locals {
            entries.push(paint(&d.name, "\x1b[1;32m"));
        }
        for d in tags {
            // git colors the `tag: ` prefix and the tag name as two separate spans
            // (`\e[1;33mtag: \e[m\e[1;33m<name>\e[m`), both bold yellow.
            entries.push(format!(
                "{}{}",
                paint("tag: ", "\x1b[1;33m"),
                paint(&d.name, "\x1b[1;33m")
            ));
        }
        for d in remotes {
            entries.push(paint(&d.name, "\x1b[1;31m"));
        }
    }

    if entries.is_empty() {
        return;
    }

    // `%d` wraps in ` (…)`; `%D` emits the bare, comma-separated list.
    if wrap {
        out.extend_from_slice(punct(" (").as_bytes());
    }
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(punct(", ").as_bytes());
        }
        out.extend_from_slice(e.as_bytes());
    }
    if wrap {
        out.extend_from_slice(punct(")").as_bytes());
    }
}

/// Current time in epoch seconds, for relative dates.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// git's `show_date_relative`: render `then` as "N units ago" relative to `now`,
/// with the same unit thresholds and rounding.
fn format_relative(then: i64, now: i64) -> String {
    if now < then {
        return "in the future".to_string();
    }
    let mut diff = (now - then) as u64;
    if diff < 90 {
        return unit_ago(diff, "second");
    }
    diff = (diff + 30) / 60; // minutes
    if diff < 90 {
        return unit_ago(diff, "minute");
    }
    diff = (diff + 30) / 60; // hours
    if diff < 36 {
        return unit_ago(diff, "hour");
    }
    diff = (diff + 12) / 24; // days
    if diff < 14 {
        return unit_ago(diff, "day");
    }
    if diff < 70 {
        return unit_ago((diff + 3) / 7, "week");
    }
    if diff < 365 {
        return unit_ago((diff + 15) / 30, "month");
    }
    // Years and months for the first ~5 years, then years alone.
    if diff < 1825 {
        let totalmonths = diff * 12 * 10 / 365;
        let years = totalmonths / 120;
        let months = (totalmonths % 120) / 10;
        if months > 0 {
            return format!("{}, {} ago", unit(years, "year"), unit(months, "month"));
        }
        return unit_ago(years, "year");
    }
    unit_ago((diff + 183) / 365, "year")
}

/// `"N unit ago"` / `"N units ago"` with git's singular/plural rule.
fn unit_ago(n: u64, name: &str) -> String {
    format!("{} ago", unit(n, name))
}

/// `"1 unit"` or `"N units"`.
fn unit(n: u64, name: &str) -> String {
    if n == 1 {
        format!("1 {name}")
    } else {
        format!("{n} {name}s")
    }
}

/// Write a signature's timestamp in `mode`, the shared body of `%ad`/`%cd` and
/// their fixed-format `%ai`/`%aI` cousins.
fn expand_date(
    out: &mut Vec<u8>,
    sig: &gix::actor::SignatureRef<'_>,
    mode: DateMode,
    now: i64,
) -> Result<()> {
    let t = sig.time()?;
    out.extend_from_slice(fmt_time(t.seconds, t.offset, mode, now).as_bytes());
    Ok(())
}

/// Format a timestamp, routing the clock-relative `relative` mode (which needs
/// `now`) to [`format_relative`] and everything else to [`format_date`].
fn fmt_time(seconds: i64, offset: i32, mode: DateMode, now: i64) -> String {
    match mode {
        DateMode::Relative => format_relative(seconds, now),
        other => format_date(seconds, offset, other),
    }
}

/// git's `%b`: the message body — everything after the blank line that ends the
/// subject paragraph. An empty string when the message is a subject only.
fn body(msg: &[u8]) -> Vec<u8> {
    // Skip leading blank lines, then the subject paragraph, then the single blank
    // line separating it from the body.
    let mut rest = msg;
    while let Some(stripped) = rest.strip_prefix(b"\n") {
        rest = stripped;
    }
    match rest.windows(2).position(|w| w == b"\n\n") {
        Some(pos) => rest[pos + 2..].to_vec(),
        None => Vec::new(),
    }
}

/// git's `%f`: the subject sanitised into a filename — `istitlechar` bytes
/// (alphanumeric, `.`, `_`) kept, every other run folded to a single `-`, runs of
/// `.` collapsed, and trailing `.` trimmed.
fn sanitized_subject(subj: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    // 2 = at start, 1 = a separator run is pending, 0 = mid-word.
    let mut space: u8 = 2;
    let mut i = 0;
    while i < subj.len() {
        let c = subj[i];
        if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' {
            if space == 1 {
                out.push(b'-');
            }
            space = 0;
            out.push(c);
            if c == b'.' {
                while i + 1 < subj.len() && subj[i + 1] == b'.' {
                    i += 1;
                }
            }
        } else {
            space |= 1;
        }
        i += 1;
    }
    while out.last() == Some(&b'.') {
        out.pop();
    }
    out
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

/// The per-commit rendering knobs threaded down from [`log`].
struct RenderCtx<'a> {
    /// `--abbrev-commit`: shorten the commit id on the header/oneline.
    abbrev_commit: bool,
    /// `--date=`: the format `%ad`/`%cd` and the `Date`/`*Date` lines follow.
    date_mode: DateMode,
    /// `--parents`: the commit's own parent ids, decorating the header/oneline.
    /// Empty when the flag is off. Full-length ids unless `abbrev_commit`.
    extra: Vec<u8>,
    /// Whether `%C`/`%C(...)` color placeholders and `%C(auto)`-gated decoration
    /// emit ANSI escapes (git's `want_color`).
    want_color: bool,
    /// Current time in epoch seconds, for relative dates (`%cr`/`%ar`).
    now: i64,
    /// Commit→refs map plus HEAD info for `%d`/`%D`; `None` when the format has no
    /// decoration placeholder.
    decorations: Option<&'a Decorations>,
}

/// Render one commit's header in the selected format. Built-in formats end with
/// a newline; user formats, `oneline`, and `reference` do not, because their
/// record ending is supplied by the separator/terminator rule in [`log`].
fn render_entry(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    pretty: &Pretty,
    ctx: &RenderCtx<'_>,
) -> Result<()> {
    let id = if ctx.abbrev_commit {
        commit.id().shorten_or_id().to_string()
    } else {
        commit.id().to_string()
    };

    match pretty {
        Pretty::Oneline => {
            out.extend_from_slice(id.as_bytes());
            out.extend_from_slice(&ctx.extra);
            out.push(b' ');
            out.extend_from_slice(&subject(commit.message_raw()?));
        }
        Pretty::Reference => {
            // `%h (%s, %ad)` with `--date=short` unless `--date=` overrode it.
            let date_mode = match ctx.date_mode {
                DateMode::Default => DateMode::Short,
                other => other,
            };
            let author = commit.author()?;
            let t = author.time()?;
            out.extend_from_slice(commit.id().shorten_or_id().to_string().as_bytes());
            out.extend_from_slice(b" (");
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.extend_from_slice(b", ");
            out.extend_from_slice(fmt_time(t.seconds, t.offset, date_mode, ctx.now).as_bytes());
            out.push(b')');
        }
        Pretty::User(fmt) => expand_format(out, commit, fmt, ctx)?,
        Pretty::Raw => {
            let author = commit.author()?;
            let committer = commit.committer()?;
            out.extend_from_slice(b"commit ");
            // Raw always shows the full commit id; `--parents` still decorates it.
            out.extend_from_slice(commit.id().to_string().as_bytes());
            out.extend_from_slice(&ctx.extra);
            out.push(b'\n');
            writeln!(out, "tree {}", commit.tree_id()?)?;
            for pid in commit.parent_ids() {
                writeln!(out, "parent {pid}")?;
            }
            write_raw_ident(out, b"author", &author)?;
            write_raw_ident(out, b"committer", &committer)?;
            out.push(b'\n');
            indent_message(out, commit.message_raw()?);
        }
        Pretty::Medium | Pretty::Short | Pretty::Full | Pretty::Fuller => {
            let author = commit.author()?;
            out.extend_from_slice(b"commit ");
            out.extend_from_slice(id.as_bytes());
            out.extend_from_slice(&ctx.extra);
            out.push(b'\n');

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

            match pretty {
                Pretty::Fuller => {
                    let committer = commit.committer()?;
                    let at = author.time()?;
                    let ct = committer.time()?;
                    write_person(out, b"Author:     ", &author);
                    writeln!(
                        out,
                        "AuthorDate: {}",
                        fmt_time(at.seconds, at.offset, ctx.date_mode, ctx.now)
                    )?;
                    write_person(out, b"Commit:     ", &committer);
                    writeln!(
                        out,
                        "CommitDate: {}",
                        fmt_time(ct.seconds, ct.offset, ctx.date_mode, ctx.now)
                    )?;
                }
                Pretty::Full => {
                    let committer = commit.committer()?;
                    write_person(out, b"Author: ", &author);
                    write_person(out, b"Commit: ", &committer);
                }
                _ => {
                    // medium / short
                    write_person(out, b"Author: ", &author);
                    if matches!(pretty, Pretty::Medium) {
                        let time = author.time()?;
                        writeln!(
                            out,
                            "Date:   {}",
                            fmt_time(time.seconds, time.offset, ctx.date_mode, ctx.now)
                        )?;
                    }
                }
            }
            out.push(b'\n');

            if matches!(pretty, Pretty::Short) {
                // `short` shows only the subject, indented four spaces.
                out.extend_from_slice(b"    ");
                out.extend_from_slice(&subject(commit.message_raw()?));
                out.push(b'\n');
            } else {
                indent_message(out, commit.message_raw()?);
            }
        }
    }
    Ok(())
}

/// Write git's `<label> <name> <<email>>` header line.
fn write_person(out: &mut Vec<u8>, label: &[u8], sig: &gix::actor::SignatureRef<'_>) {
    out.extend_from_slice(label);
    out.extend_from_slice(sig.name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(sig.email);
    out.extend_from_slice(b">\n");
}

/// Write a raw-format identity line: `<role> <name> <<email>> <seconds> +ZZZZ`.
fn write_raw_ident(out: &mut Vec<u8>, role: &[u8], sig: &gix::actor::SignatureRef<'_>) -> Result<()> {
    let t = sig.time()?;
    let (sign, off) = if t.offset < 0 { ('-', -t.offset) } else { ('+', t.offset) };
    out.extend_from_slice(role);
    out.push(b' ');
    out.extend_from_slice(sig.name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(sig.email);
    out.push(b'>');
    writeln!(
        out,
        " {} {sign}{:02}{:02}",
        t.seconds,
        off / 3600,
        (off % 3600) / 60
    )?;
    Ok(())
}

/// Indent a commit message four spaces per line, exactly as git's `pp_remainder`:
/// every line — blank ones included — is prefixed, and trailing blank lines are
/// dropped.
fn indent_message(out: &mut Vec<u8>, msg: &[u8]) {
    let mut lines: Vec<&[u8]> = msg.split(|&b| b == b'\n').collect();
    while lines.last() == Some(&&b""[..]) {
        lines.pop();
    }
    for line in lines {
        out.extend_from_slice(b"    ");
        out.extend_from_slice(line);
        out.push(b'\n');
    }
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

    write_stat_summary(out, files.len(), total_added, total_deleted)
}

/// git's `--stat`/`--shortstat` summary line: ` N files changed, A insertions(+),
/// D deletions(-)`, with the `insertions`/`deletions` clauses appearing on git's
/// same conditions.
fn write_stat_summary(
    out: &mut Vec<u8>,
    n: usize,
    total_added: usize,
    total_deleted: usize,
) -> Result<()> {
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

/// git's `--numstat`: `<added>\t<deleted>\t<path>` per file, with `-\t-` for a
/// binary file whose line counts are undefined.
fn emit_numstat(out: &mut Vec<u8>, files: &[FileChange]) {
    for f in files {
        if f.is_binary {
            out.extend_from_slice(b"-\t-\t");
        } else {
            out.extend_from_slice(format!("{}\t{}\t", f.added, f.deleted).as_bytes());
        }
        out.extend_from_slice(&f.path);
        out.push(b'\n');
    }
}

/// git's `--shortstat`: the `--stat` summary line only. Binary files contribute
/// nothing to the insertion/deletion totals, exactly as the full stat block.
fn emit_shortstat(out: &mut Vec<u8>, files: &[FileChange]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut total_added = 0usize;
    let mut total_deleted = 0usize;
    for f in files {
        if f.is_binary {
            continue;
        }
        total_added += f.added;
        total_deleted += f.deleted;
    }
    write_stat_summary(out, files.len(), total_added, total_deleted)
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

/// The `--date=` output modes this port renders byte-for-byte, plus `relative`,
/// which is measured against the current wall clock. The remaining process-time /
/// zone-dependent modes (`human`, `local`) are still rejected rather than faked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DateMode {
    /// git's `DATE_NORMAL`: `Www Mmm D HH:MM:SS YYYY +ZZZZ`.
    Default,
    /// `short`: `YYYY-MM-DD`.
    Short,
    /// `iso`/`iso8601`: `YYYY-MM-DD HH:MM:SS +ZZZZ`.
    Iso,
    /// `iso-strict`/`iso8601-strict`: `YYYY-MM-DDTHH:MM:SS+ZZ:ZZ`.
    IsoStrict,
    /// `rfc`/`rfc2822`: `Www, D Mmm YYYY HH:MM:SS +ZZZZ`.
    Rfc,
    /// `unix`: the raw epoch seconds, no timezone.
    Unix,
    /// `raw`: `<seconds> +ZZZZ`.
    Raw,
    /// `relative`: `N <unit> ago`, measured against the current time.
    Relative,
}

/// `--color=<when>` (and `--color`/`--no-color`): whether `%C`/`%d` emit ANSI.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorWhen {
    Always,
    Never,
    /// Color when stdout is a terminal (or we are paging to one).
    Auto,
}

/// Map a `--date=` value to a [`DateMode`]. `None` for a value git accepts but
/// this port renders time/zone-dependently (surfaced terse) or does not know.
fn parse_date_mode(spec: &str) -> Option<DateMode> {
    Some(match spec {
        "default" | "normal" => DateMode::Default,
        "short" => DateMode::Short,
        "iso" | "iso8601" => DateMode::Iso,
        "iso-strict" | "iso8601-strict" => DateMode::IsoStrict,
        "rfc" | "rfc2822" => DateMode::Rfc,
        "unix" => DateMode::Unix,
        "raw" => DateMode::Raw,
        "relative" => DateMode::Relative,
        _ => return None,
    })
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Format a timestamp in the requested [`DateMode`], matching git byte-for-byte.
fn format_date(seconds: i64, offset: i32, mode: DateMode) -> String {
    match mode {
        DateMode::Default => format_git_date(seconds, offset),
        // Relative dates need the current time; callers route them through
        // `fmt_time`, but keep this arm self-contained rather than unreachable.
        DateMode::Relative => format_relative(seconds, now_secs()),
        DateMode::Unix => format!("{seconds}"),
        DateMode::Raw => {
            let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
            format!("{seconds} {sign}{:02}{:02}", off / 3600, (off % 3600) / 60)
        }
        DateMode::Short | DateMode::Iso | DateMode::IsoStrict | DateMode::Rfc => {
            let local = seconds + offset as i64;
            let days = local.div_euclid(86_400);
            let secs = local.rem_euclid(86_400);
            let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);
            let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
            let (year, month, day) = civil_from_days(days);
            let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
            let (oh, om) = (off / 3600, (off % 3600) / 60);
            match mode {
                DateMode::Short => format!("{year}-{month:02}-{day:02}"),
                DateMode::Iso => format!(
                    "{year}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02} {sign}{oh:02}{om:02}"
                ),
                DateMode::IsoStrict => format!(
                    "{year}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}{sign}{oh:02}:{om:02}"
                ),
                DateMode::Rfc => format!(
                    "{}, {day} {} {year} {hour:02}:{min:02}:{sec:02} {sign}{oh:02}{om:02}",
                    WEEKDAYS[weekday],
                    MONTHS[(month - 1) as usize],
                ),
                _ => unreachable!(),
            }
        }
    }
}

/// Format a commit time exactly like stock `git log`'s default (`DATE_NORMAL`)
/// mode: `Www Mmm <sp-padded-day> HH:MM:SS YYYY +ZZZZ`, in the commit's own
/// timezone offset. Done by hand because gix's exported `DEFAULT` format uses an
/// unpadded day (`%-d`) whereas git space-pads it (`%e`); nothing else in the
/// crate lets us construct a custom format string.
fn format_git_date(seconds: i64, offset: i32) -> String {
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
