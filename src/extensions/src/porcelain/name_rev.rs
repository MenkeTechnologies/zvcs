//! `git name-rev` — find symbolic names for given revs.
//!
//! This is a faithful port of git's `builtin/name-rev.c`: the tip table and its
//! `cmp_by_tag_and_age` ordering, the LIFO first-parent-first walk in `name_rev`,
//! the `effective_distance` / `is_better_name` preference rules (including the
//! 65535 merge-traversal weight), the commit-date cutoff with its one-day slop,
//! and the `~<n>` / `^<n>` / `^0` name composition. Output is byte-identical to
//! stock git for the covered forms.
//!
//! Covered: `<commit-ish>...`, `--tags`, `--refs=<pattern>`, `--exclude=<pattern>`,
//! `--no-refs`, `--no-exclude`, `--name-only`, `--annotate-stdin` (and the
//! deprecated `--stdin`), `--undefined`/`--no-undefined`, `--always`, plus the
//! "Skipping." / `undefined` / `fatal: cannot describe` failure paths and their
//! exit codes (0 / 0 / 128).
//!
//! Not covered, and rejected rather than approximated:
//!   * `--all` — git emits one line per commit in the order of its *internal
//!     parsed-object hash table* (`get_indexed_object`), whose layout depends on
//!     which trees/tags/commits git happened to parse and on the table's growth
//!     history. gitoxide has no such structure, so the line order cannot be
//!     reproduced; any ordering we invented would silently differ.
//!   * `--peel-tag` — an undocumented internal flag with no stable contract.
//!
//! Known deviation: git prefers commit-graph generation numbers over commit
//! dates when deciding the traversal cutoff. gitoxide's walk here is date-based
//! only, so in a repository with a commit-graph *and* badly skewed commit dates
//! the pruned set can differ. Without a commit-graph the two are identical.

use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufWriter, Write};
use std::process::ExitCode;
use std::rc::Rc;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::objs::Kind;
use gix::prelude::ObjectIdExt;

/// How many generations are maximally preferred over _one_ merge traversal.
const MERGE_TRAVERSAL_WEIGHT: i32 = 65535;
/// One day of slop on the date cutoff, to tolerate slight clock skew.
const CUTOFF_DATE_SLOP: i64 = 86400;
/// git's `TIME_MAX` sentinel for "no tagger date seen yet".
const TIME_MAX: i64 = i64::MAX;

/// git's `ref_rev_parse_rules`, as (prefix, suffix) pairs around the short name.
const REV_PARSE_RULES: [(&str, &str); 6] = [
    ("", ""),
    ("refs/", ""),
    ("refs/tags/", ""),
    ("refs/heads/", ""),
    ("refs/remotes/", ""),
    ("refs/remotes/", "/HEAD"),
];

/// The exact `usage_with_options` block git prints when the invocation mixes a
/// commit list with `--all`/`--annotate-stdin`.
const USAGE: &str = "\
usage: git name-rev [<options>] <commit>...
   or: git name-rev [<options>] --all
   or: git name-rev [<options>] --annotate-stdin

    --[no-]name-only      print only ref-based names (no object names)
    --[no-]tags           only use tags to name the commits
    --[no-]refs <pattern> only use refs matching <pattern>
    --[no-]exclude <pattern>
                          ignore refs matching <pattern>

    --[no-]all            list all commits reachable from all refs
    --[no-]annotate-stdin annotate text from stdin
    --[no-]undefined      allow to print `undefined` names (default)
    --[no-]always         show abbreviated commit object as fallback
";

/// git's `struct rev_name`: the best name found so far for one commit.
struct RevName {
    /// The tip this name descends from, e.g. `tags/v1` or `master^2`.
    tip_name: Rc<str>,
    taggerdate: i64,
    /// First-parent hops from `tip_name`, rendered as the `~<n>` suffix.
    generation: i32,
    /// Total walk cost, with merges charged `MERGE_TRAVERSAL_WEIGHT`.
    distance: i32,
    from_tag: bool,
}

/// git's `struct tip_table_entry`: one naming source, derived from one ref.
struct Tip {
    /// The object the ref points at, *unpeeled* (used for exact-match lookups).
    oid: ObjectId,
    /// The ref name as git prints it (`master`, `tags/v1`, `remotes/origin/x`).
    refname: String,
    /// The commit the ref peels to, if any; tips without one never name anything.
    commit: Option<ObjectId>,
    taggerdate: i64,
    from_tag: bool,
    /// Whether an annotated tag was dereferenced to reach `commit` (adds `^0`).
    deref: bool,
}

/// Decoded commit facts, cached so each object is read at most once.
struct CommitInfo {
    date: i64,
    parents: Vec<ObjectId>,
}

/// `git name-rev` — see the module docs for the covered surface.
pub fn name_rev(args: &[String]) -> Result<ExitCode> {
    // Tolerate both dispatch conventions (with or without the subcommand at [0]).
    let args = match args.first() {
        Some(a) if a == "name-rev" => &args[1..],
        _ => args,
    };

    let mut name_only = false;
    let mut tags_only = false;
    let mut ref_filters: Vec<String> = Vec::new();
    let mut exclude_filters: Vec<String> = Vec::new();
    let mut all = false;
    let mut annotate_stdin = false;
    let mut allow_undefined = true;
    let mut always = false;
    let mut revs: Vec<String> = Vec::new();

    let mut i = 0;
    let mut no_more_opts = false;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            revs.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "--" => no_more_opts = true,
            "--name-only" => name_only = true,
            "--no-name-only" => name_only = false,
            "--tags" => tags_only = true,
            "--no-tags" => tags_only = false,
            "--all" => all = true,
            "--no-all" => all = false,
            "--annotate-stdin" => annotate_stdin = true,
            "--no-annotate-stdin" => annotate_stdin = false,
            "--stdin" => {
                eprintln!(
                    "warning: --stdin is deprecated. Please use --annotate-stdin instead, \
                     which is functionally equivalent.\n\
                     This option will be removed in a future release."
                );
                annotate_stdin = true;
            }
            "--undefined" => allow_undefined = true,
            "--no-undefined" => allow_undefined = false,
            "--always" => always = true,
            "--no-always" => always = false,
            "--no-refs" => ref_filters.clear(),
            "--no-exclude" => exclude_filters.clear(),
            "--refs" | "--exclude" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("option `{a}` requires a value"))?;
                if a == "--refs" {
                    ref_filters.push(v.clone());
                } else {
                    exclude_filters.push(v.clone());
                }
            }
            _ if a.starts_with("--refs=") => ref_filters.push(a["--refs=".len()..].to_string()),
            _ if a.starts_with("--exclude=") => {
                exclude_filters.push(a["--exclude=".len()..].to_string())
            }
            "--peel-tag" | "--no-peel-tag" => {
                bail!("--peel-tag is an internal git flag and is not ported")
            }
            _ => bail!(
                "unsupported flag {a:?} (ported: --name-only, --tags, --refs, --exclude, \
                 --annotate-stdin, --undefined, --always)"
            ),
        }
        i += 1;
    }

    // git: `if (all + annotate_stdin + !!argc > 1)` -> error + usage, exit 129.
    if (all as u8) + (annotate_stdin as u8) + u8::from(!revs.is_empty()) > 1 {
        eprintln!("error: Specify either a list, or --all, not both!");
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    if all {
        bail!(
            "--all is not ported: git orders its output by the layout of its internal \
             parsed-object hash table, which gitoxide has no equivalent for \
             (ported: <commit-ish>..., --annotate-stdin)"
        );
    }

    let repo = gix::discover(".")?;
    let hexsz = repo.object_hash().len_in_hex();

    // git disables the cutoff entirely for --annotate-stdin (and --all).
    let mut cutoff: i64 = if annotate_stdin { 0 } else { TIME_MAX };

    let mut cache: HashMap<ObjectId, Rc<CommitInfo>> = HashMap::new();
    let mut out = BufWriter::new(std::io::stdout().lock());

    // Resolve the requested revisions, and lower the cutoff to the oldest of them.
    // Unresolvable arguments are reported and skipped; the exit code stays 0.
    let mut targets: Vec<(String, ObjectId, Kind)> = Vec::new();
    for spec in &revs {
        let Ok(id) = repo.rev_parse_single(spec.as_str()) else {
            eprintln!("Could not get sha1 for {spec}. Skipping.");
            continue;
        };
        let oid = id.detach();
        let Ok(object) = repo.find_object(oid) else {
            eprintln!("Could not get object for {spec}. Skipping.");
            continue;
        };
        let kind = object.kind;
        if let Some(commit) = peel_to_commit(&repo, oid) {
            let date = commit_info(&repo, &mut cache, commit).date;
            if cutoff > date {
                cutoff = date;
            }
        }
        targets.push((spec.clone(), oid, kind));
    }

    // Apply the clock-skew slop (git's `adjust_cutoff_timestamp_for_slop`).
    if cutoff != 0 {
        cutoff = cutoff.saturating_sub(CUTOFF_DATE_SLOP);
    }

    let tips = collect_tips(&repo, tags_only, name_only, &ref_filters, &exclude_filters)?;

    // "Try to set better names first, so that worse ones spread less."
    let mut order: Vec<usize> = (0..tips.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&tips[a], &tips[b]);
        b.from_tag
            .cmp(&a.from_tag)
            .then(a.taggerdate.cmp(&b.taggerdate))
    });

    let mut names: HashMap<ObjectId, RevName> = HashMap::new();
    for ix in order {
        let tip = &tips[ix];
        if let Some(commit) = tip.commit {
            walk_from_tip(&repo, &mut cache, &mut names, commit, tip, cutoff);
        }
    }

    // git's `get_exact_ref_match` bsearches the tip table sorted by object id.
    let mut by_oid: Vec<(ObjectId, &str)> =
        tips.iter().map(|t| (t.oid, t.refname.as_str())).collect();
    by_oid.sort_by(|a, b| a.0.cmp(&b.0));
    let exact = |oid: &ObjectId| -> Option<String> {
        by_oid
            .binary_search_by(|probe| probe.0.cmp(oid))
            .ok()
            .map(|ix| by_oid[ix].1.to_string())
    };

    if annotate_stdin {
        annotate(&mut out, hexsz, name_only, &names, &exact)?;
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    for (caller, oid, kind) in &targets {
        if !name_only {
            write!(out, "{caller} ")?;
        }
        let name = if *kind == Kind::Commit {
            names.get(oid).map(render_name)
        } else {
            exact(oid)
        };
        match name {
            Some(name) => writeln!(out, "{name}")?,
            None if allow_undefined => writeln!(out, "undefined")?,
            None if always => {
                let short = oid.attach(&repo).shorten_or_id();
                writeln!(out, "{short}")?;
            }
            None => {
                out.flush()?;
                eprintln!("fatal: cannot describe '{oid}'");
                return Ok(ExitCode::from(128));
            }
        }
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Build the tip table: one entry per ref that survives the `--tags`,
/// `--exclude` and `--refs` filters, mirroring git's `name_ref`.
fn collect_tips(
    repo: &gix::Repository,
    tags_only: bool,
    name_only: bool,
    ref_filters: &[String],
    exclude_filters: &[String],
) -> Result<Vec<Tip>> {
    // `shorten_unambiguous` needs to test candidate ref names for existence, so
    // materialise the full set of ref names up front.
    let mut all_names: HashSet<String> = HashSet::new();
    for reference in repo.references()?.all()? {
        let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
        all_names.insert(reference.name().as_bstr().to_string());
    }

    let mut tips = Vec::new();
    for reference in repo.references()?.all()? {
        let mut reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
        let full = reference.name().as_bstr().to_string();
        let is_tag_ref = full.starts_with("refs/tags/");

        if tags_only && !is_tag_ref {
            continue;
        }
        if exclude_filters
            .iter()
            .any(|f| subpath_matches(&full, f).is_some())
        {
            continue;
        }

        // `--tags --name-only` prints bare tag names; so does a --refs pattern
        // that matched a sub-path rather than the whole ref name.
        let mut can_abbreviate = tags_only && name_only;
        if !ref_filters.is_empty() {
            let mut matched = false;
            for f in ref_filters {
                // Every pattern is checked even after a match, so that a pattern
                // matching a sub-path can still unlock the abbreviated form.
                match subpath_matches(&full, f) {
                    None => {}
                    Some(0) => matched = true,
                    Some(_) => {
                        matched = true;
                        can_abbreviate = true;
                    }
                }
            }
            if !matched {
                continue;
            }
        }

        // Symbolic refs are followed; refs whose object is missing are still
        // recorded (git keeps them in the tip table with no commit).
        let Ok(id) = reference.follow_to_object() else {
            continue;
        };
        let oid = id.detach();

        // Peel the tag chain, remembering the innermost tagger date.
        let mut taggerdate = TIME_MAX;
        let mut deref = false;
        let mut peeled = repo.find_object(oid).ok();
        while matches!(&peeled, Some(o) if o.kind == Kind::Tag) {
            let object = peeled.take().expect("matched Some just above");
            let Ok(tag) = gix::objs::TagRef::from_bytes(&object.data, object.id.kind()) else {
                break;
            };
            taggerdate = tag.tagger().ok().flatten().map_or(0, |t| t.seconds());
            deref = true;
            peeled = repo.find_object(tag.target()).ok();
        }

        let mut commit = None;
        let mut from_tag = false;
        if let Some(object) = &peeled {
            if object.kind == Kind::Commit {
                commit = Some(object.id);
                from_tag = is_tag_ref;
                if taggerdate == TIME_MAX {
                    taggerdate = gix::objs::CommitRef::from_bytes(&object.data, object.id.kind())
                        .ok()
                        .and_then(|c| c.committer().ok().map(|s| s.seconds()))
                        .unwrap_or(0);
                }
            }
        }

        let refname = if can_abbreviate {
            shorten_unambiguous(&full, &all_names)
        } else if let Some(short) = full.strip_prefix("refs/heads/") {
            short.to_string()
        } else {
            full.strip_prefix("refs/").unwrap_or(&full).to_string()
        };

        tips.push(Tip {
            oid,
            refname,
            commit,
            taggerdate,
            from_tag,
            deref,
        });
    }
    Ok(tips)
}

/// Spread `tip`'s name backwards through history — git's `name_rev`.
///
/// A LIFO stack with the parents re-pushed in reverse gives the first parent
/// priority, which is what makes `~<n>` chains follow the mainline.
fn walk_from_tip(
    repo: &gix::Repository,
    cache: &mut HashMap<ObjectId, Rc<CommitInfo>>,
    names: &mut HashMap<ObjectId, RevName>,
    start: ObjectId,
    tip: &Tip,
    cutoff: i64,
) {
    if commit_info(repo, cache, start).date < cutoff {
        return;
    }
    if !is_better_name(names.get(&start), tip.taggerdate, 0, 0, tip.from_tag) {
        return;
    }
    let tip_name: Rc<str> = if tip.deref {
        format!("{}^0", tip.refname).into()
    } else {
        Rc::from(tip.refname.as_str())
    };
    names.insert(
        start,
        RevName {
            tip_name,
            taggerdate: tip.taggerdate,
            generation: 0,
            distance: 0,
            from_tag: tip.from_tag,
        },
    );

    let mut stack = vec![start];
    let mut pending: Vec<ObjectId> = Vec::new();
    while let Some(commit) = stack.pop() {
        let Some(name) = names.get(&commit) else {
            continue;
        };
        let (cur_tip, cur_gen, cur_dist) = (name.tip_name.clone(), name.generation, name.distance);
        let parents = commit_info(repo, cache, commit).parents.clone();

        pending.clear();
        for (ix, parent) in parents.iter().enumerate() {
            let parent_number = ix as i32 + 1;
            if commit_info(repo, cache, *parent).date < cutoff {
                continue;
            }
            let (generation, distance) = if parent_number > 1 {
                (0, cur_dist.saturating_add(MERGE_TRAVERSAL_WEIGHT))
            } else {
                (cur_gen.saturating_add(1), cur_dist.saturating_add(1))
            };
            if !is_better_name(
                names.get(parent),
                tip.taggerdate,
                generation,
                distance,
                tip.from_tag,
            ) {
                continue;
            }
            let parent_tip: Rc<str> = if parent_number > 1 {
                parent_name(&cur_tip, cur_gen, parent_number).into()
            } else {
                cur_tip.clone()
            };
            names.insert(
                *parent,
                RevName {
                    tip_name: parent_tip,
                    taggerdate: tip.taggerdate,
                    generation,
                    distance,
                    from_tag: tip.from_tag,
                },
            );
            pending.push(*parent);
        }

        // "The first parent must come out first from the stack."
        while let Some(p) = pending.pop() {
            stack.push(p);
        }
    }
}

/// git's `effective_distance`: any non-zero generation costs a merge traversal.
fn effective_distance(distance: i32, generation: i32) -> i32 {
    distance.saturating_add(if generation > 0 {
        MERGE_TRAVERSAL_WEIGHT
    } else {
        0
    })
}

/// Whether the candidate name beats `existing` — git's `is_better_name`, with
/// `None` standing in for "no name yet" (always better).
fn is_better_name(
    existing: Option<&RevName>,
    taggerdate: i64,
    generation: i32,
    distance: i32,
    from_tag: bool,
) -> bool {
    let Some(name) = existing else {
        return true;
    };
    let old = effective_distance(name.distance, name.generation);
    let new = effective_distance(distance, generation);

    // If both are tags, we prefer the nearer one.
    if from_tag && name.from_tag {
        return old > new;
    }
    // Favor a tag over a non-tag.
    if name.from_tag != from_tag {
        return from_tag;
    }
    // Two non-tags: favor shorter hops, then the older date, else keep the current.
    if old != new {
        return old > new;
    }
    if name.taggerdate != taggerdate {
        return name.taggerdate > taggerdate;
    }
    false
}

/// git's `get_parent_name`: the name a non-first parent inherits, `<base>^<n>`
/// (with the mainline hops folded in as `~<gen>` when there are any).
fn parent_name(tip_name: &str, generation: i32, parent_number: i32) -> String {
    let base = tip_name.strip_suffix("^0").unwrap_or(tip_name);
    if generation > 0 {
        format!("{base}~{generation}^{parent_number}")
    } else {
        format!("{base}^{parent_number}")
    }
}

/// git's `get_rev_name` for a commit: the tip name plus the `~<n>` hop count.
fn render_name(name: &RevName) -> String {
    if name.generation == 0 {
        name.tip_name.to_string()
    } else {
        let base = name.tip_name.strip_suffix("^0").unwrap_or(&name.tip_name);
        format!("{base}~{}", name.generation)
    }
}

/// Peel `oid` through any chain of tag objects and return the commit it names.
fn peel_to_commit(repo: &gix::Repository, oid: ObjectId) -> Option<ObjectId> {
    let mut current = repo.find_object(oid).ok()?;
    loop {
        match current.kind {
            Kind::Commit => return Some(current.id),
            Kind::Tag => {
                let target = gix::objs::TagRef::from_bytes(&current.data, current.id.kind())
                    .ok()?
                    .target();
                current = repo.find_object(target).ok()?;
            }
            _ => return None,
        }
    }
}

/// Read (and memoise) the committer date and parents of `oid`. Objects that are
/// missing or undecodable behave like git's failed `parse_commit`: date 0, no
/// parents.
fn commit_info(
    repo: &gix::Repository,
    cache: &mut HashMap<ObjectId, Rc<CommitInfo>>,
    oid: ObjectId,
) -> Rc<CommitInfo> {
    if let Some(hit) = cache.get(&oid) {
        return hit.clone();
    }
    let info = repo
        .find_object(oid)
        .ok()
        .filter(|o| o.kind == Kind::Commit)
        .and_then(|o| {
            let commit = gix::objs::CommitRef::from_bytes(&o.data, oid.kind()).ok()?;
            Some(CommitInfo {
                date: commit.committer().ok().map_or(0, |s| s.seconds()),
                parents: commit.parents().collect(),
            })
        })
        .unwrap_or_else(|| CommitInfo {
            date: 0,
            parents: Vec::new(),
        });
    let info = Rc::new(info);
    cache.insert(oid, info.clone());
    info
}

/// git's `subpath_matches`: try `filter` against `path` and against every
/// sub-path starting after a `/`, returning the offset of the first match.
///
/// Patterns are matched with `wildmatch` in git's default mode, where `*`
/// crosses `/` freely.
fn subpath_matches(path: &str, filter: &str) -> Option<usize> {
    let mut offset = 0usize;
    loop {
        let sub = &path[offset..];
        if gix::glob::wildmatch(
            filter.as_bytes().as_bstr(),
            sub.as_bytes().as_bstr(),
            gix::glob::wildmatch::Mode::empty(),
        ) {
            return Some(offset);
        }
        match sub.find('/') {
            Some(ix) => offset += ix + 1,
            None => return None,
        }
    }
}

/// git's `refs_shorten_unambiguous_ref`: the shortest suffix of `refname` that
/// no earlier rev-parse rule would resolve to a different existing ref.
fn shorten_unambiguous(refname: &str, all_names: &HashSet<String>) -> String {
    // Rule 0 always matches, so it is never a candidate; scan from the most
    // specific rule down, which yields the shortest name first.
    //
    // git matches rules with `sscanf`, whose greedy `%s` makes the last rule
    // shorten `refs/remotes/origin/HEAD` to `origin/HEAD` rather than `origin`.
    // Anchored suffix matching is used here instead. The two only diverge for
    // `refs/remotes/*/HEAD`, which is unreachable under `--tags`.
    for i in (1..REV_PARSE_RULES.len()).rev() {
        let (prefix, suffix) = REV_PARSE_RULES[i];
        let Some(rest) = refname.strip_prefix(prefix) else {
            continue;
        };
        let short = if suffix.is_empty() {
            rest
        } else {
            match rest.strip_suffix(suffix) {
                Some(s) => s,
                None => continue,
            }
        };
        if short.is_empty() {
            continue;
        }
        let ambiguous = REV_PARSE_RULES[..i]
            .iter()
            .any(|(p, s)| all_names.contains(&format!("{p}{short}{s}")));
        if !ambiguous {
            return short.to_string();
        }
    }
    refname.to_string()
}

/// git's `--annotate-stdin`: rewrite every standalone full-length lowercase hex
/// object name on stdin as `<hex> (<name>)`, or as `<name>` under `--name-only`.
///
/// Only object ids git would have parsed get substituted; here that is exactly
/// the commits the naming walk reached, plus non-commit ref tips.
fn annotate<W: Write>(
    out: &mut W,
    hexsz: usize,
    name_only: bool,
    names: &HashMap<ObjectId, RevName>,
    exact: &dyn Fn(&ObjectId) -> Option<String>,
) -> Result<()> {
    let ishex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut raw = Vec::new();
    loop {
        raw.clear();
        if reader.read_until(b'\n', &mut raw)? == 0 {
            break;
        }
        // git reads with `strbuf_getline` (dropping a trailing CRLF or LF) and
        // then appends a single LF, so line endings are normalised.
        if raw.last() == Some(&b'\n') {
            raw.pop();
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
        }
        raw.push(b'\n');

        let mut counter = 0usize;
        let mut start = 0usize;
        for i in 0..raw.len() {
            if !ishex(raw[i]) {
                counter = 0;
                continue;
            }
            counter += 1;
            if counter != hexsz || raw.get(i + 1).is_some_and(|&b| ishex(b)) {
                continue;
            }
            counter = 0;

            let hex = &raw[i + 1 - hexsz..=i];
            let Ok(oid) = ObjectId::from_hex(hex) else {
                continue;
            };
            let Some(name) = names.get(&oid).map(render_name).or_else(|| exact(&oid)) else {
                continue;
            };
            if name_only {
                // Drop the hex itself, keeping only the text that preceded it.
                out.write_all(&raw[start..i + 1 - hexsz])?;
                out.write_all(name.as_bytes())?;
            } else {
                out.write_all(&raw[start..=i])?;
                write!(out, " ({name})")?;
            }
            start = i + 1;
        }
        out.write_all(&raw[start..])?;
    }
    Ok(())
}
